//! Model-free feed updates (spec 3.5 §7.8.2 "Imports and feeds", §7.8.3 "Model-free updates",
//! "Bounds on automated feeds", A-I24, A-I27).
//!
//! A [`ContentSource`] delivers records — supplier and point-of-sale feeds, exports, wire
//! feeds. A [`FeedMapping`] turns each record into item fields through declared types, units
//! and bounds; nothing in this module calls a model. [`FeedMapping::changes`] compares the
//! mapped values with the items the site holds and sorts each changed field into its lane:
//! live values go to the edge's [`LiveStore`](crate::live::LiveStore), released values become
//! item versions for the batcher. [`bounds::check`] then decides whether the delivery may
//! proceed at all: a delivery that breaks its [`FeedBounds`](awp_types::FeedBounds) is held
//! whole (A-I27).
//!
//! | Source | Delivers |
//! |--------|----------|
//! | [`InMemorySource`] | Records pushed by the caller, such as a webhook's deliveries, drained by each pull |
//! | [`CsvFileSource`] | Every row of a CSV export, read again at each pull |

pub mod bounds;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use async_trait::async_trait;
use awp_types::{Bound, ContentId, ContentItem, ContentKind, ContentState, LanguageTag};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::content::SiteConfig;
use crate::error::{Result, SiteError};
use crate::lanes::{Lane, lane_of};

/// One record of a feed delivery: a supplier row, a point-of-sale update, a wire item.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeedRecord(pub BTreeMap<String, Value>);

impl FeedRecord {
    /// A record from `(column, value)` pairs.
    pub fn new(pairs: impl IntoIterator<Item = (impl Into<String>, Value)>) -> Self {
        Self(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
}

/// Delivers feed records (spec 3.5 §7.8.2 "Imports and feeds").
///
/// Each pull is one delivery: one reviewable change set, checked against the feed's bounds
/// as a whole. Implementations run [`content_source_conformance`].
#[async_trait]
pub trait ContentSource: Send + Sync {
    /// The feed's name, as the owner configured it.
    fn name(&self) -> &str;

    /// The next delivery's records; empty when nothing new arrived.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot be read.
    async fn pull(&self) -> Result<Vec<FeedRecord>>;
}

/// A source the caller pushes deliveries into, such as a webhook receiver; each pull drains
/// the oldest delivery.
///
/// # Example
///
/// ```rust
/// use adk_site::feed::{ContentSource, FeedRecord, InMemorySource};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_site::Result<()> {
/// let source = InMemorySource::new("supplier");
/// source.deliver(vec![FeedRecord::new([("sku", "A1".into()), ("price", 10.into())])]).await;
/// assert_eq!(source.pull().await?.len(), 1);
/// assert!(source.pull().await?.is_empty());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct InMemorySource {
    name: String,
    deliveries: Mutex<VecDeque<Vec<FeedRecord>>>,
}

impl InMemorySource {
    /// An empty source named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), deliveries: Mutex::new(VecDeque::new()) }
    }

    /// Queues one delivery.
    pub async fn deliver(&self, records: Vec<FeedRecord>) {
        self.deliveries.lock().await.push_back(records);
    }
}

#[async_trait]
impl ContentSource for InMemorySource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn pull(&self) -> Result<Vec<FeedRecord>> {
        Ok(self.deliveries.lock().await.pop_front().unwrap_or_default())
    }
}

/// A CSV export read whole at each pull: the first row names the columns; values are strings
/// that the mapping's types parse (RFC 4180 quoting).
///
/// # Example
///
/// ```rust
/// use adk_site::feed::{ContentSource, CsvFileSource};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_site::Result<()> {
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("stock.csv");
/// std::fs::write(&path, "sku,price\nA1,\"1,250\"\n").unwrap();
/// let records = CsvFileSource::new("export", &path).pull().await?;
/// assert_eq!(records[0].0["price"], "1,250");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct CsvFileSource {
    name: String,
    path: PathBuf,
}

impl CsvFileSource {
    /// A source reading `path`.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self { name: name.into(), path: path.into() }
    }
}

