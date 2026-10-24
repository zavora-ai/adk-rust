//! The MongoDB [`DriverStore`] (spec 3.2 §7.7, §13 Q8; features `drivers`, `mongodb`).

use adk_core::ApprovalScope;
use async_trait::async_trait;
use mongodb::bson::{Document, doc};
use mongodb::{Collection, Database};

use super::mongodb::{
    collect, connect, create_indexes, json, retry_upsert, sorted, store_error, text,
};
use crate::drivers::{DriverPark, DriverRecord, DriverStore};
use crate::error::Result;

const DRIVERS: &str = "adk_kernel_drivers";
const PARKS: &str = "adk_kernel_driver_parks";

/// A [`DriverStore`] in MongoDB, shared by every replica (spec 3.2 §7.7, §13 Q8; features
/// `drivers`, `mongodb`). **Alpha** until its live conformance suite passes in CI.
///
/// | Collection | `_id` | Contents |
/// |------------|-------|----------|
/// | `adk_kernel_drivers` | The driver id | `record`, the record as JSON text |
/// | `adk_kernel_driver_parks` | `{app, user, session, call}`: the park's scope and call id | `driver`, the fields `app`, `user`, `session` and `call_id` that `list_parks` sorts on, and `park`, the park as JSON text |
///
/// Sorts use MongoDB's binary string comparison, so drivers order by their ids' UTF-8
/// bytes and parks by scope, then call id, as on every backend. Every write is a
/// single-document write, so a standalone `mongod` serves it as a replica set does.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::stores::MongoDriverStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoDriverStore {
    db: Database,
}

impl MongoDriverStore {
    /// A store over `db`, such as the process store's.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Connects to `connection_string` and uses the database `database_name`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the connection string
    /// is invalid.
    pub async fn connect(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(connect(connection_string, database_name).await?))
    }

    /// The database the store writes to.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Creates the indexes `list_parks` reads; an index that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the server cannot be
    /// reached or refuses an index.
    pub async fn migrate(&self) -> Result<()> {
        create_indexes(
            &self.parks(),
            &[
                ("adk_kernel_driver_parks_driver", doc! { "driver": 1 }),
                (
                    "adk_kernel_driver_parks_order",
                    doc! { "app": 1, "user": 1, "session": 1, "call_id": 1 },
                ),
            ],
        )
        .await
    }

    fn records(&self) -> Collection<Document> {
        self.db.collection(DRIVERS)
    }

    fn parks(&self) -> Collection<Document> {
        self.db.collection(PARKS)
    }
}

/// The `_id` of the park of `call_id` in `scope`.
fn park_id(scope: &ApprovalScope, call_id: &str) -> Document {
    doc! {
        "app": &scope.app_name,
        "user": &scope.user_id,
        "session": &scope.session_id,
        "call": call_id,
    }
}

#[async_trait]
impl DriverStore for MongoDriverStore {
    async fn put(&self, record: DriverRecord) -> Result<()> {
        let id = record.id().to_string();
        let document = doc! { "_id": &id, "record": serde_json::to_string(&record)? };
        let records = self.records();
        retry_upsert("write the driver table", || {
            records.replace_one(doc! { "_id": &id }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<DriverRecord>> {
        let document = self
            .records()
            .find_one(doc! { "_id": id })
            .await
            .map_err(|error| store_error("read the driver table", &error))?;
        document.map(|document| json("driver", text(&document, "record")?)).transpose()
    }

    async fn list(&self) -> Result<Vec<DriverRecord>> {
        let cursor = self
            .records()
            .find(Document::new())
            .with_options(sorted(doc! { "_id": 1 }, None))
            .await
            .map_err(|error| store_error("list the driver table", &error))?;
        collect(cursor, "list the driver table")
            .await?
            .iter()
            .map(|document| json("driver", text(document, "record")?))
            .collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let result = self
            .records()
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|error| store_error("delete from the driver table", &error))?;
        Ok(result.deleted_count == 1)
    }

    async fn put_park(&self, park: DriverPark) -> Result<()> {
        let id = park_id(&park.scope, &park.call_id);
        let document = doc! {
            "_id": id.clone(),
            "driver": &park.driver,
            "app": &park.scope.app_name,
            "user": &park.scope.user_id,
            "session": &park.scope.session_id,
            "call_id": &park.call_id,
            "park": serde_json::to_string(&park)?,
        };
        let parks = self.parks();
        retry_upsert("write the driver table", || {
            parks.replace_one(doc! { "_id": id.clone() }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn get_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<Option<DriverPark>> {
        let document = self
            .parks()
            .find_one(doc! { "_id": park_id(scope, call_id) })
            .await
            .map_err(|error| store_error("read the driver table", &error))?;
        document.map(|document| json("parked driver call", text(&document, "park")?)).transpose()
    }

    async fn list_parks(&self, driver: Option<&str>) -> Result<Vec<DriverPark>> {
        let filter = driver.map_or_else(Document::new, |driver| doc! { "driver": driver });
        let cursor = self
            .parks()
            .find(filter)
            .with_options(sorted(doc! { "app": 1, "user": 1, "session": 1, "call_id": 1 }, None))
            .await
            .map_err(|error| store_error("list the driver table", &error))?;
        collect(cursor, "list the driver table")
            .await?
            .iter()
            .map(|document| json("parked driver call", text(document, "park")?))
            .collect()
    }

    async fn delete_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<bool> {
        let result = self
            .parks()
            .delete_one(doc! { "_id": park_id(scope, call_id) })
            .await
            .map_err(|error| store_error("delete from the driver table", &error))?;
        Ok(result.deleted_count == 1)
    }
}
