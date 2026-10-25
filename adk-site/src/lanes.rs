//! The three lanes of a constantly updated site (spec 3.5 §7.8.3 "Three lanes", A-I26).
//!
//! A content kind declares the lane of each field in `site.toml`; an item may add live fields
//! of its own through its `volatile` list:
//!
//! ```toml
//! [lanes.product]
//! volatile = ["stock", "availability"]
//!
//! [lanes.article]
//! stream = ["entries"]
//! ```
//!
//! | Lane | Fields | Delivered | Rule |
//! |------|--------|-----------|------|
//! | [`Lane::Release`] | Every field not declared otherwise | Batched signed releases | Carries every claim (A-I26) |
//! | [`Lane::Live`] | `volatile` | The edge's read-only capabilities; with script off the page shows the last released value and its time | Never carries a claim |
//! | [`Lane::Stream`] | `stream` | An edge feed of signed entries, folded into a signed release | Never carries a claim; every entry ends up in a release |
//!
//! A claim is a value an agent may act on — pay, quote, cite: [`CLAIM_FIELDS`] and a kind's
//! own `claims`. [`SiteConfig::validate`] refuses a kind that declares a claim field live or
//! streamed, and [`SiteProject::issues`](crate::content::SiteProject::issues) refuses an item
//! that does, so no build — and so no release — serves a claim outside the release lane.

use std::collections::BTreeSet;
use std::fmt;

use awp_types::{ContentId, ContentItem, ContentKind};
use serde::{Deserialize, Serialize};

use crate::content::{SiteConfig, kind_name};

/// Fields that carry claims for every kind: prices and the terms of a quote, what a page
/// states and cites, the times an agent books against, and final results (spec 3.5 A-I26).
pub const CLAIM_FIELDS: &[&str] = &[
    "price",
    "currency",
    "sku",
    "title",
    "summary",
    "description",
    "validUntil",
    "startsAt",
    "endsAt",
    "publishedAt",
    "result",
    "corrections",
    "withdrawal",
];

/// The lane a field travels in (spec 3.5 §7.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Batched signed releases: claims and everything else not declared live or streamed.
    Release,
    /// State served live by the edge: stock, availability, live scores, queue times, counts.
    Live,
    /// Append-only coverage: live blogs, tickers, emergency updates.
    Stream,
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Lane::Release => "release",
            Lane::Live => "live",
            Lane::Stream => "stream",
        })
    }
}

/// The lanes one content kind declares, `[lanes.<kind>]` in `site.toml` (spec 3.5 §7.8.3).
///
/// # Example
///
/// ```rust
/// use adk_site::content::SiteConfig;
/// use adk_site::lanes::{Lane, lane_of};
/// use awp_types::ContentKind;
///
/// let config = SiteConfig::from_toml(r#"
/// name = "Shop"
/// domain = "shop.example"
/// languages = ["en"]
/// [lanes.product]
/// volatile = ["stock"]
/// "#).unwrap();
/// assert_eq!(lane_of(&config, ContentKind::Product, &[], "stock"), Lane::Live);
/// assert_eq!(lane_of(&config, ContentKind::Product, &[], "price"), Lane::Release);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KindLanes {
    /// Fields served live by the edge and never released.
    pub volatile: Vec<String>,
    /// Append-only fields delivered as a stream and folded into releases.
    pub stream: Vec<String>,
    /// Further fields of this kind that carry claims, beside [`CLAIM_FIELDS`], such as a
    /// match's `finalScore`.
    pub claims: Vec<String>,
}

/// A claim field declared outside the release lane (spec 3.5 A-I26).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneViolation {
    /// The item, when an item's own `volatile` list declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ContentId>,
    /// The kind.
    pub kind: ContentKind,
    /// The field.
    pub field: String,
    /// The lane it was declared in.
    pub lane: Lane,
}

impl fmt::Display for LaneViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let owner = match &self.item {
            Some(item) => format!("item '{item}'"),
            None => format!("[lanes.{}]", kind_name(self.kind)),
        };
        write!(
            f,
            "{owner} declares the claim field '{}' {}; a value an agent may act on is served only from a signed release, so keep it out of `volatile` and `stream` (A-I26)",
            self.field,
            match self.lane {
                Lane::Live => "volatile",
                Lane::Stream => "a stream",
                Lane::Release => "released",
            }
        )
    }
}

/// Whether `field` of `kind` carries a claim (spec 3.5 A-I26).
pub fn is_claim(config: &SiteConfig, kind: ContentKind, field: &str) -> bool {
    CLAIM_FIELDS.contains(&field)
        || config.lanes.get(&kind).is_some_and(|lanes| lanes.claims.iter().any(|c| c == field))
}

/// The lane of `field` for an item of `kind` whose own `volatile` list is `item_volatile`.
///
/// A field an item lists as volatile is live; otherwise the kind's declaration decides, and
/// an undeclared field is released.
pub fn lane_of(
    config: &SiteConfig,
    kind: ContentKind,
    item_volatile: &[String],
    field: &str,
) -> Lane {
    if item_volatile.iter().any(|f| f == field) {
        return Lane::Live;
    }
    match config.lanes.get(&kind) {
        Some(lanes) if lanes.volatile.iter().any(|f| f == field) => Lane::Live,
        Some(lanes) if lanes.stream.iter().any(|f| f == field) => Lane::Stream,
        _ => Lane::Release,
    }
}

