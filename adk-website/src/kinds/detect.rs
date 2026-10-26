//! Kind detection for discovery (spec 3.5 §7.16.1: "onboarding proposes a kind from
//! discovery (a WooCommerce shop, a `.gov` domain, a blog theme, a donation plugin)", §7.18
//! stage 2).
//!
//! Detection proposes; the owner confirms. Each signal adds weight to the kinds it points
//! at, and the proposal names its reasons so the owner's card can show them.

use std::collections::BTreeMap;

use adk_site::migrate::{FeatureKind, SiteInventory};
use awp_types::{ContentKind, SiteKind};
use serde::{Deserialize, Serialize};

/// What discovery saw that bears on the site kind.
///
/// # Example
///
/// ```rust
/// use adk_website::kinds::{KindEvidence, detect};
/// use awp_types::SiteKind;
///
/// let evidence = KindEvidence { domain: Some("kaunti.go.ke".into()), ..KindEvidence::default() };
/// assert_eq!(detect(&evidence).unwrap().kind, SiteKind::Government);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindEvidence {
    /// The site's domain.
    pub domain: Option<String>,
    /// Dynamic features found.
    pub features: Vec<FeatureKind>,
    /// schema.org types and Open Graph types found, such as `NGO` or `article`.
    pub types: Vec<String>,
    /// Plugin, theme and markup markers found in the pages, such as `woocommerce`.
    pub markers: Vec<String>,
    /// Item counts by content kind.
    pub items: BTreeMap<ContentKind, usize>,
}

impl KindEvidence {
    /// The evidence in an inventory: its origin's domain, its features, the types its facts
    /// and Open Graph tags name, plugin markers in its items and the mix of its items.
    pub fn from_inventory(inventory: &SiteInventory) -> Self {
        let domain = inventory
            .origin
            .as_deref()
            .and_then(|origin| url::Url::parse(origin).ok())
            .and_then(|url| url.host_str().map(str::to_string));
        let mut types: Vec<String> = inventory
            .facts
            .iter()
            .filter(|fact| fact.key.ends_with("type") || fact.key.ends_with("@type"))
            .map(|fact| fact.value.clone())
            .collect();
        for url in &inventory.urls {
            if let Some(kind) = url.page.as_ref().and_then(|page| page.open_graph.get("og:type")) {
                types.push(kind.clone());
            }
        }
        let mut markers = Vec::new();
        for item in &inventory.items {
            let html = item.html.to_ascii_lowercase();
            for marker in MARKERS {
                if html.contains(marker.0) && !markers.iter().any(|m: &String| m == marker.0) {
                    markers.push(marker.0.to_string());
                }
            }
        }
        let mut items = BTreeMap::new();
        for item in &inventory.items {
            *items.entry(item.kind).or_insert(0) += 1;
        }
        Self {
            domain,
            features: inventory.features.iter().map(|feature| feature.kind).collect(),
            types,
            markers,
            items,
        }
    }
}

/// A proposed kind with the reasons behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindProposal {
    /// The kind proposed.
    pub kind: SiteKind,
    /// Its weight.
    pub score: u32,
    /// Why, for the owner's card.
    pub reasons: Vec<String>,
    /// The runners-up, best first.
    pub alternatives: Vec<SiteKind>,
}

/// Markers in page markup and the kind each points at, with its weight.
const MARKERS: &[(&str, SiteKind, u32)] = &[
    ("woocommerce", SiteKind::Ecommerce, 6),
    ("add-to-cart", SiteKind::Ecommerce, 4),
    ("give-form", SiteKind::Nonprofit, 6),
    ("donate", SiteKind::Nonprofit, 3),
    ("donation", SiteKind::Nonprofit, 3),
    ("charitable", SiteKind::Nonprofit, 3),
    ("tribe-events", SiteKind::Events, 4),
    ("tickets", SiteKind::Events, 2),
    ("festival", SiteKind::Events, 2),
    ("conference", SiteKind::Events, 2),
    ("admissions", SiteKind::Education, 3),
    ("prospectus", SiteKind::Education, 3),
    ("appointment", SiteKind::Health, 2),
    ("patients", SiteKind::Health, 3),
    ("sermon", SiteKind::Community, 3),
    ("parish", SiteKind::Community, 3),
    ("congregation", SiteKind::Community, 3),
    ("newsroom", SiteKind::Media, 3),
];

