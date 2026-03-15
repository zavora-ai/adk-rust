//! MongoDB session service backend.
//!
//! Provides [`MongoSessionService`] for session persistence using MongoDB.
//! Enabled via the `mongodb` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! let service = MongoSessionService::new("mongodb://localhost:27017", "adk_sessions").await?;
//! service.migrate().await?;
//! ```

use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mongodb::bson::{self, Document, doc};
use mongodb::options::{FindOptions, IndexOptions, UpdateOptions};
use mongodb::{Client, Database, IndexModel};
use serde_json::Value;
use std::collections::HashMap;
use tracing::instrument;
use uuid::Uuid;

/// MongoDB-backed session service implementing [`SessionService`](crate::SessionService).
///
/// Uses four collections for the three-tier state model:
/// - `sessions` — session documents with session-level state
/// - `events` — event documents linked by `session_id`
/// - `app_states` — application-level state keyed by `app_name`
/// - `user_states` — user-level state keyed by `(app_name, user_id)`
pub struct MongoSessionService {
    db: Database,
}

impl MongoSessionService {
    /// Connect to MongoDB using a connection string and database name.
    ///
    /// Returns an error with "mongodb connection failed" context if the
    /// connection cannot be established.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let service = MongoSessionService::new(
    ///     "mongodb://localhost:27017",
    ///     "adk_sessions",
    /// ).await?;
    /// ```
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        let client_options = mongodb::options::ClientOptions::parse(connection_string)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("mongodb connection failed: {e}")))?;
        let client = Client::with_options(client_options)
            .map_err(|e| adk_core::AdkError::Session(format!("mongodb connection failed: {e}")))?;
        let db = client.database(database_name);
        Ok(Self { db })
    }

    /// Registry collection name for tracking applied migration versions.
    const REGISTRY_COLLECTION: &'static str = "_adk_session_migrations";

    /// Compiled-in MongoDB migration steps.
    ///
    /// Each entry is `(version, description)`. The actual migration logic is
    /// dispatched by version number in [`run_mongo_session_step`].
    const MONGO_SESSION_MIGRATIONS: &'static [(i64, &'static str)] =
        &[(1, "create initial indexes")];

    /// Run versioned migrations for MongoDB session storage.
    ///
    /// The runner:
    /// 1. Creates the registry collection with a unique index on `version`.
    /// 2. Detects baseline — if `sessions` collection exists but registry is
    ///    empty, records v1 as already applied.
    /// 3. Reads the maximum applied version from the registry.
    /// 4. Returns an error if the database version exceeds the compiled-in max.
    /// 5. Executes each unapplied step idempotently and records it.
    pub async fn migrate(&self) -> Result<()> {
        // Step 1: Ensure registry collection has a unique index on `version`
        self.db
            .collection::<Document>(Self::REGISTRY_COLLECTION)
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "version": 1 })
                    .options(
                        IndexOptions::builder()
                            .unique(true)
                            .name("idx_migration_version_unique".to_string())
                            .build(),
                    )
                    .build(),
            )
            .await
            .map_err(|e| {
                adk_core::AdkError::Session(format!("migration registry creation failed: {e}"))
            })?;

        // Step 2: Read current max applied version
        let mut max_applied = self.read_max_applied_version().await?;

        // Step 3: Baseline detection — if registry is empty but sessions
        // collection already exists, record v1 as applied.
        if max_applied == 0 {
            let existing = self.detect_existing_tables().await?;
            if existing {
                if let Some(&(version, description)) = Self::MONGO_SESSION_MIGRATIONS.first() {
                    self.record_migration(version, description).await?;
                    max_applied = version;
                }
            }
        }

        // Step 4: Compiled-in max version
        let max_compiled = Self::MONGO_SESSION_MIGRATIONS.last().map(|s| s.0).unwrap_or(0);

        // Step 5: Version mismatch check
        if max_applied > max_compiled {
            return Err(adk_core::AdkError::Session(format!(
                "schema version mismatch: database is at v{max_applied} \
                 but code only knows up to v{max_compiled}. \
                 Upgrade your ADK version."
            )));
        }

        // Step 6: Execute unapplied steps idempotently
        for &(version, description) in Self::MONGO_SESSION_MIGRATIONS {
            if version <= max_applied {
                continue;
            }

            run_mongo_session_step(&self.db, version).await.map_err(|e| {
                adk_core::AdkError::Session(format!(
                    "{}",
                    crate::migration::MigrationError {
                        version,
                        description: description.to_string(),
                        cause: e.to_string(),
                    }
                ))
            })?;

            self.record_migration(version, description).await?;
        }

        Ok(())
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    pub async fn schema_version(&self) -> Result<i64> {
        // Check if registry collection exists
        let collections = self.db.list_collection_names().await.map_err(|e| {
            adk_core::AdkError::Session(format!("schema version query failed: {e}"))
        })?;
        if !collections.contains(&Self::REGISTRY_COLLECTION.to_string()) {
            return Ok(0);
        }

        self.read_max_applied_version().await
    }

    /// Read the maximum applied version from the registry collection.
    async fn read_max_applied_version(&self) -> Result<i64> {
        use mongodb::options::FindOneOptions;

        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let opts = FindOneOptions::builder().sort(doc! { "version": -1 }).build();
        let result = registry.find_one(doc! {}).with_options(opts).await.map_err(|e| {
            adk_core::AdkError::Session(format!("migration registry read failed: {e}"))
        })?;

        match result {
            Some(doc) => {
                let version = doc.get_i64("version").unwrap_or(0);
                Ok(version)
            }
            None => Ok(0),
        }
    }

    /// Detect whether the `sessions` collection already exists (baseline).
    async fn detect_existing_tables(&self) -> Result<bool> {
        let collections =
            self.db.list_collection_names().await.map_err(|e| {
                adk_core::AdkError::Session(format!("baseline detection failed: {e}"))
            })?;
        Ok(collections.contains(&"sessions".to_string()))
    }

    /// Record a successfully applied migration step in the registry.
    async fn record_migration(&self, version: i64, description: &str) -> Result<()> {
        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let now = chrono_to_bson_dt(Utc::now());
        registry
            .insert_one(doc! {
                "version": version,
                "description": description,
                "applied_at": now,
            })
            .await
            .map_err(|e| {
                adk_core::AdkError::Session(format!(
                    "{}",
                    crate::migration::MigrationError {
                        version,
                        description: description.to_string(),
                        cause: format!("registry record failed: {e}"),
                    }
                ))
            })?;
        Ok(())
    }
}

