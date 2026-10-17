//! Redis-backed session service.
//!
//! Uses `fred` for async Redis operations with support for standalone
//! and cluster deployments. Sessions are stored as Redis hashes, events
//! as sorted sets (scored by timestamp), and state tiers as separate hashes.
//!
//! # Data Model
//!
//! The service picks its key layout from the connection: a standalone server (or Sentinel)
//! keeps the names every release has written, and a Redis Cluster connection (`cluster_nodes`
//! or a `redis-cluster://` URL) puts each session's keys under one hash tag.
//!
//! | Contents | Type | Standalone key | Cluster key |
//! |---|---|---|---|
//! | Session metadata + session-tier state | Hash | `<app>:<user>:<session>` | `adk:session:{<app>:<user>:<session>}` |
//! | Events scored by timestamp | Sorted Set | `<app>:<user>:<session>:events` | `adk:session:{<app>:<user>:<session>}:events` |
//! | App-tier state | Hash | `app_state:<app>` | `adk:app_state:{<app>}` |
//! | User-tier state | Hash | `user_state:<app>:<user>` | `adk:user_state:{<app>:<user>}` |
//! | Session IDs for list lookups | Set | `sessions_idx:<app>:<user>` | `adk:sessions_idx:{<app>:<user>}` |
//! | Reverse lookup to `<app>:<user>` | String | `session_lookup:<session>` | `adk:session_lookup:{<session>}` |
//!
//! Each `<segment>` is percent-encoded (`%` → `%25`, `:` → `%3A`) so an identifier
//! containing `:` cannot spill into the next segment. In the standalone layout an app name
//! equal to one of the key-family prefixes (`app_state`, `user_state`, `sessions_idx`,
//! `session_lookup`) has its first byte encoded too (`user_state` → `%75ser_state`) in the
//! keys that start with the app name, and identifiers without these characters or names
//! produce the same keys as before the encoding was introduced. The cluster layout also
//! encodes `{` (`%7B`) and `}` (`%7D`), so no identifier can move a key's hash slot.
//!
//! Standalone servers keep the standalone layout so sessions written by earlier releases
//! stay readable. Moving a deployment from a standalone server to a cluster means copying
//! its keys to the cluster names.
//!
//! # Atomicity
//!
//! | Operation | Standalone | Cluster |
//! |---|---|---|
//! | `create` | One `MULTI`/`EXEC`: session hash, app and user tier fields, index entry, lookup | Index entry, lookup, and the app and user tier fields first, then the session hash |
//! | `append_event` | One script: event, session-tier compare-and-swap, app and user tier fields | One script: event and session-tier compare-and-swap; then the app and the user tier fields, one `HSET` each |
//! | `delete` | One `MULTI`/`EXEC`: session hash, events, lookup, index entry | Session hash and events in one `DEL`, then the lookup and the index entry |
//! | `delete_all_sessions` | One `MULTI`/`EXEC` over every session of the user | Each session as in `delete`, then the index |
//!
//! A Redis Cluster runs a script or transaction only when every key it names hashes to one
//! slot, and the app and user tiers are shared by many sessions, so in cluster mode they are
//! written outside the session's script. The session tier keeps its compare-and-swap, and
//! tier fields are still written one field at a time, so concurrent writers of different
//! fields keep each other's values. What changes in cluster mode:
//!
//! - A reader can see an appended event before its app and user tier fields.
//! - An append that fails after its script returns an error with the event already stored.
//! - A `create` that fails part-way can leave its app and user tier fields, and an index
//!   entry and a lookup for a session that does not exist; `list` skips such entries and
//!   `get` reports the session as not found.
//!
//! Every operation validates its identifiers (non-empty, at most
//! [`MAX_ID_LEN`](adk_core::identity::MAX_ID_LEN) bytes, no NUL) before it sends a command.

use crate::service::{validate_scope, validate_session_id};
use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::{ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::prelude::*;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;
use tracing::instrument;
use uuid::Uuid;

/// Configuration for connecting to Redis.
///
/// The `Debug` output redacts credentials embedded in `url` (the userinfo
/// password and any `*password*` query parameter), so the config is safe to log.
#[derive(Clone)]
pub struct RedisSessionConfig {
    /// Redis connection URL (e.g. `redis://localhost:6379`).
    pub url: String,
    /// Optional TTL applied to session and events keys.
    pub ttl: Option<Duration>,
    /// Optional cluster node addresses for cluster mode.
    pub cluster_nodes: Option<Vec<String>>,
}

impl std::fmt::Debug for RedisSessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSessionConfig")
            .field("url", &redact_url_credentials(&self.url))
            .field("ttl", &self.ttl)
            .field("cluster_nodes", &self.cluster_nodes)
            .finish()
    }
}

const REDACTED: &str = "[REDACTED]";

