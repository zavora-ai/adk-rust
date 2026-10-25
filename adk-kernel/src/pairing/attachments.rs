//! What a surface keeps beside a paired device or a person (spec 3.3 §5.4, §8.2): a phone's
//! push subscription, a passkey, the claim that sends one wake-up once across replicas.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who an attachment belongs to.
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::AttachmentHolder;
///
/// let holder = AttachmentHolder::Device("0a1b2c3d4e5f6071".into());
/// assert_eq!(holder.key(), "device:0a1b2c3d4e5f6071");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "camelCase")]
#[non_exhaustive]
pub enum AttachmentHolder {
    /// A paired device; revoking it removes its attachments.
    Device(String),
    /// A person, such as the operator's passkey for the web home.
    User(String),
}

impl AttachmentHolder {
    /// The holder as one string, `device:<id>` or `user:<id>`, which stores key rows by.
    pub fn key(&self) -> String {
        match self {
            Self::Device(id) => format!("device:{id}"),
            Self::User(id) => format!("user:{id}"),
        }
    }
}

/// One thing kept beside a device or a person (spec 3.3 §5.4, §8.2): a record of `kind`
/// under `key`, which the surface that wrote it reads back. The store never reads `record`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::{AttachmentHolder, DeviceAttachment};
///
/// let subscription = DeviceAttachment::new(
///     AttachmentHolder::Device("0a1b2c3d4e5f6071".into()),
///     "push",
///     "subscription",
///     serde_json::json!({ "endpoint": "https://web.push.apple.com/…" }),
///     chrono::Utc::now(),
/// );
/// assert_eq!(subscription.kind, "push");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeviceAttachment {
    /// Whose it is.
    pub holder: AttachmentHolder,
    /// What it is, such as `push` or `passkey`.
    pub kind: String,
    /// Which one of its kind.
    pub key: String,
    /// What the surface keeps.
    pub record: Value,
    /// When it was last written.
    pub updated_at: DateTime<Utc>,
}

impl DeviceAttachment {
    /// An attachment of `holder`.
    pub fn new(
        holder: AttachmentHolder,
        kind: impl Into<String>,
        key: impl Into<String>,
        record: Value,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self { holder, kind: kind.into(), key: key.into(), record, updated_at }
    }
}
