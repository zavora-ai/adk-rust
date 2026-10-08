//! Knowledge-graph-backed memory service.
//!
//! [`GraphMemoryService`] stores the agent's long-term memory as a **bi-temporal
//! knowledge graph** — entities, typed relations, and observations — backed by
//! SQLite (via `sqlx`). It is the *semantic* memory tier: structured facts the
//! agent accrues about a user, distinct from document retrieval (`adk-rag`)
//! and complementary to the flat backends in this crate.
//!
//! # Two surfaces
//!
//! - **[`MemoryService`]** (the narrow contract the realtime runner consumes):
//!   [`search`](GraphMemoryService::search) renders relevant *current* facts as
//!   [`MemoryEntry`]s for context injection, and
//!   [`add_session`](GraphMemoryService::add_session) cheaply records each turn
//!   into an episodic log (heavy fact-extraction is left to an out-of-band
//!   consolidation pass — never the hot path).
//! - **Inherent graph ops** ([`create_entities`](GraphMemoryService::create_entities),
//!   [`search_nodes`](GraphMemoryService::search_nodes), …): the rich, typed API
//!   intended to be exposed as agent tools for explicit recall/curation.
//!
//! # Bi-temporal facts
//!
//! Observations and relations carry `valid_from` and an optional `valid_to`. A
//! fact is *current* while `valid_to IS NULL`; superseding a fact stamps
//! `valid_to` rather than deleting it, so history is preserved and queries never
//! surface stale facts. This is what keeps a year of drift survivable.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::GraphMemoryService;
//!
//! let kg = GraphMemoryService::new("sqlite::memory:").await?;
//! kg.migrate().await?;
//! kg.create_entities("app", "shai", vec![
//!     adk_memory::graph::CreateEntityInput {
//!         name: "Shai".into(),
//!         entity_type: "person".into(),
//!         observations: vec!["Relocated to the Bay Area".into()],
//!     },
//! ]).await?;
//! ```

use crate::service::*;
use adk_core::{Content, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// Sentinel query the realtime runner uses at session start; we answer it with
/// the compact profile card rather than a topic match.
const PROFILE_QUERY: &str = "session context";

// ─── Public data model ───────────────────────────────────────────────────────

/// An entity with its currently-valid observations.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entity {
    /// Unique (per user) entity name.
    pub name: String,
    /// Free-form category, e.g. "person", "place", "preference".
    pub entity_type: String,
    /// Currently-valid observations about this entity.
    pub observations: Vec<Observation>,
}

/// A single fact attached to an entity.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Observation {
    /// Stable id (used to invalidate the fact later).
    pub id: i64,
    /// The fact text.
    pub content: String,
    /// When the fact became true.
    pub valid_from: DateTime<Utc>,
}

/// A typed edge between two entities.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Relation {
    /// Stable id.
    pub id: i64,
    /// Source entity name.
    pub source: String,
    /// Relation type, e.g. "works_at".
    pub relation_type: String,
    /// Target entity name.
    pub target: String,
}

/// An entity plus its current relations and a relevance score.
#[derive(Debug, Clone, Serialize)]
pub struct GraphSearchResult {
    /// The matched entity.
    pub entity: Entity,
    /// Relations where this entity is the source or target (currently valid).
    pub relations: Vec<Relation>,
    /// Relevance score (higher is better).
    pub score: f64,
}

/// Input for [`GraphMemoryService::create_entities`].
#[derive(Debug, Clone, Deserialize)]
pub struct CreateEntityInput {
    /// Entity name.
    pub name: String,
    /// Entity category.
    #[serde(default = "default_entity_type")]
    pub entity_type: String,
    /// Initial observations.
    #[serde(default)]
    pub observations: Vec<String>,
}

fn default_entity_type() -> String {
    "general".to_string()
}

/// Input for [`GraphMemoryService::create_relations`].
#[derive(Debug, Clone, Deserialize)]
pub struct CreateRelationInput {
    /// Source entity name.
    pub source: String,
    /// Relation type.
    pub relation_type: String,
    /// Target entity name.
    pub target: String,
}

// ─── Service ─────────────────────────────────────────────────────────────────