/// Replaces the userinfo password and every `*password*` query value of a
/// connection URL with `[REDACTED]`.
fn redact_url_credentials(url: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };

    let mut redacted = match base.split_once("://") {
        Some((scheme, rest)) => {
            let authority_end = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(authority_end);
            match authority.rsplit_once('@') {
                // Without a `user:` prefix the whole userinfo can be a bare password.
                Some((userinfo, host)) => match userinfo.split_once(':') {
                    Some((user, _password)) => format!("{scheme}://{user}:{REDACTED}@{host}{path}"),
                    None => format!("{scheme}://{REDACTED}@{host}{path}"),
                },
                None => base.to_string(),
            }
        }
        None => base.to_string(),
    };

    if let Some(query) = query {
        let params: Vec<String> = query
            .split('&')
            .map(|param| match param.split_once('=') {
                Some((name, _value)) if name.to_ascii_lowercase().contains("password") => {
                    format!("{name}={REDACTED}")
                }
                _ => param.to_string(),
            })
            .collect();
        redacted.push('?');
        redacted.push_str(&params.join("&"));
    }

    redacted
}

// --- Key generation functions ---

/// Prefixes of the key families that do not start with the app name.
///
/// A session key for an app with one of these names would otherwise share the
/// keyspace of that family, e.g. `session_key("user_state", a, u)` and
/// `user_state_key(a, u)`.
const RESERVED_PREFIXES: [&str; 4] = ["app_state", "user_state", "sessions_idx", "session_lookup"];

/// Percent-encodes `%` and `:` so an encoded segment never contains the `:` delimiter.
fn encode_segment(segment: &str) -> Cow<'_, str> {
    if !segment.contains([':', '%']) {
        return Cow::Borrowed(segment);
    }
    let mut encoded = String::with_capacity(segment.len() + 8);
    for c in segment.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            other => encoded.push(other),
        }
    }
    Cow::Owned(encoded)
}

/// Percent-encodes `%`, `:`, `{`, and `}`, so a segment of a cluster key can neither spill
/// into the next segment nor move the key's hash slot.
fn encode_cluster_segment(segment: &str) -> Cow<'_, str> {
    if !segment.contains([':', '%', '{', '}']) {
        return Cow::Borrowed(segment);
    }
    let mut encoded = String::with_capacity(segment.len() + 8);
    for c in segment.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            '{' => encoded.push_str("%7B"),
            '}' => encoded.push_str("%7D"),
            other => encoded.push(other),
        }
    }
    Cow::Owned(encoded)
}

/// Reverses [`encode_segment`].
///
/// Only `%25` and `%3A` are decoded; any other `%` is kept as written, so
/// lookup values stored before the encoding existed still parse.
fn decode_segment(segment: &str) -> Cow<'_, str> {
    if !segment.contains('%') {
        return Cow::Borrowed(segment);
    }
    let mut decoded = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(pos) = rest.find('%') {
        decoded.push_str(&rest[..pos]);
        match rest.get(pos..pos + 3) {
            Some("%25") => decoded.push('%'),
            Some("%3A" | "%3a") => decoded.push(':'),
            _ => {
                decoded.push('%');
                rest = &rest[pos + 1..];
                continue;
            }
        }
        rest = &rest[pos + 3..];
    }
    decoded.push_str(rest);
    Cow::Owned(decoded)
}

/// Encodes the app segment of keys that start with the app name.
///
/// Applies [`encode_segment`], then encodes the first byte of an app name that
/// equals a [`RESERVED_PREFIXES`] entry.
fn leading_app_segment(app: &str) -> Cow<'_, str> {
    let encoded = encode_segment(app);
    if RESERVED_PREFIXES.contains(&encoded.as_ref()) {
        // Reserved prefixes are ASCII, so the first byte is a whole character.
        let (first, rest) = encoded.split_at(1);
        return Cow::Owned(format!("%{:02X}{rest}", first.as_bytes()[0]));
    }
    encoded
}

/// Session metadata hash key of the standalone layout: `<app>:<user>:<session>`.
pub fn session_key(app: &str, user: &str, session: &str) -> String {
    format!("{}:{}:{}", leading_app_segment(app), encode_segment(user), encode_segment(session))
}

/// Events sorted set key of the standalone layout: `<app>:<user>:<session>:events`.
pub fn events_key(app: &str, user: &str, session: &str) -> String {
    format!(
        "{}:{}:{}:events",
        leading_app_segment(app),
        encode_segment(user),
        encode_segment(session)
    )
}

/// App-level state hash key of the standalone layout: `app_state:<app>`.
pub fn app_state_key(app: &str) -> String {
    format!("app_state:{}", encode_segment(app))
}

/// User-level state hash key of the standalone layout: `user_state:<app>:<user>`.
pub fn user_state_key(app: &str, user: &str) -> String {
    format!("user_state:{}:{}", encode_segment(app), encode_segment(user))
}

