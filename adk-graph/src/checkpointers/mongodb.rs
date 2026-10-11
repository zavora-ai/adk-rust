//! MongoDB [`Checkpointer`].

use async_trait::async_trait;
use mongodb::bson::{Bson, Document, doc};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, FindOptions, IndexOptions};
use mongodb::{Client, Collection, Database, IndexModel};

use super::{decode, encode, storage_error};
use crate::checkpoint::{Checkpointer, RetentionPolicy, conflict, duplicate_id, ensure_newer};
use crate::error::Result;
use crate::state::Checkpoint;

const BACKEND: &str = "mongodb";
const CHECKPOINTS: &str = "adk_graph_checkpoints";
const THREADS: &str = "adk_graph_threads";
const THREAD_VERSION_INDEX: &str = "adk_graph_checkpoints_thread_version";

/// MongoDB's duplicate key error code.
const DUPLICATE_KEY: i32 = 11000;

fn duplicate_key_message(error: &mongodb::error::Error) -> Option<&str> {
    match error.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(write)) if write.code == DUPLICATE_KEY => {
            Some(&write.message)
        }
        ErrorKind::Command(command) if command.code == DUPLICATE_KEY => Some(&command.message),
        _ => None,
    }
}

fn version_param(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| storage_error(BACKEND, format!("version {version} exceeds a 64-bit integer")))
}

/// MongoDB-backed [`Checkpointer`].
///
/// | Collection | `_id` | Contents |
/// |------------|-------|----------|
/// | `adk_graph_checkpoints` | Checkpoint ID | `thread_id`, `version`, `created_at`, and the checkpoint as JSON in `data` |
/// | `adk_graph_threads` | Thread ID | `head`, the thread's highest version |
///
/// A save inserts the checkpoint — the `_id` and a unique index on
/// `(thread_id, version)` refuse a stored ID or a concurrent save of the same version
/// — then advances the head with one conditional upsert filtered on `head < version`.
/// When the head is already at or past the version the upsert collides with the
/// existing thread document, and the inserted checkpoint is removed again. No
/// multi-document transaction is needed, so a standalone `mongod` works. A reader can
/// briefly list a checkpoint whose save is being refused; `load` never returns one,
/// because the refused version is below the head.
///
/// # Example
///
/// ```rust,no_run
/// use adk_graph::MongoCheckpointer;
///
/// # async fn example() -> adk_graph::Result<()> {
/// let checkpointer = MongoCheckpointer::new("mongodb://localhost:27017", "app").await?;
/// checkpointer.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoCheckpointer {
    db: Database,
}

impl MongoCheckpointer {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        let fail =
            |e: mongodb::error::Error| storage_error(BACKEND, format!("connection failed: {e}"));
        let options = ClientOptions::parse(connection_string).await.map_err(fail)?;
        Ok(Self::from_database(
            Client::with_options(options).map_err(fail)?.database(database_name),
        ))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Creates the unique `(thread_id, version)` index the version rule relies on.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        self.checkpoints()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "thread_id": 1, "version": -1 })
                    .options(
                        IndexOptions::builder()
                            .name(THREAD_VERSION_INDEX.to_string())
                            .unique(true)
                            .build(),
                    )
                    .build(),
            )
            .await
            .map_err(|e| storage_error(BACKEND, format!("migration failed: {e}")))?;
        Ok(())
    }

    fn checkpoints(&self) -> Collection<Document> {
        self.db.collection(CHECKPOINTS)
    }

    fn threads(&self) -> Collection<Document> {
        self.db.collection(THREADS)
    }

    async fn head(&self, thread_id: &str) -> Result<u64> {
        let thread = self
            .threads()
            .find_one(doc! { "_id": thread_id })
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        Ok(thread
            .and_then(|thread| match thread.get("head") {
                Some(Bson::Int64(head)) => Some(*head as u64),
                Some(Bson::Int32(head)) => Some(*head as u64),
                _ => None,
            })
            .unwrap_or(0))
    }

    async fn find(&self, filter: Document, options: FindOptions) -> Result<Vec<Checkpoint>> {
        let mut cursor = self
            .checkpoints()
            .find(filter)
            .with_options(options)
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        let mut checkpoints = Vec::new();
        while cursor.advance().await.map_err(|e| storage_error(BACKEND, e))? {
            let document = cursor.deserialize_current().map_err(|e| storage_error(BACKEND, e))?;
            let data = document
                .get_str("data")
                .map_err(|e| storage_error(BACKEND, format!("field 'data' is corrupt: {e}")))?;
            checkpoints.push(decode(BACKEND, data)?);
        }
        Ok(checkpoints)
    }
}

#[async_trait]
impl Checkpointer for MongoCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        ensure_newer(checkpoint, None)?;
        let version = version_param(checkpoint.version)?;
        let document = doc! {
            "_id": &checkpoint.checkpoint_id,
            "thread_id": &checkpoint.thread_id,
            "version": version,
            "created_at": mongodb::bson::DateTime::from_millis(checkpoint.created_at.timestamp_millis()),
            "data": encode(checkpoint)?,
        };
        if let Err(error) = self.checkpoints().insert_one(document).await {
            return Err(match duplicate_key_message(&error) {
                Some(message) if message.contains(THREAD_VERSION_INDEX) => {
                    conflict(checkpoint, self.head(&checkpoint.thread_id).await?)
                }
                Some(_) => duplicate_id(&checkpoint.checkpoint_id),
                None => storage_error(BACKEND, error),
            });
        }
        let advanced = self
            .threads()
            .update_one(
                doc! { "_id": &checkpoint.thread_id, "head": { "$lt": version } },
                doc! { "$set": { "head": version } },
            )
            .upsert(true)
            .await;
        match advanced {
            Ok(_) => Ok(checkpoint.checkpoint_id.clone()),
            Err(error) => {
                self.checkpoints()
                    .delete_one(doc! { "_id": &checkpoint.checkpoint_id })
                    .await
                    .map_err(|e| storage_error(BACKEND, e))?;
                match duplicate_key_message(&error) {
                    Some(_) => Err(conflict(checkpoint, self.head(&checkpoint.thread_id).await?)),
                    None => Err(storage_error(BACKEND, error)),
                }
            }
        }
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        let options = FindOptions::builder().sort(doc! { "version": -1 }).limit(1).build();
        Ok(self.find(doc! { "thread_id": thread_id }, options).await?.pop())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self.find(doc! { "_id": checkpoint_id }, FindOptions::default()).await?.pop())
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let options = FindOptions::builder().sort(doc! { "version": 1 }).build();
        self.find(doc! { "thread_id": thread_id }, options).await
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
        let mut filter = doc! { "thread_id": thread_id };
        if let Some(before) = before {
            filter.insert("version", doc! { "$lt": version_param(before)? });
        }
        let options = FindOptions::builder()
            .sort(doc! { "version": -1 })
            .limit(i64::try_from(limit).unwrap_or(i64::MAX))
            .build();
        self.find(filter, options).await
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        self.checkpoints()
            .delete_many(doc! { "thread_id": thread_id })
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        self.threads()
            .delete_one(doc! { "_id": thread_id })
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let expired = policy.expired(&self.list(thread_id).await?);
        if expired.is_empty() {
            return Ok(0);
        }
        let result = self
            .checkpoints()
            .delete_many(doc! { "thread_id": thread_id, "_id": { "$in": expired } })
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        Ok(result.deleted_count as usize)
    }
}