#[async_trait]
impl ContentSource for CsvFileSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn pull(&self) -> Result<Vec<FeedRecord>> {
        let text = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|e| SiteError::io(&self.path, e))?;
        parse_csv(&text).map_err(|message| SiteError::InvalidContent {
            path: self.path.display().to_string(),
            message,
        })
    }
}

/// Parses CSV text with a header row into records (RFC 4180).
///
/// # Errors
///
/// A message for an unterminated quote or a row longer than the header.
pub fn parse_csv(text: &str) -> std::result::Result<Vec<FeedRecord>, String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                other => field.push(other),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                any = false;
            }
            other => field.push(other),
        }
    }
    if quoted {
        return Err("a quoted field is not closed".into());
    }
    if any {
        row.push(field);
        rows.push(row);
    }
    let mut rows = rows.into_iter().filter(|r| !(r.len() == 1 && r[0].is_empty()));
    let Some(header) = rows.next() else { return Ok(Vec::new()) };
    rows.enumerate()
        .map(|(index, row)| {
            if row.len() > header.len() {
                return Err(format!("row {} has more fields than the header", index + 2));
            }
            Ok(FeedRecord(header.iter().cloned().zip(row.into_iter().map(Value::String)).collect()))
        })
        .collect()
}

/// The type a mapped field must parse as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldType {
    /// A whole number.
    Integer,
    /// A number; strings with thousands separators (`1,250.50`) parse.
    Number,
    /// Text.
    Text,
    /// `true`, `false`, `1`, `0`, `yes`, `no`.
    Boolean,
    /// A calendar date, `YYYY-MM-DD`, kept as written.
    Date,
    /// An RFC 3339 time, normalised to UTC.
    Timestamp,
}

/// A unit conversion applied to a number after parsing: the item's value is the record's
/// times `factor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unit {
    /// The record's unit, such as `cents`.
    pub from: String,
    /// The item's unit, such as `KES`.
    pub to: String,
    /// The multiplier from `from` to `to`.
    pub factor: f64,
}

/// How one item field is filled from a record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMapping {
    /// The record column.
    pub from: String,
    /// The type it must parse as.
    pub ty: FieldType,
    /// A conversion for numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Unit>,
    /// A bound on the converted value; breaking it holds the whole delivery (A-I27).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<Bound>,
    /// Whether every record must carry the column.
    #[serde(default)]
    pub required: bool,
}

impl FieldMapping {
    /// A mapping of column `from` as `ty`, optional, without unit or bound.
    pub fn new(from: impl Into<String>, ty: FieldType) -> Self {
        Self { from: from.into(), ty, unit: None, bound: None, required: false }
    }

    /// Converts numbers with `unit`.
    #[must_use]
    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = Some(unit);
        self
    }

    /// Bounds the converted value.
    #[must_use]
    pub fn bound(mut self, bound: Bound) -> Self {
        self.bound = Some(bound);
        self
    }

    /// Requires the column in every record.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// How a feed's records become item fields: declared types, units and bounds, and no model
/// (spec 3.5 §7.8.3 "Model-free updates").
///
/// # Example
///
/// ```rust
/// use adk_site::content::SiteConfig;
/// use adk_site::feed::{FeedMapping, FeedRecord, FieldMapping, FieldType, Unit};
/// use awp_types::ContentKind;
///
/// let mapping = FeedMapping::new("supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
///     .field("price", FieldMapping::new("price_cents", FieldType::Number)
///         .unit(Unit { from: "cents".into(), to: "KES".into(), factor: 0.01 }))
///     .field("stock", FieldMapping::new("qty", FieldType::Integer));
/// let record = FeedRecord::new([("sku", "A1".into()), ("price_cents", "125000".into()), ("qty", 7.into())]);
/// let mapped = mapping.map(&record, chrono::Utc::now()).unwrap();
/// assert_eq!(mapped.id.0, "A1");
/// assert_eq!(mapped.fields["price"], 1250.0);
/// assert_eq!(mapped.fields["stock"], 7);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedMapping {
    /// The feed's name.
    pub feed: String,
    /// The kind of the items it updates: its collection.
    pub kind: ContentKind,
    /// The language of the items it updates.
    pub language: LanguageTag,
    /// The column holding each record's key.
    pub key: String,
    /// Prepended to the key to make the item id.
    #[serde(default)]
    pub id_prefix: String,
    /// Item field → how it is filled.
    pub fields: BTreeMap<String, FieldMapping>,
    /// Whether a record for an unknown key creates an item; otherwise it is reported and
    /// skipped.
    #[serde(default)]
    pub create: bool,
}

