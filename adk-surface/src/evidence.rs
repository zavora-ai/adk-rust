//! Approve with the image (spec 3.7 §7.8, §8.2, X11): a held call that sends, keeps or acts
//! on an image shows that exact image on its card.
//!
//! A held call carries its images as [`adk_core::Evidence`] — each image's digest, its
//! artifact, its size and an overlay of the regions the egress gate covered — and its
//! fingerprint binds the digests, so a decision applies to the images the approver saw and no
//! other. The engine projects that evidence into [`CardMedia`] beside the card's call:
//!
//! | Part | From |
//! |---|---|
//! | The image, its size and digest, and its regions | The inbox entry's evidence |
//! | What was covered, by class, and whether everything was | The regions the gate filled |
//! | Where it goes, what it is, the estimated cost, what it was cut from | A feature's [`MediaFacts`], through a [`CardHint`](crate::CardHint) |
//! | Kept until, or deleted | The source ([`Source::evidence_kept`](crate::Source::evidence_kept)) and the retention |
//!
//! The image itself never travels in a card: a surface loads it from the source with the
//! viewer's own authorisation (`GET /api/surface/decisions/{id}/evidence/{index}` on the web
//! home), and the source checks its digest before serving it.
//!
//! What a surface may answer follows the boards' choices (A13, A15, A17, A18):
//!
//! | Situation | Answers |
//! |---|---|
//! | The image is deleted, or every pixel of it is covered | Deny only, on every surface: there is nothing to send |
//! | Voice | Deny only: voice cannot show the image, so it cannot say yes to sending it |
//! | Chat and the tray, through the engine | Deny and "Open in home"; a chat adapter that attaches the image offers Allow once itself |
//! | The web home, the phone, the terminal | As for any card, without **Always allow** |
//!
//! # Example
//!
//! ```rust
//! use adk_surface::{ImageFacts, MediaFacts};
//!
//! let facts = MediaFacts::new()
//!     .to("Claude", "claude-sonnet-5-5", ["api.anthropic.com"])
//!     .with_data_class("document_crop")
//!     .with_estimated_cost(4_400, Some("851 image tokens".into()))
//!     .with_cut_from("Page 2 of claim-0412.pdf")
//!     .with_image(ImageFacts::new().with_field("Document kind"));
//! assert_eq!(facts.hosts, vec!["api.anthropic.com".to_string()]);
//! ```

use chrono::{DateTime, Duration, FixedOffset, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::words;

/// How many days an image held as evidence is kept, unless a feature says otherwise
/// (spec 3.7 Q13).
pub const EVIDENCE_RETENTION_DAYS: u32 = 30;

/// The share of an image's pixels that counts as all of it, so rounding at the edges of a
/// covering box never leaves a "nothing to send" image allowable.
const ALL_COVERED: f64 = 0.995;

// ---------------------------------------------------------------------------
// What a feature says
// ---------------------------------------------------------------------------

/// What a feature knows about the images one of its calls would send, which the evidence
/// alone does not say: where they go, what they are, the estimated cost and what they were cut
/// from (spec 3.7 §8.2). A [`CardHint`](crate::CardHint) carries it.
///
/// # Example
///
/// ```rust
/// use adk_surface::{CardHint, MediaFacts};
///
/// let hint = CardHint::new().with_media(
///     MediaFacts::new().to("Gemini", "gemini-3.8-flash", ["generativelanguage.googleapis.com"]),
/// );
/// assert!(hint.media.is_some());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaFacts {
    /// Who receives the images, as a person knows them: "Claude".
    pub provider: Option<String>,
    /// The model: `claude-sonnet-5-5`.
    pub model: Option<String>,
    /// The hosts the images go to: `api.anthropic.com`.
    pub hosts: Vec<String>,
    /// The data class the images are released as, such as `document_crop`.
    pub data_class: Option<String>,
    /// The estimated cost of the call in micro-USD: the preflight's hold.
    pub estimated_cost_micro_usd: Option<u64>,
    /// What the estimate counts: "851 image tokens".
    pub estimate_detail: Option<String>,
    /// What the call cost, once it ran.
    pub actual_cost_micro_usd: Option<u64>,
    /// What the actual cost counts: "851 image tokens, 63 answer tokens".
    pub actual_detail: Option<String>,
    /// What the images were cut from: "Page 2 of claim-0412.pdf".
    pub cut_from: Option<String>,
    /// How they were cut: "nothing resized".
    pub cut_note: Option<String>,
    /// Where an authorised viewer opens the source image: a path in the home. Absent when the
    /// viewer may not (board choice A9).
    pub source_href: Option<String>,
    /// How many days the images are kept as evidence; [`EVIDENCE_RETENTION_DAYS`] when unset.
    pub retention_days: Option<u32>,
    /// One entry per image, in the order of the evidence.
    pub images: Vec<ImageFacts>,
}

