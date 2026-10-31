//! Redis-backed [`SubjectRegistry`] and [`ConsentStore`].
//!
//! # Key layout
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `{adk:subjects}:s:<subject>` | Hash | `subject`, `pseudonym`, `state`, `linked_to_key`, `linked_to_hex`, `linked_to`, `linked_at_us`, `created_at_us` |
//! | `{adk:subjects}:pseudonyms` | Hash | Pseudonym → subject segment |
//! | `{adk:subjects}:linked:<subject>` | Set | The segments of the visitor ids linked to the subject |
//! | `{adk:consent}:records` | Hash | Record id → JSON without `revokedAt` |
//! | `{adk:consent}:revoked` | Hash | Record id → revocation time (µs) |
//! | `{adk:consent}:subject:<subject>` | Sorted set | The subject's record ids, scored by grant time (µs) |
//!
//! A subject segment is the hex encoding of `kind:id`, so no identifier can spill into the
//! next segment. Registration, state changes, links, removal, grants, revocations, and
//! erasure each run as one Lua script, so concurrent callers see one pseudonym per subject,
//! one link per visitor id, and one first revocation. A sorted set orders equal scores by
//! member, so records granted at one instant read back by id.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentRecord, ConsentStore, SubjectRecord, SubjectRegistry, SubjectState,
    consent_conflict_error, erasing_error, link_consumed_error, validate_link,
};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::prelude::*;

use crate::durable_common::{hex, storage_error};
use crate::redis_stores::{connect, load_scripts};
use crate::subject_common::{
    SUBJECT_STORAGE, StoredSubject, consent_parts, consent_record, subject_key,
};

const PSEUDONYMS: &str = "{adk:subjects}:pseudonyms";
const LINKED_PREFIX: &str = "{adk:subjects}:linked:";
const RECORDS: &str = "{adk:consent}:records";
const REVOKED: &str = "{adk:consent}:revoked";

fn subject_hash(segment: &str) -> String {
    format!("{{adk:subjects}}:s:{segment}")
}

fn consent_index(subject: &SubjectRef) -> String {
    format!("{{adk:consent}}:subject:{}", hex(&subject_key(subject)))
}

fn failed(error: impl std::fmt::Display) -> AdkError {
    storage_error(SUBJECT_STORAGE, format!("redis subject store failed: {error}"))
}

/// KEYS: the subject hash, the pseudonym index. ARGV: subject JSON, pseudonym, state,
/// registration time (µs), the subject's segment, `1` to overwrite the state of a
/// registered subject.
const REGISTER_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  redis.call('HSET', KEYS[1], 'subject', ARGV[1], 'pseudonym', ARGV[2], 'state', ARGV[3],
    'created_at_us', ARGV[4])
  redis.call('HSET', KEYS[2], ARGV[2], ARGV[5])
elseif ARGV[6] == '1' then
  redis.call('HSET', KEYS[1], 'state', ARGV[3])
end
return 1
"#;

/// KEYS: the visitor's hash, the identified subject's hash, the pseudonym index, the
/// identified subject's linked set. ARGV: visitor JSON, visitor pseudonym, identified JSON,
/// identified pseudonym, now (µs), visitor segment, identified segment, identified key.
const LINK_SCRIPT: &str = r#"
local visitor = redis.call('HGET', KEYS[1], 'state')
local identified = redis.call('HGET', KEYS[2], 'state')
if visitor == 'erasing' then return 'erasing_visitor' end
if identified == 'erasing' then return 'erasing_identified' end
if redis.call('HEXISTS', KEYS[1], 'linked_to_key') == 1 then return 'consumed' end
if not identified then
  redis.call('HSET', KEYS[2], 'subject', ARGV[3], 'pseudonym', ARGV[4], 'state', 'active',
    'created_at_us', ARGV[5])
  redis.call('HSET', KEYS[3], ARGV[4], ARGV[7])
