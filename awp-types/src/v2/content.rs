//! The content model (spec 3.5 §7.8.2) and batching and feed bounds for constantly updated
//! sites (spec 3.5 §7.8.3).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::charter::PriceRule;
use super::identity::Proof;
use super::release::Digest;

/// A BCP 47 language tag, such as `en`, `sw` or `pt-BR` (spec 3.5 §7.8.2).
///
/// The tag is checked for form — subtags of one to eight ASCII letters or digits joined by
/// `-`, the first all letters — and kept as written.
///
/// # Example
///
/// ```
/// use awp_types::LanguageTag;
///
/// let tag: LanguageTag = "pt-BR".parse().unwrap();
/// assert_eq!(tag.as_str(), "pt-BR");
/// assert!("".parse::<LanguageTag>().is_err());
/// assert!("en_US".parse::<LanguageTag>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Parses a language tag.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageTagError`] when the tag is not of BCP 47 form.
    pub fn parse(tag: &str) -> Result<Self, LanguageTagError> {
        let mut subtags = tag.split('-');
        let primary = subtags.next().unwrap_or_default();
        let primary_ok =
            (1..=8).contains(&primary.len()) && primary.bytes().all(|b| b.is_ascii_alphabetic());
        let rest_ok = subtags.all(|subtag| {
            (1..=8).contains(&subtag.len()) && subtag.bytes().all(|b| b.is_ascii_alphanumeric())
        });
        if primary_ok && rest_ok {
            Ok(Self(tag.to_owned()))
        } else {
            Err(LanguageTagError(tag.to_owned()))
        }
    }

    /// Returns the tag.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for LanguageTag {
    type Err = LanguageTagError;

    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        Self::parse(tag)
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for LanguageTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for LanguageTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tag = String::deserialize(deserializer)?;
        Self::parse(&tag).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for LanguageTag {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LanguageTag".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[A-Za-z]{1,8}(-[A-Za-z0-9]{1,8})*$",
            "description": "A BCP 47 language tag."
        })
    }
}

/// A string that is not a BCP 47 language tag (spec 3.5 §7.8.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a BCP 47 language tag such as `en` or `pt-BR`")]
pub struct LanguageTagError(pub String);

/// A content item's id in the site's content repository (spec 3.5 §7.8.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ContentId(pub String);

impl From<&str> for ContentId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Markdown text (spec 3.5 §7.8.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Markdown(pub String);

/// A reference to a confirmed fact a claim rests on (spec 3.5 §7.8.2, A-I18).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct FactRef(pub String);

/// A content kind (spec 3.5 §7.8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentKind {
    /// A standing page.
    Page,
    /// A blog post.
    Post,
    /// A news article.
    Article,
    /// A product.
    Product,
    /// An event.
    Event,
    /// A notice, such as an official notice.
    Notice,
    /// A service.
    Service,
    /// A person.
    Person,
    /// A collection of other items.
    Collection,
}

/// Where a content item stands (spec 3.5 §7.8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentState {
    /// Being written.
    Draft,
    /// Waiting for its approval chain.
    InReview,
    /// Approved, not yet released.
    Approved,
    /// Approved and waiting for its publication time.
    Scheduled,
    /// Live in a signed release.
    Published,
    /// Live with a visible correction note.
    Corrected,
    /// Replaced by a notice or answering 410 Gone.
    Withdrawn,
}

/// A typed content item: a Markdown body with front matter, structured fields and media
/// (spec 3.5 §7.8.2).
///
/// # Example
///
/// ```
/// use std::collections::BTreeMap;
///
/// use awp_types::{ContentItem, ContentKind, ContentState};
///
/// let item = ContentItem {
///     id: "panel-400".into(),
///     kind: ContentKind::Product,
///     slug: "panel-400".into(),
///     language: "en".parse().unwrap(),
///     fields: BTreeMap::from([("price".to_owned(), 25_000.into()), ("stock".to_owned(), 12.into())]),
///     volatile: vec!["stock".into()],
///     body: None,
///     media: vec![],
///     sources: vec![],
///     schedule: None,
///     state: ContentState::Draft,
/// };
/// assert!(item.is_volatile("stock"));
/// assert!(!item.is_volatile("price"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ContentItem {
    /// Item id.
    pub id: ContentId,
    /// Content kind.
    pub kind: ContentKind,
    /// URL slug.
    pub slug: String,
    /// Language.
    pub language: LanguageTag,
    /// Typed fields per kind: price and SKU, byline, `startsAt`, `validUntil`.
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    /// Fields the edge serves live and that are never released, such as stock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volatile: Vec<String>,
    /// Markdown body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Markdown>,
    /// Media.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaRef>,
    /// The confirmed facts its claims rest on (A-I18).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<FactRef>,
    /// Publication, embargo, expiry and reversal times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// Where the item stands.
    pub state: ContentState,
}

impl ContentItem {
    /// Returns `true` when `field` is served live by the edge and never released.
    pub fn is_volatile(&self, field: &str) -> bool {
        self.volatile.iter().any(|volatile| volatile == field)
    }
}

/// A content item's schedule (spec 3.5 §7.8.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// When to publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_at: Option<DateTime<Utc>>,
    /// Not before this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embargo_until: Option<DateTime<Utc>>,
    /// When to withdraw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// When to revert to the previous version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_at: Option<DateTime<Utc>>,
}

/// A media file beside a content item (spec 3.5 §7.8.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MediaRef {
    /// Path in the content repository.
    pub path: String,
    /// Sniffed media type.
    pub content_type: String,
    /// Digest of the file's bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Digest>,
    /// Alt text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// Whether a person confirmed the alt text; the accessibility check needs it.
    #[serde(default, skip_serializing_if = "super::serde_util::is_false")]
    pub alt_confirmed: bool,
    /// Whether the image was generated; generated images are labelled as illustrations.
    #[serde(default, skip_serializing_if = "super::serde_util::is_false")]
    pub generated: bool,
}