impl MediaFacts {
    /// Facts that say nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// The images go to `provider`'s `model` at `hosts`.
    #[must_use]
    pub fn to(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
        hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self.hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// The images are released as `class`, such as `document_crop`.
    #[must_use]
    pub fn with_data_class(mut self, class: impl Into<String>) -> Self {
        self.data_class = Some(class.into());
        self
    }

    /// The preflight estimates `micro_usd`, counting `detail`.
    #[must_use]
    pub fn with_estimated_cost(mut self, micro_usd: u64, detail: Option<String>) -> Self {
        self.estimated_cost_micro_usd = Some(micro_usd);
        self.estimate_detail = detail;
        self
    }

    /// The call cost `micro_usd`, counting `detail`.
    #[must_use]
    pub fn with_actual_cost(mut self, micro_usd: u64, detail: Option<String>) -> Self {
        self.actual_cost_micro_usd = Some(micro_usd);
        self.actual_detail = detail;
        self
    }

    /// The images were cut from `source`.
    #[must_use]
    pub fn with_cut_from(mut self, source: impl Into<String>) -> Self {
        self.cut_from = Some(source.into());
        self
    }

    /// How they were cut: "nothing resized".
    #[must_use]
    pub fn with_cut_note(mut self, note: impl Into<String>) -> Self {
        self.cut_note = Some(note.into());
        self
    }

    /// An authorised viewer opens the source image at `href`, a path in the home.
    #[must_use]
    pub fn with_source_href(mut self, href: impl Into<String>) -> Self {
        self.source_href = Some(href.into());
        self
    }

    /// The images are kept as evidence for `days`.
    #[must_use]
    pub fn with_retention_days(mut self, days: u32) -> Self {
        self.retention_days = Some(days);
        self
    }

    /// Adds the facts of the next image.
    #[must_use]
    pub fn with_image(mut self, image: ImageFacts) -> Self {
        self.images.push(image);
        self
    }

    /// These facts with `later`'s where these set none.
    pub(crate) fn then(self, later: Self) -> Self {
        Self {
            provider: self.provider.or(later.provider),
            model: self.model.or(later.model),
            hosts: if self.hosts.is_empty() { later.hosts } else { self.hosts },
            data_class: self.data_class.or(later.data_class),
            estimated_cost_micro_usd: self
                .estimated_cost_micro_usd
                .or(later.estimated_cost_micro_usd),
            estimate_detail: self.estimate_detail.or(later.estimate_detail),
            actual_cost_micro_usd: self.actual_cost_micro_usd.or(later.actual_cost_micro_usd),
            actual_detail: self.actual_detail.or(later.actual_detail),
            cut_from: self.cut_from.or(later.cut_from),
            cut_note: self.cut_note.or(later.cut_note),
            source_href: self.source_href.or(later.source_href),
            retention_days: self.retention_days.or(later.retention_days),
            images: if self.images.is_empty() { later.images } else { self.images },
        }
    }
}

/// What a feature knows about one image of a call: the field it shows and why it was sent.
///
/// # Example
///
/// ```rust
/// use adk_surface::ImageFacts;
///
/// let po = ImageFacts::new().with_field("PO number").with_why("Handwriting");
/// assert_eq!(po.why.as_deref(), Some("Handwriting"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFacts {
    /// The field the image shows: "PO number".
    pub field: Option<String>,
    /// Why it is sent rather than read here: "Handwriting".
    pub why: Option<String>,
}

impl ImageFacts {
    /// Facts that say nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// The image shows `field`.
    #[must_use]
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    /// The image is sent because of `why`.
    #[must_use]
    pub fn with_why(mut self, why: impl Into<String>) -> Self {
        self.why = Some(why.into());
        self
    }
}

/// Whether an evidence image is still kept, as the source knows it.
///
/// # Example
///
/// ```rust
/// use adk_surface::EvidenceKept;
///
/// assert_ne!(EvidenceKept::Kept, EvidenceKept::Deleted);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceKept {
    /// The image is stored.
    Kept,
    /// The image is gone: its retention ran out, or it went with its source.
    Deleted,
    /// The source cannot tell: the card shows the image and the image route says.
    Unknown,
}

// ---------------------------------------------------------------------------
// What the card carries
// ---------------------------------------------------------------------------

/// What a region on an evidence image covers, by the class the egress gate redacted it as
/// (spec 3.7 §7.8 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CoverClass {
    /// A face, or the whole person where no face was found (spec 3.7 Q3).
    Face,
    /// Personal text: names, emails, phone numbers and numbers like them.
    PersonalText,
    /// A field the document schema marks personal.
    SchemaField,
    /// A box the person covered.
    Marked,
    /// Another class a feature names.
    Other,
}

/// A region drawn over an evidence image, in the image's own pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRegion {
    /// The class.
    pub class: CoverClass,
    /// The region's tag on a large picture: "face", or "face · whole person" for a person
    /// covered whole.
    pub tag: String,
    /// The tag on a small picture: "face".
    pub short_tag: String,
    /// Left edge, pixels.
    pub x0: f64,
    /// Top edge, pixels.
    pub y0: f64,
    /// Right edge, pixels, exclusive.
    pub x1: f64,
    /// Bottom edge, pixels, exclusive.
    pub y1: f64,
}

/// What was covered of one class, as a line of "Covered before it leaves".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoveredLine {
    /// The class.
    pub class: CoverClass,
    /// How many regions.
    pub count: u32,
    /// "1 face", "3 personal text", "2 fields your schema marks personal".
    pub text: String,
    /// Which images, when the call sends several and not all of them: "in image 3".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// An evidence image that is no longer kept (board choice A13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurgedView {
    /// "This image was deleted on 18 Dec".
    pub title: String,
    /// "Evidence is kept for 30 days. What it was, …".
    pub text: String,
    /// The day it went, "18 Dec", when its retention ran out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    /// The shorter line a small picture shows: "Image deleted 18 Dec".
    pub short: String,
}

/// One image a held call would send, as its card shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceImage {
    /// Its place in the call's evidence: what the image route takes.
    pub index: u32,
    /// `sha256:<hex>` of the image's bytes, in full: the exact call and the terminal show it.
    pub digest: String,
    /// The digest in short, `sha256:c52ca5…ce7550` (board choice S4).
    pub digest_short: String,
    /// `image/png`.
    pub mime_type: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// "1012 × 638 PNG".
    pub size_text: String,
    /// Where the surface loads the image with the viewer's own authorisation; absent when it
    /// is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// The regions over it, covered before it leaves.
    pub regions: Vec<EvidenceRegion>,
    /// Every pixel of it is covered.
    pub covered_all: bool,
    /// What was covered of it, in one line: "1 field your schema marks personal", "nothing
    /// covered".
    pub covered_text: String,
    /// The feature's one-line description of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// The field it shows: "PO number".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Why it is sent rather than read here: "Handwriting".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// It is no longer kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purged: Option<PurgedView>,
}

