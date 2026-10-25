//! Inventory to content (spec 3.5 §7.8.2 "Content model", S3, S17): every item becomes a
//! [`ContentItem`] with a sanitised Markdown body that keeps its URL, uploads keep their paths,
//! and taxonomy, feed and legacy redirects become 301s.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, LanguageTag, Markdown, Redirect, Schedule,
};
use bytes::Bytes;
use regex::Regex;
use serde_json::Value;
use url::Url;

use super::error::Result;
use super::inventory::{ItemStatus, SiteInventory, SourceKind};
use super::sanitize::{Dropped, Sanitiser};
use crate::content::kind_directory;
use crate::media::{MediaPipeline, MediaPolicy, MediaUpload};

/// How to convert.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// The site's default language, for items whose language the source does not know.
    pub language: LanguageTag,
    /// The sanitiser.
    pub sanitiser: Sanitiser,
    /// Item keys and media paths left out: refused or quarantined subjects.
    pub excluded: BTreeSet<String>,
    /// The media pipeline uploads go through (type sniffing, metadata stripped).
    pub media: MediaPolicy,
}

impl ConvertOptions {
    /// Options for a site in `language` with the default sanitiser and media policy.
    pub fn new(language: LanguageTag) -> Self {
        Self {
            language,
            sanitiser: Sanitiser::new(),
            excluded: BTreeSet::new(),
            media: MediaPolicy::default(),
        }
    }

    /// Leaves out these item keys and media paths.
    #[must_use]
    pub fn excluding(mut self, keys: impl IntoIterator<Item = String>) -> Self {
        self.excluded.extend(keys);
        self
    }
}

/// The converted site.
#[derive(Debug, Clone, Default)]
pub struct Converted {
    /// The items, sorted by id.
    pub items: Vec<ContentItem>,
    /// Uploads by their legacy path, for [`SiteProject::documents`](crate::content::SiteProject::documents),
    /// so they keep their URLs.
    pub documents: BTreeMap<String, Bytes>,
    /// Redirects for the site configuration: legacy redirects, taxonomy archives, feeds and
    /// items whose URL could not be kept.
    pub redirects: Vec<Redirect>,
    /// Item key to content id.
    pub ids: BTreeMap<String, ContentId>,
    /// Item key to the URL path its page has.
    pub paths: BTreeMap<String, String>,
    /// What the sanitiser dropped, by item key.
    pub dropped: BTreeMap<String, Vec<Dropped>>,
    /// What was not converted, and why.
    pub notes: Vec<String>,
}

/// The built-in generator's tag slug, so a taxonomy archive redirects to its tag page.
fn tag_slug(text: &str) -> String {
    let slug = slug_part(text);
    if slug.is_empty() { "tag".into() } else { slug }
}

