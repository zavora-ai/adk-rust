//! The Firestore [`AgentStore`] (spec 3.3 §5.7, §10 K7, feature `firestore`).

use std::sync::Arc;

use async_trait::async_trait;
use firestore::FirestoreDb;
use serde::{Deserialize, Serialize};

use super::firestore::{
    TxFuture, connect, delete_where, doc_id, json, store_error, transaction_error,
    transaction_options, tx_fail,
};
use crate::agents::{AgentRecord, AgentStore};
use crate::error::Result;

const AGENTS: &str = "adk_kernel_agents";

/// One agent, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentDoc {
    id: String,
    version: i64,
    state: String,
    record: String,
}

impl AgentDoc {
    fn new(record: &AgentRecord) -> Result<Self> {
        Ok(Self {
            id: record.id.clone(),
            version: i64::try_from(record.version).map_err(|_| {
                store_error("write the agent table", &"the version exceeds a signed 64-bit integer")
            })?,
            state: record.state.as_str().to_string(),
            record: serde_json::to_string(record)?,
        })
    }

    fn decode(&self) -> Result<AgentRecord> {
        json("agent", &self.record)
    }
}

/// An [`AgentStore`] in Firestore, shared by every replica (spec 3.3 §5.7, §10 K7, feature
/// `firestore`).
///
/// One `adk_kernel_agents` document per agent, named by the hex of its id: `id`, `version`,
/// `state` and `record`, the record as JSON text. `put_if_version` reads and writes the
/// document in one serializable transaction, so of two replicas changing one agent from the
/// same version the second retries, sees the first's write and changes nothing. `list` reads
/// every document and orders them by id in memory; an agent table is small.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::FirestoreAgentStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreAgentStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreAgentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreAgentStore").finish_non_exhaustive()
    }
}

impl FirestoreAgentStore {
    /// A store over `db`, such as the process store's.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Connects to `project_id` with Application Default Credentials.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the client cannot be
    /// created.
    pub async fn connect(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await?))
    }

    /// The client the store uses.
    pub fn db(&self) -> &FirestoreDb {
        &self.db
    }

    /// Prepares the store's storage; Firestore needs nothing created, so this does nothing.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl AgentStore for FirestoreAgentStore {
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> Result<bool> {
        let doc_name = Arc::new(doc_id("agent", &record.id)?);
        let doc = Arc::new(AgentDoc::new(&record)?);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, bool> {
                    let (doc_name, doc) = (doc_name.clone(), doc.clone());
                    Box::pin(async move {
                        let stored: Option<AgentDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(AGENTS)
                            .obj()
                            .one(doc_name.as_str())
                            .await
                            .map_err(tx_fail("read the agent table"))?;
                        let version = stored
                            .filter(|stored| stored.id == doc.id)
                            .map_or(0, |stored| stored.version);
                        if u64::try_from(version).ok() != Some(expected) {
                            return Ok(false);
                        }
                        db.fluent()
                            .update()
                            .in_col(AGENTS)
                            .document_id(doc_name.as_str())
                            .object(doc.as_ref())
                            .add_to_transaction(transaction)
                            .map_err(tx_fail("write the agent table"))?;
                        Ok(true)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|error| transaction_error(error, "write the agent table"))
    }

    async fn get(&self, id: &str) -> Result<Option<AgentRecord>> {
        let Ok(doc_name) = doc_id("agent", id) else {
            return Ok(None);
        };
        let doc: Option<AgentDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(AGENTS)
            .obj()
            .one(doc_name)
            .await
            .map_err(|error| store_error("read the agent table", &error))?;
        doc.filter(|doc| doc.id == id).as_ref().map(AgentDoc::decode).transpose()
    }

    async fn list(&self) -> Result<Vec<AgentRecord>> {
        let mut docs: Vec<AgentDoc> = self
            .db
            .fluent()
            .select()
            .from(AGENTS)
            .obj()
            .query()
            .await
            .map_err(|error| store_error("list the agent table", &error))?;
        docs.sort_by(|a, b| a.id.cmp(&b.id));
        docs.iter().map(AgentDoc::decode).collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let Ok(doc_name) = doc_id("agent", id) else {
            return Ok(false);
        };
        let id = id.to_string();
        let matches = move |doc: &AgentDoc| doc.id == id && doc.state == "proposed";
        delete_where(&self.db, AGENTS, doc_name, matches, "delete from the agent table").await
    }
}
