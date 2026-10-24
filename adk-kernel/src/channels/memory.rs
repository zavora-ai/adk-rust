//! The in-memory [`ChannelBindingStore`] (spec 3.3 §10 K1).

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::{ChannelBinding, ChannelBindingStore, ChannelPairingRequest};
use crate::error::Result;

/// A [`ChannelBindingStore`] in process memory, for tests and a kernel with no durable store
/// (spec 3.3 §10 K1). Its pairings end with the process.
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::{ChannelBinding, ChannelBindingStore, InMemoryChannelBindingStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryChannelBindingStore::new();
/// let binding = ChannelBinding::new("telegram", "4217", "4217", "ana", chrono::Utc::now());
/// store.put_binding(binding.clone()).await?;
/// assert_eq!(store.get_binding(&binding.id).await?, Some(binding));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryChannelBindingStore {
    bindings: RwLock<BTreeMap<String, ChannelBinding>>,
    requests: RwLock<BTreeMap<(String, String), ChannelPairingRequest>>,
}

impl InMemoryChannelBindingStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ChannelBindingStore for InMemoryChannelBindingStore {
    async fn put_binding(&self, binding: ChannelBinding) -> Result<()> {
        self.bindings.write().await.insert(binding.id.clone(), binding);
        Ok(())
    }

    async fn get_binding(&self, id: &str) -> Result<Option<ChannelBinding>> {
        Ok(self.bindings.read().await.get(id).cloned())
    }

    async fn list_bindings(&self) -> Result<Vec<ChannelBinding>> {
        let mut bindings: Vec<_> = self.bindings.read().await.values().cloned().collect();
        bindings.sort_by(|a, b| a.paired_at.cmp(&b.paired_at).then_with(|| a.id.cmp(&b.id)));
        Ok(bindings)
    }

    async fn put_request(&self, request: ChannelPairingRequest) -> Result<()> {
        let key = (request.channel.clone(), request.account.clone());
        self.requests.write().await.insert(key, request);
        Ok(())
    }

    async fn get_request(
        &self,
        channel: &str,
        account: &str,
    ) -> Result<Option<ChannelPairingRequest>> {
        Ok(self.requests.read().await.get(&(channel.to_string(), account.to_string())).cloned())
    }

    async fn take_request(&self, code_hash: &str) -> Result<Option<ChannelPairingRequest>> {
        let mut requests = self.requests.write().await;
        let key = requests
            .iter()
            .find(|(_, request)| request.code_hash == code_hash)
            .map(|(key, _)| key.clone());
        Ok(key.and_then(|key| requests.remove(&key)))
    }

    async fn list_requests(&self) -> Result<Vec<ChannelPairingRequest>> {
        let mut requests: Vec<_> = self.requests.read().await.values().cloned().collect();
        requests.sort_by(|a, b| {
            (a.requested_at, &a.channel, &a.account).cmp(&(b.requested_at, &b.channel, &b.account))
        });
        Ok(requests)
    }

    async fn prune_requests(&self, before: DateTime<Utc>) -> Result<usize> {
        let mut requests = self.requests.write().await;
        let count = requests.len();
        requests.retain(|_, request| request.expires_at > before);
        Ok(count - requests.len())
    }
}
