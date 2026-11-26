//! The egress boundary (spec 3.7 §7.8): [`ReleasedImage`], the only image a remote backend
//! accepts, and the [`EgressGate`], the only code that constructs one.
//!
//! The gate runs six steps — destination, minimise, redact, decide, record, release. With
//! no rules an image leaves as it is; a class the rules list is always redacted before the
//! decision, so no configuration releases an image before its listed classes are covered
//! (V-I2). A `ReleasedImage` has no public constructor (V-I1), and neither has
//! a [`ReleasedClip`], the video counterpart a remote video analyzer takes.

mod clip;
mod config;
mod gate;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use clip::{ClipReleaseRequest, ReleasedClip};
pub use config::{EgressAction, EgressConfig, EgressRuleConfig, RuleTarget, VisionConfig};
pub use gate::{EgressGate, EgressPreview, ReleaseRequest, StagedEgress};

use crate::backend::BackendId;
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::Result;
use crate::geometry::{BBox, RegionSet, TransformChain};
use crate::media::DecodedImage;
use crate::sent::SentImage;

/// The kind of data an image is, which egress rules are written per: `document_crop`,
/// `photo`, `screen`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DataClass(String);

impl DataClass {
    /// A data class.
    pub fn new(class: impl Into<String>) -> Self {
        Self(class.into())
    }

    /// The class as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DataClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for DataClass {
    fn from(class: &str) -> Self {
        Self::new(class)
    }
}

/// A class of content the gate redacts before release: `faces`, `personal_text`,
/// `schema_personal_fields`, or `marked` for regions the caller names.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RedactionClass(String);

impl RedactionClass {
    /// A redaction class.
    pub fn new(class: impl Into<String>) -> Self {
        Self(class.into())
    }

    /// Faces: the class an egress rule requires. `FaceRedactor` covers each face a face
    /// detector finds, and each person it finds no face for as the whole person;
    /// `PersonRedactor` covers every detected person's whole box (spec 3.7 Q3).
    pub fn faces() -> Self {
        Self::new("faces")
    }

    /// A person's whole box, covering a face that a redactor of [`faces`](Self::faces) could
    /// not place: no face detector runs, or the person shows no face it can find. Boxes are
    /// drawn and counted on captions as this class; receipts count them under `faces`.
    pub fn people() -> Self {
        Self::new("people")
    }

    /// Personal text found by local OCR and the PII patterns.
    pub fn personal_text() -> Self {
        Self::new("personal_text")
    }

    /// Fields a document schema marks personal.
    pub fn schema_personal_fields() -> Self {
        Self::new("schema_personal_fields")
    }

    /// Regions the caller marked for redaction.
    pub fn marked() -> Self {
        Self::new("marked")
    }

    /// The class as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RedactionClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Finds the regions of one [`RedactionClass`] in an image, on the device.
///
/// A redactor sees the minimised image the gate is about to release and answers in its
/// pixels; the gate fills every box with solid colour, because blur and pixelation of text
/// can be reversed.
#[async_trait]
pub trait Redactor: Send + Sync {
    /// The class this redactor finds.
    fn class(&self) -> RedactionClass;

    /// The boxes to fill, in `image`'s pixels.
    async fn find(&self, cx: &VisionCx, image: &SentImage) -> Result<Vec<BBox>>;

    /// The boxes to fill, each with the class it is drawn as on an approval card's overlay
    /// and named as on its caption: by default every box of [`find`](Self::find) as
    /// [`class`](Self::class). A faces redactor names the whole-person boxes it falls back to
    /// [`RedactionClass::people`], so a card says which faces were covered as the whole
    /// person. The receipt counts every box under [`class`](Self::class).
    ///
    /// # Errors
    ///
    /// As [`find`](Self::find).
    async fn find_classified(
        &self,
        cx: &VisionCx,
        image: &SentImage,
    ) -> Result<Vec<(RedactionClass, BBox)>> {
        let class = self.class();
        Ok(self.find(cx, image).await?.into_iter().map(|bbox| (class.clone(), bbox)).collect())
    }
}

/// How a release was decided.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum EgressDecision {
    /// A rule, or the default, allows this destination and data class.
    Allowed {
        /// The index of the matching `[[egress.rule]]`, or `None` for the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<usize>,
    },
    /// The rule asks a person, and a person approved this exact egress digest.
    Approved {
        /// The index of the matching rule, or `None` for the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<usize>,
    },
}

/// What left the machine: written before the request is sent (V-I3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressReceipt {
    /// A unique identifier for this release.
    pub id: String,
    /// The source image's digest.
    pub source: Digest,
    /// The digest of the exact bytes released.
    pub egress: Digest,
    /// The backend the image is released to.
    pub backend: BackendId,
    /// The destination hosts.
    pub hosts: Vec<String>,
    /// The data class.
    pub data_class: DataClass,
    /// The transforms from the source to the released image, redaction included.
    pub chain: TransformChain,
    /// Boxes filled, by redaction class.
    pub redactions: BTreeMap<RedactionClass, u32>,
    /// How the release was decided.
    pub decision: EgressDecision,
    /// The estimated cost of the call, when the backend is priced per unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_micro_usd: Option<u64>,
    /// The artifact the egress copy is stored in, when the gate has a media store and the
    /// call a media scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<crate::media::ArtifactLocator>,
    /// When the image was released.
    pub released_at: DateTime<Utc>,
}