/// Execute a single MongoDB session migration step by version number.
///
/// Each step is idempotent — re-running a step that has already been applied
/// completes without error (MongoDB's `create_index` is a no-op if the index
/// already exists with the same specification).
async fn run_mongo_session_step(db: &Database, version: i64) -> Result<()> {
    match version {
        1 => mongo_session_v1(db).await,
        _ => Err(adk_core::AdkError::Session(format!("unknown migration version: {version}"))),
    }
}

/// V1: Create initial indexes on sessions, events, app_states, and user_states.
///
/// This matches the original `migrate()` index creation logic.
async fn mongo_session_v1(db: &Database) -> Result<()> {
    // sessions: unique compound index on (app_name, user_id, session_id)
    db.collection::<Document>("sessions")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "app_name": 1, "user_id": 1, "session_id": 1 })
                .options(
                    IndexOptions::builder()
                        .unique(true)
                        .name("idx_sessions_unique".to_string())
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("index creation failed: {e}")))?;

    // events: index on (session_id, timestamp)
    db.collection::<Document>("events")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "session_id": 1, "timestamp": 1 })
                .options(IndexOptions::builder().name("idx_events_session_ts".to_string()).build())
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("index creation failed: {e}")))?;

    // app_states: unique index on app_name
    db.collection::<Document>("app_states")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "app_name": 1 })
                .options(
                    IndexOptions::builder()
                        .unique(true)
                        .name("idx_app_states_unique".to_string())
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("index creation failed: {e}")))?;

    // user_states: unique compound index on (app_name, user_id)
    db.collection::<Document>("user_states")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "app_name": 1, "user_id": 1 })
                .options(
                    IndexOptions::builder()
                        .unique(true)
                        .name("idx_user_states_unique".to_string())
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("index creation failed: {e}")))?;

    Ok(())
}

/// Convert a `HashMap<String, Value>` to a BSON `Document`.
fn state_to_bson(
    state: &HashMap<String, Value>,
) -> std::result::Result<Document, adk_core::AdkError> {
    let json_value = serde_json::to_value(state)
        .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
    let bson_value = bson::to_bson(&json_value)
        .map_err(|e| adk_core::AdkError::Session(format!("bson conversion failed: {e}")))?;
    match bson_value {
        bson::Bson::Document(doc) => Ok(doc),
        _ => Ok(Document::new()),
    }
}

