//! Bounds on automated feeds (spec 3.5 §7.8.3 "Bounds on automated feeds", A-I25, A-I27).
//!
//! | Bound | Breaks when | Effect |
//! |-------|-------------|--------|
//! | `maxChangeFraction` | The delivery changes more than this share of its collection's items (default 0.2) | The whole delivery is held |
//! | `fieldBounds` and each mapping's `bound` | A value is outside its bound, or does not parse | The whole delivery is held |
//! | `maxRate` | More records arrive in a minute than allowed | The whole delivery is held |
//! | `priceRule` | One item's price moves more than the rule allows | That item is held for approval; the rest proceeds (A-I25) |
//!
//! A held delivery raises a P2 alarm with a [`ChangeSummary`] of what it would change, and
//! nothing from it publishes — neither its live values nor its releases — until the owner
//! decides (A-I27).

use std::collections::BTreeMap;

use awp_types::{ContentId, ContentItem, FeedBounds, PriceRule, Rate};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{FeedChanges, MappedRecord, RecordProblem, within};

/// One way a delivery breaks its bounds (spec 3.5 A-I27).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "bound", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum BoundBreak {
    /// The delivery changes too large a share of its collection.
    ChangeFraction {
        /// Items it changes.
        changed: usize,
        /// Items in the collection.
        collection: usize,
        /// The largest share allowed.
        max: f32,
    },
    /// A value is outside its bound or does not parse.
    Field {
        /// The record's key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// The item field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        /// What is wrong.
        message: String,
    },
    /// Records arrive faster than allowed.
    Rate {
        /// Records in the last minute, this delivery included.
        records: u64,
        /// The most allowed.
        max: Rate,
    },
}

/// What a delivery would change: the summary a held delivery's alarm carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSummary {
    /// Records delivered.
    pub records: usize,
    /// Items it changes in any lane.
    pub items_changed: usize,
    /// Items in the collection.
    pub collection: usize,
    /// `items_changed / collection`.
    pub fraction: f64,
    /// Live values it sets.
    pub live_values: usize,
    /// New item versions it releases.
    pub releases: usize,
    /// Items it creates.
    pub new_items: usize,
    /// Field → items whose value changes.
    pub fields: BTreeMap<String, usize>,
    /// Price movements, when prices change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices: Option<PriceSummary>,
    /// Up to ten changed items.
    pub sample: Vec<ContentId>,
}

/// How a delivery moves prices.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceSummary {
    /// Prices that change.
    pub changed: usize,
    /// Prices that rise.
    pub raised: usize,
    /// Prices that fall.
    pub lowered: usize,
    /// The largest change, in percent of the old price.
    pub largest_change_pct: f64,
    /// Changes the price rule would hold.
    pub outside_rule: usize,
}

/// The verdict on one delivery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundsReport {
    /// Every bound it breaks; empty when it may proceed.
    pub breaks: Vec<BoundBreak>,
    /// What it would change.
    pub summary: ChangeSummary,
}

impl BoundsReport {
    /// Whether the delivery is held whole (A-I27).
    pub fn held(&self) -> bool {
        !self.breaks.is_empty()
    }
}

/// The inputs of one bounds check.
#[derive(Debug)]
pub struct Delivery<'a> {
    /// Records delivered.
    pub records: usize,
    /// The records that mapped.
    pub mapped: &'a [MappedRecord],
    /// The records that did not map or broke a mapping bound.
    pub problems: &'a [RecordProblem],
    /// What the mapped records change.
    pub changes: &'a FeedChanges,
    /// Items in the feed's collection.
    pub collection: usize,
    /// Records the feed delivered in the last minute, this delivery included.
    pub records_last_minute: u64,
    /// The delivery time, which anchors date bounds.
    pub now: DateTime<Utc>,
}

/// The percentage by which a price moves, when it moves.
fn price_change_pct(before: &ContentItem, after: &ContentItem) -> Option<(f64, f64, f64)> {
    let old = before.fields.get("price").and_then(Value::as_f64)?;
    let new = after.fields.get("price").and_then(Value::as_f64)?;
    if old == new {
        return None;
    }
    let pct = if old == 0.0 { f64::INFINITY } else { (new - old).abs() * 100.0 / old };
    Some((old, new, pct))
}

fn minor_units(price: f64) -> u64 {
    (price.max(0.0) * 100.0).round() as u64
}