/// A notice on the card about its images: why nothing can be sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaNotice {
    /// Why it says so.
    pub kind: MediaNoticeKind,
    /// The lead words, in bold: "Nothing to send."
    pub title: String,
    /// The rest.
    pub text: String,
}

/// Why a card's images cannot be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MediaNoticeKind {
    /// The image was deleted (board choice A13).
    Purged,
    /// Every pixel is covered (board choice A15).
    AllCovered,
}

/// Where the images go, in words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoesTo {
    /// Who receives them: "Claude".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model: `claude-sonnet-5-5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The hosts, joined: `api.anthropic.com`.
    pub host: String,
}

/// A held call's images on its card (spec 3.7 §8.2, X11): what would leave, after redaction,
/// with what was covered, where it goes, what it costs, how long it is kept and why nothing
/// can be sent when that is so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardMedia {
    /// "The exact image that would leave", "The 3 images that would leave", "The image that
    /// left".
    pub title: String,
    /// The images, in the order of the call's evidence.
    pub images: Vec<EvidenceImage>,
    /// Where they go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goes_to: Option<GoesTo>,
    /// What they are: "Document crop", "3 document crops".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub what: Option<String>,
    /// What qualifies `what`: "never the page".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub what_note: Option<String>,
    /// What was covered before they leave, by class.
    pub covered: Vec<CoveredLine>,
    /// Every pixel of every image is covered.
    pub covered_all: bool,
    /// "100% of the image", when so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_all_text: Option<String>,
    /// "Up to $0.0044" while waiting; "$0.0024" once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_text: Option<String>,
    /// What the cost counts: "851 image tokens".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_detail: Option<String>,
    /// What they were cut from: "Page 2 of claim-0412.pdf".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_from: Option<String>,
    /// How: "nothing resized".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_note: Option<String>,
    /// "Kept as evidence until 18 Dec".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_text: Option<String>,
    /// Where an authorised viewer opens the source image (board choice A9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_href: Option<String>,
    /// Why nothing can be sent, when that is so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<MediaNotice>,
    /// This surface may tighten the crop or add a redaction, which holds the call again with
    /// the new image (board choices A7, A8).
    pub can_change: bool,
    /// The digest of the image a changed image replaced, when this call was held again with
    /// a changed image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// The fixed line under the card (board choice A21).
    pub fixed_line: String,
}

impl CardMedia {
    /// Something on the card can be sent: every image is kept and not wholly covered.
    pub fn sendable(&self) -> bool {
        self.notice.is_none()
    }
}

/// A change to an evidence image before it leaves (board choices A7, A8): the crop can only
/// shrink and covering can only grow, so a change never shows more than the image it changes.
///
/// # Example
///
/// ```rust
/// use adk_surface::ImageChange;
///
/// let change = ImageChange { image: 0, crop: Some([0, 0, 1012, 450]), cover: vec![[592, 476, 760, 506]] };
/// assert!(change.check(1012, 638).is_ok());
/// let wider = ImageChange { image: 0, crop: Some([0, 0, 1100, 450]), cover: Vec::new() };
/// assert!(wider.check(1012, 638).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageChange {
    /// The image's place in the call's evidence.
    pub image: u32,
    /// The new edges `[x0, y0, x1, y1]` in the image's pixels, inside the image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<[u32; 4]>,
    /// Boxes `[x0, y0, x1, y1]` in the image's pixels to fill solid black.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cover: Vec<[u32; 4]>,
}

impl ImageChange {
    /// Checks the change against an image of `width` × `height`: the crop lies inside it and
    /// leaves something, each box lies inside it, and the change changes something.
    ///
    /// # Errors
    ///
    /// A sentence saying what is wrong, for the person.
    pub fn check(&self, width: u32, height: u32) -> std::result::Result<(), String> {
        let inside = |[x0, y0, x1, y1]: [u32; 4]| x0 < x1 && y0 < y1 && x1 <= width && y1 <= height;
        if let Some(crop) = self.crop {
            if !inside(crop) {
                return Err(format!(
                    "the crop must lie inside the {width} × {height} image and keep some of it; \
                     it can only cut the image smaller"
                ));
            }
            if crop == [0, 0, width, height] && self.cover.is_empty() {
                return Err("the crop keeps the whole image, so nothing changes".into());
            }
        }
        if let Some(bad) = self.cover.iter().find(|bbox| !inside(**bbox)) {
            return Err(format!(
                "the box {bad:?} must lie inside the {width} × {height} image and cover something"
            ));
        }
        if self.crop.is_none() && self.cover.is_empty() {
            return Err("cut the image smaller or cover part of it; nothing changes".into());
        }
        Ok(())
    }
}

/// What a change made: the image it replaces and the new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageChanged {
    /// The digest of the image the change replaces.
    pub from: String,
    /// The digest of the new image, held again for the person's decision.
    pub to: String,
    /// The new image's width.
    pub width: u32,
    /// The new image's height.
    pub height: u32,
}

/// An image a surface loads for a card, after the source checked its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceBytes {
    /// `image/png`.
    pub mime_type: String,
    /// The image's bytes, whose SHA-256 is the evidence's digest.
    pub bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// The images of an inbox entry that its card shows: image evidence, in order.
pub(crate) fn images(request: &adk_core::ApprovalRequest) -> Vec<&adk_core::ImageEvidence> {
    request
        .evidence
        .iter()
        .filter_map(|item| match item {
            adk_core::Evidence::Image(image) => Some(image),
            _ => None,
        })
        .collect()
}

