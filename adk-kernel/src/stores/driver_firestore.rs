//! The Firestore [`DriverStore`] (spec 3.2 §7.7, §13 Q8; features `drivers`, `firestore`).

use adk_core::ApprovalScope;
use async_trait::async_trait;
use firestore::FirestoreDb;
use serde::{Deserialize, Serialize};

use super::firestore::{connect, delete_where, digest_id, doc_id, json, store_error};
use crate::drivers::store::park_key;
use crate::drivers::{DriverPark, DriverRecord, DriverStore};
use crate::error::Result;

const DRIVERS: &str = "adk_kernel_drivers";
const PARKS: &str = "adk_kernel_driver_parks";

/// One driver's record, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DriverDoc {
    id: String,
    record: String,
}

/// One parked call, as stored, with the scope and call id it is keyed by.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParkDoc {
    app: String,
    user: String,
    session: String,
    call_id: String,
    driver: String,
    park: String,
}

impl ParkDoc {
    fn is_for(&self, scope: &ApprovalScope, call_id: &str) -> bool {
        self.app == scope.app_name
            && self.user == scope.user_id
            && self.session == scope.session_id
            && self.call_id == call_id
    }
}

/// The document ID of the park of `call_id` in `scope`.
fn park_doc_id(scope: &ApprovalScope, call_id: &str) -> String {
    digest_id(&[&scope.app_name, &scope.user_id, &scope.session_id, call_id])
}

/// A [`DriverStore`] in Firestore, shared by every replica (spec 3.2 §7.7, §13 Q8; features
/// `drivers`, `firestore`).
///
/// | Collection (document ID) | Contents |
/// |--------------------------|----------|
/// | `adk_kernel_drivers` (hex of the driver id) | `id` and `record`, the record as JSON text |
/// | `adk_kernel_driver_parks` (SHA-256 of the scope and call id) | `app`, `user`, `session`, `call_id`, `driver`, and `park`, the park as JSON text |
///
/// A park's document stores the scope and call id it is addressed by, and every read
/// compares them, so two keys that share a digest never read each other's park. `list`
/// and `list_parks` read every record or every park of one driver with an equality query
/// and order them in memory, so no composite index is needed. `delete` and `delete_park`
/// read and delete in one transaction, so of two concurrent deletes one answers `true`.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::FirestoreDriverStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreDriverStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreDriverStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreDriverStore").finish_non_exhaustive()
    }
}

impl FirestoreDriverStore {
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
impl DriverStore for FirestoreDriverStore {
    async fn put(&self, record: DriverRecord) -> Result<()> {
        let doc =
            DriverDoc { id: record.id().to_string(), record: serde_json::to_string(&record)? };
        self.db
            .fluent()
            .update()
            .in_col(DRIVERS)
            .document_id(doc_id("driver", &doc.id)?)
            .object(&doc)
            .execute::<DriverDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write the driver table", &error))
    }

    async fn get(&self, id: &str) -> Result<Option<DriverRecord>> {
        let Ok(doc_name) = doc_id("driver", id) else {
            return Ok(None);
        };
        let doc: Option<DriverDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(DRIVERS)
            .obj()
            .one(doc_name)
            .await
            .map_err(|error| store_error("read the driver table", &error))?;
        doc.filter(|doc| doc.id == id).map(|doc| json("driver", &doc.record)).transpose()
    }

    async fn list(&self) -> Result<Vec<DriverRecord>> {
        let mut docs: Vec<DriverDoc> = self
            .db
            .fluent()
            .select()
            .from(DRIVERS)
            .obj()
            .query()
            .await
            .map_err(|error| store_error("list the driver table", &error))?;
        docs.sort_by(|a, b| a.id.cmp(&b.id));
        docs.iter().map(|doc| json("driver", &doc.record)).collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let Ok(doc_name) = doc_id("driver", id) else {
            return Ok(false);
        };
        let id = id.to_string();
        let matches = move |doc: &DriverDoc| doc.id == id;
        delete_where(&self.db, DRIVERS, doc_name, matches, "delete from the driver table").await
    }

    async fn put_park(&self, park: DriverPark) -> Result<()> {
        let doc = ParkDoc {
            app: park.scope.app_name.clone(),
            user: park.scope.user_id.clone(),
            session: park.scope.session_id.clone(),
            call_id: park.call_id.clone(),
            driver: park.driver.clone(),
            park: serde_json::to_string(&park)?,
        };
        self.db
            .fluent()
            .update()
            .in_col(PARKS)
            .document_id(park_doc_id(&park.scope, &park.call_id))
            .object(&doc)
            .execute::<ParkDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write the driver table", &error))
    }

    async fn get_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<Option<DriverPark>> {
        let doc: Option<ParkDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(PARKS)
            .obj()
            .one(park_doc_id(scope, call_id))
            .await
            .map_err(|error| store_error("read the driver table", &error))?;
        doc.filter(|doc| doc.is_for(scope, call_id))
            .map(|doc| json("parked driver call", &doc.park))
            .transpose()
    }

    async fn list_parks(&self, driver: Option<&str>) -> Result<Vec<DriverPark>> {
        let docs: Vec<ParkDoc> = self
            .db
            .fluent()
            .select()
            .from(PARKS)
            .filter(|q| q.for_all([driver.and_then(|driver| q.field("driver").eq(driver))]))
            .obj()
            .query()
            .await
            .map_err(|error| store_error("list the driver table", &error))?;
        let mut parks = docs
            .iter()
            .map(|doc| json::<DriverPark>("parked driver call", &doc.park))
            .collect::<Result<Vec<_>>>()?;
        parks.sort_by_key(|park| park_key(&park.scope, &park.call_id));
        Ok(parks)
    }

    async fn delete_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<bool> {
        let (scope, call) = (scope.clone(), call_id.to_string());
        let doc_name = park_doc_id(&scope, call_id);
        let matches = move |doc: &ParkDoc| doc.is_for(&scope, &call);
        delete_where(&self.db, PARKS, doc_name, matches, "delete from the driver table").await
    }
}
