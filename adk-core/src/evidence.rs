//! Evidence an approver sees with a held call.
//!
//! A call that sends, keeps or acts on an image is held with the exact image as
//! [`Evidence`]: its digest, where it is stored, and an optional overlay of regions. Evidence
//! travels by reference and digest, never as pixels, so held calls, approval requests and
//! audit records stay small, and a surface loads the image through the artifact service with
//! the viewer's own authorisation.
//!
//! | Carried by | Set by |
//! |------------|--------|
//! | [`GovernedCall::evidence`](crate::GovernedCall::evidence) | [`Tool::approval_evidence`](crate::Tool::approval_evidence) through [`GovernedCall::for_tool`](crate::GovernedCall::for_tool), or [`GovernedCall::with_evidence`](crate::GovernedCall::with_evidence) |
//! | [`ToolConfirmationRequest::evidence`](crate::ToolConfirmationRequest::evidence) | The governed path, from the call |
//! | [`ApprovalRequest::evidence`](crate::ApprovalRequest::evidence) | The call's evidence, then any [`ApprovalRoute::evidence`](crate::ApprovalRoute::evidence) |
//!
//! The digests of a held call's own evidence are part of its
//! [fingerprint](crate::ToolConfirmationRequest::fingerprint), so a decision binds to the
//! exact images the approver saw: a call whose image changed before it ran is a different call
//! and is held again. A call without evidence keeps the fingerprint it always had.
//!
//! # Example
//!
//! ```rust
//! use adk_core::{ArtifactLocator, Evidence, ImageEvidence, ToolConfirmationRequest};
//! use serde_json::json;
//!
//! let crop = ImageEvidence::new(
//!     "sha256:9f2c0a7e5b1d4c8a6f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a",
//!     ArtifactLocator::new("desk", "ana", "s1", "egress/invoice-7-total.png", 1),
//!     "image/png",
//!     640,
//!     180,
//! )
//! .with_regions(json!({ "regions": [{ "label": "total", "shape": { "box": [12, 8, 300, 60] } }] }))
//! .with_caption("Invoice 7, total field, after redaction");
//!
//! let call = ToolConfirmationRequest::new("vision_describe", json!({ "question": "What is the total?" }))
//!     .with_evidence([Evidence::from(crop)]);
//! assert_eq!(call.evidence[0].digest(), "sha256:9f2c0a7e5b1d4c8a6f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a");
//! assert_eq!(serde_json::to_value(&call.evidence[0]).unwrap()["kind"], "image");
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where an artifact version lives in an `ArtifactService`: application, user, session,
/// file name and version.
///
/// # Example
///
/// ```rust
/// use adk_core::ArtifactLocator;
///
/// let locator = ArtifactLocator::new("desk", "ana", "s1", "egress/crop.png", 3);
/// assert_eq!(
///     serde_json::to_value(&locator).unwrap(),
///     serde_json::json!({
///         "appName": "desk", "userId": "ana", "sessionId": "s1",
///         "fileName": "egress/crop.png", "version": 3
///     })
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ArtifactLocator {
    /// The application namespace.
    pub app_name: String,
    /// The owning user.
    pub user_id: String,
    /// The session the artifact belongs to.
    pub session_id: String,
    /// The artifact's file name.
    pub file_name: String,
    /// The exact version, so the evidence never moves to a later save.
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

/// One item of evidence held with a call, tagged by `kind` on the wire.
///
/// # Example
///
/// ```rust
/// use adk_core::{ArtifactLocator, Evidence, ImageEvidence};
///
/// let digest = "sha256:0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8";
/// let evidence = Evidence::from(ImageEvidence::new(
///     digest,
///     ArtifactLocator::new("desk", "ana", "s1", "egress/card.png", 1),
///     "image/png",
///     1024,
///     640,
/// ));
/// let json = serde_json::to_value(&evidence).unwrap();
/// assert_eq!(json["kind"], "image");
/// assert_eq!(serde_json::from_value::<Evidence>(json).unwrap(), evidence);
/// assert_eq!(evidence.digest(), digest);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum Evidence {
    /// An image the call would send, keep or act on.
    Image(ImageEvidence),
}

impl Evidence {
    /// The content digest of the evidence, such as `sha256:<hex>` of an image's encoded
    /// bytes. A held call's fingerprint includes it.
    pub fn digest(&self) -> &str {
        match self {
            Self::Image(image) => &image.digest,
        }
    }
}

impl From<ImageEvidence> for Evidence {
    fn from(image: ImageEvidence) -> Self {
        Self::Image(image)
    }
}

/// An image held as evidence: its digest, its artifact, its format and size, and an optional
/// overlay of regions and caption.
///
/// `regions` is a `RegionSet` in the wire form `adk-vision` publishes
/// (`adk-vision/schema/region-set.json`), carried as JSON so surfaces can draw it without
/// depending on `adk-vision`.
///
/// # Example
///
/// ```rust
/// use adk_core::{ArtifactLocator, ImageEvidence};
/// use serde_json::json;
///
/// let image = ImageEvidence::new(
///     "sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
///     ArtifactLocator::new("desk", "ana", "s1", "egress/receipt.png", 2),
///     "image/png",
///     800,
///     1200,
/// )
/// .with_caption("Receipt, faces and card number filled");
/// assert_eq!(image.caption.as_deref(), Some("Receipt, faces and card number filled"));
/// assert!(serde_json::to_value(&image).unwrap().get("regions").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ImageEvidence {
    /// `sha256:<hex>` of the image's encoded bytes.
    pub digest: String,
    /// Where the image is stored.
    pub artifact: ArtifactLocator,
    /// `image/png`, `image/jpeg` or `image/webp`.
    pub mime_type: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Regions to draw over the image, as a `RegionSet`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Value>,
    /// One line describing the image for the approver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl ImageEvidence {
    /// The image with `digest`, stored at `artifact`, of `mime_type` and `width` × `height`
    /// pixels.
    pub fn new(
        digest: impl Into<String>,
        artifact: ArtifactLocator,
        mime_type: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            digest: digest.into(),
            artifact,
            mime_type: mime_type.into(),
            width,
            height,
            regions: None,
            caption: None,
        }
    }

    /// Draws `regions`, a `RegionSet`, over the image.
    #[must_use]
    pub fn with_regions(mut self, regions: Value) -> Self {
        self.regions = Some(regions);
        self
    }

    /// Describes the image for the approver.
    #[must_use]
    pub fn with_caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}