/// What the projection needs beyond the inbox entry.
pub(crate) struct MediaInput<'a> {
    pub(crate) facts: Option<&'a MediaFacts>,
    pub(crate) kept: &'a [EvidenceKept],
    /// The answer, when decided: whether the images left.
    pub(crate) decided: Option<crate::model::DecidedAnswer>,
    /// The asking agent's name, as the card shows it.
    pub(crate) agent: String,
    /// The time the card is drawn and the zone its dates read in.
    pub(crate) now: DateTime<Utc>,
    pub(crate) offset: FixedOffset,
    /// The source holds a call again with a changed image.
    pub(crate) can_change: bool,
    /// The digest a changed image replaced.
    pub(crate) replaces: Option<String>,
}

/// The card's media for `request`, or `None` when it holds no image.
pub(crate) fn project(
    request: &adk_core::ApprovalRequest,
    input: &MediaInput<'_>,
) -> Option<CardMedia> {
    let evidence = images(request);
    if evidence.is_empty() {
        return None;
    }
    let facts = input.facts.cloned().unwrap_or_default();
    let retention = facts.retention_days.unwrap_or(EVIDENCE_RETENTION_DAYS);
    let kept_until = request.created_at + Duration::days(i64::from(retention));
    let kept_day = day(kept_until, input.offset);
    let several = evidence.len() > 1;

    let mut shown = Vec::with_capacity(evidence.len());
    for (index, image) in evidence.iter().enumerate() {
        let kept = input.kept.get(index).copied().unwrap_or(EvidenceKept::Unknown);
        let regions = regions_of(image);
        let covered_all = coverage(&regions, image.width, image.height) >= ALL_COVERED;
        let purged = (kept == EvidenceKept::Deleted).then(|| {
            if input.now >= kept_until {
                PurgedView {
                    title: words::image_deleted_on(&kept_day),
                    text: words::evidence_kept_for(retention),
                    day: Some(kept_day.clone()),
                    short: words::image_deleted_short(&kept_day),
                }
            } else {
                PurgedView {
                    title: words::IMAGE_DELETED.to_string(),
                    text: words::IMAGE_DELETED_EARLY.to_string(),
                    day: None,
                    short: words::IMAGE_DELETED.to_string(),
                }
            }
        });
        let image_facts = facts.images.get(index).cloned().unwrap_or_default();
        let covered_text = covered_words(&count_classes(&regions), ", ");
        shown.push(EvidenceImage {
            index: index as u32,
            digest: image.digest.clone(),
            digest_short: short_digest(&image.digest),
            mime_type: image.mime_type.clone(),
            width: image.width,
            height: image.height,
            size_text: words::image_size(image.width, image.height, &image.mime_type),
            href: purged.is_none().then(|| evidence_href(&request.id, index)),
            regions,
            covered_all,
            covered_text: if covered_text.is_empty() {
                words::NOTHING_COVERED.to_string()
            } else {
                covered_text
            },
            caption: image.caption.clone(),
            field: image_facts.field,
            why: image_facts.why,
            purged,
        });
    }

    // What was covered, by class across every image; with several images, which ones.
    let mut covered: Vec<CoveredLine> = Vec::new();
    for class in [
        CoverClass::Face,
        CoverClass::PersonalText,
        CoverClass::SchemaField,
        CoverClass::Marked,
        CoverClass::Other,
    ] {
        let per_image: Vec<u32> = shown
            .iter()
            .map(|image| image.regions.iter().filter(|region| region.class == class).count() as u32)
            .collect();
        let count: u32 = per_image.iter().sum();
        if count == 0 {
            continue;
        }
        let which: Vec<usize> =
            per_image.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, _)| i + 1).collect();
        let note = (several && which.len() < shown.len()).then(|| words::in_images(&which));
        covered.push(CoveredLine { class, count, text: words::covered_class(class, count), note });
    }

    let covered_all = shown.iter().all(|image| image.covered_all);
    let purged = shown.iter().any(|image| image.purged.is_some());
    let left = matches!(
        input.decided,
        Some(crate::model::DecidedAnswer::Yes | crate::model::DecidedAnswer::Edited)
    );
    let decided = input.decided.is_some();
    let notice = if decided {
        None
    } else if purged {
        Some(MediaNotice {
            kind: MediaNoticeKind::Purged,
            title: words::NOTHING_TO_SEND.to_string(),
            text: words::nothing_to_send_purged(&input.agent),
        })
    } else if covered_all {
        Some(MediaNotice {
            kind: MediaNoticeKind::AllCovered,
            title: words::EVERYTHING_IS_COVERED.to_string(),
            text: words::everything_covered(facts.provider.as_deref()),
        })
    } else {
        None
    };
    let title = match (left, purged, several) {
        (true, _, false) => words::THE_IMAGE_THAT_LEFT.to_string(),
        (true, _, true) => words::the_images_that_left(shown.len()),
        (false, true, false) => words::THE_IMAGE_THAT_WOULD_HAVE_LEFT.to_string(),
        (false, true, true) => words::the_images_that_would_have_left(shown.len()),
        (false, false, false) => words::THE_EXACT_IMAGE_THAT_WOULD_LEAVE.to_string(),
        (false, false, true) => words::the_images_that_would_leave(shown.len()),
    };
    let goes_to = (!facts.hosts.is_empty()).then(|| GoesTo {
        provider: facts.provider.clone(),
        model: facts.model.clone(),
        host: facts.hosts.join(", "),
    });
    let (what, what_note) = match &facts.data_class {
        Some(class) if several => {
            (Some(words::data_class_many(class, shown.len())), words::data_class_note(class))
        }
        Some(class) => (Some(words::data_class(class)), None),
        None => (None, None),
    };
    let (cost_text, cost_detail) = match facts.actual_cost_micro_usd {
        Some(actual) if decided && left => {
            (Some(words::money(actual)), facts.actual_detail.clone())
        }
        _ => match facts.estimated_cost_micro_usd {
            Some(estimate) if covered_all && !decided => {
                (Some(words::up_to_if_sent(estimate)), None)
            }
            Some(estimate) => (Some(words::up_to(estimate)), facts.estimate_detail.clone()),
            None => (None, None),
        },
    };
    let kept_text = if purged {
        None
    } else if decided {
        Some(words::as_evidence_until(&kept_day))
    } else {
        Some(words::kept_as_evidence_until(&kept_day))
    };
    let can_change = input.can_change && !decided && !purged && !covered_all && !several;
    Some(CardMedia {
        title,
        images: shown,
        goes_to,
        what,
        what_note,
        covered,
        covered_all,
        covered_all_text: covered_all.then(|| words::ALL_OF_THE_IMAGE.to_string()),
        cost_text,
        cost_detail,
        cut_from: facts.cut_from.clone(),
        cut_note: facts.cut_note.clone(),
        kept_text,
        source_href: facts.source_href.clone().filter(|href| local_path(href)),
        notice,
        can_change,
        replaces: input.replaces.clone(),
        fixed_line: words::IMAGES_LEAVE_ONLY_COVERED.to_string(),
    })
}