/// The lane of one of `item`'s fields.
pub fn item_lane(config: &SiteConfig, item: &ContentItem, field: &str) -> Lane {
    lane_of(config, item.kind, &item.volatile, field)
}

/// An item's live fields: its own `volatile` list, then the ones its kind declares, without
/// repeats.
pub fn live_fields<'a>(config: &'a SiteConfig, item: &'a ContentItem) -> Vec<&'a str> {
    let mut seen = BTreeSet::new();
    item.volatile
        .iter()
        .chain(config.lanes.get(&item.kind).into_iter().flat_map(|l| l.volatile.iter()))
        .map(String::as_str)
        .filter(|field| seen.insert(*field))
        .collect()
}

/// The stream fields `kind` declares.
pub fn stream_fields(config: &SiteConfig, kind: ContentKind) -> &[String] {
    config.lanes.get(&kind).map_or(&[], |lanes| lanes.stream.as_slice())
}

/// Lane declarations in `config` that break A-I26: a claim field declared volatile or
/// streamed, or a field declared both.
pub fn config_violations(config: &SiteConfig) -> Vec<LaneViolation> {
    let mut violations = Vec::new();
    for (kind, lanes) in &config.lanes {
        for (fields, lane) in [(&lanes.volatile, Lane::Live), (&lanes.stream, Lane::Stream)] {
            for field in fields {
                if is_claim(config, *kind, field) {
                    violations.push(LaneViolation {
                        item: None,
                        kind: *kind,
                        field: field.clone(),
                        lane,
                    });
                }
            }
        }
    }
    violations
}

/// The claim fields `item` lists in its own `volatile` list (spec 3.5 A-I26).
pub fn item_violations(config: &SiteConfig, item: &ContentItem) -> Vec<LaneViolation> {
    item.volatile
        .iter()
        .filter(|field| is_claim(config, item.kind, field))
        .map(|field| LaneViolation {
            item: Some(item.id.clone()),
            kind: item.kind,
            field: field.clone(),
            lane: Lane::Live,
        })
        .collect()
}

/// Fields declared in two lanes of one kind, as `(kind, field)`.
pub(crate) fn overlapping(config: &SiteConfig) -> Vec<(ContentKind, String)> {
    let mut overlaps = Vec::new();
    for (kind, lanes) in &config.lanes {
        for field in &lanes.volatile {
            if lanes.stream.contains(field) {
                overlaps.push((*kind, field.clone()));
            }
        }
    }
    overlaps
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use awp_types::ContentState;

    use super::*;

    fn config() -> SiteConfig {
        let mut config = SiteConfig::new("Shop", "shop.example", vec!["en".parse().unwrap()]);
        config.lanes.insert(
            ContentKind::Product,
            KindLanes { volatile: vec!["stock".into()], ..KindLanes::default() },
        );
        config.lanes.insert(
            ContentKind::Article,
            KindLanes {
                stream: vec!["entries".into()],
                claims: vec!["finalScore".into()],
                ..KindLanes::default()
            },
        );
        config
    }

    fn item(volatile: &[&str]) -> ContentItem {
        ContentItem {
            id: "panel".into(),
            kind: ContentKind::Product,
            slug: "panel".into(),
            language: "en".parse().unwrap(),
            fields: BTreeMap::new(),
            volatile: volatile.iter().map(|f| (*f).to_string()).collect(),
            body: None,
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        }
    }

    #[test]
    fn kinds_and_items_declare_lanes() {
        let config = config();
        let panel = item(&["queue"]);
        assert_eq!(item_lane(&config, &panel, "stock"), Lane::Live);
        assert_eq!(item_lane(&config, &panel, "queue"), Lane::Live);
        assert_eq!(item_lane(&config, &panel, "price"), Lane::Release);
        assert_eq!(lane_of(&config, ContentKind::Article, &[], "entries"), Lane::Stream);
        assert_eq!(live_fields(&config, &panel), vec!["queue", "stock"]);
        assert_eq!(stream_fields(&config, ContentKind::Article), ["entries".to_string()]);
        assert!(config_violations(&config).is_empty());
        assert!(is_claim(&config, ContentKind::Article, "finalScore"));
        assert!(!is_claim(&config, ContentKind::Product, "finalScore"));
    }

    #[test]
    fn claims_stay_in_the_release_lane() {
        let mut config = config();
        config.lanes.get_mut(&ContentKind::Product).unwrap().volatile.push("price".into());
        config.lanes.get_mut(&ContentKind::Article).unwrap().stream.push("finalScore".into());
        let violations = config_violations(&config);
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(violations[0].to_string().contains("A-I26"));
        let found = item_violations(&config, &item(&["stock", "price"]));
        assert_eq!(
            found,
            vec![LaneViolation {
                item: Some("panel".into()),
                kind: ContentKind::Product,
                field: "price".into(),
                lane: Lane::Live,
            }]
        );
    }
}