/// Session index set key of the standalone layout: `sessions_idx:<app>:<user>`.
pub fn index_key(app: &str, user: &str) -> String {
    format!("sessions_idx:{}:{}", encode_segment(app), encode_segment(user))
}

/// Reverse lookup value naming the owner of a session: `<app>:<user>`.
fn lookup_value(app: &str, user: &str) -> String {
    format!("{}:{}", encode_segment(app), encode_segment(user))
}

/// Parses a [`lookup_value`] back into `(app, user)`.
fn parse_lookup_value(value: &str) -> Option<(String, String)> {
    let (app, user) = value.split_once(':')?;
    Some((decode_segment(app).into_owned(), decode_segment(user).into_owned()))
}

/// The hash tag shared by one session's keys in the cluster layout.
fn cluster_session_tag(app: &str, user: &str, session: &str) -> String {
    format!(
        "{{{}:{}:{}}}",
        encode_cluster_segment(app),
        encode_cluster_segment(user),
        encode_cluster_segment(session)
    )
}

/// Which key names a [`RedisSessionService`] reads and writes; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyLayout {
    /// The names every release has written to a standalone server.
    Standalone,
    /// Hash-tagged names, so each session's keys share one Redis Cluster slot.
    Cluster,
}

impl KeyLayout {
    fn session(self, app: &str, user: &str, session: &str) -> String {
        match self {
            Self::Standalone => session_key(app, user, session),
            Self::Cluster => format!("adk:session:{}", cluster_session_tag(app, user, session)),
        }
    }

    fn events(self, app: &str, user: &str, session: &str) -> String {
        match self {
            Self::Standalone => events_key(app, user, session),
            Self::Cluster => {
                format!("adk:session:{}:events", cluster_session_tag(app, user, session))
            }
        }
    }

    fn app_state(self, app: &str) -> String {
        match self {
            Self::Standalone => app_state_key(app),
            Self::Cluster => format!("adk:app_state:{{{}}}", encode_cluster_segment(app)),
        }
    }

    fn user_state(self, app: &str, user: &str) -> String {
        match self {
            Self::Standalone => user_state_key(app, user),
            Self::Cluster => format!(
                "adk:user_state:{{{}:{}}}",
                encode_cluster_segment(app),
                encode_cluster_segment(user)
            ),
        }
    }

    fn index(self, app: &str, user: &str) -> String {
        match self {
            Self::Standalone => index_key(app, user),
            Self::Cluster => format!(
                "adk:sessions_idx:{{{}:{}}}",
                encode_cluster_segment(app),
                encode_cluster_segment(user)
            ),
        }
    }

    fn lookup(self, session: &str) -> String {
        match self {
            Self::Standalone => format!("session_lookup:{}", encode_segment(session)),
            Self::Cluster => {
                format!("adk:session_lookup:{{{}}}", encode_cluster_segment(session))
            }
        }
    }
}

/// Appends an event and its state delta in one atomic step.
///
/// `KEYS`: session hash, events sorted set, then the state tier hashes written in the same
/// step (app, user), if any. `ARGV`: session-tier write flag (`"1"`), the `state` value the
/// session hash must still hold, its replacement, `updated_at`, the event score, the event
/// JSON, then for each tier key a field count followed by that many field/value pairs.
///
/// Returns `1` when applied, `0` when the session tier changed since it was read, and `-1`
/// when the session does not exist.
const APPEND_EVENT_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if ARGV[1] == '1' then
  local current = redis.call('HGET', KEYS[1], 'state')
  if not current then
    current = ''
  end
  if current ~= ARGV[2] then
    return 0
  end
  redis.call('HSET', KEYS[1], 'state', ARGV[3])
end
redis.call('HSET', KEYS[1], 'updated_at', ARGV[4])
redis.call('ZADD', KEYS[2], ARGV[5], ARGV[6])
local index = 7
for tier = 3, #KEYS do
  local count = tonumber(ARGV[index])
  index = index + 1
  for _ = 1, count do
    redis.call('HSET', KEYS[tier], ARGV[index], ARGV[index + 1])
    index = index + 2
  end
end
return 1
";

/// Attempts [`APPEND_EVENT_SCRIPT`] makes before giving up on a session whose state keeps
/// changing between the read and the write.
const MAX_STATE_WRITE_ATTEMPTS: usize = 32;

/// Serializes each state value to JSON, the form the state hashes store.
fn state_fields(state: &HashMap<String, Value>) -> Result<Vec<(String, String)>> {
    state
        .iter()
        .map(|(key, value)| {
            serde_json::to_string(value).map(|serialized| (key.clone(), serialized)).map_err(|e| {
                adk_core::AdkError::session(format!("serialize state value failed: {e}"))
            })
        })
        .collect()
}