/// The web home's route for the `index`th image of decision `id`, under `/api/surface`.
pub(crate) fn evidence_href(id: &str, index: usize) -> String {
    format!("/api/surface/decisions/{}/evidence/{index}", percent_segment(id))
}

/// Where decision `id` opens in the web home: `/ui/decisions/<id>`.
#[cfg(feature = "tui")]
pub(crate) fn decision_path(id: &str) -> String {
    format!("/ui/decisions/{}", percent_segment(id))
}

/// A path on this server: one slash, no scheme.
fn local_path(href: &str) -> bool {
    href.starts_with('/') && !href.starts_with("//")
}

/// `id` as one path segment.
pub(crate) fn percent_segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for byte in id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `sha256:c52ca5…ce7550`: the scheme, the first six and the last six hex digits
/// (board choice S4).
///
/// # Example
///
/// ```rust
/// let digest = "sha256:c52ca5e48473595c3e5f3a67166abffb47bf1833af9c61b28f56dc769ece7550";
/// assert_eq!(adk_surface::short_digest(digest), "sha256:c52ca5…ce7550");
/// ```
pub fn short_digest(digest: &str) -> String {
    let (scheme, hex) = digest.split_once(':').unwrap_or(("", digest));
    let chars: Vec<char> = hex.chars().collect();
    if chars.len() <= 14 {
        return digest.to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 6..].iter().collect();
    if scheme.is_empty() { format!("{head}…{tail}") } else { format!("{scheme}:{head}…{tail}") }
}

/// `when` as "18 Dec" at `offset`.
fn day(when: DateTime<Utc>, offset: FixedOffset) -> String {
    when.with_timezone(&offset).format("%-d %b").to_string()
}

/// The class a region's label names.
fn class_of(label: &str) -> CoverClass {
    match label {
        "faces" | "face" | "person" | "people" => CoverClass::Face,
        "personal_text" => CoverClass::PersonalText,
        "schema_personal_fields" | "schema_field" | "schema_fields" => CoverClass::SchemaField,
        "marked" => CoverClass::Marked,
        _ => CoverClass::Other,
    }
}

/// The regions of an image's `RegionSet` overlay, as boxes in its pixels: a box as it is, a
/// polygon by its bounds. Points and masks are not drawn on a card.
fn regions_of(image: &adk_core::ImageEvidence) -> Vec<EvidenceRegion> {
    let Some(set) = &image.regions else { return Vec::new() };
    let Some(regions) = set.get("regions").and_then(Value::as_array) else {
        return Vec::new();
    };
    let (width, height) = (f64::from(image.width), f64::from(image.height));
    regions
        .iter()
        .filter_map(|region| {
            let shape = region.get("shape")?;
            let number = |key: &str| shape.get(key).and_then(Value::as_f64);
            let (x0, y0, x1, y1) = match shape.get("kind").and_then(Value::as_str)? {
                "box" => (number("x0")?, number("y0")?, number("x1")?, number("y1")?),
                "polygon" => {
                    let pairs: Vec<(f64, f64)> = shape
                        .get("points")?
                        .as_array()?
                        .iter()
                        .filter_map(|point| {
                            let pair = point.as_array()?;
                            Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
                        })
                        .collect();
                    if pairs.is_empty() {
                        return None;
                    }
                    let fold =
                        |pick: fn(&(f64, f64)) -> f64, start: f64, f: fn(f64, f64) -> f64| {
                            pairs.iter().map(pick).fold(start, f)
                        };
                    (
                        fold(|p| p.0, f64::INFINITY, f64::min),
                        fold(|p| p.1, f64::INFINITY, f64::min),
                        fold(|p| p.0, f64::NEG_INFINITY, f64::max),
                        fold(|p| p.1, f64::NEG_INFINITY, f64::max),
                    )
                }
                _ => return None,
            };
            if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
                return None;
            }
            let (x0, y0) = (x0.clamp(0.0, width), y0.clamp(0.0, height));
            let (x1, y1) = (x1.clamp(0.0, width), y1.clamp(0.0, height));
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let label = region.get("label").and_then(Value::as_str).unwrap_or_default();
            let class = class_of(label);
            Some(EvidenceRegion {
                class,
                tag: words::region_tag(class, label),
                short_tag: words::region_short_tag(class, label),
                x0,
                y0,
                x1,
                y1,
            })
        })
        .collect()
}

/// How many regions of each class, in the order of [`CoverClass`].
fn count_classes(regions: &[EvidenceRegion]) -> Vec<(CoverClass, u32)> {
    let mut counts: Vec<(CoverClass, u32)> = Vec::new();
    for region in regions {
        match counts.iter_mut().find(|(class, _)| *class == region.class) {
            Some((_, count)) => *count += 1,
            None => counts.push((region.class, 1)),
        }
    }
    counts.sort_by_key(|(class, _)| *class as u8);
    counts
}

