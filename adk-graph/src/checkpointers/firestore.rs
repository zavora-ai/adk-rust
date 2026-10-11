//! Firestore [`Checkpointer`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use firestore::errors::{BackoffError, FirestoreError};
use firestore::{
    FirestoreDb, FirestoreQueryDirection, FirestoreTransaction, FirestoreTransactionOptions,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{decode, encode, storage_error};
use crate::checkpoint::{Checkpointer, RetentionPolicy, conflict, duplicate_id, ensure_newer};
use crate::error::{GraphError, Result};
use crate::state::Checkpoint;

const BACKEND: &str = "firestore";
const THREADS: &str = "adk_graph_threads";
const CHECKPOINTS: &str = "checkpoints";
const IDS: &str = "adk_graph_checkpoint_ids";

/// How long one save may spend retrying conflicting transactions.
const TRANSACTION_DEADLINE: chrono::TimeDelta = chrono::TimeDelta::seconds(120);

type TxResult<T> = std::result::Result<T, BackoffError<GraphError>>;
type TxFuture<'b, T> = Pin<Box<dyn Future<Output = TxResult<T>> + Send + 'b>>;

/// A thread: its ID and highest version.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThreadDoc {
    thread_id: String,
    head: i64,
}

/// One checkpoint, under its thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointDoc {
    checkpoint_id: String,
    thread_id: String,
    version: i64,
    data: String,
}

/// Where a checkpoint ID is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdDoc {
    checkpoint_id: String,
    thread_id: String,
    version: i64,
}

/// Document IDs cannot hold `/` and are length-limited, so identifiers are addressed
/// by their SHA-256; each document also stores the identifier and every read checks it.
fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Zero-padded, so document ID order is version order.
fn version_id(version: i64) -> String {
    format!("{version:020}")
}

fn version_param(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| storage_error(BACKEND, format!("version {version} exceeds a 64-bit integer")))
}

fn retry_or_fail(error: FirestoreError) -> BackoffError<GraphError> {
    let retryable = matches!(&error, FirestoreError::DatabaseError(db) if db.retry_possible);
    if retryable {
        BackoffError::transient(storage_error(BACKEND, error))
    } else {
        BackoffError::permanent(storage_error(BACKEND, error))
    }
}

/// Firestore-backed [`Checkpointer`].
///
/// | Document | Contents |
/// |----------|----------|
/// | `adk_graph_threads/{sha256(thread)}` | The thread ID and its highest version, `head` |
/// | `adk_graph_threads/{sha256(thread)}/checkpoints/{version, zero-padded}` | The checkpoint as JSON in `data` |
/// | `adk_graph_checkpoint_ids/{sha256(id)}` | The thread and version holding a checkpoint ID |
///
/// A save is one read-write transaction: it reads the thread document and the ID
/// document, then writes the checkpoint, the ID, and the new head. Firestore
/// transactions are serializable, so of two concurrent saves of one thread the second
/// is aborted, retried, and sees the first's head. Every query orders or filters on
/// the single `version` field, which Firestore serves from its automatic indexes.
///
/// # Example
///
/// ```rust,no_run
/// use adk_graph::FirestoreCheckpointer;
///
/// # async fn example() -> adk_graph::Result<()> {
/// let checkpointer = FirestoreCheckpointer::new("my-gcp-project").await?;
/// checkpointer.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreCheckpointer {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreCheckpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreCheckpointer").finish_non_exhaustive()
    }
}