/// schema.org and Open Graph types and the kind each points at, with its weight.
const TYPES: &[(&str, SiteKind, u32)] = &[
    ("governmentorganization", SiteKind::Government, 6),
    ("governmentoffice", SiteKind::Government, 6),
    ("ngo", SiteKind::Nonprofit, 6),
    ("nonprofit", SiteKind::Nonprofit, 5),
    ("educationalorganization", SiteKind::Education, 6),
    ("school", SiteKind::Education, 5),
    ("collegeoruniversity", SiteKind::Education, 6),
    ("newsmediaorganization", SiteKind::Media, 6),
    ("newsarticle", SiteKind::Media, 4),
    ("medicalclinic", SiteKind::Health, 6),
    ("physician", SiteKind::Health, 6),
    ("dentist", SiteKind::Health, 6),
    ("hospital", SiteKind::Health, 6),
    ("medicalbusiness", SiteKind::Health, 5),
    ("placeofworship", SiteKind::Community, 6),
    ("church", SiteKind::Community, 6),
    ("mosque", SiteKind::Community, 6),
    ("sportsclub", SiteKind::Community, 5),
    ("event", SiteKind::Events, 3),
    ("festival", SiteKind::Events, 5),
    ("store", SiteKind::Ecommerce, 4),
    ("onlinestore", SiteKind::Ecommerce, 6),
    ("product", SiteKind::Ecommerce, 3),
    ("localbusiness", SiteKind::Business, 4),
    ("professionalservice", SiteKind::Business, 4),
    ("organization", SiteKind::Business, 1),
    ("person", SiteKind::Personal, 3),
    ("profile", SiteKind::Personal, 3),
    ("blog", SiteKind::Personal, 3),
    ("blogposting", SiteKind::Personal, 2),
    ("article", SiteKind::Personal, 1),
];

/// The kind the domain's suffix points at, with its weight.
fn domain_kind(domain: &str) -> Option<(SiteKind, u32, &'static str)> {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').collect();
    let tld = labels.last().copied().unwrap_or_default();
    // A second-level label under a country code, as in `go.ke`, `gov.uk` or `ac.ke`.
    let second = match labels.len() {
        3.. if tld.len() == 2 => labels[labels.len() - 2],
        _ => "",
    };
    if matches!(tld, "gov" | "mil") || matches!(second, "gov" | "go" | "gob" | "gouv" | "govt") {
        return Some((SiteKind::Government, 10, "a government domain"));
    }
    if tld == "edu" || matches!(second, "edu" | "ac" | "sch") || labels.contains(&"k12") {
        return Some((SiteKind::Education, 8, "an education domain"));
    }
    match tld {
        "ngo" | "charity" => Some((SiteKind::Nonprofit, 5, "a nonprofit domain")),
        "org" => Some((SiteKind::Nonprofit, 1, "an .org domain")),
        "church" => Some((SiteKind::Community, 5, "a church domain")),
        "shop" | "store" => Some((SiteKind::Ecommerce, 3, "a shop domain")),
        "news" | "press" => Some((SiteKind::Media, 4, "a news domain")),
        "health" | "clinic" | "dental" | "doctor" => Some((SiteKind::Health, 4, "a health domain")),
        "events" => Some((SiteKind::Events, 4, "an events domain")),
        "blog" | "me" => Some((SiteKind::Personal, 2, "a personal domain")),
        _ => None,
    }
}

