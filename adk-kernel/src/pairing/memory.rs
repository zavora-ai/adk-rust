//! The in-memory [`DeviceStore`] (spec 3.3 §10 K2).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::{AttachmentHolder, DeviceAttachment, DeviceRecord, DeviceStore, PairingCode};
use crate::error::Result;

/// A [`DeviceStore`] in process memory, for tests and a kernel with no durable store (spec
/// 3.3 §10 K2). Its devices end with the process, so they must pair again after a restart.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::pairing::{InMemoryDeviceStore, IssueCode, Pairing};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
/// let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await?;
/// pairing.claim(&issued.code, Some("Ana's phone")).await?;
/// assert_eq!(pairing.devices().await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryDeviceStore {
    codes: RwLock<BTreeMap<String, PairingCode>>,
    devices: RwLock<BTreeMap<String, DeviceRecord>>,
    attempts: AtomicU64,
    attachments: RwLock<BTreeMap<(AttachmentHolder, String, String), DeviceAttachment>>,
}

impl InMemoryDeviceStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DeviceStore for InMemoryDeviceStore {
    async fn put_code(&self, code: PairingCode) -> Result<()> {
        self.codes.write().await.insert(code.code_hash.clone(), code);
        Ok(())
    }

    async fn take_code(&self, code_hash: &str) -> Result<Option<PairingCode>> {
        Ok(self.codes.write().await.remove(code_hash))
    }

    async fn prune_codes(&self, before: DateTime<Utc>) -> Result<usize> {
        let mut codes = self.codes.write().await;
        let count = codes.len();
        codes.retain(|_, code| code.expires_at > before);
        Ok(count - codes.len())
    }

    async fn clear_codes(&self) -> Result<usize> {
        let mut codes = self.codes.write().await;
        let count = codes.len();
        codes.clear();
        Ok(count)
    }

    async fn put_device(&self, device: DeviceRecord) -> Result<()> {
        self.devices.write().await.insert(device.id.clone(), device);
        Ok(())
    }

    async fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        Ok(self.devices.read().await.get(id).cloned())
    }

    async fn list_devices(&self) -> Result<Vec<DeviceRecord>> {
        let mut devices: Vec<DeviceRecord> = self.devices.read().await.values().cloned().collect();
        devices.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(devices)
    }

    async fn count_claim_attempt(&self) -> Result<u64> {
        Ok(self.attempts.fetch_add(1, Ordering::SeqCst) + 1)
    }

    async fn reset_claim_attempts(&self) -> Result<()> {
        self.attempts.store(0, Ordering::SeqCst);
        Ok(())
    }

    async fn put_attachment(&self, attachment: DeviceAttachment) -> Result<()> {
        let key = (attachment.holder.clone(), attachment.kind.clone(), attachment.key.clone());
        self.attachments.write().await.insert(key, attachment);
        Ok(())
    }

    async fn put_attachment_if_absent(&self, attachment: DeviceAttachment) -> Result<bool> {
        let key = (attachment.holder.clone(), attachment.kind.clone(), attachment.key.clone());
        let mut attachments = self.attachments.write().await;
        if attachments.contains_key(&key) {
            return Ok(false);
        }
        attachments.insert(key, attachment);
        Ok(true)
    }

    async fn attachments(
        &self,
        holder: &AttachmentHolder,
        kind: &str,
    ) -> Result<Vec<DeviceAttachment>> {
        Ok(self
            .attachments
            .read()
            .await
            .values()
            .filter(|attachment| &attachment.holder == holder && attachment.kind == kind)
            .cloned()
            .collect())
    }

    async fn remove_attachment(
        &self,
        holder: &AttachmentHolder,
        kind: &str,
        key: &str,
    ) -> Result<bool> {
        let key = (holder.clone(), kind.to_string(), key.to_string());
        Ok(self.attachments.write().await.remove(&key).is_some())
    }

    async fn remove_attachments(&self, holder: &AttachmentHolder) -> Result<usize> {
        let mut attachments = self.attachments.write().await;
        let count = attachments.len();
        attachments.retain(|(owner, _, _), _| owner != holder);
        Ok(count - attachments.len())
    }
}