/// Whether a change from `before` to `after` keeps its price within `rule`; a new item or an
/// unchanged price is within it (spec 3.5 A-I25).
///
/// # Example
///
/// ```rust
/// use adk_site::feed::bounds::price_within_rule;
/// use awp_types::{ContentItem, ContentKind, ContentState, PriceRule};
///
/// let item = |price: f64| ContentItem {
///     id: "p".into(), kind: ContentKind::Product, slug: "p".into(), language: "en".parse().unwrap(),
///     fields: [("price".to_string(), price.into())].into(), volatile: vec![], body: None,
///     media: vec![], sources: vec![], schedule: None, state: ContentState::Published,
/// };
/// let rule = PriceRule::default();
/// assert!(price_within_rule(&rule, Some(&item(100.0)), &item(105.0)));
/// assert!(!price_within_rule(&rule, Some(&item(100.0)), &item(115.0)));
/// ```
pub fn price_within_rule(
    rule: &PriceRule,
    before: Option<&ContentItem>,
    after: &ContentItem,
) -> bool {
    let Some(before) = before else { return true };
    match price_change_pct(before, after) {
        None => true,
        Some((old, new, _)) => rule.allows(minor_units(old), minor_units(new)),
    }
}

/// Checks one delivery against its bounds (spec 3.5 A-I27).
///
/// # Example
///
/// ```rust
/// use adk_site::feed::FeedChanges;
/// use adk_site::feed::bounds::{Delivery, check};
/// use awp_types::{FeedBounds, PriceRule, Rate};
///
/// let bounds = FeedBounds {
///     max_change_fraction: FeedBounds::DEFAULT_MAX_CHANGE_FRACTION,
///     price_rule: PriceRule::default(),
///     field_bounds: Default::default(),
///     max_rate: Rate { per_minute: 100 },
/// };
/// let changes = FeedChanges::default();
/// let report = check(&bounds, &Delivery {
///     records: 150, mapped: &[], problems: &[], changes: &changes, collection: 1_000,
///     records_last_minute: 150, now: chrono::Utc::now(),
/// });
/// assert!(report.held(), "150 records in a minute break a rate of 100");
/// ```
pub fn check(bounds: &FeedBounds, delivery: &Delivery<'_>) -> BoundsReport {
    let mut breaks = Vec::new();
    let summary = summarise(bounds, delivery);
    if delivery.records_last_minute > u64::from(bounds.max_rate.per_minute) {
        breaks
            .push(BoundBreak::Rate { records: delivery.records_last_minute, max: bounds.max_rate });
    }
    let collection = delivery.collection.max(1);
    if summary.items_changed as f64 > f64::from(bounds.max_change_fraction) * collection as f64 {
        breaks.push(BoundBreak::ChangeFraction {
            changed: summary.items_changed,
            collection: delivery.collection,
            max: bounds.max_change_fraction,
        });
    }
    for problem in delivery.problems {
        breaks.push(BoundBreak::Field {
            key: problem.key.clone(),
            field: problem.field.clone(),
            message: problem.message.clone(),
        });
    }
    for record in delivery.mapped {
        for (field, bound) in &bounds.field_bounds {
            if let Some(value) = record.fields.get(field)
                && !within(bound, value, delivery.now)
            {
                breaks.push(BoundBreak::Field {
                    key: Some(record.key.clone()),
                    field: Some(field.clone()),
                    message: format!("{value} breaks the feed bound {bound:?}"),
                });
            }
        }
    }
    BoundsReport { breaks, summary }
}

fn summarise(bounds: &FeedBounds, delivery: &Delivery<'_>) -> ChangeSummary {
    let changes = delivery.changes;
    let changed = changes.changed_items();
    let mut fields: BTreeMap<String, usize> = BTreeMap::new();
    for live in &changes.live {
        for field in live.fields.keys() {
            *fields.entry(field.clone()).or_default() += 1;
        }
    }
    let mut prices = PriceSummary::default();
    for change in &changes.release {
        for (field, value) in &change.after.fields {
            let before = change.before.as_ref().and_then(|b| b.fields.get(field));
            if field != "updatedAt" && before != Some(value) {
                *fields.entry(field.clone()).or_default() += 1;
            }
        }
        if let Some(before) = &change.before
            && let Some((old, new, pct)) = price_change_pct(before, &change.after)
        {
            prices.changed += 1;
            if new > old {
                prices.raised += 1;
            } else {
                prices.lowered += 1;
            }
            prices.largest_change_pct = prices.largest_change_pct.max(pct);
            if !bounds.price_rule.allows(minor_units(old), minor_units(new)) {
                prices.outside_rule += 1;
            }
        }
    }
    ChangeSummary {
        records: delivery.records,
        items_changed: changed.len(),
        collection: delivery.collection,
        fraction: changed.len() as f64 / delivery.collection.max(1) as f64,
        live_values: changes.live.iter().map(|c| c.fields.len()).sum(),
        releases: changes.release.len(),
        new_items: changes.release.iter().filter(|c| c.before.is_none()).count(),
        fields,
        prices: (prices.changed > 0).then_some(prices),
        sample: changed.into_iter().take(10).collect(),
    }
}

