//! The inventory a source produces (spec 3.5 §7.8 "Sources", §7.18 stage 2).

use std::collections::BTreeMap;

use awp_types::{ContentKind, Digest};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gate::FactStatus;

/// Which source produced an inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceKind {
    /// The public `wp-json/wp/v2` collections.
    WordPressRest,
    /// A WordPress export file.
    Wxr,
    /// A crawl of any site.
    Crawl,
    /// Business profiles, listings, reviews and social pages.
    Presence,
    /// A database dump and uploads replayed in an isolated container.
    IsolatedReplay,
}

/// Everything a source found: URLs, items, media, taxonomies, menus, dynamic features and,
/// for a presence, facts and reviews (spec 3.5 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::{SiteInventory, SourceKind};
///
/// let inventory = SiteInventory::new(SourceKind::Crawl, chrono::Utc::now());
/// assert!(inventory.items.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteInventory {
    /// The source.
    pub source: SourceKind,
    /// The legacy site's public origin, such as `https://example.com`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The site's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    /// The site's description or tagline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The site's language, when it declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// When the inventory was taken.
    pub taken_at: DateTime<Utc>,
    /// Every URL seen, with how it answered.
    #[serde(default)]
    pub urls: Vec<InventoryUrl>,
    /// Posts, pages, products and other items.
    #[serde(default)]
    pub items: Vec<InventoryItem>,
    /// Uploads and other media.
    #[serde(default)]
    pub media: Vec<InventoryMedia>,
    /// Categories, tags and other taxonomies.
    #[serde(default)]
    pub taxonomies: Vec<Taxonomy>,
    /// Navigation menus.
    #[serde(default)]
    pub menus: Vec<Menu>,
    /// Dynamic features found: forms, search, comments, shop, bookings, logins.
    #[serde(default)]
    pub features: Vec<DetectedFeature>,
    /// Facts found, each with its source; every one `Assumed` until the owner confirms it.
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// Reviews found on public platforms.
    #[serde(default)]
    pub reviews: Vec<ReviewQuote>,
    /// What the source did not read, and why.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl SiteInventory {
    /// An empty inventory.
    pub fn new(source: SourceKind, taken_at: DateTime<Utc>) -> Self {
        Self {
            source,
            origin: None,
            site_name: None,
            description: None,
            language: None,
            taken_at,
            urls: Vec::new(),
            items: Vec::new(),
            media: Vec::new(),
            taxonomies: Vec::new(),
            menus: Vec::new(),
            features: Vec::new(),
            facts: Vec::new(),
            reviews: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The item with `key`.
    pub fn item(&self, key: &str) -> Option<&InventoryItem> {
        self.items.iter().find(|item| item.key == key)
    }

    /// The URL record for `path`.
    pub fn url(&self, path: &str) -> Option<&InventoryUrl> {
        self.urls.iter().find(|url| url.path == path)
    }

    /// Records a feature, merging evidence into an existing one of the same kind.
    pub fn add_feature(&mut self, feature: DetectedFeature) {
        match self.features.iter_mut().find(|f| f.kind == feature.kind) {
            Some(existing) => {
                for evidence in feature.evidence {
                    if !existing.evidence.contains(&evidence) {
                        existing.evidence.push(evidence);
                    }
                }
                for field in feature.fields {
                    if !existing.fields.iter().any(|f| f.name == field.name) {
                        existing.fields.push(field);
                    }
                }
            }
            None => self.features.push(feature),
        }
    }

    /// The facts found, as a claim check: each one `Assumed`, so a page that cites one fails
    /// the launch gate until the owner confirms it (A-I18, S2).
    pub fn fact_table(&self) -> crate::gate::FactTable {
        crate::gate::FactTable::new(
            self.facts.iter().map(|f| (awp_types::FactRef(f.id.clone()), f.status)),
        )
    }

    /// Whether a feature of `kind` was found.
    pub fn has_feature(&self, kind: FeatureKind) -> bool {
        self.features.iter().any(|f| f.kind == kind)
    }

    /// The `n` most important HTML pages answering 200: the home page first, then by inbound
    /// internal links, then by path (spec 3.5 §7.8.1 "top N pages").
    pub fn top_pages(&self, n: usize) -> Vec<&InventoryUrl> {
        let mut pages: Vec<&InventoryUrl> =
            self.urls.iter().filter(|u| u.status == 200 && u.page.is_some()).collect();
        pages.sort_by(|a, b| {
            (b.path == "/")
                .cmp(&(a.path == "/"))
                .then(b.inbound_links.cmp(&a.inbound_links))
                .then(a.path.cmp(&b.path))
        });
        pages.truncate(n);
        pages
    }
}

/// One URL of the legacy site and how it answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryUrl {
    /// The absolute public URL.
    pub url: String,
    /// Its path and query, such as `/about/`.
    pub path: String,
    /// The HTTP status, or 200 for a URL read from an export.
    pub status: u16,
    /// Where a redirect points, as a path on the same site or an absolute URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_to: Option<String>,
    /// The response's media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The digest of the response body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Digest>,
    /// The item this URL shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// What an HTML page says about itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageMeta>,
    /// Internal links pointing here.
    #[serde(default)]
    pub inbound_links: u32,
    /// The body as read, for the detectors; never serialised.
    #[serde(skip)]
    pub body: Option<Bytes>,
}

/// The metadata and main text of an HTML page (spec 3.5 §7.8.1 "metadata preserved",
/// "main-text similarity").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    /// `<title>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `<meta name="description">`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `<link rel="canonical">`, as a path when it is on the same site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    /// `<html lang>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Open Graph properties, such as `og:title`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub open_graph: BTreeMap<String, String>,
    /// The visible text of the main content, whitespace collapsed.
    #[serde(default)]
    pub main_text: String,
}