/// "1 face, 3 personal text", joined by `separator`; empty when nothing was covered.
pub(crate) fn covered_words(counts: &[(CoverClass, u32)], separator: &str) -> String {
    counts
        .iter()
        .map(|(class, count)| words::covered_class(*class, *count))
        .collect::<Vec<_>>()
        .join(separator)
}

/// The share of a `width` × `height` image the regions cover, from 0 to 1: the exact area of
/// their union over the image's.
pub(crate) fn coverage(regions: &[EvidenceRegion], width: u32, height: u32) -> f64 {
    let area = f64::from(width) * f64::from(height);
    if area <= 0.0 || regions.is_empty() {
        return 0.0;
    }
    let mut xs: Vec<f64> = regions.iter().flat_map(|r| [r.x0, r.x1]).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    let mut union = 0.0;
    for pair in xs.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        let mut spans: Vec<(f64, f64)> = regions
            .iter()
            .filter(|r| r.x0 <= left && r.x1 >= right)
            .map(|r| (r.y0, r.y1))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut covered = 0.0;
        let mut current: Option<(f64, f64)> = None;
        for (top, bottom) in spans {
            current = match current {
                Some((start, end)) if top <= end => Some((start, end.max(bottom))),
                Some((start, end)) => {
                    covered += end - start;
                    Some((top, bottom))
                }
                None => Some((top, bottom)),
            };
        }
        if let Some((start, end)) = current {
            covered += end - start;
        }
        union += covered * (right - left);
    }
    (union / area).clamp(0.0, 1.0)
}