/// Proposes a kind from `evidence` (spec 3.5 §7.16.1), or `None` when nothing points at
/// one and the owner is asked.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::FeatureKind;
/// use adk_website::kinds::{KindEvidence, detect};
/// use awp_types::SiteKind;
///
/// let shop = KindEvidence {
///     features: vec![FeatureKind::Shop],
///     markers: vec!["woocommerce".into()],
///     ..KindEvidence::default()
/// };
/// assert_eq!(detect(&shop).unwrap().kind, SiteKind::Ecommerce);
/// assert!(detect(&KindEvidence::default()).is_none());
/// ```
pub fn detect(evidence: &KindEvidence) -> Option<KindProposal> {
    let mut scores: BTreeMap<SiteKind, (u32, Vec<String>)> = BTreeMap::new();
    let mut add = |kind: SiteKind, weight: u32, reason: String| {
        let entry = scores.entry(kind).or_default();
        entry.0 += weight;
        entry.1.push(reason);
    };
    if let Some((kind, weight, reason)) = evidence.domain.as_deref().and_then(domain_kind) {
        add(kind, weight, reason.to_string());
    }
    for feature in &evidence.features {
        let (kind, weight) = match feature {
            FeatureKind::Shop => (SiteKind::Ecommerce, 6),
            FeatureKind::Bookings => (SiteKind::Business, 3),
            FeatureKind::ContactForm => (SiteKind::Business, 1),
            FeatureKind::Newsletter => (SiteKind::Personal, 1),
            FeatureKind::Comments => (SiteKind::Personal, 1),
            _ => continue,
        };
        add(kind, weight, format!("the site has a {feature:?} feature"));
    }
    for found in &evidence.types {
        let lookup = |name: &str| {
            let key = name.to_ascii_lowercase().replace([' ', '_', '-'], "");
            let key = key.trim_start_matches("schema:").trim_start_matches("https://schema.org/");
            TYPES.iter().find(|(known, _, _)| *known == key)
        };
        // A schema.org subtype counts as its nearest listed ancestor: a `Bakery` is a
        // `LocalBusiness`.
        let entry = lookup(found).or_else(|| {
            adk_site::migrate::sources::schema_org_ancestry(found).into_iter().find_map(lookup)
        });
        if let Some((_, kind, weight)) = entry {
            add(*kind, *weight, format!("its pages describe it as {found}"));
        }
    }
    for marker in &evidence.markers {
        if let Some((_, kind, weight)) = MARKERS.iter().find(|(name, _, _)| *name == marker) {
            add(*kind, *weight, format!("its pages carry `{marker}`"));
        }
    }
    let total: usize = evidence.items.values().sum();
    let count = |kind: ContentKind| evidence.items.get(&kind).copied().unwrap_or(0);
    let (products, posts, articles) =
        (count(ContentKind::Product), count(ContentKind::Post), count(ContentKind::Article));
    if total >= 3 && products * 3 >= total {
        add(SiteKind::Ecommerce, 4, format!("{products} of its {total} items are products"));
    }
    if total >= 3 && posts * 2 > total && products == 0 {
        add(SiteKind::Personal, 3, format!("{posts} of its {total} items are posts"));
    }
    if total >= 3 && articles * 2 > total {
        add(SiteKind::Media, 3, format!("{articles} of its {total} items are articles"));
    }
    let mut ranked: Vec<(SiteKind, u32, Vec<String>)> =
        scores.into_iter().map(|(kind, (score, reasons))| (kind, score, reasons)).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut ranked = ranked.into_iter();
    let (kind, score, reasons) = ranked.next()?;
    Some(KindProposal {
        kind,
        score,
        reasons,
        alternatives: ranked.map(|(kind, ..)| kind).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(domain: Option<&str>, markers: &[&str], types: &[&str]) -> KindEvidence {
        KindEvidence {
            domain: domain.map(str::to_string),
            markers: markers.iter().map(|m| (*m).to_string()).collect(),
            types: types.iter().map(|t| (*t).to_string()).collect(),
            ..KindEvidence::default()
        }
    }

    #[test]
    fn a_schema_org_subtype_counts_as_its_listed_ancestor() {
        assert_eq!(detect(&evidence(None, &[], &["Bakery"])).unwrap().kind, SiteKind::Business);
        assert_eq!(detect(&evidence(None, &[], &["HairSalon"])).unwrap().kind, SiteKind::Business);
        assert_eq!(detect(&evidence(None, &[], &["Pharmacy"])).unwrap().kind, SiteKind::Health);
        assert_eq!(detect(&evidence(None, &[], &["Synagogue"])).unwrap().kind, SiteKind::Community);
        assert!(detect(&evidence(None, &[], &["Recipe"])).is_none());
    }

    #[test]
    fn the_spec_examples_detect_their_kinds() {
        let cases = [
            (evidence(None, &["woocommerce"], &[]), SiteKind::Ecommerce),
            (evidence(Some("www.nakuru.go.ke"), &[], &[]), SiteKind::Government),
            (evidence(Some("parks.gov.uk"), &[], &[]), SiteKind::Government),
            (evidence(Some("data.gov"), &[], &[]), SiteKind::Government),
            (evidence(None, &["give-form"], &[]), SiteKind::Nonprofit),
            (evidence(None, &[], &["Blog"]), SiteKind::Personal),
            (evidence(Some("uon.ac.ke"), &[], &[]), SiteKind::Education),
            (evidence(None, &[], &["MedicalClinic"]), SiteKind::Health),
            (evidence(None, &[], &["NewsMediaOrganization"]), SiteKind::Media),
            (evidence(None, &[], &["PlaceOfWorship"]), SiteKind::Community),
            (evidence(None, &[], &["LocalBusiness"]), SiteKind::Business),
        ];
        for (evidence, kind) in cases {
            let proposal = detect(&evidence).unwrap();
            assert_eq!(proposal.kind, kind, "{evidence:?}: {proposal:?}");
            assert!(!proposal.reasons.is_empty());
        }
    }

    #[test]
    fn a_blog_of_posts_is_personal() {
        let mut blog = KindEvidence::default();
        blog.items.insert(ContentKind::Post, 12);
        blog.items.insert(ContentKind::Page, 3);
        blog.features.push(FeatureKind::Newsletter);
        assert_eq!(detect(&blog).unwrap().kind, SiteKind::Personal);
    }
}