fn slug_part(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Removes WordPress shortcodes from stored post content; the features they render are
/// mapped to capabilities instead.
fn strip_shortcodes(content: &str) -> String {
    static SHORTCODE: OnceLock<Regex> = OnceLock::new();
    let shortcode = SHORTCODE.get_or_init(|| {
        Regex::new(r#"\[/?[a-z][a-z0-9_-]*(?:\s+[^\]]*)?/?\]"#).expect("static regex")
    });
    shortcode.replace_all(content, "").into_owned()
}

fn keepable_path(path: &str) -> bool {
    path.starts_with('/') && path.ends_with('/') && !path.contains("..") && !path.contains('?')
}

fn text_of(html: &str) -> String {
    let document = scraper::Html::parse_fragment(html);
    super::html::collapse(&document.root_element().text().collect::<String>())
}

/// Converts an inventory into content items, uploads at their legacy paths and redirects.
///
/// # Errors
///
/// None today; reserved for stores that fail.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::convert::{ConvertOptions, to_content};
/// use adk_site::migrate::{InventoryItem, ItemStatus, SiteInventory, SourceKind};
/// use awp_types::ContentKind;
///
/// let mut inventory = SiteInventory::new(SourceKind::Wxr, chrono::Utc::now());
/// inventory.items.push(InventoryItem {
///     key: "wp:post:1".into(), kind: ContentKind::Post, slug: "hello".into(), url: None,
///     path: "/2026/10/hello/".into(), title: "Hello".into(), html: "<p>Hi</p>".into(),
///     excerpt: None, status: ItemStatus::Published, published_at: None, modified_at: None,
///     language: None, categories: vec![], tags: vec![], fields: Default::default(),
///     media: vec![], author: None,
/// });
/// let converted = to_content(&inventory, &ConvertOptions::new("en".parse().unwrap())).unwrap();
/// assert_eq!(converted.items[0].id.0, "posts/hello/en");
/// assert_eq!(converted.paths["wp:post:1"], "/2026/10/hello/");
/// ```
pub fn to_content(inventory: &SiteInventory, options: &ConvertOptions) -> Result<Converted> {
    let mut converted = Converted::default();
    let origin = inventory.origin.as_deref().and_then(|o| Url::parse(o).ok());
    let raw_content = inventory.source == SourceKind::Wxr;
    let mut used: BTreeSet<(ContentKind, String, String)> = BTreeSet::new();
    let nav: BTreeMap<String, usize> = inventory
        .menus
        .first()
        .map(|menu| {
            menu.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.parent.is_none())
                .map(|(i, e)| (e.target.clone(), i + 1))
                .collect()
        })
        .unwrap_or_default();
    for item in &inventory.items {
        if options.excluded.contains(&item.key) {
            converted.notes.push(format!("{} left out: excluded", item.key));
            continue;
        }
        // A regional tag of the site's language (`en-US` on an `en` site) is the site's language.
        let primary = |tag: &str| tag.split('-').next().unwrap_or(tag).to_ascii_lowercase();
        let language = item
            .language
            .as_deref()
            .filter(|l| primary(l) != primary(options.language.as_str()))
            .and_then(|l| l.parse::<LanguageTag>().ok())
            .unwrap_or_else(|| options.language.clone());
        let home = item.path == "/";
        let mut slug = if home {
            "index".to_string()
        } else {
            let base = if item.kind == ContentKind::Page {
                item.path
                    .trim_matches('/')
                    .split('/')
                    .map(slug_part)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("/")
            } else {
                slug_part(&item.slug)
            };
            if base.is_empty() { slug_part(&item.title) } else { base }
        };
        if slug.is_empty() {
            slug = slug_part(&item.key);
        }
        let mut unique = slug.clone();
        let mut n = 2;
        while !used.insert((item.kind, unique.clone(), language.to_string())) {
            unique = format!("{slug}-{n}");
            n += 1;
        }
        let slug = unique;
        let id = ContentId(format!("{}/{slug}/{language}", kind_directory(item.kind)));
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert("title".into(), Value::String(item.title.clone()));
        if let Some(excerpt) = item.excerpt.as_deref().map(text_of).filter(|e| !e.is_empty()) {
            fields.insert("summary".into(), Value::String(excerpt));
        }
        let page_path = if home {
            "/".to_string()
        } else if keepable_path(&item.path) {
            fields.insert("path".into(), Value::String(item.path.clone()));
            item.path.clone()
        } else {
            let default = match item.kind {
                ContentKind::Page | ContentKind::Collection => format!("/{slug}/"),
                kind => format!("/{}/{slug}/", kind_directory(kind)),
            };
            if item.path.starts_with('/') && !item.path.contains('?') && item.path != default {
                converted.redirects.push(Redirect {
                    from: item.path.clone(),
                    to: default.clone(),
                    status: 301,
                });
            }
            default
        };
        if let Some(at) = item.published_at {
            fields.insert("publishedAt".into(), Value::String(at.to_rfc3339()));
        }
        if let Some(at) = item.modified_at {
            fields.insert("updatedAt".into(), Value::String(at.to_rfc3339()));
        }
        let mut tags: Vec<String> = item.categories.iter().chain(&item.tags).cloned().collect();
        tags.retain(|t| !t.eq_ignore_ascii_case("uncategorized"));
        tags.sort();
        tags.dedup();
        if !tags.is_empty() {
            fields
                .insert("tags".into(), Value::Array(tags.into_iter().map(Value::String).collect()));
        }
        if let Some(author) = &item.author {
            fields.insert("byline".into(), Value::String(author.clone()));
        }
        for (key, value) in &item.fields {
            fields.entry(key.clone()).or_insert_with(|| value.clone());
        }
        if let Some(order) = nav.get(&item.path) {
            fields.insert("navOrder".into(), Value::from(*order as u64));
        }
        let page = origin.as_ref().and_then(|o| o.join(&item.path).ok());
        let body_html = if raw_content { strip_shortcodes(&item.html) } else { item.html.clone() };
        let sanitised = options.sanitiser.sanitise(&body_html, page.as_ref());
        if !sanitised.dropped.is_empty() {
            converted.dropped.insert(item.key.clone(), sanitised.dropped.clone());
        }
        let (state, schedule) = match item.status {
            ItemStatus::Published => (ContentState::Published, None),
            ItemStatus::Scheduled => (
                ContentState::Scheduled,
                item.published_at
                    .map(|at| Schedule { publish_at: Some(at), ..Schedule::default() }),
            ),
            ItemStatus::Draft | ItemStatus::Pending | ItemStatus::Private => {
                (ContentState::Draft, None)
            }
        };
        let content = ContentItem {
            id: id.clone(),
            kind: item.kind,
            slug,
            language,
            fields,
            volatile: if item.kind == ContentKind::Product && item.fields.contains_key("stock") {
                vec!["stock".into()]
            } else {
                Vec::new()
            },
            body: (!sanitised.markdown.trim().is_empty()).then_some(Markdown(sanitised.markdown)),
            media: Vec::new(),
            sources: Vec::new(),
            schedule,
            state,
        };
        converted.ids.insert(item.key.clone(), id);
        converted.paths.insert(item.key.clone(), page_path);
        converted.items.push(content);
    }
    converted.items.sort_by(|a, b| a.id.cmp(&b.id));
    let pipeline = MediaPipeline::new(options.media.clone());
    for media in &inventory.media {
        if options.excluded.contains(&media.path) {
            converted.notes.push(format!("{} left out: excluded", media.path));
            continue;
        }
        let Some(bytes) = &media.bytes else {
            converted.notes.push(format!("{} not copied: its bytes were not read", media.path));
            continue;
        };
        if !media.path.starts_with('/') || media.path.contains("..") || media.path.ends_with('/') {
            converted.notes.push(format!("{} not copied: not a file path", media.path));
            continue;
        }
        let name = media.path.rsplit('/').next().unwrap_or(&media.path);
        match pipeline.process(&MediaUpload {
            name,
            bytes,
            alt: media.alt.clone(),
            generated: false,
        }) {
            // Re-encoded to drop metadata, kept at its legacy path so its URL keeps working.
            Ok(processed) => {
                converted.documents.insert(media.path.clone(), processed.bytes);
            }
            Err(error) => converted.notes.push(format!("{} not copied: {error}", media.path)),
        }
    }
    let listing =
        if inventory.items.iter().any(|i| i.kind == ContentKind::Post) { "/posts/" } else { "/" };
    let published_tags: BTreeSet<String> = converted
        .items
        .iter()
        .filter(|i| i.state == ContentState::Published)
        .filter_map(|i| i.fields.get("tags").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(tag_slug)
        .collect();
    for taxonomy in &inventory.taxonomies {
        for term in &taxonomy.terms {
            let Some(path) = &term.path else { continue };
            if keepable_path(path) {
                let tag = tag_slug(&term.name);
                let to = if published_tags.contains(&tag) {
                    format!("/tags/{tag}/")
                } else {
                    listing.to_string()
                };
                converted.redirects.push(Redirect { from: path.clone(), to, status: 301 });
            }
        }
    }
    let origin_text = inventory.origin.clone().unwrap_or_default();
    for url in &inventory.urls {
        let Some(target) = &url.redirect_to else { continue };
        if !matches!(url.status, 301 | 308) || url.path.contains('?') {
            continue;
        }
        let to = target.strip_prefix(&origin_text).unwrap_or(target).to_string();
        if to.starts_with('/') && to != url.path {
            converted.redirects.push(Redirect { from: url.path.clone(), to, status: 301 });
        }
    }
    let taxonomy_paths: BTreeSet<&String> = inventory
        .taxonomies
        .iter()
        .flat_map(|t| t.terms.iter().filter_map(|term| term.path.as_ref()))
        .collect();
    for url in &inventory.urls {
        // Pages that are no item: archives, listings and server endpoints the static site
        // replaces; their URLs keep answering with a 301.
        if url.status != 200
            || url.item.is_some()
            || url.page.is_none()
            || url.path == "/"
            || url.path.contains('?')
            || taxonomy_paths.contains(&url.path)
        {
            continue;
        }
        let to = if url.path.ends_with(".php") { "/" } else { listing };
        converted.redirects.push(Redirect { from: url.path.clone(), to: to.into(), status: 301 });
    }
    if inventory.urls.iter().any(|u| u.path == "/feed/") {
        converted.redirects.push(Redirect {
            from: "/feed/".into(),
            to: "/feed.xml".into(),
            status: 301,
        });
    }
    let kept: BTreeSet<&String> = converted.paths.values().collect();
    let mut seen = BTreeSet::new();
    converted.redirects.retain(|r| !kept.contains(&r.from) && seen.insert(r.from.clone()));
    converted.redirects.sort_by(|a, b| a.from.cmp(&b.from));
    tracing::info!(
        items = converted.items.len(),
        documents = converted.documents.len(),
        redirects = converted.redirects.len(),
        "inventory converted"
    );
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcodes_are_removed() {
        assert_eq!(
            strip_shortcodes(
                "a [contact-form-7 id=\"1\" title=\"x\"] b [gallery/] [/caption] c [1]"
            ),
            "a  b   c [1]"
        );
    }

    #[test]
    fn slugs_are_url_safe() {
        assert_eq!(slug_part("Hello, World!"), "hello-world");
        assert_eq!(slug_part("  über Café "), "über-café");
        assert_eq!(tag_slug("!!"), "tag");
    }
}