end
if not visitor then
  redis.call('HSET', KEYS[1], 'subject', ARGV[1], 'pseudonym', ARGV[2], 'state', 'active',
    'created_at_us', ARGV[5])
  redis.call('HSET', KEYS[3], ARGV[2], ARGV[6])
end
redis.call('HSET', KEYS[1], 'linked_to_key', ARGV[8], 'linked_to_hex', ARGV[7],
  'linked_to', ARGV[3], 'linked_at_us', ARGV[5])
redis.call('SADD', KEYS[4], ARGV[6])
return 'linked'
"#;

/// KEYS: the subject hash, the pseudonym index. ARGV: the subject's segment, the linked-set
/// key prefix.
const REMOVE_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
local pseudonym = redis.call('HGET', KEYS[1], 'pseudonym')
local linked = redis.call('HGET', KEYS[1], 'linked_to_hex')
redis.call('DEL', KEYS[1])
if pseudonym then redis.call('HDEL', KEYS[2], pseudonym) end
if linked then redis.call('SREM', ARGV[2] .. linked, ARGV[1]) end
return 1
"#;

/// KEYS: records, revocations, the subject's index. ARGV: id, JSON, grant time (µs),
/// revocation time (µs) or empty.
const GRANT_SCRIPT: &str = r#"
if redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[2]) == 0 then return 0 end
if ARGV[4] ~= '' then redis.call('HSET', KEYS[2], ARGV[1], ARGV[4]) end
redis.call('ZADD', KEYS[3], ARGV[3], ARGV[1])
return 1
"#;

/// KEYS: records, revocations. ARGV: id, revocation time (µs).
const REVOKE_SCRIPT: &str = r#"
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then return 0 end
redis.call('HSETNX', KEYS[2], ARGV[1], ARGV[2])
return 1
"#;

/// KEYS: the subject's index, records, revocations. Returns `[json, revoked, ...]` in
/// grant order.
const LIST_SCRIPT: &str = r#"
local out = {}
for _, id in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  local json = redis.call('HGET', KEYS[2], id)
  if json then
    out[#out + 1] = json
    out[#out + 1] = redis.call('HGET', KEYS[3], id) or ''
  end
end
return out
"#;

/// KEYS: the subject's index, records, revocations. Returns how many records it erased.
const ERASE_SCRIPT: &str = r#"
local ids = redis.call('ZRANGE', KEYS[1], 0, -1)
for _, id in ipairs(ids) do
  redis.call('HDEL', KEYS[2], id)
  redis.call('HDEL', KEYS[3], id)
end
redis.call('DEL', KEYS[1])
return #ids
"#;

fn parse_micros(text: &str) -> Result<Option<i64>> {
    if text.is_empty() {
        return Ok(None);
    }
    text.parse().map(Some).map_err(|_| failed(format!("stored time '{text}' is corrupt")))
}

/// Redis-backed [`SubjectRegistry`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::SubjectRegistry;
/// use adk_session::RedisSubjectRegistry;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let registry = RedisSubjectRegistry::new("redis://localhost:6379").await?;
/// registry.migrate().await?;
/// let record = registry.ensure(&SubjectRef::human("alice")).await?;
/// assert!(record.pseudonym.starts_with("psn:"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisSubjectRegistry {
    client: Client,
    clock: Arc<dyn Clock>,
}

impl RedisSubjectRegistry {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(failed)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps registration and link times.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Loads the registry's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &[REGISTER_SCRIPT, LINK_SCRIPT, REMOVE_SCRIPT])
            .await
            .map_err(failed)
    }

    async fn register(
        &self,
        subject: &SubjectRef,
        state: SubjectState,
        overwrite: bool,
    ) -> Result<()> {
        let segment = hex(&subject_key(subject));
        let fresh = StoredSubject::fresh(subject, state, self.clock.now())?;
        let _: i64 = self
            .client
            .eval(
                REGISTER_SCRIPT,
                vec![subject_hash(&segment), PSEUDONYMS.to_owned()],
                vec![
                    fresh.subject,
                    fresh.pseudonym,
                    fresh.state,
                    fresh.created_at_us.to_string(),
                    segment,
                    if overwrite { "1" } else { "0" }.to_owned(),
                ],
            )
            .await
            .map_err(failed)?;
        Ok(())
    }

    async fn read(&self, segment: &str) -> Result<Option<SubjectRecord>> {
        let fields: HashMap<String, String> =
            self.client.hgetall(subject_hash(segment)).await.map_err(failed)?;
        if fields.is_empty() {
            return Ok(None);
        }
        let field = |name: &str| {
            fields
                .get(name)
                .cloned()
                .ok_or_else(|| failed(format!("stored subject lacks '{name}'")))
        };
        let micros = |name: &str| -> Result<Option<i64>> {
            fields.get(name).map_or(Ok(None), |text| parse_micros(text))
        };
        StoredSubject {
            subject: field("subject")?,
            pseudonym: field("pseudonym")?,
            state: field("state")?,
            linked_to_key: fields.get("linked_to_key").cloned(),
            linked_to: fields.get("linked_to").cloned(),
            linked_at_us: micros("linked_at_us")?,
            created_at_us: micros("created_at_us")?
                .ok_or_else(|| failed("stored subject lacks 'created_at_us'"))?,
        }
        .decode()
        .map(Some)
    }

    async fn required(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        self.get(subject)
            .await?
            .ok_or_else(|| failed(format!("a {} was written but does not read back", subject.kind)))
    }
}

