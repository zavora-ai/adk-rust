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
//! | `mem:{app}:{user}:{session}` | List | JSON-encoded memory entries |
//! | `mem_idx:{app}:{user}` | Set | Session IDs with stored memories |
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

        let idx = index_key(&req.app_name, &req.user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

        let mut memories = Vec::new();

        for sid in &session_ids {
            let key = entries_key(&req.app_name, &req.user_id, sid);
            let raw_entries: Vec<String> = self
                .client
                .lrange(&key, 0, -1)
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
                        return Ok(SearchResponse { memories });
                    }
                }
            }
        }

        Ok(SearchResponse { memories })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let idx = index_key(app_name, user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

        if !session_ids.is_empty() {
            let keys: Vec<String> =
                session_ids.iter().map(|sid| entries_key(app_name, user_id, sid)).collect();
            let pipeline = self.client.pipeline();
            for key in &keys {
                pipeline
                    .del::<(), _>(key)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
            }
            pipeline
                .del::<(), _>(&idx)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
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
}