/// Writes the fields of `state` into the hash at `key`; nothing when `state` is empty.
///
/// `client` is a client or a transaction, so the same writes run inside `MULTI`/`EXEC` on a
/// standalone server and as single commands on a cluster.
async fn write_state_hash<C: HashesInterface + Sync>(
    client: &C,
    key: &str,
    state: &HashMap<String, Value>,
) -> Result<()> {
    if state.is_empty() {
        return Ok(());
    }
    let _: () = client
        .hset(key, state_fields(state)?)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis hset failed: {e}")))?;
    Ok(())
}

/// The keys and values [`SessionService::create`] writes for one session.
struct NewSession<'a> {
    session_key: String,
    session_fields: Vec<(String, String)>,
    app_key: String,
    app_delta: &'a HashMap<String, Value>,
    user_key: String,
    user_delta: &'a HashMap<String, Value>,
    index_key: String,
    lookup_key: String,
    lookup_value: String,
    session_id: &'a str,
}

/// Writes `new` through `client`, the session hash last.
///
/// Until the session hash exists, `get` reports the session as not found, `list` skips its
/// index entry, and an append through its lookup finds no session, so on a cluster, where
/// these writes are separate commands, a failure part-way leaves no session that can be read
/// without its index entry, lookup, or initial tier fields.
async fn write_new_session<C>(client: &C, new: NewSession<'_>) -> Result<()>
where
    C: HashesInterface + KeysInterface + SetsInterface + Sync,
{
    let _: () = client
        .sadd(&new.index_key, new.session_id)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis sadd failed: {e}")))?;
    let _: () = client
        .set(&new.lookup_key, new.lookup_value, None, None, false)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis set failed: {e}")))?;
    // Only the delta's fields, so concurrent writers of other fields are kept.
    write_state_hash(client, &new.app_key, new.app_delta).await?;
    write_state_hash(client, &new.user_key, new.user_delta).await?;
    let _: () = client
        .hset(&new.session_key, new.session_fields)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis hset failed: {e}")))?;
    Ok(())
}

/// Removes one session through `client`: its hash and events first, in one `DEL` (one slot
/// in either layout), then its lookup and index entry.
async fn remove_session<C>(
    client: &C,
    layout: KeyLayout,
    app: &str,
    user: &str,
    session: &str,
) -> Result<()>
where
    C: KeysInterface + SetsInterface + Sync,
{
    let _: () = client
        .del(vec![layout.session(app, user, session), layout.events(app, user, session)])
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis del failed: {e}")))?;
    let _: () = client
        .del(layout.lookup(session))
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis del failed: {e}")))?;
    let _: () = client
        .srem(layout.index(app, user), session)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("redis srem failed: {e}")))?;
    Ok(())
}

/// Redis-backed session service.
///
/// Stores sessions using Redis hashes, events as sorted sets scored by
/// timestamp, and three-tier state (app, user, session) as separate hashes.
/// Supports optional TTL on session keys and Redis Cluster, where each session's keys
/// share one hash tag (see the module docs for both key layouts).
///
/// # Example
///
/// ```rust,ignore
/// use adk_session::{RedisSessionConfig, RedisSessionService};
///
/// let config = RedisSessionConfig {
///     url: "redis://localhost:6379".into(),
///     ttl: None,
///     cluster_nodes: None,
/// };
/// let service = RedisSessionService::new(config).await?;
/// ```
pub struct RedisSessionService {
    client: Client,
    ttl: Option<Duration>,
    layout: KeyLayout,
}

impl RedisSessionService {
    /// Connect to Redis (standalone or cluster).
    ///
    /// A connection with `cluster_nodes`, or a `redis-cluster://` URL, uses the cluster key
    /// layout; any other connection uses the standalone layout.
    ///
    /// # Errors
    ///
    /// Returns an error with "redis connection failed" context if the
    /// connection cannot be established.
    pub async fn new(config: RedisSessionConfig) -> Result<Self> {
        let redis_config = if let Some(ref nodes) = config.cluster_nodes {
            let hosts: Vec<(String, u16)> = nodes
                .iter()
                .map(|n| {
                    let parts: Vec<&str> = n.rsplitn(2, ':').collect();
                    if parts.len() == 2 {
                        let port = parts[0].parse::<u16>().unwrap_or(6379);
                        (parts[1].to_string(), port)
                    } else {
                        (n.clone(), 6379)
                    }
                })
                .collect();
            Config { server: ServerConfig::new_clustered(hosts), ..Default::default() }
        } else {
            Config::from_url(&config.url)
                .map_err(|e| adk_core::AdkError::session(format!("redis connection failed: {e}")))?
        };
        let layout = if redis_config.server.is_clustered() {
            KeyLayout::Cluster
        } else {
            KeyLayout::Standalone
        };

        let client = Builder::from_config(redis_config)
            .build()
            .map_err(|e| adk_core::AdkError::session(format!("redis connection failed: {e}")))?;

        client
            .init()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis connection failed: {e}")))?;

        Ok(Self { client, ttl: config.ttl, layout })
    }