/// How updates coalesce into releases (spec 3.5 §7.8.3).
///
/// Changes arriving within `window` coalesce into one release; a `flushNow` change publishes
/// immediately with whatever is pending. `window` is serialized in seconds.
///
/// # Example
///
/// ```
/// use std::time::Duration;
///
/// use awp_types::BatchPolicy;
///
/// let policy = BatchPolicy::default();
/// assert_eq!(policy.window, Duration::from_secs(30));
/// assert_eq!(policy.max_items, 10_000);
/// assert!(BatchPolicy::WINDOW_RANGE.contains(&policy.window));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BatchPolicy {
    /// Coalescing window, in seconds on the wire.
    #[serde(with = "super::serde_util::duration_secs")]
    #[cfg_attr(feature = "json-schema", schemars(with = "u64"))]
    pub window: Duration,
    /// Most changed items per release.
    pub max_items: usize,
    /// Most changed bytes per release.
    pub max_bytes: u64,
    /// Changes that publish immediately.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flush_now: Vec<Trigger>,
}

impl BatchPolicy {
    /// The windows the spec allows, five seconds to five minutes.
    pub const WINDOW_RANGE: std::ops::RangeInclusive<Duration> =
        Duration::from_secs(5)..=Duration::from_secs(300);
}

impl Default for BatchPolicy {
    /// A 30 s window, 10,000 items and 256 MiB per release, and every trigger flushing.
    fn default() -> Self {
        Self {
            window: Duration::from_secs(30),
            max_items: 10_000,
            max_bytes: 256 * 1024 * 1024,
            flush_now: vec![
                Trigger::Correction,
                Trigger::Takedown,
                Trigger::EmergencyNotice,
                Trigger::OwnerRequest,
            ],
        }
    }
}

/// A change that publishes immediately (spec 3.5 §7.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Trigger {
    /// A correction.
    Correction,
    /// A takedown or withdrawal.
    Takedown,
    /// An emergency notice.
    EmergencyNotice,
    /// The owner asked for it.
    OwnerRequest,
}

/// Bounds on an automated feed; a batch that breaks them is held whole (spec 3.5 §7.8.3,
/// A-I27).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FeedBounds {
    /// Largest share of a collection's items one batch may change; the spec's default is 0.2.
    pub max_change_fraction: f32,
    /// Price changes the feed may publish without the owner.
    pub price_rule: PriceRule,
    /// Per-field bounds, such as `price > 0` or `stock ≤ 100,000`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub field_bounds: BTreeMap<String, Bound>,
    /// Most records per minute.
    pub max_rate: Rate,
}

impl FeedBounds {
    /// The spec's default largest share of a collection one batch may change.
    pub const DEFAULT_MAX_CHANGE_FRACTION: f32 = 0.2;
}

/// A bound on one feed field (spec 3.5 §7.8.3).
///
/// # Example
///
/// ```
/// use awp_types::Bound;
/// use chrono::{Duration, Utc};
///
/// let positive = Bound::Range { minimum: None, exclusive_minimum: Some(0.0), maximum: None, exclusive_maximum: None };
/// assert!(positive.allows_number(0.01));
/// assert!(!positive.allows_number(0.0));
///
/// let now = Utc::now();
/// let within_a_year = Bound::DateWithin { days: 365 };
/// assert!(within_a_year.allows_date(now + Duration::days(30), now));
/// assert!(!within_a_year.allows_date(now + Duration::days(400), now));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Bound {
    /// A numeric range in JSON Schema's vocabulary.
    Range {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        /// Exclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclusive_minimum: Option<f64>,
        /// Inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
        /// Exclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclusive_maximum: Option<f64>,
    },
    /// A date within this many days of the batch, before or after.
    DateWithin {
        /// Days either side of the batch time.
        days: u32,
    },
}

impl Bound {
    /// Returns `true` when `value` is within a `Range`; a `DateWithin` bound allows no number.
    pub fn allows_number(&self, value: f64) -> bool {
        match self {
            Bound::Range { minimum, exclusive_minimum, maximum, exclusive_maximum } => {
                minimum.is_none_or(|min| value >= min)
                    && exclusive_minimum.is_none_or(|min| value > min)
                    && maximum.is_none_or(|max| value <= max)
                    && exclusive_maximum.is_none_or(|max| value < max)
            }
            Bound::DateWithin { .. } => false,
        }
    }

    /// Returns `true` when `value` is within `days` of `now`; a `Range` bound allows no date.
    pub fn allows_date(&self, value: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        match self {
            Bound::DateWithin { days } => {
                (value - now).abs() <= chrono::Duration::days(i64::from(*days))
            }
            Bound::Range { .. } => false,
        }
    }
}

/// A rate in records per minute (spec 3.5 §7.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Rate {
    /// Records per minute.
    pub per_minute: u32,
}

/// One entry of a stream — a live blog, a ticker, an emergency update — signed by the edge
/// with a delegated `receipt` key and folded into a signed release (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StreamEntry {
    /// The content item the stream belongs to.
    pub stream: ContentId,
    /// The entry's number in the stream, from 1 without gaps.
    pub n: u64,
    /// When the edge published it.
    pub published_at: DateTime<Utc>,
    /// The entry's text.
    pub markdown: Markdown,
    /// The number of an entry it corrects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects: Option<u64>,
    /// The edge's detached JWS over the JCS form of the entry without `proof`.
    pub proof: Proof,
}

impl StreamEntry {
    /// How often an open stream is folded into a signed release, at most.
    pub const FOLD_INTERVAL: Duration = Duration::from_secs(300);
}