#[async_trait]
impl SubjectRegistry for RedisSubjectRegistry {
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
        self.read(&hex(&subject_key(subject))).await
    }

    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        self.register(subject, SubjectState::Active, false).await?;
        self.required(subject).await
    }

    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
        let segment: Option<String> =
            self.client.hget(PSEUDONYMS, pseudonym).await.map_err(failed)?;
        let Some(segment) = segment else { return Ok(None) };
        Ok(self.read(&segment).await?.map(|record| record.subject))
    }

    async fn link(&self, visitor: &SubjectRef, identified: &SubjectRef) -> Result<SubjectRecord> {
        validate_link(visitor, identified)?;
        let (visitor_segment, identified_segment) =
            (hex(&subject_key(visitor)), hex(&subject_key(identified)));
        let now = self.clock.now();
        let fresh_visitor = StoredSubject::fresh(visitor, SubjectState::Active, now)?;
        let fresh_identified = StoredSubject::fresh(identified, SubjectState::Active, now)?;
        let outcome: String = self
            .client
            .eval(
                LINK_SCRIPT,
                vec![
                    subject_hash(&visitor_segment),
                    subject_hash(&identified_segment),
                    PSEUDONYMS.to_owned(),
                    format!("{LINKED_PREFIX}{identified_segment}"),
                ],
                vec![
                    fresh_visitor.subject,
                    fresh_visitor.pseudonym,
                    fresh_identified.subject,
                    fresh_identified.pseudonym,
                    now.timestamp_micros().to_string(),
                    visitor_segment,
                    identified_segment,
                    subject_key(identified),
                ],
            )
            .await
            .map_err(failed)?;
        match outcome.as_str() {
            "linked" => self.required(visitor).await,
            "consumed" => Err(link_consumed_error()),
            "erasing_visitor" => Err(erasing_error(visitor)),
            "erasing_identified" => Err(erasing_error(identified)),
            other => Err(failed(format!("unexpected link script reply '{other}'"))),
        }
    }

    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
        let segments: Vec<String> = self
            .client
            .smembers(format!("{LINKED_PREFIX}{}", hex(&subject_key(identified))))
            .await
            .map_err(failed)?;
        let mut linked = Vec::with_capacity(segments.len());
        for segment in segments {
            linked.extend(self.read(&segment).await?.map(|record| record.subject));
        }
        linked.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(linked)
    }

    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord> {
        self.register(subject, state, true).await?;
        self.required(subject).await
    }

    async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
        let segment = hex(&subject_key(subject));
        let removed: i64 = self
            .client
            .eval(
                REMOVE_SCRIPT,
                vec![subject_hash(&segment), PSEUDONYMS.to_owned()],
                vec![segment, LINKED_PREFIX.to_owned()],
            )
            .await
            .map_err(failed)?;
        Ok(removed == 1)
    }
}

