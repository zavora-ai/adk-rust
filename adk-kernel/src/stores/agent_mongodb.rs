//! The MongoDB [`AgentStore`] (spec 3.3 §5.7, §10 K7, feature `mongodb`).

use async_trait::async_trait;
use mongodb::bson::{Document, doc};
use mongodb::{Collection, Database};

use super::mongodb::{collect, connect, is_duplicate_key, json, sorted, store_error, text};
use crate::agents::{AgentRecord, AgentStore};
use crate::error::{KernelError, Result};

const AGENTS: &str = "adk_kernel_agents";

/// An [`AgentStore`] in MongoDB, shared by every replica (spec 3.3 §5.7, §10 K7, feature
/// `mongodb`).
///
/// One `adk_kernel_agents` document per agent, `_id` its id: `version` and `state`, the
/// fields a write compares and a delete reads, beside `record`, the record as JSON text, so
/// it reads back exactly as written. `list` sorts by `_id` under MongoDB's binary string
/// comparison, so ids order by their UTF-8 bytes. A new agent is one insert, refused by the
/// `_id` index when the id exists; a change is one update filtered on `_id` and `version`.
/// Every write is a single-document write, so a standalone `mongod` serves it as a replica
/// set does.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::MongoAgentStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoAgentStore {
    db: Database,
}

impl MongoAgentStore {
    /// A store over `db`, such as the process store's.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Connects to `connection_string` and uses the database `database_name`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the connection string is invalid.
    pub async fn connect(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(connect(connection_string, database_name).await?))
    }

    /// The database the store writes to.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Prepares the store; `_id`, the only index it reads, always exists, so this does
    /// nothing.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    fn agents(&self) -> Collection<Document> {
        self.db.collection(AGENTS)
    }
}

fn stored_version(version: u64) -> Result<i64> {
    i64::try_from(version).map_err(|_| KernelError::Store {
        backend: super::mongodb::BACKEND,
        reason: format!("agent version {version} exceeds a signed 64-bit integer"),
    })
}

/// The fields of `record` other than `_id`.
fn fields(record: &AgentRecord) -> Result<Document> {
    Ok(doc! {
        "version": stored_version(record.version)?,
        "state": record.state.as_str(),
        "record": serde_json::to_string(record)?,
    })
}

fn decode(document: &Document) -> Result<AgentRecord> {
    json("agent", text(document, "record")?)
}

#[async_trait]
impl AgentStore for MongoAgentStore {
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> Result<bool> {
        if expected == 0 {
            let mut document = doc! { "_id": &record.id };
            document.extend(fields(&record)?);
            return match self.agents().insert_one(document).await {
                Ok(_) => Ok(true),
                Err(error) if is_duplicate_key(&error) => Ok(false),
                Err(error) => Err(store_error("write the agent table", &error)),
            };
        }
        let result = self
            .agents()
            .update_one(
                doc! { "_id": &record.id, "version": stored_version(expected)? },
                doc! { "$set": fields(&record)? },
            )
            .await
            .map_err(|error| store_error("update the agent table", &error))?;
        Ok(result.matched_count == 1)
    }

    async fn get(&self, id: &str) -> Result<Option<AgentRecord>> {
        let document = self
            .agents()
            .find_one(doc! { "_id": id })
            .await
            .map_err(|error| store_error("read the agent table", &error))?;
        document.as_ref().map(decode).transpose()
    }

    async fn list(&self) -> Result<Vec<AgentRecord>> {
        let cursor = self
            .agents()
            .find(doc! {})
            .with_options(sorted(doc! { "_id": 1 }, None))
            .await
            .map_err(|error| store_error("list the agent table", &error))?;
        collect(cursor, "list the agent table").await?.iter().map(decode).collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let result = self
            .agents()
            .delete_one(doc! { "_id": id, "state": "proposed" })
            .await
            .map_err(|error| store_error("delete from the agent table", &error))?;
        Ok(result.deleted_count == 1)
    }
}
