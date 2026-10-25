//! What the phone keeps beside a paired device or a person: push subscriptions, passkeys and
//! the claims that send each wake-up once across replicas (spec 3.3 §5.4, §8.2). They are the
//! kernel's device attachments, in the device store every replica shares, and they go when the
//! device is revoked.

use std::sync::Arc;

use adk_core::AdkError;
use adk_kernel::pairing::DeviceStore;
pub(crate) use adk_kernel::pairing::{AttachmentHolder as Holder, DeviceAttachment as Attachment};

/// Push subscriptions: one per device, key [`PUSH_KEY`].
pub(crate) const PUSH: &str = "push";
/// The key of a device's one push subscription.
pub(crate) const PUSH_KEY: &str = "subscription";
/// Passkeys: one per device (key [`DEVICE_PASSKEY_KEY`]), one per credential for a person.
pub(crate) const PASSKEY: &str = "passkey";
/// The key of a device's one passkey.
pub(crate) const DEVICE_PASSKEY_KEY: &str = "passkey";
/// Wake-up claims, by what they cover.
pub(crate) const WAKE: &str = "wake";

/// The device store's attachments, with the engine's errors.
#[derive(Clone)]
pub(crate) struct Attachments(Arc<dyn DeviceStore>);

impl Attachments {
    pub(crate) fn new(store: Arc<dyn DeviceStore>) -> Self {
        Self(store)
    }

    pub(crate) async fn put(&self, attachment: Attachment) -> Result<(), AdkError> {
        self.0.put_attachment(attachment).await.map_err(AdkError::from)
    }

    pub(crate) async fn put_if_absent(&self, attachment: Attachment) -> Result<bool, AdkError> {
        self.0.put_attachment_if_absent(attachment).await.map_err(AdkError::from)
    }

    pub(crate) async fn list(
        &self,
        holder: &Holder,
        kind: &str,
    ) -> Result<Vec<Attachment>, AdkError> {
        self.0.attachments(holder, kind).await.map_err(AdkError::from)
    }

    pub(crate) async fn remove(
        &self,
        holder: &Holder,
        kind: &str,
        key: &str,
    ) -> Result<bool, AdkError> {
        self.0.remove_attachment(holder, kind, key).await.map_err(AdkError::from)
    }
}
