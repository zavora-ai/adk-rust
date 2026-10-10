//! Neo4j session service backend.
//!
//! Provides [`Neo4jSessionService`] for session persistence using Neo4j graph database.
//! Enabled via the `neo4j` feature flag.
//!
//! # Graph Schema
//!
//! Sessions are modeled as graph nodes with typed relationships:
//!
//! ```text
//! (:Session)-[:HAS_EVENT]->(:Event)
//! (:Session)-[:HAS_APP_STATE]->(:AppState)
//! (:Session)-[:HAS_USER_STATE]->(:UserState)
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_session::Neo4jSessionService;
//!
//! let service = Neo4jSessionService::new("bolt://localhost:7687", "neo4j", "password").await?;
//! service.migrate().await?;
//! ```

use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use neo4rs::Graph;
use serde_json::Value;
use std::collections::HashMap;
use tracing::instrument;
use uuid::Uuid;

/// Neo4j-backed session service implementing [`SessionService`](crate::SessionService).
///
/// Stores sessions as graph nodes with relationships to event, app-state,
/// and user-state nodes. Each state key is a node property holding the value
/// as a JSON string, since Neo4j has no native JSON type, so an append writes
/// only its own keys and concurrent appends keep each other's writes.
pub struct Neo4jSessionService {
    graph: Graph,
}