    /// Apply TTL to session-related keys if configured.
    async fn apply_ttl(&self, session_k: &str, events_k: &str) -> Result<()> {
        if let Some(ttl) = self.ttl {
            let seconds = ttl.as_secs() as i64;
            let _: () =
                self.client.expire(session_k, seconds, None).await.map_err(|e| {
                    adk_core::AdkError::session(format!("redis expire failed: {e}"))
                })?;
            let _: () =
                self.client.expire(events_k, seconds, None).await.map_err(|e| {
                    adk_core::AdkError::session(format!("redis expire failed: {e}"))
                })?;
        }
        Ok(())
    }

    /// Read a hash as `HashMap<String, Value>` (JSON values stored as strings).
    async fn read_state_hash(&self, key: &str) -> Result<HashMap<String, Value>> {
        let raw: HashMap<String, String> = self
            .client
            .hgetall(key)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis hgetall failed: {e}")))?;
        let mut map = HashMap::new();
        for (k, v) in raw {
            let val: Value = serde_json::from_str(&v).unwrap_or(Value::String(v));
            map.insert(k, val);
        }
        Ok(map)
    }

    /// Applies `event` to the session `(app_name, user_id, session_id)` and refreshes
    /// its TTL.
    ///
    /// The app and user tiers receive only the fields in the delta. The session tier is a
    /// single JSON field, so [`APPEND_EVENT_SCRIPT`] replaces it only while it still holds
    /// the value the delta was merged into, and a changed value is merged again. On a
    /// cluster the app and user tiers live in other slots than the session, so they are
    /// written after the script instead of by it.
    async fn apply_event(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        event: &Event,
    ) -> Result<()> {
        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let layout = self.layout;
        let session_k = layout.session(app_name, user_id, session_id);
        let events_k = layout.events(app_name, user_id, session_id);
        let app_k = layout.app_state(app_name);
        let user_k = layout.user_state(app_name, user_id);
        let tiers_in_script = match layout {
            KeyLayout::Standalone => true,
            KeyLayout::Cluster => false,
        };
        let mut keys = vec![session_k.clone(), events_k.clone()];

        let event_json = serde_json::to_string(event)
            .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
        // ARGV[4..]: updated_at, score, event, then each tier's field count and fields.
        let mut tail = vec![
            event.timestamp.to_rfc3339(),
            event.timestamp.timestamp_millis().to_string(),
            event_json,
        ];
        if tiers_in_script {
            for (key, delta) in [(&app_k, &app_delta), (&user_k, &user_delta)] {
                keys.push(key.clone());
                tail.push(delta.len().to_string());
                for (field, value) in state_fields(delta)? {
                    tail.push(field);
                    tail.push(value);
                }
            }
        }

        for _ in 0..MAX_STATE_WRITE_ATTEMPTS {
            // ARGV[1..=3]: whether to write the session tier, the value it must still
            // hold, and its replacement.
            let head = if session_delta.is_empty() {
                vec!["0".to_string(), String::new(), String::new()]
            } else {
                let current: Option<String> =
                    self.client.hget(&session_k, "state").await.map_err(|e| {
                        adk_core::AdkError::session(format!("redis hget failed: {e}"))
                    })?;
                let stored: HashMap<String, Value> = current
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                // Hashes written by earlier releases also hold a copy of the app and user
                // tiers; the rewrite keeps only the session tier.
                let (_, _, mut session_state) = state_utils::extract_state_deltas(&stored);
                session_state.extend(session_delta.clone());
                let replacement = serde_json::to_string(&session_state).map_err(|e| {
                    adk_core::AdkError::session(format!("serialize state failed: {e}"))
                })?;
                vec!["1".to_string(), current.unwrap_or_default(), replacement]
            };

            let args: Vec<String> = head.into_iter().chain(tail.iter().cloned()).collect();
            let outcome: i64 = self
                .client
                .eval(APPEND_EVENT_SCRIPT, keys.clone(), args)
                .await
                .map_err(|e| adk_core::AdkError::session(format!("redis eval failed: {e}")))?;
            match outcome {
                1 => {
                    if !tiers_in_script {
                        write_state_hash(&self.client, &app_k, &app_delta).await?;
                        write_state_hash(&self.client, &user_k, &user_delta).await?;
                    }
                    return self.apply_ttl(&session_k, &events_k).await;
                }
                -1 => return Err(adk_core::AdkError::session("session not found")),
                _ => continue,
            }
        }

        Err(adk_core::AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Unavailable,
            "session.redis.state_contention",
            format!(
                "session '{session_id}' state changed on each of {MAX_STATE_WRITE_ATTEMPTS} \
                 write attempts; retry the append"
            ),
        ))
    }
}

