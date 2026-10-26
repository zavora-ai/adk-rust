//! `Presence`: business profiles, listings, reviews and social pages (spec 3.5 §7.8 "Sources",
//! §7.18 stage 2, S2). Every fact found is `Assumed`, with its source, until the owner
//! confirms it.

use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use super::super::error::Result;
use super::super::html::{PageAnalysis, analyse};
use super::super::http::FetchOptions;
use super::super::inventory::{Fact, FactSource, ReviewQuote, SiteInventory, SourceKind};
use super::super::{SiteSource, SourceContext};
use super::schema_types::schema_org_ancestry;
use crate::gate::FactStatus;

/// The JSON-LD item a profile describes its subject with: the most specific schema.org type
/// under `Organization`, `Person` or `PlaceOfWorship` (`Bakery` and the rest of the
/// `LocalBusiness` tree included), first in the page on a tie. Returns the type as written
/// and the item.
fn subject(page: &PageAnalysis) -> Option<(String, Value)> {
    fn types(value: &Value) -> Vec<String> {
        match value.get("@type") {
            Some(Value::String(t)) => vec![t.clone()],
            Some(Value::Array(ts)) => {
                ts.iter().filter_map(Value::as_str).map(String::from).collect()
            }
            _ => Vec::new(),
        }
    }
    fn walk(value: &Value, out: &mut Vec<(usize, String, Value)>) {
        match value {
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            Value::Object(map) => {
                let best = types(value)
                    .into_iter()
                    .map(|t| (schema_org_ancestry(&t), t))
                    .filter(|(chain, _)| {
                        chain
                            .iter()
                            .any(|t| matches!(*t, "Organization" | "Person" | "PlaceOfWorship"))
                    })
                    .max_by_key(|(chain, _)| chain.len());
                if let Some((chain, name)) = best {
                    out.push((chain.len(), name, value.clone()));
                }
                if let Some(graph) = map.get("@graph") {
                    walk(graph, out);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for value in &page.json_ld {
        walk(value, &mut found);
    }
    let depth = found.iter().map(|(depth, ..)| *depth).max()?;
    found.into_iter().find(|(d, ..)| *d == depth).map(|(_, name, value)| (name, value))
}

/// Reads public profiles of a business or person: listings, review pages and social pages.
///
/// Facts come from schema.org JSON-LD and Open Graph tags; each is `Assumed` and names the page
/// and property it came from. The subject is the most specific organisation, business,
/// person or place of worship on the page — any schema.org subtype, such as `Bakery` — and
/// its type is recorded as the `@type` fact. Reviews keep the reviewer's public name; none is usable on a
/// page until the owner approves it ([`ReviewQuote::usable`]).
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::migrate::{SiteSource, SourceContext, sources::Presence};
///
/// # async fn demo(ctx: &SourceContext) -> adk_site::migrate::Result<()> {
/// let presence = Presence::new(["https://listings.example/biz/example-solar".parse().unwrap()]);
/// let inventory = presence.inventory(ctx).await?;
/// assert!(inventory.facts.iter().all(|f| f.status == adk_site::gate::FactStatus::Assumed));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Presence {
    profiles: Vec<Url>,
}

impl Presence {
    /// A source over these profile pages.
    pub fn new(profiles: impl IntoIterator<Item = Url>) -> Self {
        Self { profiles: profiles.into_iter().collect() }
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(scalar).collect();
            (!parts.is_empty()).then(|| parts.join("; "))
        }
        Value::Object(map) => {
            if let Some(name) = map.get("name").and_then(scalar) {
                return Some(name);
            }
            let parts: Vec<String> = [
                "streetAddress",
                "addressLocality",
                "addressRegion",
                "postalCode",
                "addressCountry",
            ]
            .iter()
            .filter_map(|k| map.get(*k).and_then(scalar))
            .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

#[async_trait]
impl SiteSource for Presence {
    async fn inventory(&self, ctx: &SourceContext) -> Result<SiteInventory> {
        let mut inventory = SiteInventory::new(SourceKind::Presence, ctx.clock.now());
        for profile in &self.profiles {
            let fetched = match ctx.http.fetch(profile, &FetchOptions::crawler().following(3)).await
            {
                Ok(fetched) if fetched.status == 200 && fetched.is_html() => fetched,
                Ok(fetched) => {
                    inventory.notes.push(format!("{profile}: HTTP {}", fetched.status));
                    continue;
                }
                Err(error) => {
                    inventory.notes.push(format!("{profile}: {error}"));
                    continue;
                }
            };
            let retrieved_at = ctx.clock.now();
            let page = analyse(&fetched.text(), &fetched.url);
            let host = profile.host_str().unwrap_or("profile").to_string();
            let add = |inventory: &mut SiteInventory, key: &str, value: String, locator: String| {
                let n = inventory
                    .facts
                    .iter()
                    .filter(|f| f.id.starts_with(&format!("presence:{host}:{key}:")))
                    .count();
                inventory.facts.push(Fact {
                    id: format!("presence:{host}:{key}:{n}"),
                    key: key.to_string(),
                    value,
                    source: FactSource { url: profile.to_string(), retrieved_at, locator },
                    status: FactStatus::Assumed,
                });
            };
            let subject = subject(&page);
            if let Some((kind, subject)) = &subject {
                // The type itself is a fact: kind detection reads it (spec 3.5 §7.16.1).
                add(&mut inventory, "@type", kind.clone(), format!("json-ld {kind}.@type"));
                for key in [
                    "name",
                    "description",
                    "telephone",
                    "email",
                    "url",
                    "address",
                    "openingHours",
                    "priceRange",
                    "foundingDate",
                    "areaServed",
                    "sameAs",
                ] {
                    if let Some(value) = subject.get(key).and_then(scalar) {
                        add(&mut inventory, key, value, format!("json-ld {kind}.{key}"));
                    }
                }
                if let Some(specs) =
                    subject.get("openingHoursSpecification").and_then(Value::as_array)
                {
                    for spec in specs {
                        let days = spec.get("dayOfWeek").and_then(scalar).unwrap_or_default();
                        let opens = spec.get("opens").and_then(scalar).unwrap_or_default();
                        let closes = spec.get("closes").and_then(scalar).unwrap_or_default();
                        add(
                            &mut inventory,
                            "openingHours",
                            format!("{days} {opens}-{closes}"),
                            format!("json-ld {kind}.openingHoursSpecification"),
                        );
                    }
                }
                if let Some(rating) = subject.get("aggregateRating") {
                    let value = rating.get("ratingValue").and_then(scalar);
                    let count =
                        rating.get("reviewCount").or(rating.get("ratingCount")).and_then(scalar);
                    if let Some(value) = value {
                        add(
                            &mut inventory,
                            "aggregateRating",
                            format!("{value} from {} reviews", count.unwrap_or_else(|| "?".into())),
                            format!("json-ld {kind}.aggregateRating"),
                        );
                    }
                }
                let reviews = match subject.get("review") {
                    Some(Value::Array(reviews)) => reviews.clone(),
                    Some(review) => vec![review.clone()],
                    None => Vec::new(),
                };
                for review in reviews {
                    let Some(text) = review.get("reviewBody").and_then(scalar) else { continue };
                    inventory.reviews.push(ReviewQuote {
                        source: FactSource {
                            url: profile.to_string(),
                            retrieved_at,
                            locator: format!("json-ld {kind}.review"),
                        },
                        author: review.get("author").and_then(scalar),
                        rating: review
                            .pointer("/reviewRating/ratingValue")
                            .and_then(|v| {
                                v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                            })
                            .map(|r| r.round().clamp(1.0, 5.0) as u8),
                        text,
                        approved_by: None,
                    });
                }
            }
            for (property, key) in [("og:title", "name"), ("og:description", "description")] {
                if let Some(value) = page.meta.open_graph.get(property)
                    && !inventory
                        .facts
                        .iter()
                        .any(|f| f.key == key && f.source.url == profile.as_str())
                {
                    add(&mut inventory, key, value.clone(), format!("meta {property}"));
                }
            }
            if subject.is_none() && page.meta.open_graph.is_empty() {
                inventory.notes.push(format!("{profile}: no structured profile found"));
            }
        }
        tracing::info!(
            facts = inventory.facts.len(),
            reviews = inventory.reviews.len(),
            "presence read"
        );
        Ok(inventory)
    }
}