/// Convert a BSON `Document` to a `HashMap<String, Value>`.
fn bson_to_state(doc: &Document) -> HashMap<String, Value> {
    let bson_value = bson::Bson::Document(doc.clone());
    match bson::from_bson::<serde_json::Value>(bson_value) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

/// Convert a `chrono::DateTime<Utc>` to a `bson::DateTime`.
fn chrono_to_bson_dt(dt: DateTime<Utc>) -> bson::DateTime {
    bson::DateTime::from_millis(dt.timestamp_millis())
}

/// Convert a `bson::DateTime` to a `chrono::DateTime<Utc>`.
fn bson_dt_to_chrono(dt: bson::DateTime) -> DateTime<Utc> {
    let millis = dt.timestamp_millis();
    DateTime::from_timestamp_millis(millis).unwrap_or_default()
}

#[async_trait]
impl SessionService for MongoSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();
        let bson_now = chrono_to_bson_dt(now);

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;
        session
            .start_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        // Load existing app state
        let app_states_coll = self.db.collection::<Document>("app_states");
        let existing_app_state: HashMap<String, Value> = app_states_coll
            .find_one(doc! { "app_name": &req.app_name })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .and_then(|doc| doc.get_document("state").ok().map(bson_to_state))
            .unwrap_or_default();

        let mut new_app_state = existing_app_state;
        new_app_state.extend(app_delta);
        let app_state_bson = state_to_bson(&new_app_state)?;

        app_states_coll
            .update_one(
                doc! { "app_name": &req.app_name },
                doc! {
                    "$set": {
                        "app_name": &req.app_name,
                        "state": &app_state_bson,
                        "updated_at": bson_now,
                    }
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Load existing user state
        let user_states_coll = self.db.collection::<Document>("user_states");
        let existing_user_state: HashMap<String, Value> = user_states_coll
            .find_one(doc! { "app_name": &req.app_name, "user_id": &req.user_id })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .and_then(|doc| doc.get_document("state").ok().map(bson_to_state))
            .unwrap_or_default();

        let mut new_user_state = existing_user_state;
        new_user_state.extend(user_delta);
        let user_state_bson = state_to_bson(&new_user_state)?;

        user_states_coll
            .update_one(
                doc! { "app_name": &req.app_name, "user_id": &req.user_id },
                doc! {
                    "$set": {
                        "app_name": &req.app_name,
                        "user_id": &req.user_id,
                        "state": &user_state_bson,
                        "updated_at": bson_now,
                    }
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Create session with merged state
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);
        let merged_state_bson = state_to_bson(&merged_state)?;

        let sessions_coll = self.db.collection::<Document>("sessions");
        sessions_coll
            .insert_one(doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "session_id": &session_id,
                "state": &merged_state_bson,
                "created_at": bson_now,
                "updated_at": bson_now,
            })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        session
            .commit_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(Box::new(MongoSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: merged_state,
            events: Vec::new(),
            updated_at: now,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let sessions_coll = self.db.collection::<Document>("sessions");
        let session_doc = sessions_coll
            .find_one(doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "session_id": &req.session_id,
            })
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::Session("session not found".into()))?;

        let state: HashMap<String, Value> =
            session_doc.get_document("state").map(bson_to_state).unwrap_or_default();
        let updated_at = session_doc
            .get_datetime("updated_at")
            .map(|dt| bson_dt_to_chrono(*dt))
            .unwrap_or_else(|_| Utc::now());

        // Fetch events ordered by timestamp
        let events_coll = self.db.collection::<Document>("events");
        let find_options = FindOptions::builder().sort(doc! { "timestamp": 1 }).build();
        let mut cursor = events_coll
            .find(doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "session_id": &req.session_id,
            })
            .with_options(find_options)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?;

        let mut events: Vec<Event> = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| adk_core::AdkError::Session(format!("deserialize failed: {e}")))?;
            if let Some(event) = doc_to_event(&doc) {
                events.push(event);
            }
        }

        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(Box::new(MongoSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state,
            events,
            updated_at,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        let sessions_coll = self.db.collection::<Document>("sessions");
        let find_options = FindOptions::builder()
            .sort(doc! { "updated_at": -1 })
            .limit(req.limit.map(|l| l as i64))
            .skip(req.offset.map(|o| o as u64))
            .build();
        let mut cursor = sessions_coll
            .find(doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
            })
            .with_options(find_options)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?;

        let mut sessions: Vec<Box<dyn Session>> = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| adk_core::AdkError::Session(format!("deserialize failed: {e}")))?;
            let state: HashMap<String, Value> =
                doc.get_document("state").map(bson_to_state).unwrap_or_default();
            let updated_at = doc
                .get_datetime("updated_at")
                .map(|dt| bson_dt_to_chrono(*dt))
                .unwrap_or_else(|_| Utc::now());
            let session_id = doc.get_str("session_id").unwrap_or_default().to_string();

            sessions.push(Box::new(MongoSession {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id,
                state,
                events: Vec::new(),
                updated_at,
            }));
        }

        Ok(sessions)
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;
        session
            .start_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        // Delete events first
        self.db
            .collection::<Document>("events")
            .delete_many(doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "session_id": &req.session_id,
            })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("delete failed: {e}")))?;

        // Delete session
        self.db
            .collection::<Document>("sessions")
            .delete_one(doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "session_id": &req.session_id,
            })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("delete failed: {e}")))?;

        session
            .commit_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;
        session
            .start_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        // Find the session
        let sessions_coll = self.db.collection::<Document>("sessions");
        let session_doc = sessions_coll
            .find_one(doc! { "session_id": session_id })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::Session("session not found".into()))?;

        let app_name = session_doc.get_str("app_name").unwrap_or_default().to_string();
        let user_id = session_doc.get_str("user_id").unwrap_or_default().to_string();
        let existing_state: HashMap<String, Value> =
            session_doc.get_document("state").map(bson_to_state).unwrap_or_default();
        let (_, _, mut session_state) = state_utils::extract_state_deltas(&existing_state);

        // Load current app state
        let app_states_coll = self.db.collection::<Document>("app_states");
        let app_state: HashMap<String, Value> = app_states_coll
            .find_one(doc! { "app_name": &app_name })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .and_then(|doc| doc.get_document("state").ok().map(bson_to_state))
            .unwrap_or_default();

        // Load current user state
        let user_states_coll = self.db.collection::<Document>("user_states");
        let user_state: HashMap<String, Value> = user_states_coll
            .find_one(doc! { "app_name": &app_name, "user_id": &user_id })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .and_then(|doc| doc.get_document("state").ok().map(bson_to_state))
            .unwrap_or_default();

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let bson_ts = chrono_to_bson_dt(event.timestamp);

        // Update app state
        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);
        let app_state_bson = state_to_bson(&new_app_state)?;

        app_states_coll
            .update_one(
                doc! { "app_name": &app_name },
                doc! {
                    "$set": {
                        "app_name": &app_name,
                        "state": &app_state_bson,
                        "updated_at": bson_ts,
                    }
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Update user state
        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);
        let user_state_bson = state_to_bson(&new_user_state)?;

        user_states_coll
            .update_one(
                doc! { "app_name": &app_name, "user_id": &user_id },
                doc! {
                    "$set": {
                        "app_name": &app_name,
                        "user_id": &user_id,
                        "state": &user_state_bson,
                        "updated_at": bson_ts,
                    }
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Update session merged state
        session_state.extend(session_delta);
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);
        let merged_state_bson = state_to_bson(&merged_state)?;

        sessions_coll
            .update_one(
                doc! {
                    "app_name": &app_name,
                    "user_id": &user_id,
                    "session_id": session_id,
                },
                doc! {
                    "$set": {
                        "state": &merged_state_bson,
                        "updated_at": bson_ts,
                    }
                },
            )
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("update failed: {e}")))?;

        // Insert event
        let llm_response_bson = bson::to_bson(&event.llm_response)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let actions_bson = bson::to_bson(&event.actions)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let tool_ids_bson = bson::to_bson(&event.long_running_tool_ids)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        let events_coll = self.db.collection::<Document>("events");
        events_coll
            .insert_one(doc! {
                "id": &event.id,
                "session_id": session_id,
                "app_name": &app_name,
                "user_id": &user_id,
                "invocation_id": &event.invocation_id,
                "branch": &event.branch,
                "author": &event.author,
                "timestamp": bson_ts,
                "llm_response": llm_response_bson,
                "actions": actions_bson,
                "long_running_tool_ids": tool_ids_bson,
            })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        session
            .commit_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(
        app_name = %req.identity.app_name,
        user_id = %req.identity.user_id,
        session_id = %req.identity.session_id,
    ))]
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let app_name = req.identity.app_name.as_ref();
        let user_id = req.identity.user_id.as_ref();
        let session_id = req.identity.session_id.as_ref();

        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;
        session
            .start_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        // Use the full composite key — no ambiguity possible.
        let sessions_coll = self.db.collection::<Document>("sessions");
        let session_doc = sessions_coll
            .find_one(doc! { "app_name": app_name, "user_id": user_id, "session_id": session_id })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::Session("session not found".into()))?;

        let existing_state: HashMap<String, Value> =
            session_doc.get_document("state").map(bson_to_state).unwrap_or_default();
        let (_, _, mut session_state) = state_utils::extract_state_deltas(&existing_state);

        // Load current app state
        let app_states_coll = self.db.collection::<Document>("app_states");
        let app_state: HashMap<String, Value> = app_states_coll
            .find_one(doc! { "app_name": app_name })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .and_then(|doc| doc.get_document("state").ok().map(bson_to_state))
            .unwrap_or_default();

        // Load current user state
        let user_states_coll = self.db.collection::<Document>("user_states");
        let user_state: HashMap<String, Value> = user_states_coll
            .find_one(doc! { "app_name": app_name, "user_id": user_id })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            .and_then(|doc| doc.get_document("state").ok().map(bson_to_state))
            .unwrap_or_default();

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let bson_ts = chrono_to_bson_dt(event.timestamp);

        // Update app state
        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);
        let app_state_bson = state_to_bson(&new_app_state)?;

        app_states_coll
            .update_one(
                doc! { "app_name": app_name },
                doc! {
                    "$set": {
                        "app_name": app_name,
                        "state": &app_state_bson,
                        "updated_at": bson_ts,
                    }
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Update user state
        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);
        let user_state_bson = state_to_bson(&new_user_state)?;

        user_states_coll
            .update_one(
                doc! { "app_name": app_name, "user_id": user_id },
                doc! {
                    "$set": {
                        "app_name": app_name,
                        "user_id": user_id,
                        "state": &user_state_bson,
                        "updated_at": bson_ts,
                    }
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Update session merged state
        session_state.extend(session_delta);
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);
        let merged_state_bson = state_to_bson(&merged_state)?;

        sessions_coll
            .update_one(
                doc! {
                    "app_name": app_name,
                    "user_id": user_id,
                    "session_id": session_id,
                },
                doc! {
                    "$set": {
                        "state": &merged_state_bson,
                        "updated_at": bson_ts,
                    }
                },
            )
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("update failed: {e}")))?;

        // Insert event
        let llm_response_bson = bson::to_bson(&event.llm_response)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let actions_bson = bson::to_bson(&event.actions)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let tool_ids_bson = bson::to_bson(&event.long_running_tool_ids)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        let events_coll = self.db.collection::<Document>("events");
        events_coll
            .insert_one(doc! {
                "id": &event.id,
                "session_id": session_id,
                "app_name": app_name,
                "user_id": user_id,
                "invocation_id": &event.invocation_id,
                "branch": &event.branch,
                "author": &event.author,
                "timestamp": bson_ts,
                "llm_response": llm_response_bson,
                "actions": actions_bson,
                "long_running_tool_ids": tool_ids_bson,
            })
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        session
            .commit_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;
        session
            .start_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        let filter = doc! { "app_name": app_name, "user_id": user_id };

        // Delete all events for this user's sessions
        self.db
            .collection::<Document>("events")
            .delete_many(filter.clone())
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("delete_all_sessions failed: {e}")))?;

        // Delete all sessions
        self.db
            .collection::<Document>("sessions")
            .delete_many(filter)
            .session(&mut session)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("delete_all_sessions failed: {e}")))?;

        session
            .commit_transaction()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("health check failed: {e}")))?;
        Ok(())
    }
}