/// A record mapped to item fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedRecord {
    /// The item.
    pub id: ContentId,
    /// The record's key.
    pub key: String,
    /// Field → converted value.
    pub fields: BTreeMap<String, Value>,
}

/// Why a record could not be mapped or broke a bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordProblem {
    /// The record's key, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The item field, when the problem is a field's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// What is wrong.
    pub message: String,
}

impl FeedMapping {
    /// A mapping with no fields yet.
    pub fn new(
        feed: impl Into<String>,
        kind: ContentKind,
        language: LanguageTag,
        key: impl Into<String>,
    ) -> Self {
        Self {
            feed: feed.into(),
            kind,
            language,
            key: key.into(),
            id_prefix: String::new(),
            fields: BTreeMap::new(),
            create: false,
        }
    }

    /// Fills item field `field` with `mapping`.
    #[must_use]
    pub fn field(mut self, field: impl Into<String>, mapping: FieldMapping) -> Self {
        self.fields.insert(field.into(), mapping);
        self
    }

    /// Prefixes item ids with `prefix`.
    #[must_use]
    pub fn id_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.id_prefix = prefix.into();
        self
    }

    /// Lets records for unknown keys create items.
    #[must_use]
    pub fn creating(mut self) -> Self {
        self.create = true;
        self
    }

    /// Checks the mapping against the site's lanes: streams are written by the edge, not by
    /// feeds.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] for a mapping with no fields or one that fills a stream
    /// field.
    pub fn validate(&self, config: &SiteConfig) -> Result<()> {
        if self.fields.is_empty() {
            return Err(SiteError::InvalidConfig(format!(
                "feed '{}' maps no fields; map at least one",
                self.feed
            )));
        }
        for field in self.fields.keys() {
            if lane_of(config, self.kind, &[], field) == Lane::Stream {
                return Err(SiteError::InvalidConfig(format!(
                    "feed '{}' fills the stream field '{field}'; streams are written by the edge, so map it to another field",
                    self.feed
                )));
            }
        }
        Ok(())
    }

    /// Maps one record. `now` anchors date bounds.
    ///
    /// # Errors
    ///
    /// A [`RecordProblem`] for a missing key or required column, a value of the wrong type,
    /// or a bound the converted value breaks.
    pub fn map(
        &self,
        record: &FeedRecord,
        now: DateTime<Utc>,
    ) -> std::result::Result<MappedRecord, RecordProblem> {
        let key = match record.0.get(&self.key) {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                return Err(RecordProblem {
                    key: None,
                    field: None,
                    message: format!("the record has no key in column '{}'", self.key),
                });
            }
        };
        let problem = |field: &str, message: String| RecordProblem {
            key: Some(key.clone()),
            field: Some(field.to_string()),
            message,
        };
        let mut fields = BTreeMap::new();
        for (field, mapping) in &self.fields {
            let raw = match record.0.get(&mapping.from) {
                None | Some(Value::Null) => {
                    if mapping.required {
                        return Err(problem(
                            field,
                            format!("column '{}' is missing", mapping.from),
                        ));
                    }
                    continue;
                }
                Some(Value::String(s)) if s.trim().is_empty() && !mapping.required => continue,
                Some(raw) => raw,
            };
            let value = parse(raw, mapping.ty).ok_or_else(|| {
                let ty = format!("{:?}", mapping.ty).to_lowercase();
                problem(field, format!("{raw} is not a valid {ty}"))
            })?;
            let value = match (&mapping.unit, value.as_f64()) {
                (Some(unit), Some(number)) => number_value(number * unit.factor, mapping.ty),
                _ => value,
            };
            if let Some(bound) = &mapping.bound
                && !within(bound, &value, now)
            {
                return Err(problem(field, format!("{value} breaks the bound {bound:?}")));
            }
            fields.insert(field.clone(), value);
        }
        Ok(MappedRecord { id: ContentId(format!("{}{key}", self.id_prefix)), key, fields })
    }

    /// Compares mapped records with the items the site holds and returns the changes, each
    /// changed field in its lane (spec 3.5 §7.8.3).
    ///
    /// `current` holds the site's items by id. A record that changes nothing produces no
    /// change; a record for an unknown key creates an item only when the mapping allows it.
    pub fn changes(
        &self,
        config: &SiteConfig,
        records: &[MappedRecord],
        current: &BTreeMap<ContentId, ContentItem>,
        now: DateTime<Utc>,
    ) -> FeedChanges {
        let mut changes = FeedChanges::default();
        let mut seen = BTreeSet::new();
        for record in records.iter().rev() {
            // The last record for a key in a delivery wins.
            if !seen.insert(record.id.clone()) {
                continue;
            }
            let existing = current.get(&record.id);
            let base = match existing {
                Some(item) => item.clone(),
                None if self.create => self.new_item(record),
                None => {
                    changes.unknown.push(record.key.clone());
                    continue;
                }
            };
            let mut live = BTreeMap::new();
            let mut released = base.clone();
            let mut release_changed = existing.is_none();
            for (field, value) in &record.fields {
                if base.fields.get(field) == Some(value) {
                    continue;
                }
                match lane_of(config, base.kind, &base.volatile, field) {
                    Lane::Live => {
                        live.insert(field.clone(), value.clone());
                    }
                    Lane::Release | Lane::Stream => {
                        released.fields.insert(field.clone(), value.clone());
                        release_changed = true;
                    }
                }
            }
            if !live.is_empty() {
                changes.live.push(LiveChange { id: record.id.clone(), fields: live });
            }
            if release_changed {
                released.fields.insert(
                    "updatedAt".into(),
                    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true).into(),
                );
                changes.release.push(ReleaseChange { before: existing.cloned(), after: released });
            }
        }
        changes.live.reverse();
        changes.release.reverse();
        changes.unknown.reverse();
        changes
    }

    fn new_item(&self, record: &MappedRecord) -> ContentItem {
        ContentItem {
            id: record.id.clone(),
            kind: self.kind,
            slug: slug(&record.key),
            language: self.language.clone(),
            fields: BTreeMap::new(),
            volatile: Vec::new(),
            body: None,
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        }
    }
}