#[async_trait]
impl SessionService for RedisSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();
        let layout = self.layout;

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let session_k = layout.session(&req.app_name, &req.user_id, &session_id);
        let app_k = layout.app_state(&req.app_name);
        let user_k = layout.user_state(&req.app_name, &req.user_id);
        let state_json = serde_json::to_string(&session_state)
            .map_err(|e| adk_core::AdkError::session(format!("serialize state failed: {e}")))?;
        let new = NewSession {
            session_key: session_k.clone(),
            session_fields: vec![
                ("app_name".into(), req.app_name.clone()),
                ("user_id".into(), req.user_id.clone()),
                ("session_id".into(), session_id.clone()),
                ("state".into(), state_json),
                ("created_at".into(), now.to_rfc3339()),
                ("updated_at".into(), now.to_rfc3339()),
            ],
            app_key: app_k.clone(),
            app_delta: &app_delta,
            user_key: user_k.clone(),
            user_delta: &user_delta,
            index_key: layout.index(&req.app_name, &req.user_id),
            lookup_key: layout.lookup(&session_id),
            lookup_value: lookup_value(&req.app_name, &req.user_id),
            session_id: &session_id,
        };

        match layout {
            KeyLayout::Standalone => {
                let trx = self.client.multi();
                write_new_session(&trx, new).await?;
                let _: () = trx.exec(true).await.map_err(|e| {
                    adk_core::AdkError::session(format!("redis transaction failed: {e}"))
                })?;
            }
            KeyLayout::Cluster => write_new_session(&self.client, new).await?,
        }

        // Apply TTL outside transaction
        let events_k = layout.events(&req.app_name, &req.user_id, &session_id);
        self.apply_ttl(&session_k, &events_k).await?;

        let app_state = self.read_state_hash(&app_k).await?;
        let user_state = self.read_state_hash(&user_k).await?;

        Ok(Box::new(RedisSession {
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
        let layout = self.layout;
        let session_k = layout.session(&req.app_name, &req.user_id, &req.session_id);

        let exists: bool = self
            .client
            .exists(&session_k)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis exists failed: {e}")))?;
        if !exists {
            return Err(crate::service::session_not_found(&req));
        }

        let raw: HashMap<String, String> = self
            .client
            .hgetall(&session_k)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis hgetall failed: {e}")))?;

        let updated_at: DateTime<Utc> =
            raw.get("updated_at").and_then(|s| s.parse().ok()).unwrap_or_else(Utc::now);

        let stored_state: HashMap<String, Value> =
            raw.get("state").and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
        let app_state = self.read_state_hash(&layout.app_state(&req.app_name)).await?;
        let user_state =
            self.read_state_hash(&layout.user_state(&req.app_name, &req.user_id)).await?;

        // Read events from sorted set (ordered by score = timestamp millis)
        let events_k = layout.events(&req.app_name, &req.user_id, &req.session_id);
        let raw_events: Vec<(String, f64)> = self
            .client
            .zrange(&events_k, 0, -1, None, false, None, true)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis zrange failed: {e}")))?;

        let mut events: Vec<Event> = raw_events
            .into_iter()
            .filter_map(|(json, _score)| match serde_json::from_str::<Event>(&json) {
                Ok(event) => Some(event),
                Err(error) => {
                    let event_id = serde_json::from_str::<Value>(&json)
                        .ok()
                        .and_then(|raw| raw.get("id").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    tracing::warn!(
                        session.id = %req.session_id,
                        event.id = %event_id,
                        error = %error,
                        "skipping stored event that failed to deserialize"
                    );
                    None
                }
            })
            .collect();

        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(Box::new(RedisSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state: state_utils::merge_current_tiers(&app_state, &user_state, &stored_state),
            events,
            updated_at,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        validate_scope(&req.app_name, &req.user_id)?;
        let layout = self.layout;
        let session_ids: Vec<String> = self
            .client
            .smembers(layout.index(&req.app_name, &req.user_id))
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis smembers failed: {e}")))?;

        let offset = req.offset.unwrap_or(0);
        let limit = req.limit.unwrap_or(usize::MAX);

        let app_state = self.read_state_hash(&layout.app_state(&req.app_name)).await?;
        let user_state =
            self.read_state_hash(&layout.user_state(&req.app_name, &req.user_id)).await?;

        let mut sessions: Vec<Box<dyn Session>> = Vec::new();
        for sid in session_ids.into_iter().skip(offset).take(limit) {
            let session_k = layout.session(&req.app_name, &req.user_id, &sid);
            let raw: HashMap<String, String> =
                self.client.hgetall(&session_k).await.map_err(|e| {
                    adk_core::AdkError::session(format!("redis hgetall failed: {e}"))
                })?;

            if raw.is_empty() {
                continue; // Key expired or was deleted
            }

            let stored_state: HashMap<String, Value> =
                raw.get("state").and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();

            let updated_at: DateTime<Utc> =
                raw.get("updated_at").and_then(|s| s.parse().ok()).unwrap_or_else(Utc::now);

            sessions.push(Box::new(RedisSession {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id: sid,
                state: state_utils::merge_current_tiers(&app_state, &user_state, &stored_state),
                events: Vec::new(),
                updated_at,
            }));
        }

        Ok(sessions)
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        req.try_identity()?;
        let layout = self.layout;
        match layout {
            KeyLayout::Standalone => {
                let trx = self.client.multi();
                remove_session(&trx, layout, &req.app_name, &req.user_id, &req.session_id).await?;
                let _: () = trx.exec(true).await.map_err(|e| {
                    adk_core::AdkError::session(format!("redis transaction failed: {e}"))
                })?;
            }
            KeyLayout::Cluster => {
                remove_session(&self.client, layout, &req.app_name, &req.user_id, &req.session_id)
                    .await?;
            }
        }
        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        validate_session_id(session_id)?;
        // Strip temp: keys
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        // Use reverse lookup to find app_name and user_id
        let lookup_val: Option<String> = self
            .client
            .get(self.layout.lookup(session_id))
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis get failed: {e}")))?;

        let lookup_val =
            lookup_val.ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let (app_name, user_id) = parse_lookup_value(&lookup_val)
            .ok_or_else(|| adk_core::AdkError::session("corrupt session lookup entry"))?;

        self.apply_event(&app_name, &user_id, session_id, &event).await
    }

    #[instrument(skip_all, fields(
        app_name = %req.identity.app_name,
        user_id = %req.identity.user_id,
        session_id = %req.identity.session_id,
    ))]
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        // The key comes straight from the identity — no reverse lookup needed.
        self.apply_event(
            req.identity.app_name.as_ref(),
            req.identity.user_id.as_ref(),
            req.identity.session_id.as_ref(),
            &event,
        )
        .await
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        validate_scope(app_name, user_id)?;
        let layout = self.layout;
        let idx_k = layout.index(app_name, user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx_k)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("redis smembers failed: {e}")))?;

        if session_ids.is_empty() {
            return Ok(());
        }

        match layout {
            KeyLayout::Standalone => {
                let trx = self.client.multi();
                for sid in &session_ids {
                    remove_session(&trx, layout, app_name, user_id, sid).await?;
                }
                let _: () = trx
                    .del(&idx_k)
                    .await
                    .map_err(|e| adk_core::AdkError::session(format!("redis del failed: {e}")))?;
                let _: () = trx.exec(true).await.map_err(|e| {
                    adk_core::AdkError::session(format!("redis transaction failed: {e}"))
                })?;
            }
            KeyLayout::Cluster => {
                for sid in &session_ids {
                    remove_session(&self.client, layout, app_name, user_id, sid).await?;
                }
                let _: () =
                    self.client.del(&idx_k).await.map_err(|e| {
                        adk_core::AdkError::session(format!("redis del failed: {e}"))
                    })?;
            }
        }

        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        let _: String = self
            .client
            .ping(None)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("health check failed: {e}")))?;
        Ok(())
    }
}

