//! Events: Standard Webhooks subscriptions, delivery with retries and suspension, replay and
//! coalesced catalogue changes (spec 3.5 §7.5, §7.8.3; AWP 2.0 §9).
//!
//! | Route | Does |
//! |---|---|
//! | `POST /awp/v2/events` | Creates a subscription owned by the caller; answers `201` with its signing secret, once |
//! | `GET`, `PATCH`, `DELETE /awp/v2/events/{id}` | Reads, changes (types, URL, reactivation) or ends the caller's subscription |
//! | `POST /awp/v2/events/replay` | Redelivers retained events under their original ids; answers `202` with `{accepted}` |
//! | `GET /awp/v2/events/changes/{seq}/{page}` | Further item ids of release `seq`'s `catalog.changed`, from page 2 |
//!
//! Subscription routes need tier `known` or above through Web Bot Auth or OAuth; a
//! subscription belongs to the requester that created it. Delivery URLs must be `https` and
//! public: the edge refuses loopback, private and link-local addresses when it registers a URL
//! and when it connects. An edge whose delivery client cannot be built with that check does
//! not build.
//!
//! Each delivery is a Standard Webhooks message — `webhook-id` (the event id),
//! `webhook-timestamp`, `webhook-signature` — with `AWP-Version: 2.0`. A failed delivery is
//! retried after 1 s, 5 s, 30 s, 5 min, 30 min and 2 h with jitter; six consecutive failures
//! suspend the subscription, which receives one final `subscription.suspended`. Events are
//! retained 72 hours for replay. A release's `catalog.changed` carries at most 1,000 item ids
//! and a `next` URL for the rest.
//!
//! An [`EventLog`] that keeps catalogue pages, the consent-reference index and pending
//! deliveries — the SQLite, PostgreSQL and Redis edge stores do — makes them survive a
//! restart: a retry the process was waiting on is resumed by
//! [`AwpEdge::spawn_delivery_resumer`](super::AwpEdge::spawn_delivery_resumer) once its lease
//! passes. A log that keeps none leaves them in the process, as before.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_awp::v2::response::awp_json;
use adk_awp::v2::store::{
    StoreError, StoreResult, SubscriptionRecord, SubscriptionStore, SubscriptionUpdate,
};
use adk_core::egress::{EgressPolicy, GuardedClient};
use adk_core::webhook::{WebhookSecret, WebhookSigner};
use async_trait::async_trait;
use awp_types::{
    CatalogChanged, CatalogChangesPage, ContentId, ErrorBody, ErrorCode, EventCatalog,
    EventEnvelope, EventSubscription, EventType, IdentityMethod, OrderUpdated, ReleasePublished,
    RequesterId, StatusChanged, SubscriptionState, SubscriptionSuspended, TrustTier,
};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tiers::EdgeCaller;

/// The subscription routes' base path.
pub const EVENTS_PATH: &str = "/awp/v2/events";
/// The longest delivery body, 64 KiB (AWP 2.0 §9.3).
pub const MAX_EVENT_BODY: usize = 64 * 1024;

/// How the edge delivers and retains events (spec 3.5 §7.5).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EventsConfig {
    /// The event types the site emits, listed in its contract.
    pub catalog: EventCatalog,
    /// The waits before each retry of a failed delivery.
    pub retry_schedule: Vec<Duration>,
    /// How long events are kept for replay.
    pub retention: chrono::Duration,
    /// Which delivery URLs the edge may reach.
    pub egress: EgressPolicy,
    /// The bound on one delivery attempt.
    pub attempt_timeout: Duration,
    /// How long a delivery's next attempt stays its process's before a resumer may take it
    /// from a durable [`EventLog`].
    pub delivery_lease: Duration,
}

impl Default for EventsConfig {
    /// The six AWP 2.0 event types; retries after 1 s, 5 s, 30 s, 5 min, 30 min and 2 h;
    /// 72 hours' retention; public `https` URLs only; ten seconds per attempt; a 70-second
    /// delivery lease.
    fn default() -> Self {
        Self {
            catalog: EventCatalog(vec![
                EventType::SiteReleasePublished,
                EventType::SiteStatusChanged,
                EventType::CatalogChanged,
                EventType::OrderUpdated,
                EventType::ConsentRevoked,
                EventType::SubscriptionSuspended,
            ]),
            retry_schedule: [1, 5, 30, 300, 1_800, 7_200].map(Duration::from_secs).to_vec(),
            retention: chrono::Duration::hours(72),
            egress: EgressPolicy::default(),
            attempt_timeout: Duration::from_secs(10),
            delivery_lease: Duration::from_secs(70),
        }
    }
}