impl Neo4jSessionService {
    /// Connect to Neo4j using URI, username, and password.
    ///
    /// Returns an error with "neo4j connection failed" context if the
    /// connection cannot be established.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let service = Neo4jSessionService::new(
    ///     "bolt://localhost:7687",
    ///     "neo4j",
    ///     "password",
    /// ).await?;
    /// ```
    pub async fn new(uri: &str, username: &str, password: &str) -> Result<Self> {
        let graph = Graph::new(uri, username, password)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("neo4j connection failed: {e}")))?;
        Ok(Self { graph })
    }

    /// Returns a reference to the underlying Neo4j graph connection.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Registry node label for tracking applied migration versions.
    const REGISTRY_LABEL: &'static str = "_AdkSessionMigration";

    /// Compiled-in Neo4j migration steps.
    ///
    /// Each entry is `(version, description, &[cypher_statements])`. All Cypher
    /// statements use `IF NOT EXISTS` for idempotent execution.
    const NEO4J_SESSION_MIGRATIONS: &'static [(i64, &'static str, &'static [&'static str])] = &[(
        1,
        "create initial constraints and indexes",
        &[
            "CREATE CONSTRAINT session_unique IF NOT EXISTS \
             FOR (s:Session) REQUIRE (s.app_name, s.user_id, s.session_id) IS UNIQUE",
            "CREATE INDEX event_session_ts IF NOT EXISTS \
             FOR (e:Event) ON (e.session_id, e.timestamp)",
            "CREATE CONSTRAINT app_state_unique IF NOT EXISTS \
             FOR (a:AppState) REQUIRE (a.app_name) IS UNIQUE",
            "CREATE CONSTRAINT user_state_unique IF NOT EXISTS \
             FOR (u:UserState) REQUIRE (u.app_name, u.user_id) IS UNIQUE",
        ],
    )];

    /// Run versioned migrations for Neo4j session storage.
    ///
    /// The runner:
    /// 1. Creates a uniqueness constraint on registry nodes.
    /// 2. Detects baseline — if `session_unique` constraint exists but
    ///    registry is empty, records v1 as already applied.
    /// 3. Reads the maximum applied version from the registry.
    /// 4. Returns an error if the database version exceeds the compiled-in max.
    /// 5. Executes each unapplied step idempotently and records it.
    pub async fn migrate(&self) -> Result<()> {
        // Step 1: Ensure registry has a uniqueness constraint on `version`
        self.graph
            .run(neo4rs::query(&format!(
                "CREATE CONSTRAINT {}_version_unique IF NOT EXISTS \
                 FOR (m:{}) REQUIRE (m.version) IS UNIQUE",
                Self::REGISTRY_LABEL.to_lowercase(),
                Self::REGISTRY_LABEL,
            )))
            .await
            .map_err(|e| {
                adk_core::AdkError::session(format!("migration registry creation failed: {e}"))
            })?;

        // Step 2: Read current max applied version
        let mut max_applied = self.read_max_applied_version().await?;

        // Step 3: Baseline detection — if registry is empty but session_unique
        // constraint already exists, record v1 as applied.
        if max_applied == 0 {
            let existing = self.detect_existing_tables().await?;
            if existing
                && let Some(&(version, description, _)) = Self::NEO4J_SESSION_MIGRATIONS.first()
            {
                self.record_migration(version, description).await?;
                max_applied = version;
            }
        }

        // Step 4: Compiled-in max version
        let max_compiled = Self::NEO4J_SESSION_MIGRATIONS.last().map(|s| s.0).unwrap_or(0);

        // Step 5: Version mismatch check
        if max_applied > max_compiled {
            return Err(adk_core::AdkError::session(format!(
                "schema version mismatch: database is at v{max_applied} \
                 but code only knows up to v{max_compiled}. \
                 Upgrade your ADK version."
            )));
        }

        // Step 6: Execute unapplied steps idempotently
        for &(version, description, cypher_statements) in Self::NEO4J_SESSION_MIGRATIONS {
            if version <= max_applied {
                continue;
            }

            for cypher in cypher_statements {
                self.graph.run(neo4rs::query(cypher)).await.map_err(|e| {
                    adk_core::AdkError::session(format!(
                        "{}",
                        crate::migration::MigrationError {
                            version,
                            description: description.to_string(),
                            cause: e.to_string(),
                        }
                    ))
                })?;
            }

            self.record_migration(version, description).await?;
        }

        Ok(())
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    pub async fn schema_version(&self) -> Result<i64> {
        self.read_max_applied_version().await
    }

    /// Read the maximum applied version from the registry nodes.
    async fn read_max_applied_version(&self) -> Result<i64> {
        let query_str =
            format!("OPTIONAL MATCH (m:{}) RETURN max(m.version) AS max_v", Self::REGISTRY_LABEL,);
        let mut row_stream = self.graph.execute(neo4rs::query(&query_str)).await.map_err(|e| {
            adk_core::AdkError::session(format!("migration registry read failed: {e}"))
        })?;

        if let Some(row) = row_stream.next().await.map_err(|e| {
            adk_core::AdkError::session(format!("migration registry read failed: {e}"))
        })? {
            // max() returns null when no nodes exist; treat as 0
            Ok(row.get::<i64>("max_v").unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    /// Detect whether the `session_unique` constraint already exists (baseline).
    async fn detect_existing_tables(&self) -> Result<bool> {
        let mut row_stream = self
            .graph
            .execute(neo4rs::query(
                "SHOW CONSTRAINTS YIELD name WHERE name = 'session_unique' RETURN name",
            ))
            .await
            .map_err(|e| adk_core::AdkError::session(format!("baseline detection failed: {e}")))?;

        let found = row_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("baseline detection failed: {e}")))?
            .is_some();

        Ok(found)
    }

    /// Record a successfully applied migration step as a registry node.
    async fn record_migration(&self, version: i64, description: &str) -> Result<()> {
        let query_str = format!(
            "CREATE (m:{} {{version: $version, description: $description, applied_at: datetime()}})",
            Self::REGISTRY_LABEL,
        );
        self.graph
            .run(
                neo4rs::query(&query_str)
                    .param("version", version)
                    .param("description", description.to_string()),
            )
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
}

/// Serialize a `HashMap<String, Value>` to a JSON string for Neo4j storage.
fn state_to_json_string(
    state: &HashMap<String, Value>,
) -> std::result::Result<String, adk_core::AdkError> {
    serde_json::to_string(state)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))
}

/// Deserialize a JSON string from Neo4j into a `HashMap<String, Value>`.
///
/// Returns an error if the string is not valid JSON. An empty string
/// is treated as an empty state (not an error).
fn json_string_to_state(
    s: &str,
) -> std::result::Result<HashMap<String, Value>, adk_core::AdkError> {
    if s.is_empty() {
        return Ok(HashMap::new());
    }
    serde_json::from_str::<HashMap<String, Value>>(s)
        .map_err(|e| adk_core::AdkError::session(format!("deserialize state failed: {e}")))
}

