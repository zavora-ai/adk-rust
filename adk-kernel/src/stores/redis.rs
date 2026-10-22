//! The Redis [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `redis`).

use async_trait::async_trait;
use chrono::NaiveDate;

use super::{ProcessFilter, ProcessStore, SpendScope};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// A [`ProcessStore`] in Redis, shared by every replica (spec 3.2 §7.1, feature `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::RedisProcessStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisProcessStore {
    client: fred::prelude::Client,
    namespace: String,
}

impl RedisProcessStore {
    /// The namespace keys sit under by default.
    pub const DEFAULT_NAMESPACE: &'static str = "adk:kernel";

    /// A store over an initialized client, such as one shared with the run store.
    pub fn from_client(client: fred::prelude::Client) -> Self {
        Self { client, namespace: Self::DEFAULT_NAMESPACE.to_string() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached.
    pub async fn connect(_url: &str) -> Result<Self> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::connect" })
    }

    /// Keeps the store's keys under `namespace`, for example to separate deployments that
    /// share a server.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// The client the store uses.
    pub fn client(&self) -> &fred::prelude::Client {
        &self.client
    }

    /// The namespace the store's keys sit under.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Records the store's layout version, refusing a database a newer release wrote.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server fails or holds a newer layout.
    pub async fn migrate(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::migrate" })
    }
}

#[async_trait]
impl ProcessStore for RedisProcessStore {
    async fn put(&self, _meta: ProcessMeta) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::put" })
    }

    async fn get(&self, _pid: &str) -> Result<Option<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::get" })
    }

    async fn list(&self, _filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::list" })
    }

    async fn set_wake(&self, _pid: &str, _wake: Option<WakeSet>) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::set_wake" })
    }

    async fn set_queue(&self, _pid: &str, _queue: QueueClass) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::set_queue" })
    }

    async fn delete(&self, _pid: &str) -> Result<bool> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::delete" })
    }

    async fn set_suspend_requested(&self, _pid: &str, _requested: bool) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::set_suspend_requested" })
    }

    async fn add_spend(
        &self,
        _pid: &str,
        _partition: &PartitionId,
        _day: NaiveDate,
        _delta: SpendDelta,
    ) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::add_spend" })
    }

    async fn spend(
        &self,
        _scope: &SpendScope,
        _from: NaiveDate,
        _to_inclusive: NaiveDate,
    ) -> Result<u64> {
        Err(KernelError::Unimplemented { item: "stores::RedisProcessStore::spend" })
    }
}
