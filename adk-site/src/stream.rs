//! The stream lane: live blogs, tickers and emergency updates (spec 3.5 §7.8.3 "Three
//! lanes", A-I26).
//!
//! A reporter's entry goes to the edge, which signs it with its delegated `receipt` key and
//! serves it at once in a feed the page polls ([`StreamLane::since`]). Every
//! `fold_interval` (default five minutes) and when the stream closes, the entries are folded
//! into the story's stream field and released through the batcher, so every entry ends up
//! in a signed release. Entries are [`StreamEntry`] documents with a detached JWS over their
//! JCS form without `proof`, the same proof every AWP 2.0 document carries; a fold refuses
//! an entry whose proof does not verify.
//!
//! A stream belongs to a story that is already published: the story passes its approval
//! chain once, and its entries and folds run at the live job's level.

use std::collections::BTreeMap;
use std::time::Duration;

use adk_auth::signed_audit::{
    AuditSigner, Ed25519Signer, NoteVerifier, SignatureAlgorithm, canonicalize,
};
use async_trait::async_trait;
use awp_types::{
    ContentId, ContentItem, ContentState, Markdown, Proof, PublicKeyJwk, StreamEntry, without_proof,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{Result, SiteError};

/// The key that signs stream entries: the edge's delegated `receipt` key (spec 3.5 §7.3,
/// §7.8.3). The edge implements it over its key store; [`Ed25519EntrySigner`] holds a seed.
#[async_trait]
pub trait EntrySigner: Send + Sync {
    /// The key id in `keys.json`.
    fn kid(&self) -> &str;

    /// The public key.
    fn public_key(&self) -> PublicKeyJwk;

    /// Signs `message` with Ed25519.
    ///
    /// # Errors
    ///
    /// [`SiteError::Signing`] when the key cannot sign.
    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>>;
}

/// An Ed25519 [`EntrySigner`] holding its seed in memory; `Debug` shows the key id only.
pub struct Ed25519EntrySigner {
    kid: String,
    inner: Ed25519Signer,
}

impl std::fmt::Debug for Ed25519EntrySigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519EntrySigner").field("kid", &self.kid).finish_non_exhaustive()
    }
}

impl Ed25519EntrySigner {
    /// A signer for key `kid` from a 32-byte seed.
    ///
    /// # Errors
    ///
    /// [`SiteError::Signing`] when `kid` is empty or holds whitespace or `+`.
    pub fn from_seed(kid: impl Into<String>, seed: [u8; 32]) -> Result<Self> {
        let kid = kid.into();
        let inner = Ed25519Signer::from_seed(kid.clone(), seed)
            .map_err(|e| SiteError::Signing(e.to_string()))?;
        Ok(Self { kid, inner })
    }

    /// A signer for key `kid` with a fresh random seed.
    ///
    /// # Errors
    ///
    /// As [`from_seed`](Self::from_seed).
    pub fn generate(kid: impl Into<String>) -> Result<Self> {
        let kid = kid.into();
        let inner =
            Ed25519Signer::generate(kid.clone()).map_err(|e| SiteError::Signing(e.to_string()))?;
        Ok(Self { kid, inner })
    }
}

#[async_trait]
impl EntrySigner for Ed25519EntrySigner {
    fn kid(&self) -> &str {
        &self.kid
    }