/// New live values for one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveChange {
    /// The item.
    pub id: ContentId,
    /// Field → new value.
    pub fields: BTreeMap<String, Value>,
}

/// A new released version of one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseChange {
    /// The version the site holds; `None` for a new item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<ContentItem>,
    /// The new version.
    pub after: ContentItem,
}

/// What one delivery changes, by lane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedChanges {
    /// Live values for the edge; never released.
    pub live: Vec<LiveChange>,
    /// New item versions for the batcher.
    pub release: Vec<ReleaseChange>,
    /// Keys of records for items the site does not have.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

impl FeedChanges {
    /// The items the delivery changes in any lane.
    pub fn changed_items(&self) -> BTreeSet<ContentId> {
        self.live
            .iter()
            .map(|c| c.id.clone())
            .chain(self.release.iter().map(|c| c.after.id.clone()))
            .collect()
    }

    /// Field updates in the delivery, live and released.
    pub fn field_updates(&self) -> u64 {
        let live: usize = self.live.iter().map(|c| c.fields.len()).sum();
        let released: usize = self
            .release
            .iter()
            .map(|c| {
                c.after
                    .fields
                    .iter()
                    .filter(|(k, v)| {
                        k.as_str() != "updatedAt"
                            && c.before.as_ref().is_none_or(|b| b.fields.get(*k) != Some(*v))
                    })
                    .count()
            })
            .sum();
        (live + released) as u64
    }
}

/// A URL slug for a feed key: lowercase letters and digits, other runs as `-`.
fn slug(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "item".into() } else { out }
}

