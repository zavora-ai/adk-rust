//! Redis-backed memory service.
//!
//! Provides [`RedisMemoryService`], a [`MemoryService`](crate::MemoryService) implementation
//! that stores memory entries in Redis with keyword-based search. Redis does not
//! natively support vector similarity search, so this backend uses word-overlap
//! matching similar to [`InMemoryMemoryService`](crate::InMemoryMemoryService).
//!
//! # Data Model
//!
//! | Key Pattern | Type | Contents |
//! |---|---|---|
//! | `mem:{app}:{user}:{session}` | List | JSON-encoded memory entries (global) |
//! | `mem_idx:{app}:{user}` | Set | Session IDs with stored memories (global) |
//! | `mem:{app}:{user}:p:{project}:{session}` | List | JSON-encoded memory entries (project-scoped) |
//! | `mem_idx:{app}:{user}:p:{project}` | Set | Session IDs with stored memories (project-scoped) |
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::{RedisMemoryConfig, RedisMemoryService};
//!
//! let config = RedisMemoryConfig {
//!     url: "redis://localhost:6379".into(),
//!     ttl: None,
//! };
//! let service = RedisMemoryService::new(config).await?;
//! ```

use crate::service::*;
use adk_core::Result;
use async_trait::async_trait;
use fred::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use tracing::instrument;

/// Configuration for connecting to Redis for memory storage.
#[derive(Debug, Clone)]
pub struct RedisMemoryConfig {
    /// Redis connection URL (e.g. `redis://localhost:6379`).
    pub url: String,
    /// Optional TTL applied to memory entry keys.
    pub ttl: Option<Duration>,
}

/// Redis key for memory entries: `mem:{app}:{user}:{session}`.
fn entries_key(app: &str, user: &str, session: &str) -> String {
    format!("mem:{app}:{user}:{session}")
}

/// Redis key for session index: `mem_idx:{app}:{user}`.
fn index_key(app: &str, user: &str) -> String {
    format!("mem_idx:{app}:{user}")
}

/// Redis key for project-scoped memory entries: `mem:{app}:{user}:p:{project}:{session}`.
fn project_entries_key(app: &str, user: &str, project: &str, session: &str) -> String {
    format!("mem:{app}:{user}:p:{project}:{session}")
}

/// Redis key for project session index: `mem_idx:{app}:{user}:p:{project}`.
fn project_index_key(app: &str, user: &str, project: &str) -> String {
    format!("mem_idx:{app}:{user}:p:{project}")
}

/// Serializable wrapper for a memory entry stored in Redis.
#[derive(Debug, Serialize, Deserialize)]
struct StoredEntry {
    content: adk_core::Content,
    author: String,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    timestamp: chrono::DateTime<chrono::Utc>,
}

/// Redis-backed memory service.
///
/// Stores memory entries as JSON in Redis lists keyed by
/// `(app_name, user_id, session_id)`. Search uses word-overlap matching
/// across all sessions for a given app and user.
///
/// For vector similarity search, use [`PostgresMemoryService`](crate::PostgresMemoryService)
/// or [`Neo4jMemoryService`](crate::Neo4jMemoryService) instead.
///
/// # Example
///
/// ```rust,ignore
/// use adk_memory::{RedisMemoryConfig, RedisMemoryService};
///
/// let config = RedisMemoryConfig {
///     url: "redis://localhost:6379".into(),
///     ttl: None,
/// };
/// let service = RedisMemoryService::new(config).await?;
/// ```
pub struct RedisMemoryService {
    client: Client,
    ttl: Option<Duration>,
}