    fn public_key(&self) -> PublicKeyJwk {
        PublicKeyJwk::ed25519(URL_SAFE_NO_PAD.encode(self.inner.public_key()))
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>> {
        self.inner.sign(message).await.map_err(|e| SiteError::Signing(e.to_string()))
    }
}

/// The signing input of an entry: `base64url(header) . base64url(JCS(entry without proof))`.
fn signing_input(entry: &StreamEntry, header: &str) -> Result<String> {
    let value = without_proof(entry).map_err(|e| SiteError::serde("stream entry", e))?;
    let payload = canonicalize(&value).map_err(|e| SiteError::serde("stream entry", e))?;
    Ok(format!("{header}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes())))
}

/// Signs `entry` (its `proof` is ignored) and returns the detached JWS proof (AWP 2.0 §5.4).
///
/// # Errors
///
/// [`SiteError::Serde`] when the entry is not I-JSON; signing errors.
pub async fn sign_entry(entry: &StreamEntry, signer: &dyn EntrySigner) -> Result<Proof> {
    let header = format!(r#"{{"alg":"{}","kid":{}}}"#, Proof::EDDSA, Value::from(signer.kid()));
    let header = URL_SAFE_NO_PAD.encode(header.as_bytes());
    let signature = signer.sign(signing_input(entry, &header)?.as_bytes()).await?;
    Ok(Proof::eddsa(signer.kid(), format!("{header}..{}", URL_SAFE_NO_PAD.encode(signature))))
}

/// Verifies an entry's proof with `key`.
///
/// # Errors
///
/// [`SiteError::Signing`] naming what failed.
pub fn verify_entry(entry: &StreamEntry, key: &PublicKeyJwk) -> Result<()> {
    let fail =
        |m: &str| SiteError::Signing(format!("stream entry {} of {}: {m}", entry.n, entry.stream));
    if entry.proof.alg != Proof::EDDSA {
        return Err(fail("the proof is not EdDSA"));
    }
    let mut parts = entry.proof.jws.split('.');
    let (Some(header), Some(""), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(fail("the proof is not a detached compact JWS"));
    };
    let decoded =
        URL_SAFE_NO_PAD.decode(header).map_err(|_| fail("the header is not base64url"))?;
    let header_json: Value =
        serde_json::from_slice(&decoded).map_err(|_| fail("the header is not JSON"))?;
    if header_json.get("alg").and_then(Value::as_str) != Some(Proof::EDDSA)
        || header_json.get("kid").and_then(Value::as_str) != Some(entry.proof.kid.as_str())
        || header_json.get("crit").is_some()
        || header_json.get("b64").is_some()
    {
        return Err(fail("the protected header does not match the proof"));
    }
    let raw = URL_SAFE_NO_PAD.decode(&key.x).map_err(|_| fail("the key is not base64url"))?;
    let verifier = NoteVerifier::from_public_key("awp", SignatureAlgorithm::Ed25519, &raw)
        .map_err(|e| fail(&e.to_string()))?;
    let signature =
        URL_SAFE_NO_PAD.decode(signature).map_err(|_| fail("the signature is not base64url"))?;
    if verifier.verify(signing_input(entry, header)?.as_bytes(), &signature) {
        Ok(())
    } else {
        Err(fail("the signature does not match"))
    }
}

/// Where a stream stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    /// The story.
    pub item: ContentId,
    /// The story's stream field.
    pub field: String,
    /// Entries so far.
    pub entries: u64,
    /// Entries already in a release.
    pub folded: u64,
    /// When the last fold was taken.
    pub last_fold: DateTime<Utc>,
    /// Folds taken.
    pub folds: u64,
    /// When it opened.
    pub opened_at: DateTime<Utc>,
    /// When it closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct Stream {
    status: StreamStatus,
    entries: Vec<StreamEntry>,
}

/// The open streams of a site and the key that signs their entries (spec 3.5 §7.8.3).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_site::stream::{Ed25519EntrySigner, EntrySigner, StreamLane, verify_entry};
/// use awp_types::{ContentItem, ContentKind, ContentState};
/// use chrono::{Duration, Utc};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_site::Result<()> {
/// let signer = Arc::new(Ed25519EntrySigner::from_seed("edge-receipt-2026", [3; 32])?);
/// let lane = StreamLane::new(signer.clone());
/// let story = ContentItem {
///     id: "articles/count/en".into(), kind: ContentKind::Article, slug: "count".into(),
///     language: "en".parse().unwrap(), fields: [("title".to_string(), "The count".into())].into(),
///     volatile: vec![], body: None, media: vec![], sources: vec![], schedule: None,
///     state: ContentState::Published,
/// };
/// let t0 = Utc::now();
/// lane.open(&story, "entries", t0).await?;
/// let entry = lane.append(&story.id, "Polls close.", None, t0).await?;
/// verify_entry(&entry, &signer.public_key())?;
/// assert_eq!(lane.since(&story.id, 0).await?.len(), 1, "the edge feed serves it at once");
/// let folded = lane.fold(&story, t0 + Duration::minutes(5)).await?.unwrap();
/// assert_eq!(folded.fields["entries"].as_array().unwrap().len(), 1);
/// # Ok(())
/// # }
/// ```
pub struct StreamLane {
    signer: std::sync::Arc<dyn EntrySigner>,
    fold_interval: Duration,
    streams: Mutex<BTreeMap<ContentId, Stream>>,
}

impl std::fmt::Debug for StreamLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamLane").field("kid", &self.signer.kid()).finish_non_exhaustive()
    }
}

impl StreamLane {
    /// A lane signing with `signer`, folding every five minutes.
    pub fn new(signer: std::sync::Arc<dyn EntrySigner>) -> Self {
        Self {
            signer,
            fold_interval: StreamEntry::FOLD_INTERVAL,
            streams: Mutex::new(BTreeMap::new()),
        }
    }

    /// Folds every `interval` instead (at most five minutes).
    #[must_use]
    pub fn fold_interval(mut self, interval: Duration) -> Self {
        self.fold_interval = interval.min(StreamEntry::FOLD_INTERVAL);
        self
    }

