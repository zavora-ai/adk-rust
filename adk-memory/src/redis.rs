//! Redis-backed memory service.
//!
//! Provides [`RedisMemoryService`], a [`MemoryService`] implementation
//! that stores memory entries in Redis with keyword-based search. Redis does not
//! natively support vector similarity search, so this backend uses the same word-overlap
//! matching and ranking as [`InMemoryMemoryService`](crate::InMemoryMemoryService).
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
//! Each `{segment}` is percent-encoded (`%` → `%25`, `:` → `%3A`) so an identifier
//! containing `:` cannot spill into the next segment. Identifiers without either
//! character produce the same keys as before the encoding was introduced.
//!
//! # Expiry
//!
//! Each entry stores its own `expires_at`, and reads skip expired entries. A list key
//! expires natively with its latest entry, and stays persistent while it holds an entry
//! that never expires; [`purge_expired`](MemoryService::purge_expired) removes expired
//! entries from lists that are still live, and index members whose list is gone. Entries
//! written before 3.0 carry no id.
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

use crate::key::{decode_segment, encode_segment, escape_glob};
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
    /// The default TTL of each entry written without an expiry, measured from the write.
    /// `None` keeps such entries until they are deleted.
    pub ttl: Option<Duration>,
}

/// Appends a batch of entries to one list and keeps the list's native expiry at its latest
/// entry. `ARGV[1]` is the batch's latest expiry in Unix milliseconds, or empty when an entry
/// of the batch never expires; `ARGV[2..]` are the entries.
///
/// A key that held no expiry before (`PTTL` is -1) already holds an entry that never expires,
/// so it stays persistent.
const APPEND_SCRIPT: &str = r#"
local ttl = redis.call('PTTL', KEYS[1])
for i = 2, #ARGV do
  redis.call('RPUSH', KEYS[1], ARGV[i])
end
if ARGV[1] == '' then
  redis.call('PERSIST', KEYS[1])
elseif ttl ~= -1 then
  local at = tonumber(ARGV[1])
  if ttl >= 0 then
    local now = redis.call('TIME')
    local current = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000) + ttl
    if current > at then at = current end
  end
  redis.call('PEXPIREAT', KEYS[1], at)
end
return 1
"#;

/// Redis key for memory entries: `mem:{app}:{user}:{session}`.
fn entries_key(app: &str, user: &str, session: &str) -> String {
    format!("mem:{}:{}:{}", encode_segment(app), encode_segment(user), encode_segment(session))
}

/// Redis key for session index: `mem_idx:{app}:{user}`.
fn index_key(app: &str, user: &str) -> String {
    format!("mem_idx:{}:{}", encode_segment(app), encode_segment(user))
}

/// Redis key for project-scoped memory entries: `mem:{app}:{user}:p:{project}:{session}`.
fn project_entries_key(app: &str, user: &str, project: &str, session: &str) -> String {
    format!(
        "mem:{}:{}:p:{}:{}",
        encode_segment(app),
        encode_segment(user),
        encode_segment(project),
        encode_segment(session)
    )
}

/// Redis key for project session index: `mem_idx:{app}:{user}:p:{project}`.
fn project_index_key(app: &str, user: &str, project: &str) -> String {
    format!(
        "mem_idx:{}:{}:p:{}",
        encode_segment(app),
        encode_segment(user),
        encode_segment(project)
    )
}

/// `SCAN` pattern for every entry list of one user, global and project-scoped.
///
/// The `:` after the user segment stops `alice` from matching `alice2`, and the
/// glob escaping stops a user id such as `*` from matching every user.
fn user_entries_pattern(app: &str, user: &str) -> String {
    format!("mem:{}:{}:*", escape_glob(&encode_segment(app)), escape_glob(&encode_segment(user)))
}

/// `SCAN` pattern for every project index set of one user.
///
/// The global index key `mem_idx:{app}:{user}` has no suffix, so callers delete
/// it by name rather than through this pattern.
fn user_project_indexes_pattern(app: &str, user: &str) -> String {
    format!(
        "mem_idx:{}:{}:p:*",
        escape_glob(&encode_segment(app)),
        escape_glob(&encode_segment(user))
    )
}