impl RedisMemoryService {
    /// Connect to Redis for memory storage.
    pub async fn new(config: RedisMemoryConfig) -> Result<Self> {
        let redis_config = Config::from_url(&config.url)
            .map_err(|e| adk_core::AdkError::memory(format!("invalid redis url: {e}")))?;
        let client = Builder::from_config(redis_config)
            .build()
            .map_err(|e| adk_core::AdkError::memory(format!("redis client build failed: {e}")))?;
        client
            .init()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("redis connection failed: {e}")))?;
        Ok(Self { client, ttl: config.ttl })
    }

    /// Search entries in a single Redis list key, appending matches to `memories`.
    async fn search_entries_in_key(
        &self,
        key: &str,
        query_words: &HashSet<String>,
        limit: usize,
        memories: &mut Vec<MemoryEntry>,
    ) -> Result<()> {
        let raw_entries: Vec<String> = self
            .client
            .lrange(key, 0, -1)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("lrange failed: {e}")))?;

        for raw in &raw_entries {
            let stored: StoredEntry = match serde_json::from_str(raw) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let text = crate::text::extract_text(&stored.content);
            let entry_words = crate::text::extract_words(&text);
            if entry_words.is_empty() {
                continue;
            }
            if query_words.iter().any(|w| entry_words.contains(w)) {
                memories.push(MemoryEntry {
                    content: stored.content,
                    author: stored.author,
                    timestamp: stored.timestamp,
                });
                if memories.len() >= limit {
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    /// Scan Redis keys matching a pattern, collecting all results.
    async fn scan_keys(&self, pattern: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut cursor = "0".to_string();
        loop {
            let result: (String, Vec<String>) = self
                .client
                .scan_page(&cursor, pattern, Some(100), None)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("scan failed: {e}")))?;
            cursor = result.0;
            keys.extend(result.1);
            if cursor == "0" {
                break;
            }
        }
        Ok(keys)
    }
}