/// Prefix of the node property that holds one state key.
///
/// Each key is its own property holding the value as JSON, so an append writes its delta
/// with `SET n += $delta`: Neo4j locks the node for the statement and writes only the
/// delta's keys, and concurrent appends that touch different keys keep each other's
/// writes. The `state` property holds the JSON object written by `create` and by earlier
/// releases; a per-key property overrides the same key in it.
const STATE_KEY_PREFIX: &str = "state.";

/// Cypher list of `[property, json]` pairs for the per-key state properties of `node`.
fn state_entries(node: &str) -> String {
    format!("[key IN keys({node}) WHERE key STARTS WITH '{STATE_KEY_PREFIX}' | [key, {node}[key]]]")
}

/// Encodes a state delta as per-key node properties for `SET n += $delta`.
fn state_properties(
    delta: &HashMap<String, Value>,
) -> std::result::Result<HashMap<String, String>, adk_core::AdkError> {
    delta
        .iter()
        .map(|(key, value)| Ok((format!("{STATE_KEY_PREFIX}{key}"), serde_json::to_string(value)?)))
        .collect::<std::result::Result<_, serde_json::Error>>()
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))
}

/// Builds one node's state from its `state` JSON object and its per-key properties.
fn node_state(
    object: &str,
    entries: Vec<(String, String)>,
) -> std::result::Result<HashMap<String, Value>, adk_core::AdkError> {
    let mut state = json_string_to_state(object)?;
    for (property, json) in entries {
        if let Some(key) = property.strip_prefix(STATE_KEY_PREFIX) {
            let value = serde_json::from_str(&json).map_err(|e| {
                adk_core::AdkError::session(format!("deserialize state key '{key}' failed: {e}"))
            })?;
            state.insert(key.to_string(), value);
        }
    }
    Ok(state)
}

/// Reads the state of the node returned as `<column>` (its `state` object) and
/// `<column>_entries` (its per-key properties). A node that `OPTIONAL MATCH` did not find
/// has an empty state.
fn row_state(
    row: &neo4rs::Row,
    column: &str,
) -> std::result::Result<HashMap<String, Value>, adk_core::AdkError> {
    node_state(
        &row.get::<String>(column).unwrap_or_default(),
        row.get::<Vec<(String, String)>>(&format!("{column}_entries")).unwrap_or_default(),
    )
}