/// In-memory representation of a session retrieved from Redis.
struct RedisSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for RedisSession {
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

impl State for RedisSession {
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

impl Events for RedisSession {
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

    fn config(url: &str) -> RedisSessionConfig {
        RedisSessionConfig { url: url.to_string(), ttl: None, cluster_nodes: None }
    }

    #[test]
    fn debug_redacts_userinfo_password() {
        let rendered = format!("{:?}", config("redis://admin:hunter2@cache.internal:6379/0"));
        assert_eq!(
            rendered,
            "RedisSessionConfig { url: \"redis://admin:[REDACTED]@cache.internal:6379/0\", \
             ttl: None, cluster_nodes: None }"
        );
    }

    #[test]
    fn debug_redacts_password_only_userinfo() {
        let rendered = format!("{:?}", config("rediss://:hunter2@cache.internal:6380"));
        assert!(!rendered.contains("hunter2"), "password leaked: {rendered}");
        assert!(rendered.contains("rediss://:[REDACTED]@cache.internal:6380"));

        let bare = format!("{:?}", config("redis://hunter2@cache.internal"));
        assert!(!bare.contains("hunter2"), "bare userinfo leaked: {bare}");
        assert!(bare.contains("redis://[REDACTED]@cache.internal"));
    }

    #[test]
    fn debug_redacts_password_query_parameters() {
        let url = "redis-sentinel://sentinel.internal:26379/0?sentinelServiceName=main&sentinelPassword=hunter2";
        let rendered = format!("{:?}", config(url));
        assert!(!rendered.contains("hunter2"), "query password leaked: {rendered}");
        assert!(rendered.contains("sentinelServiceName=main&sentinelPassword=[REDACTED]"));
    }

    #[test]
    fn debug_keeps_credential_free_urls_unchanged() {
        assert_eq!(redact_url_credentials("redis://localhost:6379"), "redis://localhost:6379");
        assert_eq!(redact_url_credentials("localhost:6379"), "localhost:6379");
    }