impl EventsConfig {
    /// Sets the event catalogue.
    #[must_use]
    pub fn catalog(mut self, catalog: EventCatalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Sets the retry waits.
    #[must_use]
    pub fn retry_schedule(mut self, schedule: Vec<Duration>) -> Self {
        self.retry_schedule = schedule;
        self
    }

    /// Sets the egress policy; [`EgressPolicy::allow_private_networks`] only where every
    /// subscriber is trusted, such as in tests.
    #[must_use]
    pub fn egress(mut self, egress: EgressPolicy) -> Self {
        self.egress = egress;
        self
    }
}

/// An event kept for replay and who may receive it (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedEvent {
    /// The event.
    pub event: EventEnvelope,
    /// The requesters whose subscriptions receive it; `None` for every subscription naming
    /// its type.
    pub audience: Option<BTreeSet<RequesterId>>,
}

/// A delivery waiting for its next attempt, kept so a restart resumes it (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDelivery {
    /// The subscription it goes to.
    pub subscription: String,
    /// The retained event it carries.
    pub event_id: String,
    /// The failed attempts so far.
    pub failures: u32,
    /// When it is due if nobody holds it: the next attempt's time plus a lease, so a live
    /// process finishes its own retries and a restart resumes the ones a stopped process left.
    pub due: DateTime<Utc>,
}