/// Convert a BSON event document to an `Event`.
fn doc_to_event(doc: &Document) -> Option<Event> {
    let llm_response_bson = doc.get("llm_response")?;
    let actions_bson = doc.get("actions")?;
    let tool_ids_bson = doc.get("long_running_tool_ids")?;

    let llm_response = bson::from_bson(llm_response_bson.clone()).ok()?;
    let actions = bson::from_bson(actions_bson.clone()).ok()?;
    let long_running_tool_ids = bson::from_bson(tool_ids_bson.clone()).ok()?;

    let timestamp = doc
        .get_datetime("timestamp")
        .map(|dt| bson_dt_to_chrono(*dt))
        .unwrap_or_else(|_| Utc::now());

    Some(Event {
        id: doc.get_str("id").unwrap_or_default().to_string(),
        timestamp,
        invocation_id: doc.get_str("invocation_id").unwrap_or_default().to_string(),
        branch: doc.get_str("branch").unwrap_or_default().to_string(),
        author: doc.get_str("author").unwrap_or_default().to_string(),
        llm_request: None,
        llm_response,
        actions,
        long_running_tool_ids,
        provider_metadata: std::collections::HashMap::new(),
    })
}

struct MongoSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for MongoSession {
    fn id(&self) -> &str {
        &self.session_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl State for MongoSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        self.state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

impl Events for MongoSession {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}