#[cfg(test)]
mod tests {
    use awp_types::{Bound, ContentKind, ContentState};

    use super::*;
    use crate::feed::{LiveChange, ReleaseChange};

    fn item(id: &str, price: f64) -> ContentItem {
        ContentItem {
            id: id.into(),
            kind: ContentKind::Product,
            slug: id.into(),
            language: "en".parse().unwrap(),
            fields: BTreeMap::from([("price".to_string(), Value::from(price))]),
            volatile: Vec::new(),
            body: None,
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        }
    }

    fn bounds() -> FeedBounds {
        FeedBounds {
            max_change_fraction: 0.2,
            price_rule: PriceRule::default(),
            field_bounds: BTreeMap::from([(
                "stock".to_string(),
                Bound::Range {
                    minimum: Some(0.0),
                    exclusive_minimum: None,
                    maximum: Some(100_000.0),
                    exclusive_maximum: None,
                },
            )]),
            max_rate: Rate { per_minute: 10_000 },
        }
    }

    fn priced(count: usize, factor: f64) -> FeedChanges {
        FeedChanges {
            live: Vec::new(),
            release: (0..count)
                .map(|i| ReleaseChange {
                    before: Some(item(&format!("p{i}"), 100.0)),
                    after: item(&format!("p{i}"), 100.0 * factor),
                })
                .collect(),
            unknown: Vec::new(),
        }
    }

    fn delivery<'a>(
        changes: &'a FeedChanges,
        mapped: &'a [MappedRecord],
        problems: &'a [RecordProblem],
    ) -> Delivery<'a> {
        Delivery {
            records: changes.release.len(),
            mapped,
            problems,
            changes,
            collection: 1_000,
            records_last_minute: changes.release.len() as u64,
            now: Utc::now(),
        }
    }

    #[test]
    fn a_corrupted_delivery_is_held_whole_with_a_summary() {
        let changes = priced(600, 0.4);
        let report = check(&bounds(), &delivery(&changes, &[], &[]));
        assert!(report.held());
        assert!(matches!(report.breaks[0], BoundBreak::ChangeFraction { changed: 600, .. }));
        let prices = report.summary.prices.unwrap();
        assert_eq!((prices.changed, prices.lowered, prices.outside_rule), (600, 600, 600));
        assert!((prices.largest_change_pct - 60.0).abs() < 1e-9);
        assert_eq!(report.summary.sample.len(), 10);
        assert!((report.summary.fraction - 0.6).abs() < 1e-9);
    }

    #[test]
    fn a_normal_delivery_proceeds() {
        let mut changes = priced(5, 1.05);
        changes.live.push(LiveChange {
            id: "p9".into(),
            fields: BTreeMap::from([("stock".to_string(), Value::from(4))]),
        });
        let report = check(&bounds(), &delivery(&changes, &[], &[]));
        assert!(!report.held(), "{:?}", report.breaks);
        assert_eq!(report.summary.items_changed, 6);
        assert_eq!(report.summary.fields["stock"], 1);
        assert_eq!(report.summary.fields["price"], 5);
    }

    #[test]
    fn field_bounds_problems_and_rate_hold_the_delivery() {
        let changes = priced(1, 1.0);
        let mapped = [MappedRecord {
            id: "p0".into(),
            key: "p0".into(),
            fields: BTreeMap::from([("stock".to_string(), Value::from(200_000))]),
        }];
        assert!(matches!(
            check(&bounds(), &delivery(&changes, &mapped, &[])).breaks[0],
            BoundBreak::Field { .. }
        ));
        let problem = RecordProblem { key: Some("p1".into()), field: None, message: "bad".into() };
        assert!(check(&bounds(), &delivery(&changes, &[], &[problem])).held());
        let mut fast = delivery(&changes, &[], &[]);
        fast.records_last_minute = 10_001;
        assert!(matches!(check(&bounds(), &fast).breaks[0], BoundBreak::Rate { .. }));
    }

    #[test]
    fn the_price_rule_is_per_item() {
        let rule = PriceRule::default();
        assert!(price_within_rule(&rule, None, &item("p", 5.0)));
        assert!(price_within_rule(&rule, Some(&item("p", 100.0)), &item("p", 110.0)));
        assert!(!price_within_rule(&rule, Some(&item("p", 100.0)), &item("p", 110.01)));
        assert!(!price_within_rule(&rule, Some(&item("p", 0.0)), &item("p", 1.0)));
    }
}
