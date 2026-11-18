//! Media: [`ImageRef`], [`Frame`], [`VideoRef`], decoding and the [`MediaStore`]
//! (spec 3.7 §7.2).
//!
//! Pixels enter once and are stored by digest. An [`ImageRef`] — artifact locator, digest,
//! MIME type, dimensions and provenance — is what travels through events, approvals,
//! ledgers and audit records.

mod decode;
mod store;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use decode::{DecodeLimits, DecodedImage, SourceImage};
pub use store::{MediaScope, MediaStore};

use crate::digest::Digest;

/// Where an artifact lives in the `ArtifactService`: app, user, session, file name and the
/// exact version.
///
/// The same shape and wire form as the `adk_core::ArtifactLocator` that approval evidence
/// uses (spec 3.7 X4); this crate re-exports that type once it is on main.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ArtifactLocator {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session the artifact belongs to.
    pub session_id: String,
    /// Artifact file name.
    pub file_name: String,
    /// Artifact version.
    pub version: i64,
}

impl ArtifactLocator {
    /// The locator of `version` of `file_name` in the given session.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        file_name: impl Into<String>,
        version: i64,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
            file_name: file_name.into(),
            version,
        }
    }
}

/// An artifact's locator as a URI, `artifact://<app>/<user>/<session>/<file>#<version>`, the
/// form image parts carry in their `uri`.
///
/// # Example
///
/// ```
/// use adk_vision::{ArtifactLocator, artifact_uri};
///
/// let at = ArtifactLocator::new("desk", "u1", "s1", "page.png", 3);
/// assert_eq!(artifact_uri(&at), "artifact://desk/u1/s1/page.png#3");
/// ```
pub fn artifact_uri(locator: &ArtifactLocator) -> String {
    format!(
        "artifact://{}/{}/{}/{}#{}",
        locator.app_name, locator.user_id, locator.session_id, locator.file_name, locator.version
    )
}

/// The identifier of a frame source: a camera, a file, a screen or a browser.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FrameSourceId(pub String);

/// How an image came to be.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum Provenance {
    /// A file a person or a tool handed over.
    Upload,
    /// A screenshot.
    Screen {
        /// The display it shows.
        display: String,
        /// Device pixel ratio in thousandths: 2000 is a ratio of 2.
        device_pixel_ratio_milli: u32,
    },
    /// A camera frame.
    Camera {
        /// The frame source.
        source: FrameSourceId,
    },
    /// A browser screenshot.
    Browser {
        /// The page's URL.
        url: String,
    },
    /// A rendered document page.
    DocumentPage {
        /// The document's digest.
        document: Digest,
        /// The page number, from 1.
        page: u32,
        /// The render resolution.
        dpi: u32,
    },
    /// Produced by an `Editor` operation from another image.
    Derived {
        /// The digest of the image it was produced from.
        from: Digest,
        /// The digest of the operation, in canonical JSON.
        operation: Digest,
    },
}

/// An image the framework can reason about. The encoded bytes live in an artifact; this
/// reference is what travels through events, approvals, ledgers and audit records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    /// App, user, session, file name and version in the `ArtifactService`.
    pub artifact: ArtifactLocator,
    /// `sha256:<hex>` of the encoded bytes: the identity used by approvals, caches and audit.
    pub digest: Digest,
    /// `image/png`, `image/jpeg`, `image/webp` or `image/gif`.
    pub mime_type: String,
    /// Width in source pixels, after EXIF orientation is applied.
    pub width: u32,
    /// Height in source pixels, after EXIF orientation is applied.
    pub height: u32,
    /// How the image came to be.
    pub provenance: Provenance,
}

impl ImageRef {
    /// A model part for this image: `Part::InlineData` with the artifact URI in `uri` and
    /// `{"adkVision": {"digest": …, "release": null}}` in `annotations`, so the privacy
    /// guardrail can tell an image it already released from one it has not seen.
    ///
    /// `data` are the image's encoded bytes, as loaded from the artifact.
    pub fn to_part(&self, data: Vec<u8>) -> adk_core::Part {
        adk_core::Part::InlineData {
            mime_type: self.mime_type.clone(),
            data,
            uri: Some(artifact_uri(&self.artifact)),
            annotations: Some(serde_json::json!({
                "adkVision": { "digest": self.digest, "release": null }
            })),
        }
    }
}

/// A stream frame's identity and timing, without its pixels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMeta {
    /// The frame source.
    pub source: FrameSourceId,
    /// The frame's index in the stream.
    pub index: u64,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: u64,
    /// When the frame was captured.
    pub captured_at: DateTime<Utc>,
}

/// One frame of a stream. Pixels stay in memory unless the frame becomes evidence.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The frame source.
    pub source: FrameSourceId,
    /// The frame's index in the stream.
    pub index: u64,
    /// Presentation timestamp in the stream.
    pub pts: Duration,
    /// When the frame was captured.
    pub captured_at: DateTime<Utc>,
    /// The pixels.
    pub pixels: FramePixels,
}

impl Frame {
    /// The frame's identity and timing.
    pub fn meta(&self) -> FrameMeta {
        FrameMeta {
            source: self.source.clone(),
            index: self.index,
            pts_ms: u64::try_from(self.pts.as_millis()).unwrap_or(u64::MAX),
            captured_at: self.captured_at,
        }
    }
}

/// A frame's pixels: ephemeral by default, stored only as evidence.
#[derive(Clone, Debug)]
pub enum FramePixels {
    /// Decoded and never written anywhere.
    Ephemeral(Arc<DecodedImage>),
    /// Kept as evidence, a wake payload or an approval.
    Stored(Box<ImageRef>),
}

/// A video kept as an artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRef {
    /// Where the video is stored.
    pub artifact: ArtifactLocator,
    /// `sha256:<hex>` of the encoded bytes.
    pub digest: Digest,
    /// `video/mp4` or `video/quicktime`.
    pub mime_type: String,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}
