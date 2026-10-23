//! The in-memory [`DriverStore`] (spec 3.2 §7.7, §13 Q8).

use std::collections::BTreeMap;

use adk_core::ApprovalScope;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::drivers::store::park_key;
use crate::drivers::{DriverPark, DriverRecord, DriverStore};
use crate::error::Result;

/// A [`DriverStore`] in process memory, for tests and a kernel with no durable store
/// (spec 3.2 §7.7). Its records end with the process.
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, DriverRecord, DriverStore};
/// use adk_kernel::stores::InMemoryDriverStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryDriverStore::new();
/// for id in ["mail", "cal"] {
///     let manifest = DriverManifest::from_toml(&format!("id = \"{id}\"\ncommand = \"x\"\n"))?;
///     store.put(DriverRecord::new(manifest, "alice", chrono::Utc::now())).await?;
/// }
/// let ids: Vec<_> = store.list().await?.iter().map(|r| r.id().to_string()).collect();
/// assert_eq!(ids, ["cal", "mail"]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryDriverStore {
    records: RwLock<BTreeMap<String, DriverRecord>>,
    parks: RwLock<BTreeMap<(String, String, String, String), DriverPark>>,
}

impl InMemoryDriverStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DriverStore for InMemoryDriverStore {
    async fn put(&self, record: DriverRecord) -> Result<()> {
        self.records.write().await.insert(record.id().to_string(), record);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<DriverRecord>> {
        Ok(self.records.read().await.get(id).cloned())
    }

    async fn list(&self) -> Result<Vec<DriverRecord>> {
        Ok(self.records.read().await.values().cloned().collect())
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        Ok(self.records.write().await.remove(id).is_some())
    }

    async fn put_park(&self, park: DriverPark) -> Result<()> {
        let key = park_key(&park.scope, &park.call_id);
        self.parks.write().await.insert(key, park);
        Ok(())
    }

    async fn get_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<Option<DriverPark>> {
        Ok(self.parks.read().await.get(&park_key(scope, call_id)).cloned())
    }

    async fn list_parks(&self, driver: Option<&str>) -> Result<Vec<DriverPark>> {
        Ok(self
            .parks
            .read()
            .await
            .values()
            .filter(|park| driver.is_none_or(|driver| park.driver == driver))
            .cloned()
            .collect())
    }

    async fn delete_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<bool> {
        Ok(self.parks.write().await.remove(&park_key(scope, call_id)).is_some())
    }
}