    /// Opens a stream on a published story's stream field, continuing after the entries the
    /// story already carries.
    ///
    /// # Errors
    ///
    /// [`SiteError::Pipeline`] when the story is not published or the stream is open.
    pub async fn open(&self, story: &ContentItem, field: &str, at: DateTime<Utc>) -> Result<()> {
        if !matches!(story.state, ContentState::Published | ContentState::Corrected) {
            return Err(SiteError::Pipeline(format!(
                "story '{}' is not published; a live story passes its approval chain before its stream opens",
                story.id
            )));
        }
        let mut streams = self.streams.lock().await;
        if streams.get(&story.id).is_some_and(|s| s.status.closed_at.is_none()) {
            return Err(SiteError::Pipeline(format!(
                "the stream of '{}' is already open",
                story.id
            )));
        }
        let entries: Vec<StreamEntry> = story
            .fields
            .get(field)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let folded = entries.len() as u64;
        streams.insert(
            story.id.clone(),
            Stream {
                status: StreamStatus {
                    item: story.id.clone(),
                    field: field.to_string(),
                    entries: folded,
                    folded,
                    last_fold: at,
                    folds: 0,
                    opened_at: at,
                    closed_at: None,
                },
                entries,
            },
        );
        Ok(())
    }

    /// Appends a reporter's entry, signed by the edge key, and returns it as the edge feed
    /// serves it.
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for no open stream; [`SiteError::Pipeline`] when `corrects`
    /// names no earlier entry; signing errors.
    pub async fn append(
        &self,
        item: &ContentId,
        markdown: &str,
        corrects: Option<u64>,
        at: DateTime<Utc>,
    ) -> Result<StreamEntry> {
        let mut streams = self.streams.lock().await;
        let stream = streams
            .get_mut(item)
            .filter(|s| s.status.closed_at.is_none())
            .ok_or_else(|| SiteError::NotFound(format!("open stream of '{item}'")))?;
        let n = stream.entries.len() as u64 + 1;
        if corrects.is_some_and(|c| c == 0 || c >= n) {
            return Err(SiteError::Pipeline(format!(
                "entry {n} corrects an entry that does not exist"
            )));
        }
        let mut entry = StreamEntry {
            stream: item.clone(),
            n,
            published_at: at,
            markdown: Markdown(markdown.to_string()),
            corrects,
            proof: Proof::eddsa(self.signer.kid(), ""),
        };
        entry.proof = sign_entry(&entry, self.signer.as_ref()).await?;
        stream.entries.push(entry.clone());
        stream.status.entries = n;
        Ok(entry)
    }

    /// The entries after number `after`: the feed the page polls.
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an unknown stream.
    pub async fn since(&self, item: &ContentId, after: u64) -> Result<Vec<StreamEntry>> {
        let streams = self.streams.lock().await;
        let stream =
            streams.get(item).ok_or_else(|| SiteError::NotFound(format!("stream of '{item}'")))?;
        Ok(stream.entries.iter().filter(|e| e.n > after).cloned().collect())
    }

    /// Closes a stream; its last entries fold at the next [`due`](Self::due).
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for no open stream.
    pub async fn close(&self, item: &ContentId, at: DateTime<Utc>) -> Result<()> {
        let mut streams = self.streams.lock().await;
        let stream = streams
            .get_mut(item)
            .filter(|s| s.status.closed_at.is_none())
            .ok_or_else(|| SiteError::NotFound(format!("open stream of '{item}'")))?;
        stream.status.closed_at = Some(at);
        Ok(())
    }

    /// The streams whose fold is due at `now`: unfolded entries and a `fold_interval` boundary,
    /// counted from the stream's opening, passed since the last fold; or a closed stream with
    /// unfolded entries.
    ///
    /// Folds keep the stream's cadence: a fold taken late, after the boundary, does not push
    /// the next boundary back, so a caller polling every few seconds or minutes still folds
    /// every `fold_interval` (spec 3.5 §5 S19: "a signed fold lands every five minutes").
    pub async fn due(&self, now: DateTime<Utc>) -> Vec<ContentId> {
        let interval =
            chrono::Duration::from_std(self.fold_interval).unwrap_or_default().num_milliseconds();
        let slot = |status: &StreamStatus, at: DateTime<Utc>| {
            (at - status.opened_at).num_milliseconds().div_euclid(interval)
        };
        self.streams
            .lock()
            .await
            .values()
            .filter(|s| s.status.entries > s.status.folded)
            .filter(|s| {
                s.status.closed_at.is_some()
                    || interval <= 0
                    || slot(&s.status, now) > slot(&s.status, s.status.last_fold)
            })
            .map(|s| s.status.item.clone())
            .collect()
    }