/// Serializable wrapper for a memory entry stored in Redis.
#[derive(Debug, Serialize, Deserialize)]
struct StoredEntry {
    /// Absent on entries written before 3.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    content: adk_core::Content,
    author: String,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    timestamp: chrono::DateTime<chrono::Utc>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "chrono::serde::ts_milliseconds_option"
    )]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<StoredEntry> for MemoryEntry {
    fn from(stored: StoredEntry) -> Self {
        let mut entry =
            MemoryEntry::new(stored.content, stored.author).with_timestamp(stored.timestamp);
        entry.id = stored.id;
        entry.expires_at = stored.expires_at;
        entry
    }
}

/// Redis-backed memory service.
///
/// Stores memory entries as JSON in Redis lists keyed by
/// `(app_name, user_id, session_id)`. Search uses word-overlap matching
/// across all sessions for a given app and user.
///
/// For vector similarity search, use `PostgresMemoryService`
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
    default_ttl: Option<Duration>,
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
        Ok(Self { client, default_ttl: config.ttl })
    }

    /// Expires each entry written without an expiry `ttl` after it is written, replacing
    /// [`RedisMemoryConfig::ttl`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_memory::{RedisMemoryConfig, RedisMemoryService};
    /// use std::time::Duration;
    ///
    /// let config = RedisMemoryConfig { url: "redis://localhost:6379".into(), ttl: None };
    /// let service = RedisMemoryService::new(config)
    ///     .await?
    ///     .with_default_ttl(Duration::from_secs(30 * 24 * 3600));
    /// ```
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Appends `entries` to the list `key`, each stamped with its id and expiry, and records
    /// `member` in the session index `index`.
    async fn append(
        &self,
        key: &str,
        index: &str,
        member: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now();
        let mut latest = Some(i64::MIN);
        let mut args = vec![String::new()];
        for entry in entries {
            let entry = crate::expiry::stamp(entry, self.default_ttl, now);
            latest = match (latest, entry.expires_at) {
                (Some(latest), Some(at)) => Some(latest.max(at.timestamp_millis())),
                _ => None,
            };
            let stored = StoredEntry {
                id: entry.id,
                content: entry.content,
                author: entry.author,
                timestamp: entry.timestamp,
                expires_at: entry.expires_at,
            };
            args.push(
                serde_json::to_string(&stored).map_err(|e| {
                    adk_core::AdkError::memory(format!("serialization failed: {e}"))
                })?,
            );
        }
        args[0] = latest.map(|at| at.to_string()).unwrap_or_default();
        let _: i64 = self
            .client
            .eval(APPEND_SCRIPT, vec![key.to_string()], args)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("append failed: {e}")))?;
        self.client
            .sadd::<(), _, _>(index, member)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("sadd failed: {e}")))
    }

    /// Reads the list `key` as raw JSON with each entry parsed; unparseable entries are kept
    /// raw with `None`.
    async fn read_list(&self, key: &str) -> Result<Vec<(String, Option<StoredEntry>)>> {
        let raw_entries: Vec<String> = self
            .client
            .lrange(key, 0, -1)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("lrange failed: {e}")))?;
        Ok(raw_entries
            .into_iter()
            .map(|raw| {
                let stored = serde_json::from_str(&raw).ok();
                (raw, stored)
            })
            .collect())
    }

    /// Removes one occurrence of each of `raws` from the list `key`, leaving the key's
    /// expiry as it was, and returns how many were removed.
    async fn remove_raw(&self, key: &str, raws: &[String]) -> Result<u64> {
        let mut removed = 0u64;
        for raw in raws {
            let count: i64 = self
                .client
                .lrem(key, 1, raw.as_str())
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("lrem failed: {e}")))?;
            removed += count as u64;
        }
        Ok(removed)
    }

    /// Scores the entries of one Redis list key against the query, appending matches.
    async fn search_entries_in_key(
        &self,
        key: &str,
        query_words: &HashSet<String>,
        min_score: f32,
        matches: &mut Vec<(f32, MemoryEntry)>,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        for (_, stored) in self.read_list(key).await? {
            let Some(stored) = stored else { continue };
            if crate::expiry::is_expired(stored.expires_at, now) {
                continue;
            }
            let text = crate::text::extract_text(&stored.content);
            let score = crate::text::match_score(query_words, &crate::text::extract_words(&text));
            if score > 0.0 && score >= min_score {
                matches.push((score, stored.into()));
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
        let key = entries_key(app_name, user_id, session_id);
        self.append(&key, &index_key(app_name, user_id), session_id, entries).await
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10);
        let min_score = req.min_score.unwrap_or(0.0);
        let query_words = crate::text::extract_words(&req.query);
        if query_words.is_empty() {
            return Ok(SearchResponse { memories: Vec::new() });
        }

        // Every entry in scope is scored before any is returned, so the best matches win
        // regardless of the order Redis returns sessions in.
        let mut matches = Vec::new();

        // Always query the global index
        let idx = index_key(&req.app_name, &req.user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;

        for sid in &session_ids {
            let key = entries_key(&req.app_name, &req.user_id, sid);
            self.search_entries_in_key(&key, &query_words, min_score, &mut matches).await?;
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
                self.search_entries_in_key(&key, &query_words, min_score, &mut matches).await?;
            }
        }

        Ok(SearchResponse { memories: crate::text::rank_matches(matches, limit) })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn export_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserMemory>> {
        // Reads the keys `delete_user` removes: `{session}` or `p:{project}:{session}`
        // after the user's prefix, each segment percent-encoded.
        let prefix = format!("mem:{}:{}:", encode_segment(app_name), encode_segment(user_id));
        let mut memories = Vec::new();
        for key in self.scan_keys(&user_entries_pattern(app_name, user_id)).await? {
            let Some(rest) = key.strip_prefix(&prefix) else { continue };
            let segments: Vec<&str> = rest.split(':').collect();
            let (project_id, session) = match segments.as_slice() {
                [session] => (None, *session),
                ["p", project, session] => (Some(decode_segment(project)), *session),
                _ => continue,
            };
            let session = decode_segment(session);
            for (_, stored) in self.read_list(&key).await? {
                let stored =
                    stored.ok_or_else(|| adk_core::AdkError::memory("stored entry is corrupt"))?;
                memories.push(UserMemory {
                    entry: stored.into(),
                    session_id: (session != "__direct__").then(|| session.clone()),
                    project_id: project_id.clone(),
                });
            }
        }
        memories.sort_by_key(|memory| memory.entry.timestamp);
        Ok(memories)
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let entry_keys = self.scan_keys(&user_entries_pattern(app_name, user_id)).await?;
        let project_idx_keys =
            self.scan_keys(&user_project_indexes_pattern(app_name, user_id)).await?;

        let pipeline = self.client.pipeline();
        for key in entry_keys.iter().chain(&project_idx_keys) {
            pipeline
                .del::<(), _>(key)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
        }
        pipeline
            .del::<(), _>(index_key(app_name, user_id))
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("del failed: {e}")))?;
        pipeline
            .all::<()>()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("pipeline exec failed: {e}")))?;

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

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_memory(&self, app_name: &str, user_id: &str, memory_id: &str) -> Result<u64> {
        let mut removed = 0u64;
        for key in self.scan_keys(&user_entries_pattern(app_name, user_id)).await? {
            let matching: Vec<String> = self
                .read_list(&key)
                .await?
                .into_iter()
                .filter(|(_, stored)| {
                    stored.as_ref().and_then(|stored| stored.id.as_deref()) == Some(memory_id)
                })
                .map(|(raw, _)| raw)
                .collect();
            removed += self.remove_raw(&key, &matching).await?;
        }
        Ok(removed)
    }

    #[instrument(skip_all)]
    async fn purge_expired(&self) -> Result<u64> {
        let now = chrono::Utc::now();
        let mut removed = 0u64;
        for key in self.scan_keys("mem:*").await? {
            let expired: Vec<String> = self
                .read_list(&key)
                .await?
                .into_iter()
                .filter(|(_, stored)| {
                    stored
                        .as_ref()
                        .is_some_and(|stored| crate::expiry::is_expired(stored.expires_at, now))
                })
                .map(|(raw, _)| raw)
                .collect();
            removed += self.remove_raw(&key, &expired).await?;
        }
        // An index member whose list expired or emptied names nothing; the list of member
        // `m` of index `mem_idx:{rest}` is `mem:{rest}:{m}`.
        for index in self.scan_keys("mem_idx:*").await? {
            let Some(rest) = index.strip_prefix("mem_idx:") else { continue };
            let members: Vec<String> = self
                .client
                .smembers(&index)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("smembers failed: {e}")))?;
            for member in members {
                let list = format!("mem:{rest}:{}", encode_segment(&member));
                let exists: i64 = self
                    .client
                    .exists(&list)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("exists failed: {e}")))?;
                if exists == 0 {
                    self.client
                        .srem::<(), _, _>(&index, member.as_str())
                        .await
                        .map_err(|e| adk_core::AdkError::memory(format!("srem failed: {e}")))?;
                }
            }
        }
        Ok(removed)
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.client
            .ping::<String>(None)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }

    fn supports_project_scoping(&self) -> bool {
        true
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
        let key = project_entries_key(app_name, user_id, project_id, session_id);
        let index = project_index_key(app_name, user_id, project_id);
        self.append(&key, &index, session_id, entries).await
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
        let index = project_index_key(app_name, user_id, project_id);
        self.append(&key, &index, "__direct__", vec![entry]).await
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
            let matching: Vec<String> = self
                .read_list(&key)
                .await?
                .into_iter()
                .filter_map(|(raw, stored)| {
                    let text = crate::text::extract_text(&stored?.content);
                    let entry_words = crate::text::extract_words(&text);
                    (!entry_words.is_empty() && query_words.iter().any(|w| entry_words.contains(w)))
                        .then_some(raw)
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            deleted += self.remove_raw(&key, &matching).await?;
            let remaining: i64 = self
                .client
                .llen(&key)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("llen failed: {e}")))?;
            if remaining == 0 {
                self.client
                    .srem::<(), _, _>(&proj_idx, sid.as_str())
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("srem failed: {e}")))?;
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

    /// Redis `stringmatchlen` glob semantics: `*`, `?`, `[...]` (with `^` and
    /// ranges) and `\` escapes.
    fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some((b'*', rest)) => (0..=text.len()).any(|i| glob_match(rest, &text[i..])),
            Some((b'?', rest)) => !text.is_empty() && glob_match(rest, &text[1..]),
            Some((b'\\', [escaped, rest @ ..])) => {
                text.first() == Some(escaped) && glob_match(rest, &text[1..])
            }
            Some((b'[', rest)) => {
                let Some(&target) = text.first() else { return false };
                let (negate, mut rest) = match rest.split_first() {
                    Some((b'^', tail)) => (true, tail),
                    _ => (false, rest),
                };
                let mut matched = false;
                loop {
                    match rest {
                        [] => return false,
                        [b']', tail @ ..] => {
                            rest = tail;
                            break;
                        }
                        [b'\\', c, tail @ ..] => {
                            matched |= *c == target;
                            rest = tail;
                        }
                        [lo, b'-', hi, tail @ ..] if *hi != b']' => {
                            matched |= (*lo..=*hi).contains(&target);
                            rest = tail;
                        }
                        [c, tail @ ..] => {
                            matched |= *c == target;
                            rest = tail;
                        }
                    }
                }
                matched != negate && glob_match(rest, &text[1..])
            }
            Some((c, rest)) => text.first() == Some(c) && glob_match(rest, &text[1..]),
        }
    }

    fn matches(pattern: &str, key: &str) -> bool {
        glob_match(pattern.as_bytes(), key.as_bytes())
    }

    /// The keys `delete_user` finds by pattern for one user.
    fn scanned_keys_of(app: &str, user: &str) -> Vec<String> {
        vec![
            entries_key(app, user, "s1"),
            project_entries_key(app, user, "proj", "s1"),
            project_index_key(app, user, "proj"),
        ]
    }

    fn matched_by_delete_user(app: &str, user: &str, key: &str) -> bool {
        matches(&user_entries_pattern(app, user), key)
            || matches(&user_project_indexes_pattern(app, user), key)
    }

    #[test]
    fn test_matcher_detects_the_pre_fix_over_match() {
        // The pattern used before the fix had no delimiter after the user segment.
        assert!(matches("mem_idx:app:alice*", "mem_idx:app:alice2"));
    }

    #[test]
    fn test_delete_user_patterns_do_not_match_prefix_sharing_users() {
        for key in scanned_keys_of("app", "alice") {
            assert!(matched_by_delete_user("app", "alice", &key), "{key} not matched");
        }
        for other in ["alice2", "alice-admin", "alice:x", "alic", "bob"] {
            for key in scanned_keys_of("app", other).into_iter().chain([index_key("app", other)]) {
                assert!(!matched_by_delete_user("app", "alice", &key), "alice matched {key}");
            }
        }
    }

    #[test]
    fn test_glob_metacharacter_user_ids_match_only_themselves() {
        for hostile in ["*", "?????", "[a-z]*", "\\*", "a*"] {
            for key in scanned_keys_of("app", hostile) {
                assert!(matched_by_delete_user("app", hostile, &key), "{key} not matched");
            }
            for victim in ["alice", "bob", "a", "abcde"] {
                for key in scanned_keys_of("app", victim) {
                    assert!(
                        !matched_by_delete_user("app", hostile, &key),
                        "{hostile:?} pattern matched {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_glob_metacharacter_app_names_match_only_themselves() {
        assert!(matched_by_delete_user("*", "alice", &entries_key("*", "alice", "s1")));
        assert!(!matched_by_delete_user("*", "alice", &entries_key("other", "alice", "s1")));
    }

    #[test]
    fn test_colon_in_identifiers_does_not_collide() {
        assert_ne!(entries_key("a:b", "c", "s"), entries_key("a", "b:c", "s"));
        assert_ne!(index_key("a:b", "c"), index_key("a", "b:c"));
        assert_ne!(
            project_entries_key("a:b", "c", "p", "s"),
            project_entries_key("a", "b:c", "p", "s")
        );
        assert_ne!(project_index_key("a", "b:p", "x"), project_index_key("a", "b", "p:x"));
        // A global session id shaped like a project suffix stays global.
        assert_ne!(entries_key("a", "u", "p:proj:s"), project_entries_key("a", "u", "proj", "s"));
        // A user id containing `:` is not swept up by a shorter user's pattern.
        assert!(!matched_by_delete_user("a", "b", &entries_key("a", "b:c", "s")));
    }

    #[test]
    fn test_encoded_keys() {
        assert_eq!(
            [
                entries_key("a:b", "100%", "s"),
                index_key("a:b", "100%"),
                project_entries_key("a", "u", "p:1", "s"),
                project_index_key("a", "u", "p:1"),
            ],
            [
                "mem:a%3Ab:100%25:s".to_string(),
                "mem_idx:a%3Ab:100%25".to_string(),
                "mem:a:u:p:p%3A1:s".to_string(),
                "mem_idx:a:u:p:p%3A1".to_string(),
            ]
        );
    }

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
            // No project infix: exactly the prefix and three segments (an identifier may be "p").
            prop_assert_eq!(key.split(':').count(), 4, "global entries key has a project infix");
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
            prop_assert_eq!(key.split(':').count(), 3, "global index key has a project infix");
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
