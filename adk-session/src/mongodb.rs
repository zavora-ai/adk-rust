//! MongoDB session service backend.
//!
//! Provides [`MongoSessionService`] for session persistence using MongoDB.
//! Enabled via the `mongodb` feature flag.
//!
//! Supports both standalone and replica-set deployments. Transactions are
//! used automatically when the deployment supports them (replica set / sharded
//! cluster). On standalone deployments, operations execute sequentially
//! without transactions — the upsert-based writes are idempotent, so partial
//! failures leave state recoverable.
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
///
/// Automatically detects whether the MongoDB deployment supports transactions
/// (replica set or sharded cluster) and falls back to non-transactional
/// sequential writes on standalone deployments.
pub struct MongoSessionService {
    db: Database,
    supports_transactions: bool,
}

impl MongoSessionService {
    /// Connect to MongoDB using a connection string and database name.
    ///
    /// Automatically detects whether the deployment supports transactions
    /// by checking for replica set membership via the `hello` command.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        let client_options = mongodb::options::ClientOptions::parse(connection_string)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("mongodb connection failed: {e}")))?;
        let client = Client::with_options(client_options)
            .map_err(|e| adk_core::AdkError::session(format!("mongodb connection failed: {e}")))?;
        let db = client.database(database_name);

        // Detect replica set support via hello command.
        let supports_transactions = match db.run_command(doc! { "hello": 1 }).await {
            Ok(reply) => reply.get_str("setName").is_ok(),
            Err(_) => false,
        };

        if supports_transactions {
            tracing::info!("MongoDB replica set detected — transactions enabled");
        } else {
            tracing::info!(
                "MongoDB standalone detected — using sequential writes (no transactions)"
            );
        }

