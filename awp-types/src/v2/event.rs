//! Events: the Standard Webhooks envelope, the event catalogue and the payloads of the event
//! types that cross the wire (spec 3.5 §7.2, §7.5, §7.8.3, §9.3).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::consent::ConsentScope;
use super::content::ContentId;
use super::release::Digest;
use super::status::{SiteState, SiteStatus};

open_enum! {
    /// An event type name (spec 3.5 §7.2, §7.5, §9.3).
    ///
    /// # Example
    ///
    /// ```
    /// use awp_types::EventType;
    ///
    /// assert_eq!(EventType::CatalogChanged.as_str(), "catalog.changed");
    /// assert_eq!(EventType::from("site.release.published"), EventType::SiteReleasePublished);
    /// assert_eq!(EventType::from("booking.reminder").as_str(), "booking.reminder");
    /// ```
    pub enum EventType {
        /// A signed release went live; data is [`ReleasePublished`].
        SiteReleasePublished => "site.release.published",
        /// The site status changed; data is [`StatusChanged`].
        SiteStatusChanged => "site.status.changed",
        /// Catalogue items changed in one batch; data is [`CatalogChanged`].
        CatalogChanged => "catalog.changed",
        /// An order changed state; data is [`OrderUpdated`].
        OrderUpdated => "order.updated",
        /// A consent receipt was revoked; data is [`ConsentRevoked`].
        ConsentRevoked => "consent.revoked",
        /// The subscription was suspended after repeated delivery failures; data is
        /// [`SubscriptionSuspended`].
        SubscriptionSuspended => "subscription.suspended",
        @other
        /// An event type this version does not name.
        Other(String),
    }
}

/// The event types a site emits, listed in the contract (spec 3.5 §7.2, §7.5).
///
/// # Example
///
/// ```
/// use awp_types::{EventCatalog, EventType};
///
/// let catalog = EventCatalog::from(vec![EventType::SiteReleasePublished, EventType::CatalogChanged]);
/// assert!(catalog.contains(&EventType::CatalogChanged));
/// assert_eq!(serde_json::to_string(&catalog).unwrap(), r#"["site.release.published","catalog.changed"]"#);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct EventCatalog(pub Vec<EventType>);

impl EventCatalog {
    /// Returns `true` when the catalogue lists `event_type`.
    pub fn contains(&self, event_type: &EventType) -> bool {
        self.0.contains(event_type)
    }
}

impl From<Vec<EventType>> for EventCatalog {
    fn from(types: Vec<EventType>) -> Self {
        Self(types)
    }
}

/// A Standard Webhooks-compatible event (spec 3.5 §7.5).
///
/// `id` is the event id subscribers de-duplicate on; it is also sent as `webhook-id`.
///
/// # Example
///
/// ```
/// use awp_types::{CatalogChanged, EventEnvelope, EventType};
/// use chrono::Utc;
///
/// let data = CatalogChanged { release_seq: 7, items: vec!["sku-1".into()], total: 1, next: None };
/// let event = EventEnvelope::new("evt_1", EventType::CatalogChanged, Utc::now(), data);
/// let json = serde_json::to_value(&event).unwrap();
/// assert_eq!(json["type"], "catalog.changed");
/// assert_eq!(json["data"]["releaseSeq"], 7);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope<T = Value> {
    /// Event id, unique per site.
    pub id: String,
    /// Event type.
    #[serde(rename = "type")]
    pub event_type: EventType,
    /// When the event happened.
    pub timestamp: DateTime<Utc>,
    /// Event data; its shape depends on `type`.
    pub data: T,
}

impl<T> EventEnvelope<T> {
    /// Creates an event.
    pub fn new(
        id: impl Into<String>,
        event_type: EventType,
        timestamp: DateTime<Utc>,
        data: T,
    ) -> Self {
        Self { id: id.into(), event_type, timestamp, data }
    }
}

/// Data of `site.release.published` (spec 3.5 §7.5, §7.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReleasePublished {
    /// Sequence number of the release.
    pub seq: u64,
    /// URL of the release manifest.
    pub manifest: String,
    /// Digest of the release manifest.
    pub digest: Digest,
    /// For a delta release, the release it lists changes against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<u64>,
}

/// Data of `site.status.changed` (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StatusChanged {
    /// The state before the transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<SiteState>,
    /// The new status document.
    pub status: SiteStatus,
}

/// Data of `catalog.changed`: one event per batch with the changed item ids, paginated
/// beyond 1,000 (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CatalogChanged {
    /// The release that published the batch.
    pub release_seq: u64,
    /// The first page of changed item ids, at most 1,000.
    pub items: Vec<ContentId>,
    /// How many items the batch changed.
    pub total: u64,
    /// URL of the next page of item ids, when `total` exceeds the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

impl CatalogChanged {
    /// Most item ids one page carries.
    pub const PAGE_SIZE: usize = 1_000;

    /// Returns `true` when more item ids follow at `next`.
    pub fn has_more(&self) -> bool {
        (self.items.len() as u64) < self.total
    }
}

/// Data of `order.updated` (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OrderUpdated {
    /// Order id.
    pub order_id: String,
    /// The order's new state, as a snake_case code.
    pub state: String,
    /// Id of the signed receipt issued for the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Uuid>,
}

/// Data of `consent.revoked` (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConsentRevoked {
    /// Id of the revoked consent receipt.
    pub receipt: Uuid,
    /// The revoked scope.
    pub scope: ConsentScope,
    /// When the revocation took effect.
    pub revoked_at: DateTime<Utc>,
}

/// Data of `subscription.suspended`, the final event to a subscription whose deliveries
/// failed six times in a row (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionSuspended {
    /// Subscription id.
    pub subscription: String,
    /// Consecutive failed deliveries.
    pub failures: u32,
    /// The last delivery error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl SubscriptionSuspended {
    /// Consecutive failed deliveries after which a subscription is suspended.
    pub const MAX_FAILURES: u32 = 6;
}

/// The page of further item ids a `catalog.changed` event's `next` names (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CatalogChangesPage {
    /// Changed item ids, at most 1,000.
    pub items: Vec<ContentId>,
    /// URL of the next page, when more ids follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// An event subscription, created by `POST /awp/v2/events` and owned by the requester that
/// created it (spec 3.5 §7.5, §7.9).
///
/// # Example
///
/// ```
/// use awp_types::{EventSubscription, EventType, SubscriptionState};
/// use chrono::Utc;
///
/// let subscription = EventSubscription {
///     id: "sub_1".into(),
///     types: vec![EventType::CatalogChanged],
///     url: "https://agents.buyer.example/hooks/awp".into(),
///     state: SubscriptionState::Active,
///     created_at: Utc::now(),
///     failures: 0,
///     secret: None,
/// };
/// let json = serde_json::to_value(&subscription).unwrap();
/// assert!(json.get("secret").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct EventSubscription {
    /// Subscription id.
    pub id: String,
    /// The event types delivered, from the site's catalogue.
    pub types: Vec<EventType>,
    /// The `https` URL deliveries are posted to.
    pub url: String,
    /// Whether deliveries are made.
    pub state: SubscriptionState,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// Consecutive failed delivery attempts.
    #[serde(default)]
    pub failures: u32,
    /// The Standard Webhooks signing secret, `whsec_` and base64; present only in the
    /// response that creates the subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// Whether a subscription receives deliveries (spec 3.5 §7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SubscriptionState {
    /// Deliveries are made.
    Active,
    /// Suspended after six consecutive failed deliveries.
    Suspended,
}
