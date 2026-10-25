//! [`SiteSource`] implementations (spec 3.5 §7.8 "Sources", §7.18
//! "Starting point") and their conformance suite.
//!
//! | Source | Reads | Needs |
//! |--------|-------|-------|
//! | [`WordPressRest`] | The public `wp-json/wp/v2` collections, WooCommerce's Store API; drafts with a read-scoped application password from the credential broker | Drafts: a proven domain |
//! | [`Wxr`] | A WordPress export file, with its uploads directory when given | Nothing |
//! | [`Crawl`] | Any site: sitemaps and links, robots-respecting, rate-limited, signed | Nothing |
//! | [`Presence`] | Business profiles, listings, reviews and social pages: facts as `Assumed` | Nothing |
//! | [`IsolatedReplay`] | A database dump and uploads served by a container on a loopback-only network, crawled, then destroyed | A proven domain |

mod crawl;
mod presence;
mod replay;
mod wordpress;
mod wxr;

use std::collections::BTreeSet;

use awp_types::ContentKind;
use chrono::{DateTime, Utc};
use serde_json::Value;
use url::Url;

pub use crawl::Crawl;
pub use presence::Presence;
pub use replay::{
    ContainerDriver, DockerCliDriver, IsolatedReplay, ReplayContainer, ReplayReport, ReplaySpec,
};
pub use wordpress::WordPressRest;
pub use wxr::Wxr;

use super::html::{PageAnalysis, TermLink};
use super::inventory::{InventoryItem, ItemStatus, SiteInventory};
use super::{SiteSource, SourceContext};
use crate::gate::FactStatus;

/// Checks what every source promises: URLs and items keyed and pathed consistently, every
/// fact `Assumed`, and an inventory that round-trips through JSON. Panics on a violation;
/// call it from a test.
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::migrate::sources::{Wxr, source_conformance};
/// # async fn demo(ctx: &adk_site::migrate::SourceContext) {
/// let source = Wxr::from_bytes(std::fs::read("export.xml").unwrap());
/// let inventory = source_conformance(&source, ctx).await;
/// assert!(!inventory.items.is_empty());
/// # }
/// ```
pub async fn source_conformance(source: &dyn SiteSource, ctx: &SourceContext) -> SiteInventory {
    let inventory = source.inventory(ctx).await.expect("the source reads its fixture");
    let mut keys = BTreeSet::new();
    for item in &inventory.items {
        assert!(keys.insert(item.key.clone()), "item key {} is unique", item.key);
        assert!(item.path.starts_with('/'), "item {} has a path, not {}", item.key, item.path);
        assert!(
            !item.title.trim().is_empty() || item.kind == ContentKind::Page,
            "item {} has a title",
            item.key
        );
    }
    let mut paths = BTreeSet::new();
    for url in &inventory.urls {
        assert!(url.path.starts_with('/'), "URL {} has a path", url.url);
        assert!(paths.insert(url.path.clone()), "URL path {} is listed once", url.path);
        if let Some(key) = &url.item {
            assert!(keys.contains(key), "URL {} names a known item", url.url);
        }
        if (300..400).contains(&url.status) {
            assert!(url.redirect_to.is_some(), "redirect {} records its target", url.url);
        }
    }
    for media in &inventory.media {
        assert!(media.path.starts_with('/'), "media {} has a path", media.url);
    }
    for fact in &inventory.facts {
        assert_eq!(fact.status, FactStatus::Assumed, "fact {} is Assumed until confirmed", fact.id);
        assert!(!fact.source.url.is_empty(), "fact {} names its source", fact.id);
    }
    for review in &inventory.reviews {
        assert!(review.approved_by.is_none(), "a source never approves a review quote");
    }
    let json = serde_json::to_string(&inventory).expect("the inventory serialises");
    let back: SiteInventory = serde_json::from_str(&json).expect("the inventory parses");
    assert_eq!(back.items, inventory.items, "items round-trip");
    assert_eq!(back.urls.len(), inventory.urls.len(), "URLs round-trip");
    inventory
}

/// Reads a sitemap or sitemap index: the page URLs, and the nested sitemaps.
pub(crate) fn parse_sitemap(text: &str) -> (Vec<String>, Vec<String>) {
    let Ok(root) = super::xml::parse(text, "sitemap") else { return (Vec::new(), Vec::new()) };
    let local = |name: &str| name.rsplit(':').next().unwrap_or(name).to_string();
    let mut pages = Vec::new();
    let mut nested = Vec::new();
    let is_index = local(&root.name) == "sitemapindex";
    for child in &root.children {
        let Some(loc) = child.children.iter().find(|c| local(&c.name) == "loc") else { continue };
        let loc = loc.text.trim().to_string();
        if loc.is_empty() {
            continue;
        }
        if is_index { nested.push(loc) } else { pages.push(loc) }
    }
    (pages, nested)
}