    #[test]
    fn test_lookup_value_round_trips_identifiers_with_delimiters() {
        for (app, user) in
            [("app", "user"), ("a:b", "c"), ("a", "b:c"), ("100%", "%3A"), ("::", "%")]
        {
            assert_eq!(
                parse_lookup_value(&lookup_value(app, user)),
                Some((app.to_string(), user.to_string()))
            );
        }
    }

    #[test]
    fn test_lookup_value_parses_values_written_before_encoding() {
        assert_eq!(
            [
                parse_lookup_value("app:user"),
                parse_lookup_value("app:50%off"),
                parse_lookup_value("x")
            ],
            [
                Some(("app".to_string(), "user".to_string())),
                Some(("app".to_string(), "50%off".to_string())),
                None,
            ]
        );
    }

    #[test]
    fn test_lookup_key_is_encoded() {
        let lookup = |session| KeyLayout::Standalone.lookup(session);
        assert_eq!(lookup("s:1"), "session_lookup:s%3A1");
        assert_ne!(lookup("u:s"), session_key("session_lookup", "u", "s"));
    }

    /// Every key of one session in the cluster layout, in the order the session's script
    /// names them, followed by the shared tier, index, and lookup keys.
    fn cluster_keys(app: &str, user: &str, session: &str) -> [String; 6] {
        let layout = KeyLayout::Cluster;
        [
            layout.session(app, user, session),
            layout.events(app, user, session),
            layout.app_state(app),
            layout.user_state(app, user),
            layout.index(app, user),
            layout.lookup(session),
        ]
    }

    #[test]
    fn cluster_keys_of_one_session_share_a_hash_slot() {
        for (app, user, session) in [
            ("app", "user", "session"),
            ("a:b", "{u}", "s}{"),
            ("{", "}", "{}"),
            ("100%", "%7B", "x{y}z"),
        ] {
            let [session_k, events_k, _, user_k, index_k, _] = cluster_keys(app, user, session);
            assert_eq!(
                fred::util::redis_keyslot(session_k.as_bytes()),
                fred::util::redis_keyslot(events_k.as_bytes()),
                "{session_k} and {events_k}"
            );
            assert_eq!(
                fred::util::redis_keyslot(user_k.as_bytes()),
                fred::util::redis_keyslot(index_k.as_bytes()),
                "{user_k} and {index_k}"
            );
        }
    }

    #[test]
    fn cluster_keys_encode_hash_tag_delimiters() {
        assert_eq!(
            cluster_keys("a:b", "{u}", "s}{"),
            [
                "adk:session:{a%3Ab:%7Bu%7D:s%7D%7B}".to_string(),
                "adk:session:{a%3Ab:%7Bu%7D:s%7D%7B}:events".to_string(),
                "adk:app_state:{a%3Ab}".to_string(),
                "adk:user_state:{a%3Ab:%7Bu%7D}".to_string(),
                "adk:sessions_idx:{a%3Ab:%7Bu%7D}".to_string(),
                "adk:session_lookup:{s%7D%7B}".to_string(),
            ]
        );
        // Redis hashes the text between the first `{` and the next `}`; unencoded, `{x}` in
        // a user ID would become the hash tag of every session that user owns.
        let key = KeyLayout::Cluster.session("a", "{x}", "1");
        let open = key.find('{').unwrap();
        let close = open + key[open..].find('}').unwrap();
        assert_eq!(&key[open + 1..close], "a:%7Bx%7D:1");
    }

    #[test]
    fn cluster_keys_of_different_identities_never_collide() {
        let identities = [
            ("a:b", "c", "s"),
            ("a", "b:c", "s"),
            ("a", "b", "c:s"),
            ("{a}", "b", "s"),
            ("%7Ba%7D", "b", "s"),
            ("a", "u", "s:events"),
        ];
        let mut seen = std::collections::HashSet::new();
        for (app, user, session) in identities {
            let [session_k, events_k, ..] = cluster_keys(app, user, session);
            assert!(seen.insert(session_k.clone()), "{session_k} collided");
            assert!(seen.insert(events_k.clone()), "{events_k} collided");
        }
    }

    #[test]
    fn standalone_layout_keeps_existing_key_names() {
        let layout = KeyLayout::Standalone;
        assert_eq!(
            [
                layout.session("app", "{u}", "s"),
                layout.events("app", "{u}", "s"),
                layout.app_state("app"),
                layout.user_state("app", "{u}"),
                layout.index("app", "{u}"),
                layout.lookup("{s}"),
            ],
            [
                "app:{u}:s".to_string(),
                "app:{u}:s:events".to_string(),
                "app_state:app".to_string(),
                "user_state:app:{u}".to_string(),
                "sessions_idx:app:{u}".to_string(),
                "session_lookup:{s}".to_string(),
            ]
        );
    }
}
