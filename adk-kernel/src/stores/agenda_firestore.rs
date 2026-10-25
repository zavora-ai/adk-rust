//! The Firestore [`AgendaStore`] (spec 3.2 §7.1, §7.5, §13 Q8, feature `firestore`).

use std::sync::Arc;

use async_trait::async_trait;
use firestore::FirestoreDb;
use serde::{Deserialize, Serialize};

use super::firestore::{
    TxFuture, connect, delete_where, doc_id, json, store_error, transaction_error,
    transaction_options, tx_fail,
};
use crate::agenda::{AgendaFilter, AgendaItem, AgendaStateKind, AgendaStore};
use crate::error::Result;

const AGENDA: &str = "adk_kernel_agenda";

/// One agenda item, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgendaDoc {
    id: String,
    partition: String,
    state: String,
    created_us: i64,
    item: String,
}

impl AgendaDoc {
    fn new(item: &AgendaItem) -> Result<Self> {
        Ok(Self {
            id: item.id.clone(),
            partition: item.partition.as_str().to_string(),
            state: item.state.kind().as_str().to_string(),
            created_us: item.created_at.timestamp_micros(),
            item: serde_json::to_string(item)?,
        })
    }

    fn decode(&self) -> Result<AgendaItem> {
        json("agenda item", &self.item)
    }
}

/// An [`AgendaStore`] in Firestore, shared by every replica (spec 3.2 §7.1, §7.5, §13 Q8,
/// feature `firestore`).
///
/// One `adk_kernel_agenda` document per item, named by the hex of its id: `id`,
/// `partition`, `state` (its kind), `created_us` (microseconds since the Unix epoch) and
/// `item`, the item as JSON text. `list` queries with equality filters only — the
/// partition and the states — which Firestore serves from its automatic single-field
/// indexes, then orders by `created_us` and id and applies the limit in memory, so no
/// composite index is needed; a listing reads every item its filters match. `transition`
/// reads and writes the item in one serializable transaction, so of two replicas moving one
/// item from the same state the second retries, sees the first's write and moves nothing. An
/// id longer than 700 bytes is refused.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::FirestoreAgendaStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreAgendaStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreAgendaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreAgendaStore").finish_non_exhaustive()
    }
}

impl FirestoreAgendaStore {
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
impl AgendaStore for FirestoreAgendaStore {
    async fn put(&self, item: AgendaItem) -> Result<()> {
        let doc = AgendaDoc::new(&item)?;
        self.db
            .fluent()
            .update()
            .in_col(AGENDA)
            .document_id(doc_id("agenda item", &item.id)?)
            .object(&doc)
            .execute::<AgendaDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write the agenda", &error))
    }

    async fn get(&self, id: &str) -> Result<Option<AgendaItem>> {
        let Ok(doc_name) = doc_id("agenda item", id) else {
            return Ok(None);
        };
        let doc: Option<AgendaDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(AGENDA)
            .obj()
            .one(doc_name)
            .await
            .map_err(|error| store_error("read the agenda", &error))?;
        doc.filter(|doc| doc.id == id).as_ref().map(AgendaDoc::decode).transpose()
    }

    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let states: Vec<&str> = filter.states.iter().map(|state| state.as_str()).collect();
        let mut docs: Vec<AgendaDoc> = self
            .db
            .fluent()
            .select()
            .from(AGENDA)
            .filter(|q| {
                q.for_all([
                    filter.partition.as_ref().and_then(|p| q.field("partition").eq(p.as_str())),
                    if states.is_empty() { None } else { q.field("state").is_in(states.clone()) },
                ])
            })
            .obj()
            .query()
            .await
            .map_err(|error| store_error("list the agenda", &error))?;
        docs.sort_by(|a, b| (a.created_us, &a.id).cmp(&(b.created_us, &b.id)));
        docs.truncate(filter.limit.unwrap_or(usize::MAX));
        docs.iter().map(AgendaDoc::decode).collect()
    }

    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool> {
        let doc_name = Arc::new(doc_id("agenda item", &item.id)?);
        let doc = Arc::new(AgendaDoc::new(&item)?);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, bool> {
                    let (doc_name, doc) = (doc_name.clone(), doc.clone());
                    Box::pin(async move {
                        let stored: Option<AgendaDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(AGENDA)
                            .obj()
                            .one(doc_name.as_str())
                            .await
                            .map_err(tx_fail("read the agenda"))?;
                        if stored.is_none_or(|stored| {
                            stored.id != doc.id || stored.state != from.as_str()
                        }) {
                            return Ok(false);
                        }
                        db.fluent()
                            .update()
                            .in_col(AGENDA)
                            .document_id(doc_name.as_str())
                            .object(doc.as_ref())
                            .add_to_transaction(transaction)
                            .map_err(tx_fail("update the agenda"))?;
                        Ok(true)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|error| transaction_error(error, "update the agenda"))
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let Ok(doc_name) = doc_id("agenda item", id) else {
            return Ok(false);
        };
        let id = id.to_string();
        let matches = move |doc: &AgendaDoc| doc.id == id;
        delete_where(&self.db, AGENDA, doc_name, matches, "delete from the agenda").await
    }
}