/// Writes `event` and its state delta to one session inside `txn`.
///
/// The session, app, and user nodes are locked in that order, the order `create` also
/// follows for the app and user nodes, so concurrent appends cannot deadlock.
async fn write_event(
    txn: &mut neo4rs::Txn,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: &Event,
) -> Result<()> {
    let (app_delta, user_delta, session_delta) =
        state_utils::extract_state_deltas(&event.actions.state_delta);
    let now_str = event.timestamp.to_rfc3339();

    let mut updated = txn
        .execute(
            neo4rs::query(
                "MATCH (s:Session {session_id: $session_id, app_name: $app_name, \
                        user_id: $user_id}) \
                 SET s += $delta, s.updated_at = $now \
                 RETURN s.session_id AS session_id",
            )
            .param("session_id", session_id.to_string())
            .param("app_name", app_name.to_string())
            .param("user_id", user_id.to_string())
            .param("delta", state_properties(&session_delta)?)
            .param("now", now_str.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
    if updated
        .next(&mut *txn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?
        .is_none()
    {
        return Err(adk_core::AdkError::session("session not found"));
    }

    if !app_delta.is_empty() {
        txn.run(
            neo4rs::query(
                "MERGE (a:AppState {app_name: $app_name}) \
                 SET a += $delta, a.updated_at = $now",
            )
            .param("app_name", app_name.to_string())
            .param("delta", state_properties(&app_delta)?)
            .param("now", now_str.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
    }

    if !user_delta.is_empty() {
        txn.run(
            neo4rs::query(
                "MERGE (u:UserState {app_name: $app_name, user_id: $user_id}) \
                 SET u += $delta, u.updated_at = $now",
            )
            .param("app_name", app_name.to_string())
            .param("user_id", user_id.to_string())
            .param("delta", state_properties(&user_delta)?)
            .param("now", now_str.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
    }

    let llm_response_json = serde_json::to_string(&event.llm_response)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let actions_json = serde_json::to_string(&event.actions)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let tool_ids_json = serde_json::to_string(&event.long_running_tool_ids)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;

    txn.run(
        neo4rs::query(
            "MATCH (s:Session {session_id: $session_id, app_name: $app_name, \
                    user_id: $user_id}) \
             CREATE (s)-[:HAS_EVENT]->(e:Event { \
                 id: $id, \
                 session_id: $session_id, \
                 invocation_id: $invocation_id, \
                 branch: $branch, \
                 author: $author, \
                 timestamp: $timestamp, \
                 llm_response: $llm_response, \
                 actions: $actions, \
                 long_running_tool_ids: $long_running_tool_ids \
             })",
        )
        .param("session_id", session_id.to_string())
        .param("app_name", app_name.to_string())
        .param("user_id", user_id.to_string())
        .param("id", event.id.clone())
        .param("invocation_id", event.invocation_id.clone())
        .param("branch", event.branch.clone())
        .param("author", event.author.clone())
        .param("timestamp", event.timestamp.to_rfc3339())
        .param("llm_response", llm_response_json)
        .param("actions", actions_json)
        .param("long_running_tool_ids", tool_ids_json),
    )
    .await
    .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

    Ok(())
}

/// Convert a Neo4j row to an `Event`.
fn row_to_event(row: &neo4rs::Row) -> Option<Event> {
    let id = row.get::<String>("id").ok()?;
    let invocation_id = row.get::<String>("invocation_id").unwrap_or_default();
    let branch = row.get::<String>("branch").unwrap_or_default();
    let author = row.get::<String>("author").unwrap_or_default();
    let timestamp_str = row.get::<String>("timestamp").unwrap_or_default();
    let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    let llm_response_str = row.get::<String>("llm_response").unwrap_or_default();
    let actions_str = row.get::<String>("actions").unwrap_or_default();
    let tool_ids_str = row.get::<String>("long_running_tool_ids").unwrap_or_default();

    let llm_response = serde_json::from_str(&llm_response_str).ok()?;
    let actions = serde_json::from_str(&actions_str).ok()?;
    let long_running_tool_ids = serde_json::from_str(&tool_ids_str).ok()?;

    Some(Event {
        id,
        timestamp,
        invocation_id,
        branch,
        author,
        llm_response,
        actions,
        long_running_tool_ids,
        llm_request: None,
        provider_metadata: std::collections::HashMap::new(),
    })
}

#[async_trait]
impl SessionService for Neo4jSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        txn.run(
            neo4rs::query(
                "MERGE (a:AppState {app_name: $app_name}) \
                 SET a += $delta, a.updated_at = $now",
            )
            .param("app_name", req.app_name.clone())
            .param("delta", state_properties(&app_delta)?)
            .param("now", now_str.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        txn.run(
            neo4rs::query(
                "MERGE (u:UserState {app_name: $app_name, user_id: $user_id}) \
                 SET u += $delta, u.updated_at = $now",
            )
            .param("app_name", req.app_name.clone())
            .param("user_id", req.user_id.clone())
            .param("delta", state_properties(&user_delta)?)
            .param("now", now_str.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        // The session record holds session-scoped keys only; `get` reads the tiers.
        txn.run(
            neo4rs::query(
                "CREATE (s:Session { \
                     app_name: $app_name, \
                     user_id: $user_id, \
                     session_id: $session_id, \
                     state: $state, \
                     created_at: $now, \
                     updated_at: $now \
                 })",
            )
            .param("app_name", req.app_name.clone())
            .param("user_id", req.user_id.clone())
            .param("session_id", session_id.clone())
            .param("state", state_to_json_string(&session_state)?)
            .param("now", now_str.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        // Create relationships: Session -> AppState, Session -> UserState
        txn.run(
            neo4rs::query(
                "MATCH (s:Session {session_id: $session_id, app_name: $app_name, user_id: $user_id}), \
                       (a:AppState {app_name: $app_name}), \
                       (u:UserState {app_name: $app_name, user_id: $user_id}) \
                 CREATE (s)-[:HAS_APP_STATE]->(a), (s)-[:HAS_USER_STATE]->(u)",
            )
            .param("app_name", req.app_name.clone())
            .param("user_id", req.user_id.clone())
            .param("session_id", session_id.clone()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        // This transaction holds both tier locks, so the tiers read here are current.
        let mut tiers = txn
            .execute(
                neo4rs::query(&format!(
                    "MATCH (a:AppState {{app_name: $app_name}}), \
                           (u:UserState {{app_name: $app_name, user_id: $user_id}}) \
                     RETURN a.state AS app_state, {} AS app_state_entries, \
                            u.state AS user_state, {} AS user_state_entries",
                    state_entries("a"),
                    state_entries("u"),
                ))
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone()),
            )
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
        let (app_state, user_state) = match tiers
            .next(&mut txn)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        {
            Some(row) => (row_state(&row, "app_state")?, row_state(&row, "user_state")?),
            None => (HashMap::new(), HashMap::new()),
        };

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(Box::new(Neo4jSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: state_utils::merge_states(&app_state, &user_state, &session_state),
            events: Vec::new(),
            updated_at: now,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let mut row_stream = self
            .graph
            .execute(
                neo4rs::query(&format!(
                    "MATCH (s:Session {{app_name: $app_name, user_id: $user_id, session_id: $session_id}}) \
                     OPTIONAL MATCH (a:AppState {{app_name: $app_name}}) \
                     OPTIONAL MATCH (u:UserState {{app_name: $app_name, user_id: $user_id}}) \
                     RETURN s.state AS state, {} AS state_entries, s.updated_at AS updated_at, \
                            a.state AS app_state, {} AS app_state_entries, \
                            u.state AS user_state, {} AS user_state_entries",
                    state_entries("s"),
                    state_entries("a"),
                    state_entries("u"),
                ))
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone())
                .param("session_id", req.session_id.clone()),
            )
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let row = row_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
            .ok_or_else(|| crate::service::session_not_found(&req))?;

        let updated_at_str = row.get::<String>("updated_at").unwrap_or_default();
        let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        let state = state_utils::merge_current_tiers(
            &row_state(&row, "app_state")?,
            &row_state(&row, "user_state")?,
            &row_state(&row, "state")?,
        );

        // Fetch events ordered by timestamp
        let mut event_stream = self
            .graph
            .execute(
                neo4rs::query(
                    "MATCH (s:Session {app_name: $app_name, user_id: $user_id, \
                            session_id: $session_id})-[:HAS_EVENT]->(e:Event) \
                     RETURN e.id AS id, e.invocation_id AS invocation_id, \
                            e.branch AS branch, e.author AS author, \
                            e.timestamp AS timestamp, e.llm_response AS llm_response, \
                            e.actions AS actions, \
                            e.long_running_tool_ids AS long_running_tool_ids \
                     ORDER BY e.timestamp",
                )
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone())
                .param("session_id", req.session_id.clone()),
            )
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let mut events: Vec<Event> = Vec::new();
        while let Some(row) = event_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        {
            if let Some(event) = row_to_event(&row) {
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

        Ok(Box::new(Neo4jSession {
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
        let limit = req.limit.unwrap_or(i64::MAX as usize) as i64;
        let offset = req.offset.unwrap_or(0) as i64;

        let mut tier_stream = self
            .graph
            .execute(
                neo4rs::query(&format!(
                    "OPTIONAL MATCH (a:AppState {{app_name: $app_name}}) \
                     OPTIONAL MATCH (u:UserState {{app_name: $app_name, user_id: $user_id}}) \
                     RETURN a.state AS app_state, {} AS app_state_entries, \
                            u.state AS user_state, {} AS user_state_entries",
                    state_entries("a"),
                    state_entries("u"),
                ))
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone()),
            )
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
        let tiers = tier_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
        let tier = |column: &str| match &tiers {
            Some(row) => row_state(row, column),
            None => Ok(HashMap::new()),
        };
        let (app_state, user_state) = (tier("app_state")?, tier("user_state")?);

        let mut row_stream = self
            .graph
            .execute(
                neo4rs::query(&format!(
                    "MATCH (s:Session {{app_name: $app_name, user_id: $user_id}}) \
                     RETURN s.session_id AS session_id, s.state AS state, \
                            {} AS state_entries, s.updated_at AS updated_at \
                     ORDER BY s.updated_at DESC \
                     SKIP $offset LIMIT $limit",
                    state_entries("s"),
                ))
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone())
                .param("offset", offset)
                .param("limit", limit),
            )
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let mut sessions: Vec<Box<dyn Session>> = Vec::new();
        while let Some(row) = row_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        {
            let session_id = row.get::<String>("session_id").unwrap_or_default();
            let updated_at_str = row.get::<String>("updated_at").unwrap_or_default();
            let state = state_utils::merge_current_tiers(
                &app_state,
                &user_state,
                &row_state(&row, "state")?,
            );
            let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            sessions.push(Box::new(Neo4jSession {
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
        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        // DETACH DELETE session and connected event nodes
        txn.run(
            neo4rs::query(
                "MATCH (s:Session {app_name: $app_name, user_id: $user_id, \
                        session_id: $session_id}) \
                 OPTIONAL MATCH (s)-[:HAS_EVENT]->(e:Event) \
                 DETACH DELETE s, e",
            )
            .param("app_name", req.app_name)
            .param("user_id", req.user_id)
            .param("session_id", req.session_id),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        // Find the session
        let mut row_stream = txn
            .execute(
                neo4rs::query(
                    "MATCH (s:Session {session_id: $session_id}) \
                     RETURN s.app_name AS app_name, s.user_id AS user_id",
                )
                .param("session_id", session_id.to_string()),
            )
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let row = row_stream
            .next(&mut txn)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let app_name = row.get::<String>("app_name").unwrap_or_default();
        let user_id = row.get::<String>("user_id").unwrap_or_default();

        write_event(&mut txn, &app_name, &user_id, session_id, &event).await?;

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

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

        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        write_event(
            &mut txn,
            req.identity.app_name.as_ref(),
            req.identity.user_id.as_ref(),
            req.identity.session_id.as_ref(),
            &event,
        )
        .await?;

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }
    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        txn.run(
            neo4rs::query(
                "MATCH (s:Session {app_name: $app_name, user_id: $user_id}) \
                 OPTIONAL MATCH (s)-[:HAS_EVENT]->(e:Event) \
                 DETACH DELETE s, e",
            )
            .param("app_name", app_name.to_string())
            .param("user_id", user_id.to_string()),
        )
        .await
        .map_err(|e| adk_core::AdkError::session(format!("delete_all_sessions failed: {e}")))?;

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        let _ = self
            .graph
            .execute(neo4rs::query("RETURN 1"))
            .await
            .map_err(|e| adk_core::AdkError::session(format!("health check failed: {e}")))?;
        Ok(())
    }
}

struct Neo4jSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for Neo4jSession {
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

impl State for Neo4jSession {
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

impl Events for Neo4jSession {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect()
    }

    #[test]
    fn a_delta_becomes_one_json_property_per_key() {
        let delta = state(&[
            ("theme", json!("dark")),
            ("limits", json!({"daily": 5})),
            ("gone", Value::Null),
        ]);

        let properties = state_properties(&delta).unwrap();

        let expected: HashMap<String, String> = [
            ("state.theme", r#""dark""#),
            ("state.limits", r#"{"daily":5}"#),
            ("state.gone", "null"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        assert_eq!(properties, expected);
    }

    #[test]
    fn per_key_properties_override_the_state_object() {
        let object = r#"{"theme":"light","language":"en"}"#;
        let entries = vec![
            ("state.theme".to_string(), r#""dark""#.to_string()),
            ("state.flag".to_string(), "true".to_string()),
            // Not a state property; `state_entries` filters these out, and so does the reader.
            ("app_name".to_string(), "app".to_string()),
        ];

        let merged = node_state(object, entries).unwrap();

        assert_eq!(
            merged,
            state(&[("theme", json!("dark")), ("language", json!("en")), ("flag", json!(true))])
        );
    }

    #[test]
    fn written_properties_read_back_as_the_same_state() {
        let delta = state(&[("app:nested", json!({"a": [1, 2]})), ("plain", json!(3))]);
        let entries = state_properties(&delta).unwrap().into_iter().collect();

        assert_eq!(node_state("", entries).unwrap(), delta);
    }

    #[test]
    fn a_property_that_is_not_json_is_an_error() {
        let error = node_state("", vec![("state.theme".to_string(), "{".to_string())]).unwrap_err();
        assert!(error.to_string().contains("state key 'theme'"), "{error}");
    }

    #[test]
    fn the_entries_expression_selects_only_state_properties() {
        assert_eq!(
            state_entries("a"),
            "[key IN keys(a) WHERE key STARTS WITH 'state.' | [key, a[key]]]"
        );
    }
}
