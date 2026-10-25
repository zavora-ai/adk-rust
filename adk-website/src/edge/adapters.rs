//! The adapters that join the agent edge to the back office and the pipeline (spec 3.5
//! §7.9, §7.10, §7.8.2 step 7, §7.8.3).
//!
//! | Adapter | Implements | Over |
//! |---|---|---|
//! | [`KernelSignals`] | the edge's `SignalEmitter` | the kernel's `SignalSink`, every text field through the harness's screen (A-I3) |
//! | [`ReleaseSearch`] | the edge's `SiteSearch` | the harness's index of the release's Markdown twins |
//! | [`InboxThreads`] | the edge's `ThreadSink` | `adk-drivers`' `ReplyInThread` over the inbox drivers |
//! | [`SiteSubjectData`] | the edge's `SubjectData` | the edge's leads and the site's contact book |
//! | [`EdgeAnnouncer`] | `adk-site`'s `Announcer` | `site.release.published` and `catalog.changed` on the edge's events |
//! | [`DeferredAnnouncer`] | `adk-site`'s `Announcer` | the edge's announcer once it exists: the runtime is built before the edge that reads it |
//! | [`EdgeLiveStore`] | `adk-site`'s `LiveStore` | the live lane's values, read by the edge's read-only capabilities |
//! | [`ReceiptEntrySigner`] | `adk-site`'s `stream::EntrySigner` | the edge's delegated `receipt` key |

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use adk_awp::signing::jws::DocumentSigner;
use adk_awp::v2::store::{ConsentRecord, LeadStore};
use adk_core::{AdkError, Result};
use adk_harness::website::{SIGNAL_FIELD_MAX_CHARS, screen_bounded};
use adk_kernel::charter::SignalSink;
use adk_server::awp::{EdgeEvents, SignalEmitter, SiteHit, SiteSearch, SubjectData, ThreadSink};
use adk_site::live::{LiveStore, LiveValue};
use adk_site::pipeline::{Announcement, Announcer};
use async_trait::async_trait;
use awp_types::{Bounded, ContentId, PublicKeyJwk, Signal, SignalKind, ThreadRef};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::handlers::ContactBook;

async fn screened(text: &Bounded) -> Bounded {
    screen_bounded(text.as_str(), SIGNAL_FIELD_MAX_CHARS).await
}

/// Files the edge's signals in the back office's agenda through the kernel's `SignalSink`,
/// every text field screened, redacted, cut and marked untrusted (spec 3.5 §7.10, A-I3).
pub struct KernelSignals {
    sink: SignalSink,
}

impl KernelSignals {
    /// Signals into `sink`, the back office's.
    pub fn new(sink: SignalSink) -> Self {
        Self { sink }
    }
}

#[async_trait]
impl SignalEmitter for KernelSignals {
    async fn emit(&self, mut signal: Signal) -> Result<()> {
        match &mut signal.kind {
            SignalKind::LeadCaptured { answers, .. } => {
                for value in answers.values_mut() {
                    *value = screened(value).await;
                }
            }
            SignalKind::ContentGap { topic, samples, .. } => {
                *topic = screened(topic).await;
                for sample in samples.iter_mut() {
                    *sample = screened(sample).await;
                }
            }
            SignalKind::ReviewSeen { excerpt, .. } => *excerpt = screened(excerpt).await,
            _ => {}
        }
        self.sink.emit(signal).await.map(|_| ()).map_err(|e| AdkError::config(e.to_string()))
    }
}

/// The edge's search over the harness's index of the release (spec 3.5 §7.10).
pub struct ReleaseSearch {
    index: Arc<adk_harness::website::SiteSearch>,
}