impl FirestoreCheckpointer {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        let db = FirestoreDb::new(project_id)
            .await
            .map_err(|e| storage_error(BACKEND, format!("connection failed: {e}")))?;
        Ok(Self::from_db(db))
    }

    /// Uses an existing client, such as one shared with session storage.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Connects to the Firestore emulator at `FIRESTORE_EMULATOR_HOST` without
    /// Application Default Credentials, for local development and tests.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_graph::FirestoreCheckpointer;
    ///
    /// # async fn example() -> adk_graph::Result<()> {
    /// // FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
    /// let checkpointer = FirestoreCheckpointer::connect_emulator("demo-project").await?;
    /// # let _ = checkpointer;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when `FIRESTORE_EMULATOR_HOST` is unset or
    /// the client cannot be created. Without the variable the client would reach
    /// production Firestore with a token it rejects, so the call fails instead.
    pub async fn connect_emulator(project_id: &str) -> Result<Self> {
        if std::env::var_os("FIRESTORE_EMULATOR_HOST").is_none() {
            return Err(storage_error(
                BACKEND,
                "FIRESTORE_EMULATOR_HOST is not set; set it to the emulator's host:port",
            ));
        }
        let db = FirestoreDb::with_options_token_source(
            firestore::FirestoreDbOptions::new(project_id.to_string()),
            Vec::new(),
            gcloud_sdk::TokenSourceType::ExternalSource(Box::new(EmulatorOwnerToken)),
        )
        .await
        .map_err(|e| storage_error(BACKEND, format!("emulator connection failed: {e}")))?;
        Ok(Self::from_db(db))
    }

    /// Firestore creates collections on first write and serves every query from
    /// automatic single-field indexes, so there is nothing to create; the method
    /// exists so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    fn thread_parent(&self, thread_id: &str) -> Result<String> {
        self.db
            .parent_path(THREADS, digest(thread_id))
            .map(|path| path.to_string())
            .map_err(|e| storage_error(BACKEND, e))
    }

    async fn checkpoints(
        &self,
        thread_id: &str,
        direction: FirestoreQueryDirection,
        before: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<Checkpoint>> {
        let parent = self.thread_parent(thread_id)?;
        let mut select = self
            .db
            .fluent()
            .select()
            .from(CHECKPOINTS)
            .parent(&parent)
            .order_by([("version", direction)]);
        if let Some(before) = before {
            select = select.filter(|q| q.for_all([q.field("version").less_than(before)]));
        }
        if let Some(limit) = limit {
            select = select.limit(limit);
        }
        let documents: Vec<CheckpointDoc> =
            select.obj::<CheckpointDoc>().query().await.map_err(|e| storage_error(BACKEND, e))?;
        documents
            .into_iter()
            .filter(|document| document.thread_id == thread_id)
            .map(|document| decode(BACKEND, &document.data))
            .collect()
    }

    async fn remove(&self, thread_id: &str, checkpoints: &[Checkpoint]) -> Result<usize> {
        let parent = self.thread_parent(thread_id)?;
        for checkpoint in checkpoints {
            let version = version_param(checkpoint.version)?;
            self.db
                .fluent()
                .delete()
                .from(CHECKPOINTS)
                .parent(&parent)
                .document_id(version_id(version))
                .execute()
                .await
                .map_err(|e| storage_error(BACKEND, e))?;
            self.db
                .fluent()
                .delete()
                .from(IDS)
                .document_id(digest(&checkpoint.checkpoint_id))
                .execute()
                .await
                .map_err(|e| storage_error(BACKEND, e))?;
        }
        Ok(checkpoints.len())
    }
}

/// The Firestore emulator accepts the `owner` token as an administrator.
struct EmulatorOwnerToken;

#[async_trait]
impl gcloud_sdk::Source for EmulatorOwnerToken {
    async fn token(&self) -> gcloud_sdk::error::Result<gcloud_sdk::Token> {
        Ok(gcloud_sdk::Token::new(
            "Bearer".to_string(),
            "owner".into(),
            chrono::Utc::now() + chrono::Duration::hours(1),
        ))
    }
}

/// What a save transaction concluded.
enum Outcome {
    Stored,
    Behind(u64),
    DuplicateId,
}