fn number_value(number: f64, ty: FieldType) -> Value {
    if ty == FieldType::Integer || (number.fract() == 0.0 && number.abs() < 9.0e15) {
        Value::from(number as i64)
    } else {
        serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
    }
}

fn parse(raw: &Value, ty: FieldType) -> Option<Value> {
    let text = |raw: &Value| match raw {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    match ty {
        FieldType::Integer => match raw {
            Value::Number(n) => n.as_i64().map(Value::from),
            _ => text(raw)?.replace(',', "").parse::<i64>().ok().map(Value::from),
        },
        FieldType::Number => {
            let number = match raw {
                Value::Number(n) => n.as_f64()?,
                _ => text(raw)?.replace(',', "").parse::<f64>().ok()?,
            };
            number.is_finite().then(|| number_value(number, ty))
        }
        FieldType::Text => text(raw).map(Value::String),
        FieldType::Boolean => match raw {
            Value::Bool(b) => Some(Value::Bool(*b)),
            _ => match text(raw)?.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Some(Value::Bool(true)),
                "false" | "0" | "no" => Some(Value::Bool(false)),
                _ => None,
            },
        },
        FieldType::Date => {
            let text = text(raw)?;
            NaiveDate::parse_from_str(&text, "%Y-%m-%d").ok().map(|_| Value::String(text))
        }
        FieldType::Timestamp => DateTime::parse_from_rfc3339(&text(raw)?).ok().map(|d| {
            Value::String(d.with_timezone(&Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        }),
    }
}

/// Whether `value` is within `bound`: a number within a range, a date or time within the
/// days either side of `now`.
pub(crate) fn within(bound: &Bound, value: &Value, now: DateTime<Utc>) -> bool {
    if let Some(number) = value.as_f64() {
        return bound.allows_number(number);
    }
    let Some(text) = value.as_str() else { return false };
    let at = DateTime::parse_from_rfc3339(text).map(|d| d.with_timezone(&Utc)).ok().or_else(|| {
        NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc())
    });
    at.is_some_and(|at| bound.allows_date(at, now))
}

/// Checks a source against its contract: `deliver` puts `records` where the source reads
/// them, and the next pull returns them in order with their values as strings or numbers.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn content_source_conformance<F, Fut>(deliver: F)
where
    F: Fn(Vec<FeedRecord>) -> Fut,
    Fut: std::future::Future<Output = std::sync::Arc<dyn ContentSource>>,
{
    let records = vec![
        FeedRecord::new([("sku", Value::from("A1")), ("price", Value::from("12.50"))]),
        FeedRecord::new([("sku", Value::from("B2")), ("price", Value::from("7"))]),
        FeedRecord::new([("sku", Value::from("C, \"3\"")), ("price", Value::from("1,000"))]),
    ];
    let source = deliver(records.clone()).await;
    assert!(!source.name().is_empty(), "a source has a name");
    let pulled = source.pull().await.expect("pull");
    let as_text = |r: &FeedRecord| -> BTreeMap<String, String> {
        r.0.iter()
            .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string)))
            .collect()
    };
    assert_eq!(
        pulled.iter().map(as_text).collect::<Vec<_>>(),
        records.iter().map(as_text).collect::<Vec<_>>(),
        "the delivery's records, in order"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::KindLanes;

    fn mapping() -> FeedMapping {
        FeedMapping::new("supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
            .field(
                "price",
                FieldMapping::new("price", FieldType::Number).required().bound(Bound::Range {
                    minimum: None,
                    exclusive_minimum: Some(0.0),
                    maximum: None,
                    exclusive_maximum: None,
                }),
            )
            .field("stock", FieldMapping::new("qty", FieldType::Integer))
            .field("restock", FieldMapping::new("next", FieldType::Date))
    }

    fn config() -> SiteConfig {
        let mut config = SiteConfig::new("Shop", "shop.example", vec!["en".parse().unwrap()]);
        config.lanes.insert(
            ContentKind::Product,
            KindLanes { volatile: vec!["stock".into()], ..KindLanes::default() },
        );
        config
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-25T09:00:00Z").unwrap().with_timezone(&Utc)
    }

    #[test]
    fn records_map_through_types_units_and_bounds() {
        let m = mapping();
        let ok = FeedRecord::new([
            ("sku", "A1".into()),
            ("price", "1,250.50".into()),
            ("qty", "7".into()),
            ("next", "2026-11-01".into()),
        ]);
        let mapped = m.map(&ok, now()).unwrap();
        assert_eq!(mapped.fields["price"], 1250.5);
        assert_eq!(mapped.fields["stock"], 7);
        assert_eq!(mapped.fields["restock"], "2026-11-01");
        let zero = FeedRecord::new([("sku", "A1".into()), ("price", 0.into())]);
        assert!(m.map(&zero, now()).unwrap_err().message.contains("bound"));
        let text = FeedRecord::new([("sku", "A1".into()), ("price", "ten".into())]);
        assert_eq!(m.map(&text, now()).unwrap_err().field.as_deref(), Some("price"));
        let missing = FeedRecord::new([("sku", "A1".into())]);
        assert!(m.map(&missing, now()).unwrap_err().message.contains("missing"));
        assert!(m.map(&FeedRecord::default(), now()).unwrap_err().key.is_none());
    }

    #[test]
    fn changes_sort_fields_into_lanes() {
        let config = config();
        let m = mapping();
        let item = |price: f64, stock: i64| ContentItem {
            id: "A1".into(),
            kind: ContentKind::Product,
            slug: "a1".into(),
            language: "en".parse().unwrap(),
            fields: BTreeMap::from([
                ("price".to_string(), Value::from(price)),
                ("stock".to_string(), Value::from(stock)),
            ]),
            volatile: Vec::new(),
            body: None,
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        };
        let current = BTreeMap::from([(ContentId::from("A1"), item(10.5, 3))]);
        let stock_only = MappedRecord {
            id: "A1".into(),
            key: "A1".into(),
            fields: BTreeMap::from([("price".into(), 10.5.into()), ("stock".into(), 2.into())]),
        };
        let changes = m.changes(&config, &[stock_only], &current, now());
        assert!(changes.release.is_empty(), "a stock change never makes a release");
        assert_eq!(changes.live[0].fields["stock"], 2);
        assert_eq!(changes.field_updates(), 1);

        let price = MappedRecord {
            id: "A1".into(),
            key: "A1".into(),
            fields: BTreeMap::from([("price".into(), 11.into())]),
        };
        let unknown = MappedRecord { id: "Z9".into(), key: "Z9".into(), fields: BTreeMap::new() };
        let changes = m.changes(&config, &[price, unknown.clone()], &current, now());
        assert_eq!(changes.release.len(), 1);
        assert_eq!(changes.release[0].after.fields["price"], 11);
        assert_eq!(changes.release[0].after.fields["updatedAt"], "2026-10-25T09:00:00Z");
        assert_eq!(changes.unknown, vec!["Z9".to_string()]);
        let created = m.clone().creating().changes(&config, &[unknown], &current, now());
        assert_eq!(created.release[0].before, None);
        assert_eq!(created.release[0].after.slug, "z9");
    }

    #[test]
    fn mappings_never_fill_streams() {
        let mut config = config();
        config.lanes.insert(
            ContentKind::Article,
            KindLanes { stream: vec!["entries".into()], ..KindLanes::default() },
        );
        let m = FeedMapping::new("wire", ContentKind::Article, "en".parse().unwrap(), "id")
            .field("entries", FieldMapping::new("text", FieldType::Text));
        assert!(m.validate(&config).is_err());
        assert!(mapping().validate(&config).is_ok());
    }

    #[test]
    fn csv_parses_quotes_and_line_endings() {
        let records = parse_csv("sku,name\r\nA1,\"Panel, \"\"400\"\" W\"\r\nB2,\n\n").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0["name"], "Panel, \"400\" W");
        assert_eq!(records[1].0["name"], "");
        assert!(parse_csv("a\n\"open").is_err());
        assert!(parse_csv("a\n1,2").is_err());
        assert!(parse_csv("").unwrap().is_empty());
    }
}