    /// Folds a stream's entries into `story`: its stream field holds every entry so far,
    /// each with its proof verified. `None` when nothing new arrived since the last fold.
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an unknown stream; [`SiteError::Signing`] when an entry's
    /// proof does not verify.
    pub async fn fold(
        &self,
        story: &ContentItem,
        now: DateTime<Utc>,
    ) -> Result<Option<ContentItem>> {
        let key = self.signer.public_key();
        let mut streams = self.streams.lock().await;
        let stream = streams
            .get_mut(&story.id)
            .ok_or_else(|| SiteError::NotFound(format!("stream of '{}'", story.id)))?;
        if stream.status.entries == stream.status.folded {
            return Ok(None);
        }
        for entry in &stream.entries[stream.status.folded as usize..] {
            verify_entry(entry, &key)?;
        }
        let mut folded = story.clone();
        let entries =
            serde_json::to_value(&stream.entries).map_err(|e| SiteError::serde("stream", e))?;
        folded.fields.insert(stream.status.field.clone(), entries);
        stream.status.folded = stream.status.entries;
        stream.status.last_fold = now;
        stream.status.folds += 1;
        tracing::info!(stream = %story.id, entries = stream.status.entries, "site.stream.folded");
        Ok(Some(folded))
    }

    /// Every stream's status.
    pub async fn status(&self) -> Vec<StreamStatus> {
        self.streams.lock().await.values().map(|s| s.status.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u64) -> StreamEntry {
        StreamEntry {
            stream: "articles/count/en".into(),
            n,
            published_at: DateTime::parse_from_rfc3339("2026-10-25T20:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            markdown: Markdown(format!("Update {n}.")),
            corrects: None,
            proof: Proof::eddsa("edge", ""),
        }
    }

    /// S19's live blog as the soak drives it: an entry every 36 s for six hours, each followed
    /// by a poll that arrives seconds late. Folds stay on the five-minute cadence from the
    /// stream's opening (72: one per boundary and one at the close) instead of drifting by
    /// each poll's lateness (spec 3.5 §5 S19).
    #[tokio::test]
    async fn late_polls_keep_the_five_minute_fold_cadence() {
        let signer = std::sync::Arc::new(Ed25519EntrySigner::from_seed("edge", [9; 32]).unwrap());
        let lane = StreamLane::new(signer);
        let story = ContentItem {
            id: "articles/count/en".into(),
            kind: awp_types::ContentKind::Article,
            slug: "count".into(),
            language: "en".parse().unwrap(),
            fields: BTreeMap::new(),
            volatile: Vec::new(),
            body: None,
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        };
        let t0 = DateTime::parse_from_rfc3339("2026-11-03T18:00:00Z").unwrap().with_timezone(&Utc);
        lane.open(&story, "entries", t0).await.unwrap();
        let mut fold_times = Vec::new();
        for n in 0..600i64 {
            let at = t0 + chrono::Duration::seconds(n * 36);
            lane.append(&story.id, &format!("Update {n}."), None, at).await.unwrap();
            if n == 599 {
                lane.close(&story.id, at).await.unwrap();
            }
            // The poll after each entry runs late: ingesting a large delivery takes seconds.
            let poll = at + chrono::Duration::seconds(4 + n % 17);
            for id in lane.due(poll).await {
                assert_eq!(id, story.id);
                lane.fold(&story, poll).await.unwrap().unwrap();
                fold_times.push(poll);
            }
        }
        let status = lane.status().await.remove(0);
        assert_eq!((status.folded, status.folds), (600, 72), "{fold_times:?}");
        for (k, at) in fold_times.iter().take(71).enumerate() {
            let boundary = t0 + chrono::Duration::minutes(5 * (k as i64 + 1));
            assert!(
                *at >= boundary && *at - boundary < chrono::Duration::seconds(60),
                "fold {k} at {at}, boundary {boundary}"
            );
        }
        assert!(lane.due(t0 + chrono::Duration::hours(7)).await.is_empty(), "nothing left");
    }

    #[tokio::test]
    async fn entries_sign_and_tampering_is_caught() {
        let signer = Ed25519EntrySigner::from_seed("edge", [9; 32]).unwrap();
        let mut signed = entry(1);
        signed.proof = sign_entry(&signed, &signer).await.unwrap();
        assert!(signed.proof.jws.contains(".."));
        verify_entry(&signed, &signer.public_key()).unwrap();
        let mut altered = signed.clone();
        altered.markdown = Markdown("Update 1, altered.".into());
        assert!(verify_entry(&altered, &signer.public_key()).is_err());
        let other = Ed25519EntrySigner::from_seed("edge", [8; 32]).unwrap();
        assert!(verify_entry(&signed, &other.public_key()).is_err());
        assert!(format!("{signer:?}").contains("edge"));
    }
}