fn json_ld_items(value: &Value, kind: &str, out: &mut Vec<Value>) {
    match value {
        Value::Array(items) => items.iter().for_each(|v| json_ld_items(v, kind, out)),
        Value::Object(map) => {
            let typed = match map.get("@type") {
                Some(Value::String(t)) => t == kind,
                Some(Value::Array(ts)) => ts.iter().any(|t| t.as_str() == Some(kind)),
                _ => false,
            };
            if typed {
                out.push(value.clone());
            }
            if let Some(graph) = map.get("@graph") {
                json_ld_items(graph, kind, out);
            }
        }
        _ => {}
    }
}

/// The first JSON-LD item of `kind` on a page.
pub(crate) fn json_ld_item(page: &PageAnalysis, kind: &str) -> Option<Value> {
    let mut found = Vec::new();
    for value in &page.json_ld {
        json_ld_items(value, kind, &mut found);
    }
    found.into_iter().next()
}

fn date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// An item for a crawled HTML page, or `None` for listings and archives that the generator
/// rebuilds (spec 3.5 §7.8.2: collections, category and tag pages).
pub(crate) fn page_item(
    url: &Url,
    path: &str,
    page: &PageAnalysis,
    body_class: &str,
) -> Option<InventoryItem> {
    let class = body_class.to_ascii_lowercase();
    let listing =
        ["archive", "category", "tag", "author", "date", "paged", "blog", "search", "error404"]
            .iter()
            .any(|c| {
                class.split_whitespace().any(|w| w == *c || w.starts_with(&format!("{c}-paged")))
            });
    let product = json_ld_item(page, "Product");
    let article = json_ld_item(page, "BlogPosting")
        .or_else(|| json_ld_item(page, "Article"))
        .or_else(|| json_ld_item(page, "NewsArticle"));
    if (listing && path != "/") || path.ends_with(".php") {
        return None;
    }
    let kind = if class.contains("single-product") || product.is_some() {
        ContentKind::Product
    } else if class.contains("single-post") || (article.is_some() && !class.contains("page")) {
        ContentKind::Post
    } else {
        ContentKind::Page
    };
    let title = page
        .meta
        .open_graph
        .get("og:title")
        .cloned()
        .or_else(|| page.meta.title.clone())
        .unwrap_or_default();
    let slug = path.trim_matches('/').rsplit('/').next().unwrap_or_default().to_string();
    let mut item = InventoryItem {
        key: format!("crawl:{path}"),
        kind,
        slug: if slug.is_empty() { "index".into() } else { slug },
        url: Some(url.to_string()),
        path: path.to_string(),
        title,
        html: page.main_html.clone(),
        excerpt: page.meta.description.clone(),
        status: ItemStatus::Published,
        published_at: None,
        modified_at: None,
        language: page.meta.language.clone(),
        categories: Vec::new(),
        tags: Vec::new(),
        fields: Default::default(),
        media: page.media.iter().map(|(u, _)| u.to_string()).collect(),
        author: None,
    };
    for (name, kind) in &page.terms {
        let list = match kind {
            TermLink::Category => &mut item.categories,
            TermLink::Tag => &mut item.tags,
        };
        if !list.contains(name) {
            list.push(name.clone());
        }
    }
    if let Some(article) = &article {
        item.published_at = date(article.get("datePublished"));
        item.modified_at = date(article.get("dateModified"));
        item.author = article
            .get("author")
            .and_then(|a| a.get("name").or(Some(a)))
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    if let Some(product) = &product {
        if let Some(sku) = product.get("sku").and_then(Value::as_str) {
            item.fields.insert("sku".into(), Value::String(sku.into()));
        }
        let offer = product.get("offers").map(|o| {
            if let Value::Array(a) = o { a.first().cloned().unwrap_or_default() } else { o.clone() }
        });
        if let Some(offer) = offer {
            let price = offer
                .get("price")
                .and_then(|p| p.as_f64().or_else(|| p.as_str().and_then(|s| s.parse().ok())));
            if let Some(price) = price.and_then(serde_json::Number::from_f64) {
                item.fields.insert("price".into(), Value::Number(price));
            }
            if let Some(currency) = offer.get("priceCurrency").and_then(Value::as_str) {
                item.fields.insert("currency".into(), Value::String(currency.into()));
            }
        }
    }
    Some(item)
}