/// Why an image was not released.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressRefusal {
    /// The source image's digest.
    pub source: Digest,
    /// The digest of the redacted image, when the gate got that far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<Digest>,
    /// The backend.
    pub backend: BackendId,
    /// The destination hosts.
    pub hosts: Vec<String>,
    /// The data class.
    pub data_class: DataClass,
    /// The step that refused: `destination`, `minimise`, `redact`, `decide`.
    pub step: String,
    /// Why.
    pub reason: String,
    /// When.
    pub refused_at: DateTime<Utc>,
}

/// One entry in the egress record: `vision.egress.released` or `vision.egress.refused`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum EgressRecord {
    /// An image was released.
    Released(EgressReceipt),
    /// An image was not released.
    Refused(EgressRefusal),
}

impl EgressRecord {
    /// The audit action: `vision.egress.released` or `vision.egress.refused`.
    pub fn action(&self) -> &'static str {
        match self {
            Self::Released(_) => "vision.egress.released",
            Self::Refused(_) => "vision.egress.refused",
        }
    }
}

/// Where the gate writes what left the machine and what did not.
///
/// The gate writes a release before it returns the [`ReleasedImage`]; a recorder that
/// fails refuses the release.
#[async_trait]
pub trait EgressRecorder: Send + Sync {
    /// Records one entry.
    async fn record(&self, record: EgressRecord) -> Result<()>;
}

/// An [`EgressRecorder`] that keeps entries in memory, for tests and for proving that a
/// run sent nothing off the machine.
#[derive(Debug, Default)]
pub struct InMemoryEgressRecorder {
    records: Mutex<Vec<EgressRecord>>,
}

impl InMemoryEgressRecorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every entry, in order.
    pub fn records(&self) -> Vec<EgressRecord> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The releases, in order.
    pub fn released(&self) -> Vec<EgressReceipt> {
        self.records()
            .into_iter()
            .filter_map(|r| match r {
                EgressRecord::Released(receipt) => Some(receipt),
                EgressRecord::Refused(_) => None,
            })
            .collect()
    }

    /// `true` when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
}

#[async_trait]
impl EgressRecorder for InMemoryEgressRecorder {
    async fn record(&self, record: EgressRecord) -> Result<()> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).push(record);
        Ok(())
    }
}

/// An image the egress gate released to one backend: the minimised, redacted pixels, the
/// exact bytes sent, and the receipt the gate recorded.
///
/// The only input a remote backend accepts; only [`EgressGate`] constructs one (V-I1):
///
/// ```compile_fail,E0599
/// use adk_vision::ReleasedImage;
///
/// let forged = ReleasedImage::new();
/// ```
#[derive(Debug)]
pub struct ReleasedImage {
    image: SentImage,
    bytes: Arc<[u8]>,
    receipt: EgressReceipt,
}

impl ReleasedImage {
    pub(crate) fn from_gate(image: SentImage, bytes: Arc<[u8]>, receipt: EgressReceipt) -> Self {
        Self { image, bytes, receipt }
    }

    /// The released pixels and their transform chain from the source.
    pub fn image(&self) -> &SentImage {
        &self.image
    }

    /// The released pixels.
    pub fn pixels(&self) -> &DecodedImage {
        self.image.pixels()
    }

    /// The exact PNG bytes released; their digest is the receipt's `egress`.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `image/png`.
    pub fn mime_type(&self) -> &'static str {
        "image/png"
    }

    /// The receipt the gate recorded.
    pub fn receipt(&self) -> &EgressReceipt {
        &self.receipt
    }

    /// `true` when this image was released to `backend`.
    pub fn is_for(&self, backend: &BackendId) -> bool {
        &self.receipt.backend == backend
    }

    /// A model part for the released bytes, annotated with the release so the privacy
    /// guardrail knows it already passed the gate.
    pub fn to_part(&self) -> adk_core::Part {
        adk_core::Part::InlineData {
            mime_type: self.mime_type().to_string(),
            data: self.bytes.to_vec(),
            uri: None,
            annotations: Some(serde_json::json!({
                "adkVision": {
                    "digest": self.receipt.egress,
                    "release": { "id": self.receipt.id, "source": self.receipt.source },
                }
            })),
        }
    }
}

/// What an approval card shows for a held release: the exact image that would leave,
/// after redaction, with its digest, destination and what was redacted.
///
/// Maps to `adk_core::evidence::ImageEvidence` once approvals carry evidence (spec 3.7 X4):
/// the gate stores the egress copy as an artifact, and the evidence names its locator,
/// with this overlay as `with_regions` and this caption as `with_caption`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressEvidence {
    /// The digest of the image that would leave.
    pub digest: Digest,
    /// `image/png`.
    pub mime_type: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Regions to draw over the image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<RegionSet>,
    /// A one-line caption: destination, data class, redactions.
    pub caption: String,
}