/// Knowledge-graph memory backed by SQLite.
pub struct GraphMemoryService {
    pool: SqlitePool,
    /// Max entities shown in the always-on profile card.
    profile_entities: usize,
    /// Max observations per entity shown in the profile card.
    profile_observations: usize,
}

impl GraphMemoryService {
    /// Connect to SQLite (e.g. `sqlite:kg.db`, `sqlite::memory:`), creating the
    /// file if missing.
    pub async fn new(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(|e| adk_core::AdkError::memory(format!("invalid sqlite url: {e}")))?
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("sqlite connection failed: {e}")))?;
        Ok(Self { pool, profile_entities: 12, profile_observations: 5 })
    }

    /// Build from an existing pool.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool, profile_entities: 12, profile_observations: 5 }
    }

    /// Override how many entities / observations-per-entity the profile card includes.
    pub fn with_profile_budget(mut self, entities: usize, observations_per_entity: usize) -> Self {
        self.profile_entities = entities;
        self.profile_observations = observations_per_entity;
        self
    }

    const REGISTRY_TABLE: &'static str = "_adk_kg_migrations";

    const MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[(
        1,
        "create bi-temporal knowledge-graph + episodic tables",
        "\
CREATE TABLE IF NOT EXISTS kg_entities (\
    app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
    name TEXT NOT NULL, entity_type TEXT NOT NULL, updated_at TEXT NOT NULL, \
    PRIMARY KEY (app_name, user_id, name)\
);\
CREATE TABLE IF NOT EXISTS kg_observations (\
    id INTEGER PRIMARY KEY AUTOINCREMENT, \
    app_name TEXT NOT NULL, user_id TEXT NOT NULL, entity_name TEXT NOT NULL, \
    content TEXT NOT NULL, valid_from TEXT NOT NULL, valid_to TEXT\
);\
CREATE INDEX IF NOT EXISTS idx_kg_obs ON kg_observations(app_name, user_id, entity_name);\
CREATE TABLE IF NOT EXISTS kg_relations (\
    id INTEGER PRIMARY KEY AUTOINCREMENT, \
    app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
    source TEXT NOT NULL, relation_type TEXT NOT NULL, target TEXT NOT NULL, \
    valid_from TEXT NOT NULL, valid_to TEXT\
);\
CREATE INDEX IF NOT EXISTS idx_kg_rel ON kg_relations(app_name, user_id);\
CREATE TABLE IF NOT EXISTS kg_episodic (\
    id INTEGER PRIMARY KEY AUTOINCREMENT, \
    app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
    author TEXT NOT NULL, text TEXT NOT NULL, timestamp TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS idx_kg_epi ON kg_episodic(app_name, user_id);",
    )];

    /// Apply schema migrations (idempotent).
    pub async fn migrate(&self) -> Result<()> {
        let pool = self.pool.clone();
        crate::migration::sqlite_runner::run_sql_migrations(
            &pool,
            Self::REGISTRY_TABLE,
            Self::MIGRATIONS,
            || async {
                let row = sqlx::query(
                    "SELECT COUNT(*) AS cnt FROM sqlite_master \
                     WHERE type='table' AND name='kg_entities'",
                )
                .fetch_one(&pool)
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!("baseline detection failed: {e}"))
                })?;
                let count: i64 = row.try_get("cnt").unwrap_or(0);
                Ok(count > 0)
            },
        )
        .await
    }

    // ── Create ───────────────────────────────────────────────────────────────

    /// Create entities, merging observations into any that already exist.
    /// Returns the names touched.
    pub async fn create_entities(
        &self,
        app_name: &str,
        user_id: &str,
        inputs: Vec<CreateEntityInput>,
    ) -> Result<Vec<String>> {
        let now = Utc::now().to_rfc3339();
        let mut touched = Vec::new();
        let mut tx = self.begin().await?;

        for input in inputs {
            sqlx::query(
                "INSERT INTO kg_entities (app_name, user_id, name, entity_type, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(app_name, user_id, name) \
                 DO UPDATE SET entity_type = excluded.entity_type, updated_at = excluded.updated_at",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(&input.name)
            .bind(&input.entity_type)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(map_sql)?;

            for content in &input.observations {
                sqlx::query(
                    "INSERT INTO kg_observations \
                     (app_name, user_id, entity_name, content, valid_from, valid_to) \
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                )
                .bind(app_name)
                .bind(user_id)
                .bind(&input.name)
                .bind(content)
                .bind(&now)
                .execute(&mut *tx)
                .await
                .map_err(map_sql)?;
            }
            touched.push(input.name);
        }

        tx.commit().await.map_err(map_sql)?;
        Ok(touched)
    }

    /// Add typed relations. Returns the new relation ids.
    pub async fn create_relations(
        &self,
        app_name: &str,
        user_id: &str,
        inputs: Vec<CreateRelationInput>,
    ) -> Result<Vec<i64>> {
        let now = Utc::now().to_rfc3339();
        let mut ids = Vec::new();
        let mut tx = self.begin().await?;

        for input in inputs {
            let res = sqlx::query(
                "INSERT INTO kg_relations \
                 (app_name, user_id, source, relation_type, target, valid_from, valid_to) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(&input.source)
            .bind(&input.relation_type)
            .bind(&input.target)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(map_sql)?;
            ids.push(res.last_insert_rowid());
        }

        tx.commit().await.map_err(map_sql)?;
        Ok(ids)
    }

    /// Append observations to an existing entity. Returns the new observation
    /// ids, or an error if the entity is unknown.
    pub async fn add_observations(
        &self,
        app_name: &str,
        user_id: &str,
        entity_name: &str,
        contents: Vec<String>,
    ) -> Result<Vec<i64>> {
        if !self.entity_exists(app_name, user_id, entity_name).await? {
            return Err(adk_core::AdkError::memory(format!("unknown entity: {entity_name}")));
        }
        let now = Utc::now().to_rfc3339();
        let mut ids = Vec::new();
        let mut tx = self.begin().await?;
        for content in contents {
            let res = sqlx::query(
                "INSERT INTO kg_observations \
                 (app_name, user_id, entity_name, content, valid_from, valid_to) \
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(entity_name)
            .bind(&content)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(map_sql)?;
            ids.push(res.last_insert_rowid());
        }
        tx.commit().await.map_err(map_sql)?;
        Ok(ids)
    }

    // ── Bi-temporal supersession ───────────────────────────────────────────────

    /// Mark an observation no longer current (stamps `valid_to`), preserving history.
    pub async fn invalidate_observation(&self, id: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE kg_observations SET valid_to = ?1 WHERE id = ?2 AND valid_to IS NULL")
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_sql)?;
        Ok(())
    }

    /// Mark a relation no longer current.
    pub async fn invalidate_relation(&self, id: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE kg_relations SET valid_to = ?1 WHERE id = ?2 AND valid_to IS NULL")
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_sql)?;
        Ok(())
    }

    // ── Delete ──────────────────────────────────────────────────────────────

    /// Delete entities and cascade-remove their observations and relations.
    /// Returns the names actually removed.
    pub async fn delete_entities(
        &self,
        app_name: &str,
        user_id: &str,
        names: Vec<String>,
    ) -> Result<Vec<String>> {
        let mut deleted = Vec::new();
        let mut tx = self.begin().await?;
        for name in names {
            let res =
                sqlx::query("DELETE FROM kg_entities WHERE app_name=?1 AND user_id=?2 AND name=?3")
                    .bind(app_name)
                    .bind(user_id)
                    .bind(&name)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sql)?;
            if res.rows_affected() == 0 {
                continue;
            }
            sqlx::query(
                "DELETE FROM kg_observations WHERE app_name=?1 AND user_id=?2 AND entity_name=?3",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(map_sql)?;
            sqlx::query(
                "DELETE FROM kg_relations WHERE app_name=?1 AND user_id=?2 AND (source=?3 OR target=?3)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(map_sql)?;
            deleted.push(name);
        }
        tx.commit().await.map_err(map_sql)?;
        Ok(deleted)
    }

    // ── Query ─────────────────────────────────────────────────────────────────

    /// Number of entities for a user.
    pub async fn entity_count(&self, app_name: &str, user_id: &str) -> Result<usize> {
        let row =
            sqlx::query("SELECT COUNT(*) AS cnt FROM kg_entities WHERE app_name=?1 AND user_id=?2")
                .bind(app_name)
                .bind(user_id)
                .fetch_one(&self.pool)
                .await
                .map_err(map_sql)?;
        Ok(row.try_get::<i64, _>("cnt").unwrap_or(0) as usize)
    }

    /// Score entities against a free-text query, returning the best matches with
    /// their current relations. Only currently-valid facts are considered.
    ///
    /// The query is split into content tokens (see [`tokenize`]); an entity
    /// scores per token found in its name (×3), type (×2), or observations (×1),
    /// so a multi-word recall query like "dietary notes and allergies" matches an
    /// entity whose facts mention "allergic" or "peanuts" — which a whole-string
    /// substring match would miss. Matching is still substring-on-token (no
    /// stemming or embeddings).
    pub async fn search_nodes(
        &self,
        app_name: &str,
        user_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<GraphSearchResult>> {
        let tokens = tokenize(query);
        let mut results = Vec::new();

        for entity in self.load_entities(app_name, user_id, None).await? {
            let name = entity.name.to_lowercase();
            let ty = entity.entity_type.to_lowercase();
            let mut score = 0.0;
            for t in &tokens {
                if name.contains(t) {
                    score += 3.0;
                }
                if ty.contains(t) {
                    score += 2.0;
                }
                for obs in &entity.observations {
                    if obs.content.to_lowercase().contains(t) {
                        score += 1.0;
                    }
                }
            }
            if score > 0.0 {
                let relations = self.load_relations(app_name, user_id, &entity.name).await?;
                results.push(GraphSearchResult { entity, relations, score });
            }
        }
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    /// Fetch specific entities by name with their current relations.
    pub async fn open_nodes(
        &self,
        app_name: &str,
        user_id: &str,
        names: Vec<String>,
    ) -> Result<Vec<GraphSearchResult>> {
        let mut out = Vec::new();
        for entity in self.load_entities(app_name, user_id, Some(&names)).await? {
            let relations = self.load_relations(app_name, user_id, &entity.name).await?;
            out.push(GraphSearchResult { entity, relations, score: 1.0 });
        }
        Ok(out)
    }

    /// Dump all current entities and relations for a user.
    pub async fn read_graph(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<(Vec<Entity>, Vec<Relation>)> {
        let entities = self.load_entities(app_name, user_id, None).await?;
        let relations = self.load_all_relations(app_name, user_id).await?;
        Ok((entities, relations))
    }

    /// Build the always-on profile card: the most-recently-updated entities with
    /// their latest current observations, compacted for prompt injection.
    pub async fn profile_card(&self, app_name: &str, user_id: &str) -> Result<String> {
        let names: Vec<String> = sqlx::query(
            "SELECT name FROM kg_entities WHERE app_name=?1 AND user_id=?2 \
             ORDER BY updated_at DESC LIMIT ?3",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(self.profile_entities as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sql)?
        .into_iter()
        .map(|r| r.get::<String, _>("name"))
        .collect();

        if names.is_empty() {
            return Ok(String::new());
        }

        let entities = self.load_entities(app_name, user_id, Some(&names)).await?;
        let mut out = String::from("[Known about the user]\n");
        for e in &entities {
            out.push_str(&format!("• {} ({})", e.name, e.entity_type));
            if !e.observations.is_empty() {
                let recent: Vec<&str> = e
                    .observations
                    .iter()
                    .rev()
                    .take(self.profile_observations)
                    .map(|o| o.content.as_str())
                    .collect();
                out.push_str(": ");
                out.push_str(&recent.into_iter().rev().collect::<Vec<_>>().join("; "));
            }
            out.push('\n');
        }
        Ok(out)
    }

    // ── Internal helpers ───────────────────────────────────────────────────────

    async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>> {
        self.pool.begin().await.map_err(map_sql)
    }

    async fn entity_exists(&self, app_name: &str, user_id: &str, name: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT 1 AS x FROM kg_entities WHERE app_name=?1 AND user_id=?2 AND name=?3 LIMIT 1",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sql)?;
        Ok(row.is_some())
    }

    /// Load entities (optionally restricted to `names`) with their current observations.
    async fn load_entities(
        &self,
        app_name: &str,
        user_id: &str,
        names: Option<&[String]>,
    ) -> Result<Vec<Entity>> {
        let ent_rows = sqlx::query(
            "SELECT name, entity_type FROM kg_entities WHERE app_name=?1 AND user_id=?2 ORDER BY name",
        )
        .bind(app_name)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sql)?;

        let mut entities = Vec::new();
        for row in ent_rows {
            let name: String = row.get("name");
            if let Some(filter) = names
                && !filter.iter().any(|n| n == &name)
            {
                continue;
            }
            let entity_type: String = row.get("entity_type");
            let obs = sqlx::query(
                "SELECT id, content, valid_from FROM kg_observations \
                 WHERE app_name=?1 AND user_id=?2 AND entity_name=?3 AND valid_to IS NULL \
                 ORDER BY id",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(&name)
            .fetch_all(&self.pool)
            .await
            .map_err(map_sql)?
            .into_iter()
            .map(|r| Observation {
                id: r.get::<i64, _>("id"),
                content: r.get::<String, _>("content"),
                valid_from: parse_ts(&r.get::<String, _>("valid_from")),
            })
            .collect();

            entities.push(Entity { name, entity_type, observations: obs });
        }
        Ok(entities)
    }

    async fn load_relations(
        &self,
        app_name: &str,
        user_id: &str,
        entity: &str,
    ) -> Result<Vec<Relation>> {
        let rows = sqlx::query(
            "SELECT id, source, relation_type, target FROM kg_relations \
             WHERE app_name=?1 AND user_id=?2 AND (source=?3 OR target=?3) AND valid_to IS NULL \
             ORDER BY id",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(entity)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sql)?;
        Ok(rows.into_iter().map(row_to_relation).collect())
    }

    async fn load_all_relations(&self, app_name: &str, user_id: &str) -> Result<Vec<Relation>> {
        let rows = sqlx::query(
            "SELECT id, source, relation_type, target FROM kg_relations \
             WHERE app_name=?1 AND user_id=?2 AND valid_to IS NULL ORDER BY id",
        )
        .bind(app_name)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sql)?;
        Ok(rows.into_iter().map(row_to_relation).collect())
    }

    /// Recent raw turns ranked by query-token overlap (newest first within ties).
    ///
    /// Scans a bounded window of the most recent turns and keeps those mentioning
    /// at least one query token, ranked by how many distinct tokens they contain —
    /// the episodic counterpart of [`search_nodes`](Self::search_nodes)'s scoring.
    async fn recall_episodic(
        &self,
        app_name: &str,
        user_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let tokens = tokenize(query);
        // Bounded recent window; ranking happens in Rust to keep the SQL simple
        // and injection-safe regardless of token count.
        const WINDOW: i64 = 200;
        let rows = sqlx::query(
            "SELECT author, text, timestamp FROM kg_episodic \
             WHERE app_name=?1 AND user_id=?2 \
             ORDER BY id DESC LIMIT ?3",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(WINDOW)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sql)?;

        // (token_overlap, entry); rows arrive newest-first, and the sort below is
        // stable, so equal-overlap turns stay in recency order.
        let mut scored: Vec<(usize, MemoryEntry)> = rows
            .into_iter()
            .filter_map(|r| {
                let text: String = r.get("text");
                let lc = text.to_lowercase();
                let overlap = tokens.iter().filter(|t| lc.contains(t.as_str())).count();
                if overlap == 0 {
                    return None;
                }
                let author: String = r.get("author");
                Some((
                    overlap,
                    MemoryEntry {
                        content: Content::new(author.clone()).with_text(text),
                        author,
                        timestamp: parse_ts(&r.get::<String, _>("timestamp")),
                    },
                ))
            })
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        Ok(scored.into_iter().take(limit).map(|(_, m)| m).collect())
    }
}

/// Split a free-text query into lowercase content tokens for relevance matching.
///
/// Splits on non-alphanumeric boundaries, drops very short tokens and common
/// stopwords, and de-duplicates. Falls back to the whole trimmed query when
/// nothing survives, so short or single-word queries still match.
fn tokenize(query: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the", "and", "you", "your", "yours", "for", "with", "what", "that", "this", "are", "was",
        "has", "have", "had", "about", "when", "where", "which", "will", "can", "did", "does",
        "into", "from", "they", "them", "our", "but", "not", "any", "all", "know", "tell", "there",
        "their", "would", "should", "could", "been", "were", "who",
    ];
    let mut tokens: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect();
    tokens.sort();
    tokens.dedup();
    if tokens.is_empty() {
        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            tokens.push(q);
        }
    }
    tokens
}

/// Render a search result as a single compact line of fact text.
fn result_to_sentence(r: &GraphSearchResult) -> String {
    let mut s = format!("{} ({})", r.entity.name, r.entity.entity_type);
    if !r.entity.observations.is_empty() {
        let obs: Vec<&str> = r.entity.observations.iter().map(|o| o.content.as_str()).collect();
        s.push_str(": ");
        s.push_str(&obs.join("; "));
    }
    for rel in &r.relations {
        s.push_str(&format!(" | {} -[{}]-> {}", rel.source, rel.relation_type, rel.target));
    }
    s
}

fn row_to_relation(r: sqlx::sqlite::SqliteRow) -> Relation {
    Relation {
        id: r.get::<i64, _>("id"),
        source: r.get::<String, _>("source"),
        relation_type: r.get::<String, _>("relation_type"),
        target: r.get::<String, _>("target"),
    }
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now())
}

/// Concatenate the text parts of a [`Content`] (the searchable/recordable text).
fn content_text(c: &Content) -> String {
    c.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join(" ")
}

fn map_sql(e: sqlx::Error) -> adk_core::AdkError {
    adk_core::AdkError::memory(format!("knowledge-graph sqlite error: {e}"))
}

// ─── MemoryService adapter ───────────────────────────────────────────────────

#[async_trait]
impl MemoryService for GraphMemoryService {
    /// Cheap ingest: record each turn into the episodic log. Distilling turns
    /// into entities/relations is deliberately left to an out-of-band
    /// consolidation pass so the realtime write path stays fast.
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        let mut tx = self.begin().await?;
        for e in entries {
            let text = content_text(&e.content);
            if text.is_empty() {
                continue;
            }
            sqlx::query(
                "INSERT INTO kg_episodic (app_name, user_id, session_id, author, text, timestamp) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .bind(&e.author)
            .bind(&text)
            .bind(e.timestamp.to_rfc3339())
            .execute(&mut *tx)
            .await
            .map_err(map_sql)?;
        }
        tx.commit().await.map_err(map_sql)?;
        Ok(())
    }

    /// Retrieve relevant context as memory entries.
    ///
    /// The session-start sentinel (`"session context"`) or an empty query returns
    /// the compact profile card. Any other query runs hybrid recall: matching
    /// graph facts (rendered as fact sentences) plus matching raw turns.
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10);

        // Session-start / generic: inject the profile card.
        if req.query.trim().is_empty() || req.query == PROFILE_QUERY {
            let card = self.profile_card(&req.app_name, &req.user_id).await?;
            if card.is_empty() {
                return Ok(SearchResponse { memories: Vec::new() });
            }
            return Ok(SearchResponse {
                memories: vec![MemoryEntry {
                    content: Content::new("memory").with_text(card),
                    author: "memory".to_string(),
                    timestamp: Utc::now(),
                }],
            });
        }

        // Topic query: graph facts first, then raw-turn recall.
        let mut memories = Vec::new();
        for r in self.search_nodes(&req.app_name, &req.user_id, &req.query, limit).await? {
            memories.push(MemoryEntry {
                content: Content::new("memory").with_text(result_to_sentence(&r)),
                author: "memory".to_string(),
                timestamp: Utc::now(),
            });
        }
        if memories.len() < limit {
            let remaining = limit - memories.len();
            memories.extend(
                self.recall_episodic(&req.app_name, &req.user_id, &req.query, remaining).await?,
            );
        }
        memories.truncate(limit);
        Ok(SearchResponse { memories })
    }

    /// GDPR erasure: drop the user's entire graph + episodic log.
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let mut tx = self.begin().await?;
        for delete in [
            "DELETE FROM kg_entities WHERE app_name=?1 AND user_id=?2",
            "DELETE FROM kg_observations WHERE app_name=?1 AND user_id=?2",
            "DELETE FROM kg_relations WHERE app_name=?1 AND user_id=?2",
            "DELETE FROM kg_episodic WHERE app_name=?1 AND user_id=?2",
        ] {
            sqlx::query(delete)
                .bind(app_name)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(map_sql)?;
        }
        tx.commit().await.map_err(map_sql)?;
        Ok(())
    }

    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM kg_episodic WHERE app_name=?1 AND user_id=?2 AND session_id=?3")
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(map_sql)?;
        Ok(())
    }

    async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await.map_err(map_sql)?;
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn svc() -> GraphMemoryService {
        let s = GraphMemoryService::new("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    fn entry(author: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            content: Content::new(author).with_text(text),
            author: author.to_string(),
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_search_and_relations() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "shai",
            vec![CreateEntityInput {
                name: "Shai".into(),
                entity_type: "person".into(),
                observations: vec!["Relocated to the Bay Area".into()],
            }],
        )
        .await
        .unwrap();
        kg.create_relations(
            "app",
            "shai",
            vec![CreateRelationInput {
                source: "Shai".into(),
                relation_type: "house_hunting_in".into(),
                target: "Bay Area".into(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(kg.entity_count("app", "shai").await.unwrap(), 1);
        let hits = kg.search_nodes("app", "shai", "bay area", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity.name, "Shai");
        assert_eq!(hits[0].relations.len(), 1);
    }

    #[tokio::test]
    async fn search_matches_individual_query_tokens() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "alex",
            vec![CreateEntityInput {
                name: "Alex".into(),
                entity_type: "person".into(),
                observations: vec!["vegetarian".into(), "severely allergic to peanuts".into()],
            }],
        )
        .await
        .unwrap();

        // Tokens "peanuts" + "vegetarian" live in separate observations; a
        // whole-string substring of this query would score nothing.
        let hits =
            kg.search_nodes("app", "alex", "peanuts and vegetarian dishes", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity.name, "Alex");

        // No shared token → no match.
        let none = kg.search_nodes("app", "alex", "quarterly revenue forecast", 5).await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn episodic_recall_ranks_by_token_overlap() {
        let kg = svc().await;
        kg.add_session(
            "app",
            "alex",
            "s1",
            vec![
                entry("user", "I am training for a marathon in April"),
                entry("user", "I work at Acme Corp as a data engineer"),
            ],
        )
        .await
        .unwrap();

        // No entities exist, so search() falls through to token-based episodic
        // recall; "work" matches the Acme turn but not the marathon one.
        let resp = kg
            .search(SearchRequest {
                query: "where does the user work".into(),
                user_id: "alex".into(),
                app_name: "app".into(),
                limit: Some(5),
                min_score: None,
                project_id: None,
            })
            .await
            .unwrap();
        let joined: String =
            resp.memories.iter().map(|m| content_text(&m.content)).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("Acme"), "expected the Acme turn; got: {joined}");
        assert!(!joined.contains("marathon"), "marathon turn shares no token; got: {joined}");
    }

    #[test]
    fn tokenize_drops_stopwords_and_short_tokens() {
        let t = tokenize("What do you know about my allergies?");
        assert!(t.contains(&"allergies".to_string()));
        assert!(!t.iter().any(|w| w == "you" || w == "what" || w == "do"));
        // All-stopword/short query falls back to the whole trimmed query.
        assert_eq!(tokenize("is it?"), vec!["is it?".to_string()]);
    }

    #[tokio::test]
    async fn merge_observations_on_duplicate_entity() {
        let kg = svc().await;
        let mk = |obs: &str| CreateEntityInput {
            name: "Shai".into(),
            entity_type: "person".into(),
            observations: vec![obs.into()],
        };
        kg.create_entities("app", "u", vec![mk("Likes breath work")]).await.unwrap();
        kg.create_entities("app", "u", vec![mk("Dislikes somatic grounding")]).await.unwrap();

        assert_eq!(kg.entity_count("app", "u").await.unwrap(), 1, "entity must not duplicate");
        let (entities, _) = kg.read_graph("app", "u").await.unwrap();
        assert_eq!(entities[0].observations.len(), 2, "observations must merge");
    }

    #[tokio::test]
    async fn bitemporal_supersede_hides_stale_facts() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "u",
            vec![CreateEntityInput {
                name: "Shai".into(),
                entity_type: "person".into(),
                observations: vec!["Lives in the Bay Area".into()],
            }],
        )
        .await
        .unwrap();
        let (e, _) = kg.read_graph("app", "u").await.unwrap();
        let stale_id = e[0].observations[0].id;

        // Fact changed: supersede the old, add the new.
        kg.invalidate_observation(stale_id).await.unwrap();
        kg.add_observations("app", "u", "Shai", vec!["Moved to Austin".into()]).await.unwrap();

        let (e, _) = kg.read_graph("app", "u").await.unwrap();
        let current: Vec<&str> = e[0].observations.iter().map(|o| o.content.as_str()).collect();
        assert_eq!(current, vec!["Moved to Austin"], "stale fact must not surface");
    }

    #[tokio::test]
    async fn memoryservice_add_session_then_recall() {
        let kg = svc().await;
        kg.add_session(
            "app",
            "u",
            "s1",
            vec![
                entry("user", "I want to talk about my sleep schedule"),
                entry("model", "Tell me more"),
            ],
        )
        .await
        .unwrap();

        let resp = kg
            .search(SearchRequest {
                query: "sleep".into(),
                user_id: "u".into(),
                app_name: "app".into(),
                limit: Some(5),
                min_score: None,
                project_id: None,
            })
            .await
            .unwrap();
        assert!(
            resp.memories.iter().any(|m| content_text(&m.content).contains("sleep schedule")),
            "episodic recall should find the turn"
        );
    }

    #[tokio::test]
    async fn profile_card_is_returned_for_session_context() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "u",
            vec![CreateEntityInput {
                name: "Shai".into(),
                entity_type: "person".into(),
                observations: vec!["Prefers to be addressed by name".into()],
            }],
        )
        .await
        .unwrap();

        let resp = kg
            .search(SearchRequest {
                query: PROFILE_QUERY.into(),
                user_id: "u".into(),
                app_name: "app".into(),
                limit: Some(10),
                min_score: None,
                project_id: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.memories.len(), 1);
        let card = content_text(&resp.memories[0].content);
        assert!(card.contains("Shai"));
        assert!(card.contains("addressed by name"));
    }

    #[tokio::test]
    async fn delete_user_clears_everything() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "u",
            vec![CreateEntityInput {
                name: "X".into(),
                entity_type: "t".into(),
                observations: vec!["o".into()],
            }],
        )
        .await
        .unwrap();
        kg.add_session("app", "u", "s", vec![entry("user", "hello")]).await.unwrap();

        kg.delete_user("app", "u").await.unwrap();
        assert_eq!(kg.entity_count("app", "u").await.unwrap(), 0);
        let (e, r) = kg.read_graph("app", "u").await.unwrap();
        assert!(e.is_empty() && r.is_empty());
    }

    #[tokio::test]
    async fn per_user_isolation() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "alice",
            vec![CreateEntityInput {
                name: "Secret".into(),
                entity_type: "t".into(),
                observations: vec![],
            }],
        )
        .await
        .unwrap();
        assert_eq!(kg.entity_count("app", "alice").await.unwrap(), 1);
        assert_eq!(kg.entity_count("app", "bob").await.unwrap(), 0, "users must be isolated");
    }

    #[tokio::test]
    async fn cascade_delete_removes_relations() {
        let kg = svc().await;
        kg.create_entities(
            "app",
            "u",
            vec![
                CreateEntityInput {
                    name: "A".into(),
                    entity_type: "t".into(),
                    observations: vec![],
                },
                CreateEntityInput {
                    name: "B".into(),
                    entity_type: "t".into(),
                    observations: vec![],
                },
            ],
        )
        .await
        .unwrap();
        kg.create_relations(
            "app",
            "u",
            vec![CreateRelationInput {
                source: "A".into(),
                relation_type: "knows".into(),
                target: "B".into(),
            }],
        )
        .await
        .unwrap();

        kg.delete_entities("app", "u", vec!["A".into()]).await.unwrap();
        let (_, relations) = kg.read_graph("app", "u").await.unwrap();
        assert!(relations.is_empty(), "relations touching a deleted entity must be removed");
    }
}