async fn save_in_transaction(
    db: &FirestoreDb,
    transaction: &mut FirestoreTransaction<'_>,
    checkpoint: &Checkpoint,
    version: i64,
    data: &str,
) -> TxResult<Outcome> {
    let thread_key = digest(&checkpoint.thread_id);
    let thread: Option<ThreadDoc> = db
        .fluent()
        .select()
        .by_id_in(THREADS)
        .obj::<ThreadDoc>()
        .one(&thread_key)
        .await
        .map_err(retry_or_fail)?;
    if let Some(thread) = &thread {
        if thread.thread_id != checkpoint.thread_id {
            return Err(BackoffError::permanent(storage_error(
                BACKEND,
                format!("thread digest collision for '{}'", checkpoint.thread_id),
            )));
        }
        if thread.head >= version {
            return Ok(Outcome::Behind(thread.head as u64));
        }
    }
    let id_key = digest(&checkpoint.checkpoint_id);
    let existing: Option<IdDoc> = db
        .fluent()
        .select()
        .by_id_in(IDS)
        .obj::<IdDoc>()
        .one(&id_key)
        .await
        .map_err(retry_or_fail)?;
    if existing.is_some() {
        return Ok(Outcome::DuplicateId);
    }
    let parent = db.parent_path(THREADS, &thread_key).map_err(retry_or_fail)?;
    db.fluent()
        .update()
        .in_col(CHECKPOINTS)
        .document_id(version_id(version))
        .parent(&parent)
        .object(&CheckpointDoc {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            thread_id: checkpoint.thread_id.clone(),
            version,
            data: data.to_string(),
        })
        .add_to_transaction(transaction)
        .map_err(retry_or_fail)?;
    db.fluent()
        .update()
        .in_col(IDS)
        .document_id(&id_key)
        .object(&IdDoc {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            thread_id: checkpoint.thread_id.clone(),
            version,
        })
        .add_to_transaction(transaction)
        .map_err(retry_or_fail)?;
    db.fluent()
        .update()
        .in_col(THREADS)
        .document_id(&thread_key)
        .object(&ThreadDoc { thread_id: checkpoint.thread_id.clone(), head: version })
        .add_to_transaction(transaction)
        .map_err(retry_or_fail)?;
    Ok(Outcome::Stored)
}

#[async_trait]
impl Checkpointer for FirestoreCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        ensure_newer(checkpoint, None)?;
        let version = version_param(checkpoint.version)?;
        let request = Arc::new((checkpoint.clone(), encode(checkpoint)?));
        let outcome = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Outcome> {
                    let request = Arc::clone(&request);
                    Box::pin(async move {
                        let (checkpoint, data) = &*request;
                        save_in_transaction(&db, transaction, checkpoint, version, data).await
                    })
                },
                FirestoreTransactionOptions::new().with_max_elapsed_time(TRANSACTION_DEADLINE),
            )
            .await
            .map_err(|error| match error {
                FirestoreError::ErrorInTransaction(inner) => {
                    match inner.source.downcast::<GraphError>() {
                        Ok(graph_error) => *graph_error,
                        Err(source) => storage_error(BACKEND, source),
                    }
                }
                other => storage_error(BACKEND, other),
            })?;
        match outcome {
            Outcome::Stored => Ok(checkpoint.checkpoint_id.clone()),
            Outcome::Behind(head) => Err(conflict(checkpoint, head)),
            Outcome::DuplicateId => Err(duplicate_id(&checkpoint.checkpoint_id)),
        }
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .checkpoints(thread_id, FirestoreQueryDirection::Descending, None, Some(1))
            .await?
            .pop())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let location: Option<IdDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(IDS)
            .obj::<IdDoc>()
            .one(digest(checkpoint_id))
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        let Some(location) = location.filter(|location| location.checkpoint_id == checkpoint_id)
        else {
            return Ok(None);
        };
        let parent = self.thread_parent(&location.thread_id)?;
        let document: Option<CheckpointDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(CHECKPOINTS)
            .parent(&parent)
            .obj::<CheckpointDoc>()
            .one(version_id(location.version))
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        document
            .filter(|document| document.checkpoint_id == checkpoint_id)
            .map(|document| decode(BACKEND, &document.data))
            .transpose()
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        self.checkpoints(thread_id, FirestoreQueryDirection::Ascending, None, None).await
    }

    async fn list_page(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let before = before.map(version_param).transpose()?;
        let limit = u32::try_from(limit).unwrap_or(u32::MAX);
        self.checkpoints(thread_id, FirestoreQueryDirection::Descending, before, Some(limit)).await
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        let checkpoints = self.list(thread_id).await?;
        self.remove(thread_id, &checkpoints).await?;
        self.db
            .fluent()
            .delete()
            .from(THREADS)
            .document_id(digest(thread_id))
            .execute()
            .await
            .map_err(|e| storage_error(BACKEND, e))
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let checkpoints = self.list(thread_id).await?;
        let expired = policy.expired(&checkpoints);
        let doomed: Vec<Checkpoint> = checkpoints
            .into_iter()
            .filter(|checkpoint| expired.contains(&checkpoint.checkpoint_id))
            .collect();
        self.remove(thread_id, &doomed).await
    }
}
