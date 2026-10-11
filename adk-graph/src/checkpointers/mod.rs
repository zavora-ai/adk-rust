//! Checkpointers for shared databases.
//!
//! Each backend sits behind the feature of the same name and passes the
//! conformance suite (the `conformance` feature). They store the whole
//! [`Checkpoint`](crate::state::Checkpoint) as
//! JSON text next to the columns or keys they query, so a loaded checkpoint equals
//! the saved one and new checkpoint fields need no schema change.
//!
//! | Backend | Feature | Version rule enforced by |
//! |---------|---------|--------------------------|
//! | `PostgresCheckpointer` | `postgres` | A guarded `INSERT` and a unique index on `(thread_id, version)` |
//! | `RedisCheckpointer` | `redis` | One Lua script per save, reading the thread's highest version |
//! | `MongoCheckpointer` | `mongodb` | A compare-and-set on the thread's head document and a unique index on `(thread_id, version)` |
//! | `Neo4jCheckpointer` | `neo4j` | A write lock on the thread's node, taken before the head is read |
//! | `FirestoreCheckpointer` | `firestore` | A read-write transaction over the thread's head document |

#[cfg(any(
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
))]
use crate::{error::GraphError, state::Checkpoint};

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "mongodb")]
pub mod mongodb;

#[cfg(feature = "neo4j")]
pub mod neo4j;

#[cfg(feature = "firestore")]
pub mod firestore;

#[cfg(any(
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
))]
fn storage_error(backend: &str, error: impl std::fmt::Display) -> GraphError {
    GraphError::CheckpointError(format!("{backend} checkpointer: {error}"))
}

#[cfg(any(
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
))]
fn encode(checkpoint: &Checkpoint) -> crate::error::Result<String> {
    Ok(serde_json::to_string(checkpoint)?)
}

#[cfg(any(
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
))]
fn decode(backend: &str, text: &str) -> crate::error::Result<Checkpoint> {
    serde_json::from_str(text)
        .map_err(|error| storage_error(backend, format!("stored checkpoint is corrupt: {error}")))
}
