//! The live lane: state the edge serves through read-only capabilities and that is never
//! released (spec 3.5 §7.8.2 "Volatile data", §7.8.3 "Three lanes", A-I26).
//!
//! Feeds write stock, availability, scores, queue times and counts here; the edge answers
//! its read-only capabilities from it with a cache of seconds. Nothing here carries a claim,
//! and an order checks live state again at order time. Pages show the value the last release
//! carried with its time, so they work with script off.

use std::collections::BTreeMap;

use async_trait::async_trait;
use awp_types::ContentId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::Result;

/// One live value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveValue {
    /// The value.
    pub value: Value,
    /// When it was set.
    pub at: DateTime<Utc>,
    /// The feed that set it, if a feed did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed: Option<String>,
}

/// Where the edge reads live values from (spec 3.5 §7.8.3). The edge implements it over its
/// own store; [`InMemoryLiveStore`] keeps values in memory.
#[async_trait]
pub trait LiveStore: Send + Sync {
    /// Sets live values of one item.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written.
    async fn set(&self, id: &ContentId, values: BTreeMap<String, LiveValue>) -> Result<()>;

    /// The live values of one item.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read.
    async fn get(&self, id: &ContentId) -> Result<BTreeMap<String, LiveValue>>;
}

/// Live values in memory.
///
/// # Example
///
/// ```rust
/// use std::collections::BTreeMap;
/// use adk_site::live::{InMemoryLiveStore, LiveStore, LiveValue};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_site::Result<()> {
/// let store = InMemoryLiveStore::default();
/// let value = LiveValue { value: 4.into(), at: chrono::Utc::now(), feed: Some("pos".into()) };
/// store.set(&"panel".into(), BTreeMap::from([("stock".to_string(), value)])).await?;
/// assert_eq!(store.get(&"panel".into()).await?["stock"].value, 4);
/// assert_eq!(store.writes().await, 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryLiveStore {
    values: RwLock<Values>,
}

/// Live values by item and field, and the count of values written.
#[derive(Debug, Default)]
struct Values {
    items: BTreeMap<ContentId, BTreeMap<String, LiveValue>>,
    writes: u64,
}

impl InMemoryLiveStore {
    /// Values written so far, counted per field.
    pub async fn writes(&self) -> u64 {
        self.values.read().await.writes
    }
}

#[async_trait]
impl LiveStore for InMemoryLiveStore {
    async fn set(&self, id: &ContentId, values: BTreeMap<String, LiveValue>) -> Result<()> {
        let mut guard = self.values.write().await;
        guard.writes += values.len() as u64;
        guard.items.entry(id.clone()).or_default().extend(values);
        Ok(())
    }

    async fn get(&self, id: &ContentId) -> Result<BTreeMap<String, LiveValue>> {
        Ok(self.values.read().await.items.get(id).cloned().unwrap_or_default())
    }
}