/// Redis-backed [`ConsentStore`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore};
/// use adk_session::RedisConsentStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let consent = RedisConsentStore::new("redis://localhost:6379").await?;
/// consent.migrate().await?;
/// let visitor = SubjectRef::visitor("v:1");
/// consent.grant(ConsentRecord::new(visitor.clone(), ConsentPurpose::Analytics, "2026-10")).await?;
/// assert_eq!(consent.for_subject(&visitor).await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisConsentStore {
    client: Client,
}

impl RedisConsentStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(failed)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &[GRANT_SCRIPT, REVOKE_SCRIPT, LIST_SCRIPT, ERASE_SCRIPT])
            .await
            .map_err(failed)
    }
}

#[async_trait]
impl ConsentStore for RedisConsentStore {
    async fn grant(&self, record: ConsentRecord) -> Result<()> {
        let (json, granted_us, revoked_us) = consent_parts(&record)?;
        let inserted: i64 = self
            .client
            .eval(
                GRANT_SCRIPT,
                vec![RECORDS.to_owned(), REVOKED.to_owned(), consent_index(&record.subject)],
                vec![
                    record.id.clone(),
                    json,
                    granted_us.to_string(),
                    revoked_us.map(|us| us.to_string()).unwrap_or_default(),
                ],
            )
            .await
            .map_err(failed)?;
        if inserted == 0 {
            return Err(consent_conflict_error(&record.id));
        }
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
        let json: Option<String> = self.client.hget(RECORDS, id).await.map_err(failed)?;
        let Some(json) = json else { return Ok(None) };
        let revoked: Option<String> = self.client.hget(REVOKED, id).await.map_err(failed)?;
        let revoked = revoked.as_deref().map(parse_micros).transpose()?.flatten();
        consent_record(&json, revoked).map(Some)
    }

    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
        let flat: Vec<String> = self
            .client
            .eval(
                LIST_SCRIPT,
                vec![consent_index(subject), RECORDS.to_owned(), REVOKED.to_owned()],
                Vec::<String>::new(),
            )
            .await
            .map_err(failed)?;
        flat.chunks(2)
            .map(|pair| match pair {
                [json, revoked] => consent_record(json, parse_micros(revoked)?),
                _ => Err(failed("consent list script returned an odd reply")),
            })
            .collect()
    }

    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
        let known: i64 = self
            .client
            .eval(
                REVOKE_SCRIPT,
                vec![RECORDS.to_owned(), REVOKED.to_owned()],
                vec![id.to_owned(), at.timestamp_micros().to_string()],
            )
            .await
            .map_err(failed)?;
        if known == 0 {
            return Ok(None);
        }
        self.get(id).await
    }

    async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
        let erased: i64 = self
            .client
            .eval(
                ERASE_SCRIPT,
                vec![consent_index(subject), RECORDS.to_owned(), REVOKED.to_owned()],
                Vec::<String>::new(),
            )
            .await
            .map_err(failed)?;
        u64::try_from(erased).map_err(|_| failed(format!("erase script returned {erased}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keep_one_hash_tag_per_store() {
        let subject = SubjectRef::human("a:b/{c}");
        assert!(subject_hash(&hex(&subject_key(&subject))).starts_with("{adk:subjects}:s:"));
        assert!(!hex(&subject_key(&subject)).contains(['{', '}', ':']));
        assert!(consent_index(&subject).starts_with("{adk:consent}:subject:"));
    }
}