/// Where an item stands on the legacy site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemStatus {
    /// Public.
    Published,
    /// A draft, read with the owner's credential.
    Draft,
    /// Waiting for review.
    Pending,
    /// Scheduled for later.
    Scheduled,
    /// Visible to logged-in users only.
    Private,
}

/// A post, page, product or other item of the legacy site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    /// A key stable within the source, such as `wp:post:42`.
    pub key: String,
    /// The content kind it becomes.
    pub kind: ContentKind,
    /// Its slug.
    pub slug: String,
    /// Its public URL, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Its URL path, which the migrated item keeps.
    pub path: String,
    /// Its title.
    pub title: String,
    /// Its body as the legacy site stores or renders it: HTML, untrusted.
    #[serde(default)]
    pub html: String,
    /// Its excerpt, as HTML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// Where it stands.
    pub status: ItemStatus,
    /// When it was published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    /// When it was last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
    /// Its language, when the source knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Category names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Tag names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Structured fields, such as `price`, `currency` and `sku` for a product.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
    /// URLs of its media.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<String>,
    /// Its author's display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// An upload or other media file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryMedia {
    /// Its public URL.
    pub url: String,
    /// Its path on the legacy site, such as `/wp-content/uploads/2026/10/a.jpg`.
    pub path: String,
    /// The media type the legacy site declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_type: Option<String>,
    /// Its alt text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// The digest of its bytes, when they were read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Digest>,
    /// Its size in bytes, when it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Its bytes, when the source read them; never serialised.
    #[serde(skip)]
    pub bytes: Option<Bytes>,
}

impl InventoryMedia {
    /// A media record for `url` at `path` with nothing read yet.
    pub fn new(url: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            path: path.into(),
            declared_type: None,
            alt: None,
            sha256: None,
            size: None,
            bytes: None,
        }
    }

    /// Attaches the bytes read, with their digest and size.
    #[must_use]
    pub fn with_bytes(mut self, bytes: Bytes) -> Self {
        self.sha256 = Some(crate::release::file_digest(&bytes));
        self.size = Some(bytes.len() as u64);
        self.bytes = Some(bytes);
        self
    }
}

/// A taxonomy, such as categories or tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Taxonomy {
    /// Its name, such as `category` or `post_tag`.
    pub name: String,
    /// Its terms.
    pub terms: Vec<Term>,
}

/// One term of a taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    /// Its slug.
    pub slug: String,
    /// Its name.
    pub name: String,
    /// Its parent's slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Its archive URL path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A navigation menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Menu {
    /// Its name.
    pub name: String,
    /// Its entries, in order.
    pub entries: Vec<MenuEntry>,
}

/// One menu entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuEntry {
    /// Its label.
    pub title: String,
    /// Where it points: a path on the site or an absolute URL.
    pub target: String,
    /// Its parent entry's position in `entries`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
}

/// A dynamic feature the static site replaces (spec 3.5 §7.8 "Feature mapping").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeatureKind {
    /// A contact form.
    ContactForm,
    /// Site search.
    Search,
    /// Comments.
    Comments,
    /// A shop: products, cart, checkout.
    Shop,
    /// Bookings or appointments.
    Bookings,
    /// A login form.
    Login,
    /// A member area behind a login.
    MemberArea,
    /// A newsletter sign-up.
    Newsletter,
}

/// Where a feature was seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureEvidence {
    /// The page or document.
    pub url: String,
    /// What was seen, such as `form action=/wp-comments-post.php`.
    pub detail: String,
}

/// A field of a form the feature uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    /// The field's name.
    pub name: String,
    /// Its input type, such as `email` or `textarea`.
    pub input_type: String,
    /// Whether it is required.
    #[serde(default)]
    pub required: bool,
}

/// A dynamic feature found on the legacy site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedFeature {
    /// What it is.
    pub kind: FeatureKind,
    /// Where it was seen.
    pub evidence: Vec<FeatureEvidence>,
    /// The fields of its form, when it has one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FormField>,
}

/// Where a fact was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactSource {
    /// The page or document.
    pub url: String,
    /// When it was read.
    pub retrieved_at: DateTime<Utc>,
    /// Where on the page: a JSON-LD property, a meta tag.
    pub locator: String,
}

/// A fact found during discovery: `Assumed` until the owner confirms it, so it never appears
/// as a published claim (spec 3.5 §7.18 stage 2, A-I18, S2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    /// A stable id, the [`FactRef`](awp_types::FactRef) content items cite.
    pub id: String,
    /// What it is about, such as `telephone` or `openingHours`.
    pub key: String,
    /// The value found.
    pub value: String,
    /// Where it was found.
    pub source: FactSource,
    /// Its standing; sources only ever produce `Assumed`.
    pub status: FactStatus,
}

/// A review found on a public platform (spec 3.5 S2).
///
/// A quote appears on a page only with the reviewer's public attribution and the owner's
/// approval: [`usable`](Self::usable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQuote {
    /// The platform's page.
    pub source: FactSource,
    /// The reviewer's public display name, when the platform shows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The rating, one to five.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// The review's text.
    pub text: String,
    /// The owner's decision that approved quoting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
}

impl ReviewQuote {
    /// Whether the quote may appear on a page: it has a public attribution and the owner
    /// approved it.
    pub fn usable(&self) -> bool {
        self.author.as_deref().is_some_and(|a| !a.trim().is_empty()) && self.approved_by.is_some()
    }
}