#[async_trait]
impl MemoryService for RedisMemoryService {
    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, entry_count = entries.len()))]
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let key = entries_key(app_name, user_id, session_id);
        let idx = index_key(app_name, user_id);

        let pipeline = self.client.pipeline();

        for entry in &entries {
            let stored = StoredEntry {
                content: entry.content.clone(),
                author: entry.author.clone(),
                timestamp: entry.timestamp,
            };
            let json = serde_json::to_string(&stored)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            pipeline
                .rpush::<(), _, _>(&key, json)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("rpush failed: {e}")))?;
        }

        // Track this session in the user's index
        pipeline
            .sadd::<(), _, _>(&idx, session_id)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("sadd failed: {e}")))?;

        if let Some(ttl) = self.ttl {
            pipeline
                .expire::<(), _>(&key, ttl.as_secs() as i64, None)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("expire failed: {e}")))?;
        }

        pipeline
            .all::<()>()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10);
        let query_words = crate::text::extract_words(&req.query);
        if query_words.is_empty() {
            return Ok(SearchResponse { memories: Vec::new() });
        }

        let mut memories = Vec::new();

        // Always query the global index
        let idx = index_key(&req.app_name, &req.user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

        for sid in &session_ids {
            let key = entries_key(&req.app_name, &req.user_id, sid);
            self.search_entries_in_key(&key, &query_words, limit, &mut memories).await?;
            if memories.len() >= limit {
                return Ok(SearchResponse { memories });
            }
        }

        // When project_id is Some, also query the project index
        if let Some(ref project_id) = req.project_id {
            let proj_idx = project_index_key(&req.app_name, &req.user_id, project_id);
            let proj_session_ids: Vec<String> = self
                .client
                .smembers(&proj_idx)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

            for sid in &proj_session_ids {
                let key = project_entries_key(&req.app_name, &req.user_id, project_id, sid);
                self.search_entries_in_key(&key, &query_words, limit, &mut memories).await?;
                if memories.len() >= limit {
                    return Ok(SearchResponse { memories });
                }
            }
        }

        Ok(SearchResponse { memories })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        // Use SCAN to find all keys matching mem:{app}:{user}:* (global + project entries)
        let entry_pattern = format!("mem:{app_name}:{user_id}:*");
        let entry_keys = self.scan_keys(&entry_pattern).await?;

        // Use SCAN to find all index keys matching mem_idx:{app}:{user}*
        // This covers both `mem_idx:{app}:{user}` and `mem_idx:{app}:{user}:p:{project}`
        let idx_pattern = format!("mem_idx:{app_name}:{user_id}*");
        let idx_keys = self.scan_keys(&idx_pattern).await?;

        let all_keys: Vec<String> = entry_keys.into_iter().chain(idx_keys).collect();

        if !all_keys.is_empty() {
            let pipeline = self.client.pipeline();
            for key in &all_keys {
                pipeline
                    .del::<(), _>(key)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
            }
            pipeline
                .all::<()>()
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;
        }

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id))]
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        let key = entries_key(app_name, user_id, session_id);
        let idx = index_key(app_name, user_id);

        let pipeline = self.client.pipeline();
        pipeline
            .del::<(), _>(&key)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
        pipeline
            .srem::<(), _, _>(&idx, session_id)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("srem failed: {e}")))?;
        pipeline
            .all::<()>()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.client
            .ping::<String>(None)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, project_id = %project_id, entry_count = entries.len()))]
    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        validate_project_id(project_id)?;

        if entries.is_empty() {
            return Ok(());
        }

        let key = project_entries_key(app_name, user_id, project_id, session_id);
        let idx = project_index_key(app_name, user_id, project_id);

        let pipeline = self.client.pipeline();

        for entry in &entries {
            let stored = StoredEntry {
                content: entry.content.clone(),
                author: entry.author.clone(),
                timestamp: entry.timestamp,
            };
            let json = serde_json::to_string(&stored)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            pipeline
                .rpush::<(), _, _>(&key, json)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("rpush failed: {e}")))?;
        }

        pipeline
            .sadd::<(), _, _>(&idx, session_id)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("sadd failed: {e}")))?;

        if let Some(ttl) = self.ttl {
            pipeline
                .expire::<(), _>(&key, ttl.as_secs() as i64, None)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("expire failed: {e}")))?;
        }

        pipeline
            .all::<()>()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        validate_project_id(project_id)?;

        let key = project_entries_key(app_name, user_id, project_id, "__direct__");
        let idx = project_index_key(app_name, user_id, project_id);

        let stored = StoredEntry {
            content: entry.content.clone(),
            author: entry.author.clone(),
            timestamp: entry.timestamp,
        };
        let json = serde_json::to_string(&stored)
            .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;

        let pipeline = self.client.pipeline();
        pipeline
            .rpush::<(), _, _>(&key, json)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("rpush failed: {e}")))?;
        pipeline
            .sadd::<(), _, _>(&idx, "__direct__")
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("sadd failed: {e}")))?;

        if let Some(ttl) = self.ttl {
            pipeline
                .expire::<(), _>(&key, ttl.as_secs() as i64, None)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("expire failed: {e}")))?;
        }

        pipeline
            .all::<()>()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        validate_project_id(project_id)?;

        let query_words = crate::text::extract_words(query);
        if query_words.is_empty() {
            return Ok(0);
        }

        let proj_idx = project_index_key(app_name, user_id, project_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&proj_idx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

        let mut deleted = 0u64;

        for sid in &session_ids {
            let key = project_entries_key(app_name, user_id, project_id, sid);
            let raw_entries: Vec<String> = self
                .client
                .lrange(&key, 0, -1)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("lrange failed: {e}")))?;

            let mut keep = Vec::new();
            for raw in &raw_entries {
                let stored: StoredEntry = match serde_json::from_str(raw) {
                    Ok(s) => s,
                    Err(_) => {
                        keep.push(raw.clone());
                        continue;
                    }
                };
                let text = crate::text::extract_text(&stored.content);
                let entry_words = crate::text::extract_words(&text);
                if !entry_words.is_empty() && query_words.iter().any(|w| entry_words.contains(w)) {
                    deleted += 1;
                } else {
                    keep.push(raw.clone());
                }
            }

            if keep.len() != raw_entries.len() {
                let pipeline = self.client.pipeline();
                pipeline
                    .del::<(), _>(&key)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
                if keep.is_empty() {
                    pipeline
                        .srem::<(), _, _>(&proj_idx, sid.as_str())
                        .await
                        .map_err(|e| adk_core::AdkError::memory(format!("srem failed: {e}")))?;
                } else {
                    for item in &keep {
                        pipeline.rpush::<(), _, _>(&key, item.as_str()).await.map_err(|e| {
                            adk_core::AdkError::memory(format!("rpush failed: {e}"))
                        })?;
                    }
                    if let Some(ttl) = self.ttl {
                        pipeline.expire::<(), _>(&key, ttl.as_secs() as i64, None).await.map_err(
                            |e| adk_core::AdkError::memory(format!("expire failed: {e}")),
                        )?;
                    }
                }
                pipeline.all::<()>().await.map_err(|e| {
                    adk_core::AdkError::memory(format!("pipeline exec failed: {e}"))
                })?;
            }
        }

        Ok(deleted)
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        validate_project_id(project_id)?;

        let proj_idx = project_index_key(app_name, user_id, project_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&proj_idx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

        let mut deleted = 0u64;

        if !session_ids.is_empty() {
            let pipeline = self.client.pipeline();
            for sid in &session_ids {
                let key = project_entries_key(app_name, user_id, project_id, sid);
                // Count entries before deleting
                let count: i64 = self
                    .client
                    .llen(&key)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("llen failed: {e}")))?;
                deleted += count as u64;
                pipeline
                    .del::<(), _>(&key)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
            }
            pipeline
                .del::<(), _>(&proj_idx)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
            pipeline
                .all::<()>()
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Generate valid app/user/session/project identifiers (non-empty, alphanumeric).
    fn arb_identifier() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9]{0,9}".prop_map(|s| s)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// **Feature: project-scoped-memory, Property 6: Redis Key Generation Correctness**
        /// *For any* `(app, user, session)` tuple, the global `entries_key` SHALL produce
        /// a key of the form `mem:{app}:{user}:{session}` without any project component.
        /// **Validates: Requirements 9.1, 9.4**
        #[test]
        fn prop_global_entries_key_format(
            app in arb_identifier(),
            user in arb_identifier(),
            session in arb_identifier(),
        ) {
            let key = entries_key(&app, &user, &session);
            let expected = format!("mem:{app}:{user}:{session}");
            prop_assert_eq!(&key, &expected);
            // Global key must NOT contain ":p:" project infix
            prop_assert!(!key.contains(":p:"), "global entries key must not contain :p: infix");
        }

        /// **Feature: project-scoped-memory, Property 6: Redis Key Generation Correctness**
        /// *For any* `(app, user)` tuple, the global `index_key` SHALL produce
        /// a key of the form `mem_idx:{app}:{user}` without any project component.
        /// **Validates: Requirements 9.1, 9.4**
        #[test]
        fn prop_global_index_key_format(
            app in arb_identifier(),
            user in arb_identifier(),
        ) {
            let key = index_key(&app, &user);
            let expected = format!("mem_idx:{app}:{user}");
            prop_assert_eq!(&key, &expected);
            prop_assert!(!key.contains(":p:"), "global index key must not contain :p: infix");
        }

        /// **Feature: project-scoped-memory, Property 6: Redis Key Generation Correctness**
        /// *For any* `(app, user, project, session)` tuple, the `project_entries_key` SHALL
        /// produce a key containing the project identifier with the `:p:` infix.
        /// **Validates: Requirements 9.1, 9.2**
        #[test]
        fn prop_project_entries_key_format(
            app in arb_identifier(),
            user in arb_identifier(),
            project in arb_identifier(),
            session in arb_identifier(),
        ) {
            let key = project_entries_key(&app, &user, &project, &session);
            let expected = format!("mem:{app}:{user}:p:{project}:{session}");
            prop_assert_eq!(&key, &expected);
            // Project key MUST contain ":p:" infix
            prop_assert!(key.contains(":p:"), "project entries key must contain :p: infix");
            // Project key must contain the project identifier
            prop_assert!(key.contains(&project), "project entries key must contain project id");
        }

        /// **Feature: project-scoped-memory, Property 6: Redis Key Generation Correctness**
        /// *For any* `(app, user, project)` tuple, the `project_index_key` SHALL
        /// produce a key containing the project identifier with the `:p:` infix.
        /// **Validates: Requirements 9.1, 9.2**
        #[test]
        fn prop_project_index_key_format(
            app in arb_identifier(),
            user in arb_identifier(),
            project in arb_identifier(),
        ) {
            let key = project_index_key(&app, &user, &project);
            let expected = format!("mem_idx:{app}:{user}:p:{project}");
            prop_assert_eq!(&key, &expected);
            prop_assert!(key.contains(":p:"), "project index key must contain :p: infix");
            prop_assert!(key.contains(&project), "project index key must contain project id");
        }

        /// **Feature: project-scoped-memory, Property 6: Redis Key Generation Correctness**
        /// *For any* `(app, user, session, project)` tuple, the global and project keys
        /// SHALL be distinct — they must never collide.
        /// **Validates: Requirements 9.1, 9.2, 9.4**
        #[test]
        fn prop_global_and_project_keys_are_distinct(
            app in arb_identifier(),
            user in arb_identifier(),
            session in arb_identifier(),
            project in arb_identifier(),
        ) {
            let global_entry = entries_key(&app, &user, &session);
            let proj_entry = project_entries_key(&app, &user, &project, &session);
            prop_assert_ne!(&global_entry, &proj_entry,
                "global and project entry keys must be distinct");

            let global_idx = index_key(&app, &user);
            let proj_idx = project_index_key(&app, &user, &project);
            prop_assert_ne!(&global_idx, &proj_idx,
                "global and project index keys must be distinct");
        }
    }
}