        Ok(Self { db, supports_transactions })
    }

    /// Returns whether this deployment supports multi-document transactions.
    pub fn supports_transactions(&self) -> bool {
        self.supports_transactions
    }

    const REGISTRY_COLLECTION: &'static str = "_adk_session_migrations";

    const MONGO_SESSION_MIGRATIONS: &'static [(i64, &'static str)] =
        &[(1, "create initial indexes")];

    /// Run versioned migrations for MongoDB session storage.
    pub async fn migrate(&self) -> Result<()> {
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
                adk_core::AdkError::session(format!("migration registry creation failed: {e}"))
            })?;

        let mut max_applied = self.read_max_applied_version().await?;

        if max_applied == 0 {
            let existing = self.detect_existing_tables().await?;
            if existing {
                if let Some(&(version, description)) = Self::MONGO_SESSION_MIGRATIONS.first() {
                    self.record_migration(version, description).await?;
                    max_applied = version;
                }
            }
        }

        let max_compiled = Self::MONGO_SESSION_MIGRATIONS.last().map(|s| s.0).unwrap_or(0);
        if max_applied > max_compiled {
            return Err(adk_core::AdkError::session(format!(
                "schema version mismatch: database is at v{max_applied} but code only knows up to v{max_compiled}. Upgrade your ADK version."
            )));
        }

        for &(version, description) in Self::MONGO_SESSION_MIGRATIONS {
            if version <= max_applied {
                continue;
            }
            run_mongo_session_step(&self.db, version).await.map_err(|e| {
                adk_core::AdkError::session(format!(
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

    pub async fn schema_version(&self) -> Result<i64> {
        let collections = self.db.list_collection_names().await.map_err(|e| {
            adk_core::AdkError::session(format!("schema version query failed: {e}"))
        })?;
        if !collections.contains(&Self::REGISTRY_COLLECTION.to_string()) {
            return Ok(0);
        }
        self.read_max_applied_version().await
    }

    async fn read_max_applied_version(&self) -> Result<i64> {
        use mongodb::options::FindOneOptions;
        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let opts = FindOneOptions::builder().sort(doc! { "version": -1 }).build();
        let result = registry.find_one(doc! {}).with_options(opts).await.map_err(|e| {
            adk_core::AdkError::session(format!("migration registry read failed: {e}"))
        })?;
        match result {
            Some(doc) => Ok(doc.get_i64("version").unwrap_or(0)),
            None => Ok(0),
        }
    }

    async fn detect_existing_tables(&self) -> Result<bool> {
        let collections =
            self.db.list_collection_names().await.map_err(|e| {
                adk_core::AdkError::session(format!("baseline detection failed: {e}"))
            })?;
        Ok(collections.contains(&"sessions".to_string()))
    }

    async fn record_migration(&self, version: i64, description: &str) -> Result<()> {
        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let now = chrono_to_bson_dt(Utc::now());
        registry
            .insert_one(doc! { "version": version, "description": description, "applied_at": now })
            .await
            .map_err(|e| {
                adk_core::AdkError::session(format!(
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

    /// Start a transaction if supported, otherwise return None.
    async fn maybe_start_transaction(&self) -> Result<Option<mongodb::ClientSession>> {
        if self.supports_transactions {
            let mut s =
                self.db.client().start_session().await.map_err(|e| {
                    adk_core::AdkError::session(format!("session start failed: {e}"))
                })?;
            s.start_transaction().await.map_err(|e| {
                adk_core::AdkError::session(format!("transaction start failed: {e}"))
            })?;
            Ok(Some(s))
        } else {
            Ok(None)
        }
    }

    /// Commit a transaction if one is active.
    async fn maybe_commit(session: &mut Option<mongodb::ClientSession>) -> Result<()> {
        if let Some(s) = session {
            s.commit_transaction()
                .await
                .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;
        }
        Ok(())
    }
}

// ── Migration steps ──

async fn run_mongo_session_step(db: &Database, version: i64) -> Result<()> {
    match version {
        1 => mongo_session_v1(db).await,
        _ => Err(adk_core::AdkError::session(format!("unknown migration version: {version}"))),
    }
}

async fn mongo_session_v1(db: &Database) -> Result<()> {
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
        .map_err(|e| adk_core::AdkError::session(format!("index creation failed: {e}")))?;

    db.collection::<Document>("events")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "session_id": 1, "timestamp": 1 })
                .options(IndexOptions::builder().name("idx_events_session_ts".to_string()).build())
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("index creation failed: {e}")))?;

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
        .map_err(|e| adk_core::AdkError::session(format!("index creation failed: {e}")))?;

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
        .map_err(|e| adk_core::AdkError::session(format!("index creation failed: {e}")))?;

    Ok(())
}

// ── BSON helpers ──

fn state_to_bson(
    state: &HashMap<String, Value>,
) -> std::result::Result<Document, adk_core::AdkError> {
    let json_value = serde_json::to_value(state)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let bson_value = bson::to_bson(&json_value)
        .map_err(|e| adk_core::AdkError::session(format!("bson conversion failed: {e}")))?;
    match bson_value {
        bson::Bson::Document(doc) => Ok(doc),
        _ => Ok(Document::new()),
    }
}

fn bson_to_state(doc: &Document) -> HashMap<String, Value> {
    let bson_value = bson::Bson::Document(doc.clone());
    match bson::from_bson::<serde_json::Value>(bson_value) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

fn chrono_to_bson_dt(dt: DateTime<Utc>) -> bson::DateTime {
    bson::DateTime::from_millis(dt.timestamp_millis())
}

fn bson_dt_to_chrono(dt: bson::DateTime) -> DateTime<Utc> {
    let millis = dt.timestamp_millis();
    DateTime::from_timestamp_millis(millis).unwrap_or_default()
}

// ── Session-optional DB operation helpers ──
// Each helper accepts Option<&mut ClientSession>. When Some, the op runs
// inside the transaction; when None, it runs standalone.

macro_rules! with_optional_session {
    ($action:expr, $session:expr) => {
        match $session {
            Some(s) => $action.session(s).await,
            None => $action.await,
        }
    };
}

// ── SessionService implementation ──

#[async_trait]
impl SessionService for MongoSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();
        let bson_now = chrono_to_bson_dt(now);
        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let app_coll = self.db.collection::<Document>("app_states");
        let user_coll = self.db.collection::<Document>("user_states");
        let sess_coll = self.db.collection::<Document>("sessions");

        let mut txn = self.maybe_start_transaction().await?;

        // App state
        let existing_app: HashMap<String, Value> = with_optional_session!(
            app_coll.find_one(doc! { "app_name": &req.app_name }),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .and_then(|d| d.get_document("state").ok().map(bson_to_state))
        .unwrap_or_default();
        let mut new_app = existing_app;
        new_app.extend(app_delta);
        let app_bson = state_to_bson(&new_app)?;
        with_optional_session!(
            app_coll.update_one(
                doc! { "app_name": &req.app_name },
                doc! { "$set": { "app_name": &req.app_name, "state": &app_bson, "updated_at": bson_now } }
            ).with_options(UpdateOptions::builder().upsert(true).build()),
            txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        // User state
        let existing_user: HashMap<String, Value> = with_optional_session!(
            user_coll.find_one(doc! { "app_name": &req.app_name, "user_id": &req.user_id }),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .and_then(|d| d.get_document("state").ok().map(bson_to_state))
        .unwrap_or_default();
        let mut new_user = existing_user;
        new_user.extend(user_delta);
        let user_bson = state_to_bson(&new_user)?;
        with_optional_session!(
            user_coll.update_one(
                doc! { "app_name": &req.app_name, "user_id": &req.user_id },
                doc! { "$set": { "app_name": &req.app_name, "user_id": &req.user_id, "state": &user_bson, "updated_at": bson_now } }
            ).with_options(UpdateOptions::builder().upsert(true).build()),
            txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        // Session
        let merged = state_utils::merge_states(&new_app, &new_user, &session_state);
        let merged_bson = state_to_bson(&merged)?;
        with_optional_session!(
            sess_coll.insert_one(doc! {
                "app_name": &req.app_name, "user_id": &req.user_id, "session_id": &session_id,
                "state": &merged_bson, "created_at": bson_now, "updated_at": bson_now,
            }),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        MongoSessionService::maybe_commit(&mut txn).await?;

        Ok(Box::new(MongoSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: merged,
            events: Vec::new(),
            updated_at: now,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let sess_coll = self.db.collection::<Document>("sessions");
        let session_doc = sess_coll
            .find_one(doc! { "app_name": &req.app_name, "user_id": &req.user_id, "session_id": &req.session_id })
            .await.map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let state = session_doc.get_document("state").map(bson_to_state).unwrap_or_default();
        let updated_at = session_doc
            .get_datetime("updated_at")
            .map(|dt| bson_dt_to_chrono(*dt))
            .unwrap_or_else(|_| Utc::now());

        let events_coll = self.db.collection::<Document>("events");
        let mut cursor = events_coll
            .find(doc! { "app_name": &req.app_name, "user_id": &req.user_id, "session_id": &req.session_id })
            .with_options(FindOptions::builder().sort(doc! { "timestamp": 1 }).build())
            .await.map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let mut events: Vec<Event> = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| adk_core::AdkError::session(format!("deserialize failed: {e}")))?;
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
        let sess_coll = self.db.collection::<Document>("sessions");
        let opts = FindOptions::builder()
            .sort(doc! { "updated_at": -1 })
            .limit(req.limit.map(|l| l as i64))
            .skip(req.offset.map(|o| o as u64))
            .build();
        let mut cursor = sess_coll
            .find(doc! { "app_name": &req.app_name, "user_id": &req.user_id })
            .with_options(opts)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let mut sessions: Vec<Box<dyn Session>> = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| adk_core::AdkError::session(format!("deserialize failed: {e}")))?;
            sessions.push(Box::new(MongoSession {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id: doc.get_str("session_id").unwrap_or_default().to_string(),
                state: doc.get_document("state").map(bson_to_state).unwrap_or_default(),
                events: Vec::new(),
                updated_at: doc
                    .get_datetime("updated_at")
                    .map(|dt| bson_dt_to_chrono(*dt))
                    .unwrap_or_else(|_| Utc::now()),
            }));
        }
        Ok(sessions)
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let ev_coll = self.db.collection::<Document>("events");
        let sess_coll = self.db.collection::<Document>("sessions");
        let filter = doc! { "app_name": &req.app_name, "user_id": &req.user_id, "session_id": &req.session_id };

        let mut txn = self.maybe_start_transaction().await?;
        with_optional_session!(ev_coll.delete_many(filter.clone()), txn.as_mut())
            .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;
        with_optional_session!(sess_coll.delete_one(filter), txn.as_mut())
            .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;
        MongoSessionService::maybe_commit(&mut txn).await?;
        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let sess_coll = self.db.collection::<Document>("sessions");
        let app_coll = self.db.collection::<Document>("app_states");
        let user_coll = self.db.collection::<Document>("user_states");
        let ev_coll = self.db.collection::<Document>("events");

        let mut txn = self.maybe_start_transaction().await?;

        let session_doc = with_optional_session!(
            sess_coll.find_one(doc! { "session_id": session_id }),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let app_name = session_doc.get_str("app_name").unwrap_or_default().to_string();
        let user_id = session_doc.get_str("user_id").unwrap_or_default().to_string();
        let existing_state =
            session_doc.get_document("state").map(bson_to_state).unwrap_or_default();
        let (_, _, mut sess_state) = state_utils::extract_state_deltas(&existing_state);

        let cur_app: HashMap<String, Value> =
            with_optional_session!(app_coll.find_one(doc! { "app_name": &app_name }), txn.as_mut())
                .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
                .and_then(|d| d.get_document("state").ok().map(bson_to_state))
                .unwrap_or_default();

        let cur_user: HashMap<String, Value> = with_optional_session!(
            user_coll.find_one(doc! { "app_name": &app_name, "user_id": &user_id }),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .and_then(|d| d.get_document("state").ok().map(bson_to_state))
        .unwrap_or_default();

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);
        let bson_ts = chrono_to_bson_dt(event.timestamp);

        let mut new_app = cur_app;
        new_app.extend(app_delta);
        let app_bson = state_to_bson(&new_app)?;
        with_optional_session!(
            app_coll.update_one(doc! { "app_name": &app_name }, doc! { "$set": { "app_name": &app_name, "state": &app_bson, "updated_at": bson_ts } })
                .with_options(UpdateOptions::builder().upsert(true).build()),
            txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        let mut new_user = cur_user;
        new_user.extend(user_delta);
        let user_bson = state_to_bson(&new_user)?;
        with_optional_session!(
            user_coll.update_one(doc! { "app_name": &app_name, "user_id": &user_id }, doc! { "$set": { "app_name": &app_name, "user_id": &user_id, "state": &user_bson, "updated_at": bson_ts } })
                .with_options(UpdateOptions::builder().upsert(true).build()),
            txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        sess_state.extend(session_delta);
        let merged = state_utils::merge_states(&new_app, &new_user, &sess_state);
        let merged_bson = state_to_bson(&merged)?;
        with_optional_session!(
            sess_coll.update_one(
                doc! { "app_name": &app_name, "user_id": &user_id, "session_id": session_id },
                doc! { "$set": { "state": &merged_bson, "updated_at": bson_ts } }
            ),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;

        let llm_bson = bson::to_bson(&event.llm_response)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        let act_bson = bson::to_bson(&event.actions)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        let tid_bson = bson::to_bson(&event.long_running_tool_ids)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        with_optional_session!(
            ev_coll.insert_one(doc! {
                "id": &event.id, "session_id": session_id, "app_name": &app_name, "user_id": &user_id,
                "invocation_id": &event.invocation_id, "branch": &event.branch, "author": &event.author,
                "timestamp": bson_ts, "llm_response": llm_bson, "actions": act_bson, "long_running_tool_ids": tid_bson,
            }), txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        MongoSessionService::maybe_commit(&mut txn).await?;
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

        let sess_coll = self.db.collection::<Document>("sessions");
        let app_coll = self.db.collection::<Document>("app_states");
        let user_coll = self.db.collection::<Document>("user_states");
        let ev_coll = self.db.collection::<Document>("events");

        let mut txn = self.maybe_start_transaction().await?;

        let session_doc = with_optional_session!(
            sess_coll.find_one(
                doc! { "app_name": app_name, "user_id": user_id, "session_id": session_id }
            ),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let existing_state =
            session_doc.get_document("state").map(bson_to_state).unwrap_or_default();
        let (_, _, mut sess_state) = state_utils::extract_state_deltas(&existing_state);

        let cur_app: HashMap<String, Value> =
            with_optional_session!(app_coll.find_one(doc! { "app_name": app_name }), txn.as_mut())
                .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
                .and_then(|d| d.get_document("state").ok().map(bson_to_state))
                .unwrap_or_default();

        let cur_user: HashMap<String, Value> = with_optional_session!(
            user_coll.find_one(doc! { "app_name": app_name, "user_id": user_id }),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .and_then(|d| d.get_document("state").ok().map(bson_to_state))
        .unwrap_or_default();

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);
        let bson_ts = chrono_to_bson_dt(event.timestamp);

        let mut new_app = cur_app;
        new_app.extend(app_delta);
        let app_bson = state_to_bson(&new_app)?;
        with_optional_session!(
            app_coll.update_one(doc! { "app_name": app_name }, doc! { "$set": { "app_name": app_name, "state": &app_bson, "updated_at": bson_ts } })
                .with_options(UpdateOptions::builder().upsert(true).build()),
            txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        let mut new_user = cur_user;
        new_user.extend(user_delta);
        let user_bson = state_to_bson(&new_user)?;
        with_optional_session!(
            user_coll.update_one(doc! { "app_name": app_name, "user_id": user_id }, doc! { "$set": { "app_name": app_name, "user_id": user_id, "state": &user_bson, "updated_at": bson_ts } })
                .with_options(UpdateOptions::builder().upsert(true).build()),
            txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        sess_state.extend(session_delta);
        let merged = state_utils::merge_states(&new_app, &new_user, &sess_state);
        let merged_bson = state_to_bson(&merged)?;
        with_optional_session!(
            sess_coll.update_one(
                doc! { "app_name": app_name, "user_id": user_id, "session_id": session_id },
                doc! { "$set": { "state": &merged_bson, "updated_at": bson_ts } }
            ),
            txn.as_mut()
        )
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;

        let llm_bson = bson::to_bson(&event.llm_response)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        let act_bson = bson::to_bson(&event.actions)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        let tid_bson = bson::to_bson(&event.long_running_tool_ids)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        with_optional_session!(
            ev_coll.insert_one(doc! {
                "id": &event.id, "session_id": session_id, "app_name": app_name, "user_id": user_id,
                "invocation_id": &event.invocation_id, "branch": &event.branch, "author": &event.author,
                "timestamp": bson_ts, "llm_response": llm_bson, "actions": act_bson, "long_running_tool_ids": tid_bson,
            }), txn.as_mut()
        ).map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

        MongoSessionService::maybe_commit(&mut txn).await?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let filter = doc! { "app_name": app_name, "user_id": user_id };
        let ev_coll = self.db.collection::<Document>("events");
        let sess_coll = self.db.collection::<Document>("sessions");

        let mut txn = self.maybe_start_transaction().await?;
        with_optional_session!(ev_coll.delete_many(filter.clone()), txn.as_mut())
            .map_err(|e| adk_core::AdkError::session(format!("delete_all_sessions failed: {e}")))?;
        with_optional_session!(sess_coll.delete_many(filter), txn.as_mut())
            .map_err(|e| adk_core::AdkError::session(format!("delete_all_sessions failed: {e}")))?;
        MongoSessionService::maybe_commit(&mut txn).await?;
        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| adk_core::AdkError::session(format!("health check failed: {e}")))?;
        Ok(())
    }
}

// ── Event deserialization ──

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

// ── MongoSession type ──

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
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
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