impl ReleaseSearch {
    /// Search over `index`, reindexed after each release.
    pub fn new(index: Arc<adk_harness::website::SiteSearch>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl SiteSearch for ReleaseSearch {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SiteHit>> {
        Ok(self
            .index
            .search(query, limit)
            .into_iter()
            .map(|hit| SiteHit { path: hit.url, title: hit.title, snippet: hit.snippet })
            .collect())
    }
}

/// In-thread replies through the inbox drivers (spec 3.5 §7.10 `reply_in_thread`).
pub struct InboxThreads {
    reply: Arc<adk_drivers::ReplyInThread>,
    template: String,
}

impl InboxThreads {
    /// Replies through `reply` with the playbook template `template`, whose `body` field
    /// carries the text.
    pub fn new(reply: Arc<adk_drivers::ReplyInThread>, template: impl Into<String>) -> Self {
        Self { reply, template: template.into() }
    }
}

#[async_trait]
impl ThreadSink for InboxThreads {
    async fn reply(&self, thread: &ThreadRef, text: &str) -> Result<()> {
        let fields = BTreeMap::from([("body".to_string(), text.to_string())]);
        self.reply
            .reply(&thread.0, &self.template, &fields)
            .await
            .map(|_| ())
            .map_err(|e| AdkError::config(e.to_string()))
    }
}

/// A subject's data for export and erasure: the leads and the contacts their consent
/// receipts reference (spec 3.5 §7.5 "Consent").
pub struct SiteSubjectData {
    leads: Arc<dyn LeadStore>,
    contacts: Arc<dyn ContactBook>,
}

impl SiteSubjectData {
    /// The data in `leads` and `contacts`.
    pub fn new(leads: Arc<dyn LeadStore>, contacts: Arc<dyn ContactBook>) -> Self {
        Self { leads, contacts }
    }
}

fn receipt_ids(receipts: &[ConsentRecord]) -> Vec<Uuid> {
    receipts.iter().map(|record| record.receipt.id).collect()
}

#[async_trait]
impl SubjectData for SiteSubjectData {
    async fn export(&self, _subject: &str, receipts: &[ConsentRecord]) -> Result<Value> {
        let ids = receipt_ids(receipts);
        let leads: Vec<Value> = self
            .leads
            .since(chrono::DateTime::<chrono::Utc>::MIN_UTC)
            .await
            .map_err(|e| AdkError::config(e.to_string()))?
            .into_iter()
            .filter(|lead| lead.consent.iter().any(|r| ids.contains(&r.0)))
            .map(|lead| json!(lead))
            .collect();
        let contacts = self.contacts.by_receipts(&ids).await?;
        Ok(json!({ "leads": leads, "contacts": contacts }))
    }

    async fn erase(&self, _subject: &str, receipts: &[ConsentRecord]) -> Result<usize> {
        // A lead keeps only a reference; the contact book holds the person's details.
        self.contacts.erase(&receipt_ids(receipts)).await
    }
}

/// Announces releases on the edge's events (spec 3.5 §7.8.2 step 7, §7.8.3 "Events").
pub struct EdgeAnnouncer {
    events: EdgeEvents,
}

impl EdgeAnnouncer {
    /// Announces on `events`, the edge's.
    pub fn new(events: EdgeEvents) -> Self {
        Self { events }
    }
}

#[async_trait]
impl Announcer for EdgeAnnouncer {
    async fn announce(&self, announcement: &Announcement) -> adk_site::Result<()> {
        let fail = |e: adk_awp::v2::store::StoreError| adk_site::SiteError::Pipeline(e.to_string());
        self.events.release_published(announcement.release.clone()).await.map_err(fail)?;
        if let Some(catalog) = &announcement.catalog {
            self.events
                .announce_catalog(catalog.clone(), announcement.catalog_pages.clone())
                .await
                .map_err(fail)?;
        }
        Ok(())
    }
}

/// The pipeline's announcer, set once the edge exists (spec 3.5 §7.8.2 step 7): the site
/// runtime is built first, so its pipeline announces through this; a release published
/// before the edge is set is not announced.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_website::edge::adapters::DeferredAnnouncer;
///
/// let deferred = Arc::new(DeferredAnnouncer::default());
/// assert!(!deferred.is_set());
/// ```
#[derive(Default)]
pub struct DeferredAnnouncer {
    target: OnceLock<Arc<dyn Announcer>>,
}

impl DeferredAnnouncer {
    /// Announces through `target` from now on; returns `false` when one was already set.
    pub fn set(&self, target: Arc<dyn Announcer>) -> bool {
        self.target.set(target).is_ok()
    }

    /// Whether the target is set.
    pub fn is_set(&self) -> bool {
        self.target.get().is_some()
    }
}

#[async_trait]
impl Announcer for DeferredAnnouncer {
    async fn announce(&self, announcement: &Announcement) -> adk_site::Result<()> {
        match self.target.get() {
            Some(target) => target.announce(announcement).await,
            None => {
                tracing::debug!(
                    release = announcement.release.seq,
                    "no edge yet; release not announced"
                );
                Ok(())
            }
        }
    }
}

/// The live lane's values (spec 3.5 §7.8.3 "Three lanes"): written by the update engine,
/// read by the edge's read-only capabilities; never a claim (A-I26).
#[derive(Debug, Default)]
pub struct EdgeLiveStore {
    values: RwLock<BTreeMap<ContentId, BTreeMap<String, LiveValue>>>,
}

#[async_trait]
impl LiveStore for EdgeLiveStore {
    async fn set(
        &self,
        id: &ContentId,
        values: BTreeMap<String, LiveValue>,
    ) -> adk_site::Result<()> {
        self.values.write().await.entry(id.clone()).or_default().extend(values);
        Ok(())
    }

    async fn get(&self, id: &ContentId) -> adk_site::Result<BTreeMap<String, LiveValue>> {
        Ok(self.values.read().await.get(id).cloned().unwrap_or_default())
    }
}

/// Signs stream entries with the edge's delegated `receipt` key (spec 3.5 §7.8.3 "Stream").
pub struct ReceiptEntrySigner {
    signer: Arc<dyn DocumentSigner>,
}

impl ReceiptEntrySigner {
    /// Entries signed by `signer`, the edge's `receipt` key.
    pub fn new(signer: Arc<dyn DocumentSigner>) -> Self {
        Self { signer }
    }
}

#[async_trait]
impl adk_site::stream::EntrySigner for ReceiptEntrySigner {
    fn kid(&self) -> &str {
        self.signer.kid()
    }

    fn public_key(&self) -> PublicKeyJwk {
        self.signer.public_key()
    }

    async fn sign(&self, message: &[u8]) -> adk_site::Result<Vec<u8>> {
        self.signer.sign(message).await.map_err(|e| adk_site::SiteError::Pipeline(e.to_string()))
    }
}