/// Events retained for replay, and what their delivery needs across a restart (spec 3.5
/// §7.5, §7.8.3, §7.9).
///
/// [`append`](Self::append), [`between`](Self::between) and
/// [`purge_before`](Self::purge_before) are required. The provided methods keep nothing: the
/// edge then holds catalogue pages and consent references in the process, and retries last
/// as long as it does. A durable log overrides them;
/// [`edge_log_conformance_tests!`](crate::edge_log_conformance_tests) checks every method.
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Retains `event`.
    async fn append(&self, event: RetainedEvent) -> StoreResult<()>;

    /// The events at or after `since` and before `until`, oldest first.
    async fn between(
        &self,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> StoreResult<Vec<RetainedEvent>>;

    /// Drops the events, and the catalogue pages, kept before `cutoff`; returns how many
    /// events.
    async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize>;

    /// The retained event `id`.
    ///
    /// The provided method scans [`between`](Self::between); a durable log looks it up.
    async fn event(&self, id: &str) -> StoreResult<Option<RetainedEvent>> {
        Ok(self
            .between(DateTime::<Utc>::MIN_UTC, None)
            .await?
            .into_iter()
            .find(|retained| retained.event.id == id))
    }

    /// Keeps the further pages of release `seq`'s `catalog.changed`, page 2 first, stored at
    /// `at`; returns whether the log keeps them. The provided method keeps none.
    async fn put_catalog_pages(
        &self,
        seq: u64,
        at: DateTime<Utc>,
        pages: &[CatalogChangesPage],
    ) -> StoreResult<bool> {
        let _ = (seq, at, pages);
        Ok(false)
    }

    /// Page `page` (from 2) of release `seq`'s changed items.
    async fn catalog_page(&self, seq: u64, page: usize) -> StoreResult<Option<CatalogChangesPage>> {
        let _ = (seq, page);
        Ok(None)
    }

    /// Notes that `requester` referenced consent receipt `receipt`, so `consent.revoked`
    /// reaches it (AWP 2.0 §9.2); returns whether the log keeps the note. The provided
    /// method keeps none.
    async fn note_consent_reference(
        &self,
        receipt: uuid::Uuid,
        requester: &RequesterId,
    ) -> StoreResult<bool> {
        let _ = (receipt, requester);
        Ok(false)
    }

    /// The requesters that referenced consent receipt `receipt`.
    async fn consent_references(&self, receipt: uuid::Uuid) -> StoreResult<BTreeSet<RequesterId>> {
        let _ = receipt;
        Ok(BTreeSet::new())
    }

    /// Keeps `delivery`, replacing the one for the same subscription and event. The provided
    /// method keeps none.
    async fn save_delivery(&self, delivery: &PendingDelivery) -> StoreResult<()> {
        let _ = delivery;
        Ok(())
    }

    /// Takes the deliveries due at `now`, atomically: each is returned to one caller with its
    /// `due` moved to `now + lease`, so no other caller takes it while this one delivers.
    async fn claim_deliveries(
        &self,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> StoreResult<Vec<PendingDelivery>> {
        let _ = (now, lease);
        Ok(Vec::new())
    }

    /// Forgets the delivery of `event_id` to `subscription`: it succeeded or ended.
    async fn finish_delivery(&self, subscription: &str, event_id: &str) -> StoreResult<()> {
        let _ = (subscription, event_id);
        Ok(())
    }
}

/// An in-memory [`EventLog`]: what it keeps lasts the process.
#[derive(Debug, Clone, Default)]
pub struct InMemoryEventLog {
    events: Arc<Mutex<Vec<RetainedEvent>>>,
    pages: Arc<Mutex<CatalogPages>>,
    references: Arc<Mutex<HashMap<uuid::Uuid, BTreeSet<RequesterId>>>>,
    deliveries: Arc<Mutex<BTreeMap<(String, String), PendingDelivery>>>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl InMemoryEventLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl EventLog for InMemoryEventLog {
    async fn append(&self, event: RetainedEvent) -> StoreResult<()> {
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event);
        Ok(())
    }

    async fn between(
        &self,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> StoreResult<Vec<RetainedEvent>> {
        let mut found: Vec<RetainedEvent> = locked(&self.events)
            .iter()
            .filter(|retained| {
                retained.event.timestamp >= since
                    && until.is_none_or(|until| retained.event.timestamp < until)
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            (a.event.timestamp, &a.event.id).cmp(&(b.event.timestamp, &b.event.id))
        });
        Ok(found)
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
        locked(&self.pages).retain(|_, (at, _)| *at >= cutoff);
        let mut events = locked(&self.events);
        let before = events.len();
        events.retain(|retained| retained.event.timestamp >= cutoff);
        Ok(before - events.len())
    }

    async fn event(&self, id: &str) -> StoreResult<Option<RetainedEvent>> {
        Ok(locked(&self.events).iter().find(|retained| retained.event.id == id).cloned())
    }

    async fn put_catalog_pages(
        &self,
        seq: u64,
        at: DateTime<Utc>,
        pages: &[CatalogChangesPage],
    ) -> StoreResult<bool> {
        let mut stored = locked(&self.pages);
        for (index, page) in pages.iter().enumerate() {
            stored.insert((seq, index + 2), (at, page.clone()));
        }
        Ok(true)
    }

    async fn catalog_page(&self, seq: u64, page: usize) -> StoreResult<Option<CatalogChangesPage>> {
        Ok(locked(&self.pages).get(&(seq, page)).map(|(_, page)| page.clone()))
    }

    async fn note_consent_reference(
        &self,
        receipt: uuid::Uuid,
        requester: &RequesterId,
    ) -> StoreResult<bool> {
        locked(&self.references).entry(receipt).or_default().insert(requester.clone());
        Ok(true)
    }

    async fn consent_references(&self, receipt: uuid::Uuid) -> StoreResult<BTreeSet<RequesterId>> {
        Ok(locked(&self.references).get(&receipt).cloned().unwrap_or_default())
    }

    async fn save_delivery(&self, delivery: &PendingDelivery) -> StoreResult<()> {
        locked(&self.deliveries)
            .insert((delivery.subscription.clone(), delivery.event_id.clone()), delivery.clone());
        Ok(())
    }

    async fn claim_deliveries(
        &self,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> StoreResult<Vec<PendingDelivery>> {
        let mut claimed = Vec::new();
        for delivery in locked(&self.deliveries).values_mut().filter(|d| d.due <= now) {
            delivery.due = now + lease;
            claimed.push(delivery.clone());
        }
        Ok(claimed)
    }

    async fn finish_delivery(&self, subscription: &str, event_id: &str) -> StoreResult<()> {
        locked(&self.deliveries).remove(&(subscription.to_owned(), event_id.to_owned()));
        Ok(())
    }
}

/// Who referenced which consent receipt, so `consent.revoked` reaches them (AWP 2.0 §9.2):
/// in the event log when it keeps them, else in the process.
#[derive(Clone)]
pub(crate) struct ConsentReferences {
    log: Arc<dyn EventLog>,
    by_receipt: Arc<Mutex<HashMap<uuid::Uuid, BTreeSet<RequesterId>>>>,
}

impl ConsentReferences {
    pub(crate) async fn note(&self, receipts: &[uuid::Uuid], requester: &RequesterId) {
        for receipt in receipts {
            match self.log.note_consent_reference(*receipt, requester).await {
                Ok(true) => {}
                Ok(false) => {
                    locked(&self.by_receipt).entry(*receipt).or_default().insert(requester.clone());
                }
                Err(error) => {
                    tracing::warn!(error = %error, "awp consent reference kept in the process only");
                    locked(&self.by_receipt).entry(*receipt).or_default().insert(requester.clone());
                }
            }
        }
    }

    pub(crate) async fn of(&self, receipt: uuid::Uuid) -> BTreeSet<RequesterId> {
        let mut found = locked(&self.by_receipt).get(&receipt).cloned().unwrap_or_default();
        match self.log.consent_references(receipt).await {
            Ok(kept) => found.extend(kept),
            Err(error) => tracing::warn!(error = %error, "awp consent references not read"),
        }
        found
    }
}

/// The further pages of each release's `catalog.changed`, by release and page, with when
/// they were stored.
type CatalogPages = HashMap<(u64, usize), (DateTime<Utc>, CatalogChangesPage)>;

struct Inner {
    subscriptions: Arc<dyn SubscriptionStore>,
    log: Arc<dyn EventLog>,
    config: EventsConfig,
    client: GuardedClient,
    base_url: String,
    pages: Mutex<CatalogPages>,
    references: Arc<Mutex<HashMap<uuid::Uuid, BTreeSet<RequesterId>>>>,
    last_purge: Mutex<DateTime<Utc>>,
}

/// The edge's event publisher (spec 3.5 §7.5). Cheap to clone.
///
/// The edge publishes `site.status.changed` and `consent.revoked` itself; the site's release
/// pipeline publishes releases and catalogue changes, and its commerce capabilities publish
/// order updates, through this handle ([`AwpEdge::events`](super::AwpEdge::events)).
#[derive(Clone)]
pub struct EdgeEvents {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for EdgeEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeEvents").field("catalog", &self.inner.config.catalog).finish()
    }
}

fn jittered(wait: Duration) -> Duration {
    // ±20 %, from a fresh random value.
    let permille = (uuid::Uuid::new_v4().as_u128() % 401) as u32 + 800;
    wait * permille / 1_000
}

impl EdgeEvents {
    /// The edge's events over `subscriptions` and `log`.
    ///
    /// # Errors
    ///
    /// `awp.events.client_unavailable` when the delivery client cannot be built; a default
    /// client in its place would skip the egress check.
    pub(crate) fn new(
        subscriptions: Arc<dyn SubscriptionStore>,
        log: Arc<dyn EventLog>,
        config: EventsConfig,
        base_url: &str,
    ) -> adk_core::Result<Self> {
        let client = GuardedClient::builder(config.egress.clone())
            .timeout(config.attempt_timeout)
            .redirects(0)
            .user_agent(concat!("adk-awp-edge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                adk_core::AdkError::new(
                    adk_core::ErrorComponent::Server,
                    adk_core::ErrorCategory::Internal,
                    "awp.events.client_unavailable",
                    format!("AWP event delivery is unavailable: {error}"),
                )
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                subscriptions,
                log,
                config,
                client,
                base_url: base_url.trim_end_matches('/').to_owned(),
                pages: Mutex::new(HashMap::new()),
                references: Arc::default(),
                last_purge: Mutex::new(Utc::now()),
            }),
        })
    }

    /// The consent-reference index, kept beside the events.
    pub(crate) fn references(&self) -> ConsentReferences {
        ConsentReferences {
            log: Arc::clone(&self.inner.log),
            by_receipt: Arc::clone(&self.inner.references),
        }
    }

    /// The catalogue the edge emits.
    pub fn catalog(&self) -> &EventCatalog {
        &self.inner.config.catalog
    }

    /// The retained events published at or after `since` and before `until`, oldest first,
    /// each with its audience (spec 3.5 §7.5): the site's own read of what its edge
    /// announced, for the back office and the owner's views. Subscribers read events through
    /// their subscriptions and replay; the edge serves no route for this read.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let day = chrono::Utc::now() - chrono::Duration::days(1);
    /// for retained in edge.events().retained(day, None).await? {
    ///     println!("{} {}", retained.event.event_type, retained.event.id);
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the event log cannot be read.
    pub async fn retained(
        &self,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> StoreResult<Vec<RetainedEvent>> {
        self.inner.log.between(since, until).await
    }

    /// Publishes `site.release.published`; returns the event id.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the event cannot be retained or the subscriptions read.
    pub async fn release_published(&self, release: ReleasePublished) -> StoreResult<String> {
        self.emit(EventType::SiteReleasePublished, json_of(&release), None).await
    }

    /// Publishes one `catalog.changed` for a release's changed items: the first 1,000 ids, the
    /// total, and a `next` URL, `/awp/v2/events/changes/{seq}/2`, for the rest (spec 3.5
    /// §7.8.3). Returns the event id.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the event cannot be retained or the subscriptions read.
    pub async fn catalog_changed(
        &self,
        release_seq: u64,
        items: Vec<ContentId>,
    ) -> StoreResult<String> {
        let chunks: Vec<&[ContentId]> = items.chunks(CatalogChanged::PAGE_SIZE).collect();
        let url = |page: usize| (page <= chunks.len()).then(|| self.changes_url(release_seq, page));
        let catalog = CatalogChanged {
            release_seq,
            items: chunks.first().map(|chunk| chunk.to_vec()).unwrap_or_default(),
            total: items.len() as u64,
            next: url(2),
        };
        let pages = chunks
            .iter()
            .enumerate()
            .skip(1)
            .map(|(index, chunk)| CatalogChangesPage {
                items: chunk.to_vec(),
                next: url(index + 2),
            })
            .collect();
        self.announce_catalog(catalog, pages).await
    }

    /// Publishes a release's `catalog.changed` as the site pipeline built it, and serves its
    /// further pages, page 2 onwards, at `/awp/v2/events/changes/{seq}/{page}` (spec 3.5
    /// §7.8.3): the edge half of `adk-site`'s `Announcement`. Returns the event id.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the event cannot be retained or the subscriptions read.
    pub async fn announce_catalog(
        &self,
        catalog: CatalogChanged,
        pages: Vec<CatalogChangesPage>,
    ) -> StoreResult<String> {
        let now = Utc::now();
        let kept = match self.inner.log.put_catalog_pages(catalog.release_seq, now, &pages).await {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(error = %error, "awp catalogue pages kept in the process only");
                false
            }
        };
        if !kept {
            let mut stored = locked(&self.inner.pages);
            for (index, page) in pages.into_iter().enumerate() {
                stored.insert((catalog.release_seq, index + 2), (now, page));
            }
        }
        self.emit(EventType::CatalogChanged, json_of(&catalog), None).await
    }

    /// The URL of page `page` of release `seq`'s changed items.
    pub fn changes_url(&self, seq: u64, page: usize) -> String {
        format!("{}{EVENTS_PATH}/changes/{seq}/{page}", self.inner.base_url)
    }

    /// Publishes `order.updated` to the subscriptions of `requester`, the requester that
    /// placed the order. Returns the event id.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the event cannot be retained or the subscriptions read.
    pub async fn order_updated(
        &self,
        requester: &RequesterId,
        update: OrderUpdated,
    ) -> StoreResult<String> {
        let audience = BTreeSet::from([requester.clone()]);
        self.emit(EventType::OrderUpdated, json_of(&update), Some(audience)).await
    }

    pub(crate) async fn status_changed(&self, change: StatusChanged) -> StoreResult<String> {
        self.emit(EventType::SiteStatusChanged, json_of(&change), None).await
    }

    pub(crate) async fn consent_revoked(
        &self,
        revoked: awp_types::ConsentRevoked,
        audience: BTreeSet<RequesterId>,
    ) -> StoreResult<String> {
        self.emit(EventType::ConsentRevoked, json_of(&revoked), Some(audience)).await
    }

    /// Publishes an event of `event_type` with `data` to every subscription naming it, or only
    /// those of `audience`; returns the event id.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the event cannot be retained or the subscriptions read.
    pub async fn emit(
        &self,
        event_type: EventType,
        data: Value,
        audience: Option<BTreeSet<RequesterId>>,
    ) -> StoreResult<String> {
        self.emit_with_id(new_event_id(), event_type, data, audience).await
    }

    async fn emit_with_id(
        &self,
        id: String,
        event_type: EventType,
        data: Value,
        audience: Option<BTreeSet<RequesterId>>,
    ) -> StoreResult<String> {
        self.purge_if_due().await;
        let event = EventEnvelope::new(id.clone(), event_type.clone(), Utc::now(), data);
        let retained = RetainedEvent { event, audience };
        self.inner.log.append(retained.clone()).await?;
        let targets = self.inner.subscriptions.active_for(&event_type).await?;
        for record in targets {
            if admits(&retained, &record.owner) {
                self.spawn_delivery(record, retained.event.clone(), 0);
            }
        }
        tracing::debug!(event.id = %id, event.kind = %event_type, "awp event published");
        Ok(id)
    }

    async fn purge_if_due(&self) {
        let now = Utc::now();
        {
            let mut last =
                self.inner.last_purge.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if now - *last < chrono::Duration::minutes(10) {
                return;
            }
            *last = now;
        }
        let cutoff = now - self.inner.config.retention;
        if let Err(error) = self.inner.log.purge_before(cutoff).await {
            tracing::warn!(error = %error, "awp event log purge failed");
        }
        self.inner
            .pages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, (at, _)| *at >= cutoff);
    }

    /// One POST of `event` to `subscription`; `Err` with the reason on failure.
    async fn post(
        &self,
        subscription: &EventSubscription,
        event: &EventEnvelope,
    ) -> Result<(), String> {
        let request = self.inner.client.post(&subscription.url).map_err(|e| e.to_string())?;
        let secret = subscription
            .secret
            .as_deref()
            .ok_or_else(|| "the subscription has no signing secret".to_owned())
            .and_then(|secret| {
                WebhookSecret::from_base64(secret).map_err(|error| error.to_string())
            })?;
        let body = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        if body.len() > MAX_EVENT_BODY {
            return Err("the event is over 64 KiB".to_owned());
        }
        let signed = WebhookSigner::new(secret).sign_now(&event.id, &body);
        let mut request = request
            .header(header::CONTENT_TYPE, "application/json")
            .header("awp-version", "2.0")
            .body(body);
        for (name, value) in signed.header_pairs() {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => Err(format!("the endpoint answered {}", response.status().as_u16())),
            Err(error) => Err(error.without_url().to_string()),
        }
    }

    fn spawn_delivery(&self, record: SubscriptionRecord, event: EventEnvelope, failures: u32) {
        let events = self.clone();
        tokio::spawn(async move { events.deliver(record, event, failures).await });
    }

    /// How long a delivery is held by the process attempting it before another may take it.
    fn lease(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.inner.config.delivery_lease)
            .unwrap_or_else(|_| chrono::Duration::seconds(70))
    }

    /// Keeps the delivery's state in the log, so a restart resumes it.
    async fn keep(&self, subscription: &str, event: &str, failures: u32, due: DateTime<Utc>) {
        let delivery = PendingDelivery {
            subscription: subscription.to_owned(),
            event_id: event.to_owned(),
            failures,
            due,
        };
        if let Err(error) = self.inner.log.save_delivery(&delivery).await {
            tracing::warn!(error = %error, "awp delivery kept in the process only");
        }
    }

    async fn finish(&self, subscription: &str, event: &str) {
        if let Err(error) = self.inner.log.finish_delivery(subscription, event).await {
            tracing::warn!(error = %error, "awp finished delivery not cleared");
        }
    }

    /// Delivers `event` with the retry schedule, after `failures` failed attempts; suspends
    /// the subscription after six consecutive failures and makes one final
    /// `subscription.suspended` attempt.
    async fn deliver(&self, record: SubscriptionRecord, event: EventEnvelope, failures: u32) {
        let id = record.subscription.id.clone();
        let mut failures = failures;
        let mut subscription = record.subscription;
        self.keep(&id, &event.id, failures, Utc::now() + self.lease()).await;
        loop {
            match self.post(&subscription, &event).await {
                Ok(()) => {
                    self.finish(&id, &event.id).await;
                    if let Err(error) = self.inner.subscriptions.record_delivery(&id, true).await {
                        tracing::warn!(error = %error, "awp delivery outcome not recorded");
                    }
                    return;
                }
                Err(reason) => {
                    tracing::debug!(subscription.id = %id, event.id = %event.id, reason = %reason, "awp delivery failed");
                    let after = match self.inner.subscriptions.record_delivery(&id, false).await {
                        Ok(after) => after,
                        Err(error) => {
                            tracing::warn!(error = %error, "awp delivery outcome not recorded");
                            return;
                        }
                    };
                    if after.subscription.state == SubscriptionState::Suspended {
                        tracing::warn!(subscription.id = %id, "awp subscription suspended after six failed deliveries");
                        let suspended = SubscriptionSuspended {
                            subscription: id.clone(),
                            failures: after.subscription.failures,
                            last_error: Some(reason.chars().take(200).collect()),
                        };
                        let notice = EventEnvelope::new(
                            new_event_id(),
                            EventType::SubscriptionSuspended,
                            Utc::now(),
                            json_of(&suspended),
                        );
                        if let Err(reason) = self.post(&after.subscription, &notice).await {
                            tracing::debug!(reason = %reason, "awp final suspension notice not delivered");
                        }
                        self.finish(&id, &event.id).await;
                        return;
                    }
                    subscription = after.subscription;
                    let schedule = &self.inner.config.retry_schedule;
                    let Some(wait) = schedule.get(failures as usize).copied() else {
                        self.finish(&id, &event.id).await;
                        return;
                    };
                    failures += 1;
                    let wait = jittered(wait);
                    let wake = Utc::now()
                        + chrono::Duration::from_std(wait)
                            .unwrap_or_else(|_| chrono::Duration::zero());
                    self.keep(&id, &event.id, failures, wake + self.lease()).await;
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Redelivers the retained events `owner`'s subscription `id` may receive, between
    /// `since` and `until`, of `types` (default: the subscription's); returns how many.
    async fn replay(
        &self,
        owner: &RequesterId,
        id: &str,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
        types: Option<Vec<EventType>>,
    ) -> StoreResult<usize> {
        let record = self
            .inner
            .subscriptions
            .get(owner, id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("subscription {id}")))?;
        let wanted = types.unwrap_or_else(|| record.subscription.types.clone());
        let events: Vec<RetainedEvent> = self
            .inner
            .log
            .between(since.max(Utc::now() - self.inner.config.retention), until)
            .await?
            .into_iter()
            .filter(|retained| {
                record.subscription.types.contains(&retained.event.event_type)
                    && wanted.contains(&retained.event.event_type)
                    && admits(retained, owner)
            })
            .collect();
        let accepted = events.len();
        let events_handle = self.clone();
        tokio::spawn(async move {
            for retained in events {
                events_handle.deliver(record.clone(), retained.event, 0).await;
            }
        });
        Ok(accepted)
    }

    async fn page(&self, seq: u64, page: usize) -> StoreResult<Option<CatalogChangesPage>> {
        if let Some((_, found)) = locked(&self.inner.pages).get(&(seq, page)) {
            return Ok(Some(found.clone()));
        }
        self.inner.log.catalog_page(seq, page).await
    }

    /// Resumes the deliveries a stopped process left: each delivery whose lease passed is
    /// taken from the log and attempted again where its retries stood, if its subscription
    /// is still active and its event retained (spec 3.5 §7.5). Returns how many resumed.
    ///
    /// [`AwpEdge::spawn_delivery_resumer`](super::AwpEdge::spawn_delivery_resumer) calls it
    /// on an interval.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the log cannot be read.
    pub async fn resume_deliveries(&self) -> StoreResult<usize> {
        let mut resumed = 0;
        for delivery in self.inner.log.claim_deliveries(Utc::now(), self.lease()).await? {
            let Some(retained) = self.inner.log.event(&delivery.event_id).await? else {
                self.finish(&delivery.subscription, &delivery.event_id).await;
                continue;
            };
            let record = self
                .inner
                .subscriptions
                .active_for(&retained.event.event_type)
                .await?
                .into_iter()
                .find(|record| record.subscription.id == delivery.subscription)
                .filter(|record| admits(&retained, &record.owner));
            let Some(record) = record else {
                self.finish(&delivery.subscription, &delivery.event_id).await;
                continue;
            };
            tracing::info!(
                subscription.id = %delivery.subscription,
                event.id = %delivery.event_id,
                failures = delivery.failures,
                "awp delivery resumed after a restart"
            );
            self.spawn_delivery(record, retained.event, delivery.failures);
            resumed += 1;
        }
        Ok(resumed)
    }
}

fn admits(retained: &RetainedEvent, owner: &RequesterId) -> bool {
    retained.audience.as_ref().is_none_or(|audience| audience.contains(owner))
}

fn json_of<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn new_event_id() -> String {
    format!("evt_{}", uuid::Uuid::new_v4().simple())
}

// --- routes --------------------------------------------------------------------------------

fn refuse(code: ErrorCode, message: &str) -> Response {
    super::envelope(ErrorBody::new(code, message), None)
}

/// The subscriber a subscription route serves: `known` or above through Web Bot Auth or OAuth.
fn subscriber(caller: &EdgeCaller) -> Result<RequesterId, Response> {
    let Some(requester) = caller.requester.clone().filter(|_| caller.tier >= TrustTier::Known)
    else {
        return Err(refuse(
            ErrorCode::Unauthorized,
            "subscriptions need a Web Bot Auth signature or an OAuth token",
        ));
    };
    match requester.method() {
        Some(IdentityMethod::WebBotAuth | IdentityMethod::Oauth2ClientCredentials) => Ok(requester),
        _ => Err(refuse(
            ErrorCode::Forbidden,
            "subscriptions are authenticated by Web Bot Auth or OAuth",
        )),
    }
}

fn caller_of(caller: Option<Extension<EdgeCaller>>) -> EdgeCaller {
    caller.map_or_else(|| EdgeCaller::public("ip:unknown"), |Extension(caller)| caller)
}

fn without_secret(mut subscription: EventSubscription) -> EventSubscription {
    subscription.secret = None;
    subscription
}

impl EdgeEvents {
    fn check_types(&self, types: &[EventType]) -> Result<(), Response> {
        if types.is_empty() {
            return Err(refuse(ErrorCode::InvalidRequest, "name at least one event type"));
        }
        match types.iter().find(|kind| !self.inner.config.catalog.contains(kind)) {
            Some(kind) => Err(refuse(
                ErrorCode::InvalidRequest,
                &format!("`{kind}` is not in the site's event catalogue"),
            )),
            None => Ok(()),
        }
    }

    fn check_url(&self, url: &str) -> Result<(), Response> {
        self.inner
            .config
            .egress
            .check_url(url)
            .map(|_| ())
            .map_err(|denial| refuse(ErrorCode::InvalidRequest, &denial.to_string()))
    }

    pub(crate) fn router(&self) -> Router {
        Router::new()
            .route(EVENTS_PATH, post(create))
            .route(&format!("{EVENTS_PATH}/replay"), post(replay))
            .route(&format!("{EVENTS_PATH}/changes/{{seq}}/{{page}}"), get(changes))
            .route(&format!("{EVENTS_PATH}/{{id}}"), get(read).patch(update).delete(remove))
            .with_state(self.clone())
    }
}

#[derive(Deserialize)]
struct NewSubscription {
    types: Vec<EventType>,
    url: String,
}

fn store_failed(error: &StoreError) -> Response {
    match error {
        StoreError::NotFound(_) => refuse(ErrorCode::ResourceNotFound, "no such subscription"),
        StoreError::Conflict(message) => refuse(ErrorCode::InvalidRequest, message),
        _ => {
            tracing::error!(error = %error, "awp subscription store failed");
            refuse(ErrorCode::InternalError, "the subscription could not be stored")
        }
    }
}

async fn create(
    State(events): State<EdgeEvents>,
    caller: Option<Extension<EdgeCaller>>,
    Json(body): Json<NewSubscription>,
) -> Response {
    let owner = match subscriber(&caller_of(caller)) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    if let Err(response) =
        events.check_types(&body.types).and_then(|()| events.check_url(&body.url))
    {
        return response;
    }
    let mut key = [0_u8; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let secret = match WebhookSecret::from_bytes(key.to_vec()) {
        Ok(secret) => secret.to_whsec(),
        Err(error) => {
            tracing::error!(error = %error, "awp subscription secret not generated");
            return refuse(ErrorCode::InternalError, "the subscription could not be created");
        }
    };
    let subscription = EventSubscription {
        id: format!("sub_{}", uuid::Uuid::new_v4().simple()),
        types: body.types,
        url: body.url,
        state: SubscriptionState::Active,
        created_at: Utc::now(),
        failures: 0,
        secret: Some(secret),
    };
    let record = SubscriptionRecord { owner, subscription: subscription.clone() };
    if let Err(error) = events.inner.subscriptions.create(record).await {
        return store_failed(&error);
    }
    let mut response = awp_json(StatusCode::CREATED, &subscription);
    if let Ok(location) = HeaderValue::from_str(&format!("{EVENTS_PATH}/{}", subscription.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response
}

async fn read(
    State(events): State<EdgeEvents>,
    caller: Option<Extension<EdgeCaller>>,
    Path(id): Path<String>,
) -> Response {
    let owner = match subscriber(&caller_of(caller)) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    match events.inner.subscriptions.get(&owner, &id).await {
        Ok(Some(record)) => awp_json(StatusCode::OK, &without_secret(record.subscription)),
        Ok(None) => refuse(ErrorCode::ResourceNotFound, "no such subscription"),
        Err(error) => store_failed(&error),
    }
}

async fn update(
    State(events): State<EdgeEvents>,
    caller: Option<Extension<EdgeCaller>>,
    Path(id): Path<String>,
    Json(change): Json<SubscriptionUpdate>,
) -> Response {
    let owner = match subscriber(&caller_of(caller)) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    if let Some(types) = &change.types
        && let Err(response) = events.check_types(types)
    {
        return response;
    }
    if let Some(url) = &change.url
        && let Err(response) = events.check_url(url)
    {
        return response;
    }
    match events.inner.subscriptions.update(&owner, &id, change).await {
        Ok(record) => awp_json(StatusCode::OK, &without_secret(record.subscription)),
        Err(error) => store_failed(&error),
    }
}

async fn remove(
    State(events): State<EdgeEvents>,
    caller: Option<Extension<EdgeCaller>>,
    Path(id): Path<String>,
) -> Response {
    let owner = match subscriber(&caller_of(caller)) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    match events.inner.subscriptions.delete(&owner, &id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => refuse(ErrorCode::ResourceNotFound, "no such subscription"),
        Err(error) => store_failed(&error),
    }
}

#[derive(Deserialize)]
struct ReplayRequest {
    subscription: String,
    since: DateTime<Utc>,
    #[serde(default)]
    until: Option<DateTime<Utc>>,
    #[serde(default)]
    types: Option<Vec<EventType>>,
}

async fn replay(
    State(events): State<EdgeEvents>,
    caller: Option<Extension<EdgeCaller>>,
    Json(body): Json<ReplayRequest>,
) -> Response {
    let owner = match subscriber(&caller_of(caller)) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    match events.replay(&owner, &body.subscription, body.since, body.until, body.types).await {
        Ok(accepted) => awp_json(StatusCode::ACCEPTED, &json!({ "accepted": accepted })),
        Err(error) => store_failed(&error),
    }
}

async fn changes(
    State(events): State<EdgeEvents>,
    caller: Option<Extension<EdgeCaller>>,
    Path((seq, page)): Path<(u64, usize)>,
) -> Response {
    let owner = match subscriber(&caller_of(caller)) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    // Only a subscriber to catalogue changes reads their pages.
    match events.inner.subscriptions.list(&owner).await {
        Ok(records)
            if records
                .iter()
                .any(|record| record.subscription.types.contains(&EventType::CatalogChanged)) => {}
        Ok(_) => return refuse(ErrorCode::ResourceNotFound, "no such page"),
        Err(error) => return store_failed(&error),
    }
    match events.page(seq, page).await {
        Ok(Some(page)) => awp_json(StatusCode::OK, &page),
        Ok(None) => refuse(ErrorCode::ResourceNotFound, "no such page"),
        Err(error) => store_failed(&error),
    }
}