/// The bytes of an evidence image from `artifacts`, read at its exact version and checked
/// against its digest; `None` when it is no longer kept.
///
/// # Errors
///
/// `surface.invalid` when the stored bytes are not an image or do not match the digest, or
/// the artifact service's error.
pub(crate) async fn load_image(
    artifacts: &dyn adk_artifact::ArtifactService,
    image: &adk_core::ImageEvidence,
) -> crate::Result<Option<Vec<u8>>> {
    let at = &image.artifact;
    let loaded = artifacts
        .load(adk_artifact::LoadRequest {
            app_name: at.app_name.clone(),
            user_id: at.user_id.clone(),
            session_id: at.session_id.clone(),
            file_name: at.file_name.clone(),
            version: Some(at.version),
        })
        .await;
    let part = match loaded {
        Ok(loaded) => loaded.part,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let adk_core::Part::InlineData { data, .. } = part else {
        return Err(crate::SurfaceError::Invalid(
            "an evidence artifact holds no image bytes".into(),
        ));
    };
    if digest_of(&data) != image.digest {
        tracing::warn!(evidence.digest = %image.digest, "an evidence image does not match its digest; it is not shown");
        return Err(crate::SurfaceError::Invalid(
            "the stored image does not match the digest the decision names; it is not shown".into(),
        ));
    }
    Ok(Some(data))
}

/// `sha256:<hex>` of `bytes`, as evidence digests are written.
pub(crate) fn digest_of(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    format!("sha256:{:x}", sha2::Sha256::digest(bytes))
}

/// The PNG `change` makes of the PNG `bytes`: cut to its crop, then its boxes filled solid
/// black (spec 3.7 §7.8 step 3: a fill, since blur and pixelation of text can be reversed),
/// as 8-bit RGBA, with its size.
///
/// # Errors
///
/// A sentence for the person when the image is not a PNG this can read, or the change does
/// not fit it.
#[cfg(feature = "web")]
pub(crate) fn changed_png(
    bytes: &[u8],
    change: &ImageChange,
) -> std::result::Result<(Vec<u8>, u32, u32), String> {
    let unreadable =
        |error: &dyn std::fmt::Display| format!("the image could not be read as a PNG: {error}");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|error| unreadable(&error))?;
    let size = reader.output_buffer_size().ok_or_else(|| unreadable(&"it is too large"))?;
    let mut buffer = vec![0; size];
    let frame = reader.next_frame(&mut buffer).map_err(|error| unreadable(&error))?;
    let (width, height) = (frame.width, frame.height);
    change.check(width, height)?;
    let pixels: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => buffer[..frame.buffer_size()].to_vec(),
        png::ColorType::GrayscaleAlpha => buffer[..frame.buffer_size()]
            .chunks_exact(2)
            .flat_map(|ga| [ga[0], ga[0], ga[0], ga[1]])
            .collect(),
        other => return Err(unreadable(&format!("{other:?} pixels"))),
    };
    let [x0, y0, x1, y1] = change.crop.unwrap_or([0, 0, width, height]);
    let (out_width, out_height) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity((out_width * out_height * 4) as usize);
    for row in y0..y1 {
        let start = ((row * width + x0) * 4) as usize;
        out.extend_from_slice(&pixels[start..start + (out_width * 4) as usize]);
    }
    for [bx0, by0, bx1, by1] in &change.cover {
        // A box in the image's pixels, clipped to the crop and moved with it.
        let (cx0, cy0) = ((*bx0).max(x0) - x0, (*by0).max(y0) - y0);
        let (cx1, cy1) = ((*bx1).min(x1).saturating_sub(x0), (*by1).min(y1).saturating_sub(y0));
        for row in cy0..cy1 {
            for column in cx0..cx1 {
                let at = ((row * out_width + column) * 4) as usize;
                out[at..at + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, out_width, out_height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer.write_image_data(&out).map_err(|error| error.to_string())?;
    }
    Ok((encoded, out_width, out_height))
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector, ArtifactLocator,
        Evidence, ImageEvidence, ToolConfirmationRequest,
    };
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    const DIGEST: &str = "sha256:c52ca5e48473595c3e5f3a67166abffb47bf1833af9c61b28f56dc769ece7550";

    fn boxed(label: &str, [x0, y0, x1, y1]: [f64; 4]) -> Value {
        json!({ "image": DIGEST, "label": label, "shape": { "kind": "box", "x0": x0, "y0": y0, "x1": x1, "y1": y1 }, "origin": {} })
    }

    fn card_image(regions: Vec<Value>) -> ImageEvidence {
        ImageEvidence::new(
            DIGEST,
            ArtifactLocator::new("desk", "ana", "s1", "vision-c52ca5.png", 1),
            "image/png",
            1012,
            638,
        )
        .with_regions(json!({ "image": DIGEST, "width": 1012, "height": 638, "regions": regions }))
    }

    fn request(images: Vec<ImageEvidence>) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new(
            "vision_describe",
            json!({ "image": DIGEST, "backend": "claude", "prompt": "What kind of document is this?" }),
        )
        .with_evidence(images.into_iter().map(Evidence::from));
        ApprovalRequest::new(
            "dec-7c41e0",
            ApprovalScope::new("desk", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("owner")]),
        )
    }

    fn input(kept: &[EvidenceKept]) -> MediaInput<'_> {
        MediaInput {
            facts: None,
            kept,
            decided: None,
            agent: "Document desk".into(),
            now: Utc::now(),
            offset: FixedOffset::east_opt(0).unwrap(),
            can_change: true,
            replaces: None,
        }
    }

    fn the_card() -> ApprovalRequest {
        request(vec![card_image(vec![
            boxed("faces", [48.0, 150.0, 300.0, 468.0]),
            boxed("personal_text", [344.0, 176.0, 700.0, 214.0]),
            boxed("personal_text", [344.0, 250.0, 760.0, 288.0]),
            boxed("personal_text", [344.0, 324.0, 560.0, 362.0]),
            boxed("schema_personal_fields", [620.0, 324.0, 940.0, 362.0]),
            boxed("schema_personal_fields", [40.0, 520.0, 972.0, 600.0]),
        ])])
    }

    #[test]
    fn the_identity_card_shows_what_was_covered_by_class() {
        let request = the_card();
        let media = project(&request, &input(&[EvidenceKept::Kept])).unwrap();
        let lines: Vec<&str> = media.covered.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(lines, ["1 face", "3 personal text", "2 fields your schema marks personal"]);
        assert_eq!(media.title, "The exact image that would leave");
        let image = &media.images[0];
        assert_eq!(image.digest_short, "sha256:c52ca5…ce7550");
        assert_eq!(image.size_text, "1012 × 638 PNG");
        assert_eq!(image.href.as_deref(), Some("/api/surface/decisions/dec-7c41e0/evidence/0"));
        assert_eq!(image.regions[0].tag, "face");
        assert_eq!(image.regions[0].short_tag, "face");
        assert!(!media.covered_all);
        assert!(media.sendable());
        assert!(media.can_change);
        assert_eq!(media.fixed_line, words::IMAGES_LEAVE_ONLY_COVERED);
    }

    #[test]
    fn a_face_reads_face_and_a_person_covered_whole_reads_whole_person() {
        let request = request(vec![card_image(vec![
            boxed("faces", [48.0, 150.0, 300.0, 468.0]),
            boxed("people", [600.0, 100.0, 900.0, 630.0]),
        ])]);
        let media = project(&request, &input(&[EvidenceKept::Kept])).unwrap();
        let face = |tag: &str, [x0, y0, x1, y1]: [f64; 4]| EvidenceRegion {
            class: CoverClass::Face,
            tag: tag.into(),
            short_tag: "face".into(),
            x0,
            y0,
            x1,
            y1,
        };
        assert_eq!(
            media.images[0].regions,
            [
                face("face", [48.0, 150.0, 300.0, 468.0]),
                face("face · whole person", [600.0, 100.0, 900.0, 630.0]),
            ]
        );
        assert_eq!(media.covered[0].text, "2 faces");
    }

    #[test]
    fn a_deleted_image_leaves_nothing_to_send() {
        let request = the_card();
        let mut later = input(&[EvidenceKept::Deleted]);
        later.now = request.created_at + Duration::days(31);
        let media = project(&request, &later).unwrap();
        let notice = media.notice.as_ref().unwrap();
        assert_eq!(notice.kind, MediaNoticeKind::Purged);
        assert!(notice.text.contains("ask Document desk again"));
        assert!(media.images[0].href.is_none());
        let purged = media.images[0].purged.as_ref().unwrap();
        assert!(purged.title.starts_with("This image was deleted on"));
        assert!(!media.can_change);
        assert!(media.kept_text.is_none());
        assert_eq!(media.title, "The image that would have left");
    }

    #[test]
    fn a_crop_covered_in_full_leaves_nothing_to_send() {
        let request = request(vec![
            ImageEvidence::new(
                DIGEST,
                ArtifactLocator::new("desk", "ana", "s1", "vision-9cb6f7.png", 1),
                "image/png",
                422,
                160,
            )
            .with_regions(json!({ "regions": [
                boxed("schema_personal_fields", [0.0, 0.0, 422.0, 64.0]),
                boxed("schema_personal_fields", [0.0, 64.0, 422.0, 160.0]),
            ] })),
        ]);
        let media = project(&request, &input(&[EvidenceKept::Kept])).unwrap();
        assert!(media.covered_all);
        assert_eq!(media.covered_all_text.as_deref(), Some("100% of the image"));
        assert_eq!(media.notice.as_ref().unwrap().kind, MediaNoticeKind::AllCovered);
        assert!(!media.can_change);
    }

    #[test]
    fn several_images_name_which_ones_were_covered() {
        let plain = || card_image(Vec::new());
        let request = request(vec![
            plain(),
            plain(),
            card_image(vec![boxed("schema_field", [80.0, 10.0, 120.0, 40.0])]),
        ]);
        let mut facts = MediaFacts::new().with_data_class("document_crop");
        facts.images = vec![
            ImageFacts::new().with_field("PO number").with_why("Handwriting"),
            ImageFacts::new().with_field("Invoice date"),
            ImageFacts::new().with_field("Approved by"),
        ];
        let mut given = input(&[EvidenceKept::Kept; 3]);
        given.facts = Some(&facts);
        let media = project(&request, &given).unwrap();
        assert_eq!(media.title, "The 3 images that would leave");
        assert_eq!(media.what.as_deref(), Some("3 document crops"));
        assert_eq!(media.what_note.as_deref(), Some("never the page"));
        assert_eq!(media.covered[0].text, "1 field your schema marks personal");
        assert_eq!(media.covered[0].note.as_deref(), Some("in image 3"));
        assert_eq!(media.images[0].covered_text, "nothing covered");
        assert_eq!(media.images[0].field.as_deref(), Some("PO number"));
        assert!(!media.can_change, "one image of several is not changed on the card");
    }

    #[test]
    fn a_decided_card_shows_what_left_and_its_cost() {
        let request = the_card();
        let facts = MediaFacts::new()
            .to("Claude", "claude-sonnet-5-5", ["api.anthropic.com"])
            .with_estimated_cost(4_400, Some("851 image tokens".into()))
            .with_actual_cost(2_400, Some("851 image tokens, 63 answer tokens".into()));
        let mut given = input(&[EvidenceKept::Kept]);
        given.facts = Some(&facts);
        let waiting = project(&request, &given).unwrap();
        assert_eq!(waiting.cost_text.as_deref(), Some("Up to $0.0044"));
        given.decided = Some(crate::model::DecidedAnswer::Yes);
        let decided = project(&request, &given).unwrap();
        assert_eq!(decided.title, "The image that left");
        assert_eq!(decided.cost_text.as_deref(), Some("$0.0024"));
        assert!(decided.kept_text.unwrap().starts_with("As evidence until"));
        assert!(!decided.can_change);
    }

    #[test]
    fn a_change_can_only_shrink_the_crop_and_grow_the_covering() {
        let shrink = ImageChange { image: 0, crop: Some([0, 0, 1012, 450]), cover: Vec::new() };
        assert!(shrink.check(1012, 638).is_ok());
        let none = ImageChange { image: 0, crop: None, cover: Vec::new() };
        assert!(none.check(1012, 638).is_err());
        let whole = ImageChange { image: 0, crop: Some([0, 0, 1012, 638]), cover: Vec::new() };
        assert!(whole.check(1012, 638).is_err());
        let outside = ImageChange { image: 0, crop: None, cover: vec![[900, 600, 1100, 700]] };
        assert!(outside.check(1012, 638).is_err());
        let empty = ImageChange { image: 0, crop: Some([10, 10, 10, 50]), cover: Vec::new() };
        assert!(empty.check(1012, 638).is_err());
    }

    #[cfg(feature = "web")]
    #[test]
    fn a_change_cuts_the_image_and_fills_its_boxes_black() {
        let (width, height) = (8u32, 6u32);
        let white = vec![255u8; (width * height * 4) as usize];
        let mut source = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut source, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&white).unwrap();
        }
        let change = ImageChange { image: 0, crop: Some([2, 1, 8, 5]), cover: vec![[0, 0, 4, 2]] };
        let (bytes, out_width, out_height) = changed_png(&source, &change).unwrap();
        assert_eq!((out_width, out_height), (6, 4));
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        decoder.set_transformations(png::Transformations::ALPHA);
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut pixels).unwrap();
        let at = |x: usize, y: usize| pixels[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4].to_vec();
        // The box [0, 0, 4, 2] in the image is [0, 0, 2, 1] in the crop.
        assert_eq!(at(0, 0), [0, 0, 0, 255]);
        assert_eq!(at(1, 0), [0, 0, 0, 255]);
        assert_eq!(at(2, 0), [255, 255, 255, 255]);
        assert_eq!(at(0, 1), [255, 255, 255, 255]);
        assert_ne!(digest_of(&bytes), digest_of(&source));
        let wider = ImageChange { image: 0, crop: Some([0, 0, 9, 6]), cover: Vec::new() };
        assert!(changed_png(&source, &wider).is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// The union of boxes never covers more than the image, never less than the largest
        /// box, and a box over the whole image covers all of it.
        #[test]
        fn coverage_is_the_area_of_the_union(
            boxes in proptest::collection::vec((0u32..400, 0u32..300, 1u32..400, 1u32..300), 0..8),
            whole in any::<bool>(),
        ) {
            let region = |x0: f64, y0: f64, x1: f64, y1: f64| EvidenceRegion {
                class: CoverClass::Marked,
                tag: String::new(),
                short_tag: String::new(),
                x0,
                y0,
                x1,
                y1,
            };
            let mut regions: Vec<EvidenceRegion> = boxes
                .iter()
                .map(|(x, y, w, h)| {
                    region(f64::from(*x), f64::from(*y), f64::from((x + w).min(400)), f64::from((y + h).min(300)))
                })
                .filter(|r| r.x1 > r.x0 && r.y1 > r.y0)
                .collect();
            if whole {
                regions.push(region(0.0, 0.0, 400.0, 300.0));
            }
            let share = coverage(&regions, 400, 300);
            prop_assert!((0.0..=1.0).contains(&share));
            let largest = regions
                .iter()
                .map(|r| (r.x1 - r.x0) * (r.y1 - r.y0))
                .fold(0.0, f64::max)
                / 120_000.0;
            prop_assert!(share + 1e-9 >= largest);
            if whole {
                prop_assert!((share - 1.0).abs() < 1e-9);
            }
        }
    }
}
