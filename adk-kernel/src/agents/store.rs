//! The kernel's store of agents a person adds (spec 3.3 §5.7, §10 K7).

use async_trait::async_trait;

use crate::agents::record::AgentRecord;
use crate::error::Result;

/// Where the kernel keeps the agents a person adds (spec 3.3 §5.7, §10 K7), beside the
/// partition and driver stores, on every kernel backend, with one conformance suite
/// (`agent_store_conformance_tests!`, feature `conformance`):
///
/// | Backend | Type | Feature |
/// |---|---|---|
/// | In-memory | [`InMemoryAgentStore`](crate::stores::InMemoryAgentStore) | — |
/// | SQLite | `SqliteAgentStore` | `sqlite` (default) |
/// | PostgreSQL | `PostgresAgentStore` | `postgres` |
/// | Redis | `RedisAgentStore` | `redis` |
/// | MongoDB | `MongoAgentStore` | `mongodb` |
/// | Firestore | `FirestoreAgentStore` | `firestore` |
/// | Neo4j | `Neo4jAgentStore` | `neo4j` |
///
/// Every write is a compare-and-set on the stored version, so two replicas changing one
/// agent change it once. A removed agent stays as a `Removed` tombstone, so its id is never
/// given again (S-I14); only a suggestion nobody approved is ever deleted. Nothing outside
/// the kernel's agent table writes it (S-I17).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentStore};
/// use adk_kernel::stores::InMemoryAgentStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryAgentStore::new();
/// let record = AgentRecord::new("finance", AgentDefinition::new("Finance", "default"),
///     AgentRecordState::Active, AgentOrigin::ByHand, SubjectRef::human("alice"), chrono::Utc::now());
/// assert!(store.put_if_version(record.clone(), 0).await?);
/// assert!(!store.put_if_version(record.clone(), 0).await?, "the id is taken");
/// let mut paused = record.with_state(AgentRecordState::Paused);
/// paused.version = 2;
/// assert!(store.put_if_version(paused, 1).await?);
/// assert_eq!(store.get("finance").await?.unwrap().version, 2);
/// assert!(!store.delete("finance").await?, "only a suggestion is ever deleted");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait AgentStore: Send + Sync {
    /// Writes `record` when the stored record with its id has version `expected`, or, with
    /// `expected` = 0, when there is none; `false`, writing nothing, otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> Result<bool>;

    /// The record `id`, tombstones included, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn get(&self, id: &str) -> Result<Option<AgentRecord>>;

    /// Every record, tombstones included, ordered by id in byte order.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list(&self) -> Result<Vec<AgentRecord>>;

    /// Deletes the record `id` when it is a suggestion nobody approved
    /// ([`Proposed`](crate::agents::AgentRecordState::Proposed)); `false`, deleting
    /// nothing, for any other record or none.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn delete(&self, id: &str) -> Result<bool>;
}
