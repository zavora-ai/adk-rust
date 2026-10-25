//! `Wxr`: a WordPress export file (spec 3.5 §7.8 "Sources").

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use awp_types::ContentKind;
use bytes::Bytes;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use url::Url;

use super::super::error::{MigrateError, Result};
use super::super::html::detect_shortcodes;
use super::super::http::origin_of;
use super::super::inventory::{
    DetectedFeature, FeatureEvidence, FeatureKind, InventoryItem, InventoryMedia, InventoryUrl,
    ItemStatus, Menu, MenuEntry, SiteInventory, SourceKind, Taxonomy, Term,
};
use super::super::xml::{Element, parse};
use super::super::{SiteSource, SourceContext};

/// Post types of booking plugins.
const BOOKING_TYPES: &[&str] = &[
    "bookly",
    "booked_appointments",
    "mphb_room_type",
    "wc_booking",
    "wpbs_calendar",
    "ea_appointments",
];

/// A `nav_menu_item` of an export.
struct MenuItemRecord {
    order: i64,
    id: String,
    title: String,
    parent: Option<String>,
    target: String,
}

/// Reads a WordPress export (WXR) file, and the uploads directory beside it when given.
///
/// An export holds no media bytes; with [`Wxr::with_uploads`] every file in the uploads
/// directory is read, also the ones no attachment names, so files planted there are found.
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::migrate::{SiteSource, SourceContext, sources::Wxr};
///
/// # async fn demo(ctx: &SourceContext) -> adk_site::migrate::Result<()> {
/// let source = Wxr::from_bytes(std::fs::read("export.xml").unwrap()).with_uploads("wp-content/uploads");
/// let inventory = source.inventory(ctx).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Wxr {
    bytes: Vec<u8>,
    uploads: Option<PathBuf>,
    currency: Option<String>,
}

fn wp_date(text: Option<String>) -> Option<DateTime<Utc>> {
    let text = text?;
    if text.starts_with("0000") {
        return None;
    }
    NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S").ok().map(|d| d.and_utc())
}

fn postmeta(item: &Element) -> BTreeMap<String, String> {
    item.children_named("wp:postmeta")
        .filter_map(|meta| {
            Some((
                meta.child_text("wp:meta_key")?,
                meta.child_text("wp:meta_value").unwrap_or_default(),
            ))
        })
        .collect()
}

fn path_of(link: &str, origin: Option<&Url>) -> String {
    match (Url::parse(link), origin) {
        (Ok(url), _) => match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        },
        (Err(_), Some(origin)) => {
            origin.join(link).map(|u| u.path().to_string()).unwrap_or_default()
        }
        (Err(_), None) => link.to_string(),
    }
}

impl Wxr {
    /// A source over an export file's bytes.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self { bytes: bytes.into(), uploads: None, currency: None }
    }

    /// Reads media bytes from a copy of `wp-content/uploads`.
    #[must_use]
    pub fn with_uploads(mut self, dir: impl Into<PathBuf>) -> Self {
        self.uploads = Some(dir.into());
        self
    }

    /// The shop's currency, which an export does not carry.
    #[must_use]
    pub fn with_currency(mut self, currency: impl Into<String>) -> Self {
        self.currency = Some(currency.into());
        self
    }

    fn parse_inventory(&self, inventory: &mut SiteInventory) -> Result<()> {
        let text =
            std::str::from_utf8(&self.bytes).map_err(|e| MigrateError::parse("WXR file", e))?;
        let root = parse(text, "WXR file")?;
        let channel =
            root.child("channel").ok_or_else(|| MigrateError::parse("WXR file", "no <channel>"))?;
        let origin = channel
            .child_text("wp:base_blog_url")
            .or_else(|| channel.child_text("link"))
            .and_then(|l| Url::parse(&l).ok());
        inventory.origin = origin.as_ref().map(origin_of);
        inventory.site_name = channel.child_text("title");
        inventory.description = channel.child_text("description");
        inventory.language = channel.child_text("language");
        let mut categories = Vec::new();
        let mut category_ids: BTreeMap<String, String> = BTreeMap::new();
        for category in channel.children_named("wp:category") {
            let Some(slug) = category.child_text("wp:category_nicename") else { continue };
            if let Some(id) = category.child_text("wp:term_id") {
                category_ids.insert(id, format!("/category/{slug}/"));
            }
            categories.push(Term {
                name: category.child_text("wp:cat_name").unwrap_or_else(|| slug.clone()),
                parent: category.child_text("wp:category_parent"),
                path: Some(format!("/category/{slug}/")),
                slug,
            });
        }
        let tags: Vec<Term> = channel
            .children_named("wp:tag")
            .filter_map(|tag| {
                let slug = tag.child_text("wp:tag_slug")?;
                Some(Term {
                    name: tag.child_text("wp:tag_name").unwrap_or_else(|| slug.clone()),
                    parent: None,
                    path: Some(format!("/tag/{slug}/")),
                    slug,
                })
            })
            .collect();
        if !categories.is_empty() {
            inventory.taxonomies.push(Taxonomy { name: "category".into(), terms: categories });
        }
        if !tags.is_empty() {
            inventory.taxonomies.push(Taxonomy { name: "post_tag".into(), terms: tags });
        }
        let mut post_paths: BTreeMap<String, String> = BTreeMap::new();
        let mut menus: BTreeMap<String, Vec<MenuItemRecord>> = BTreeMap::new();
        let mut attachments: BTreeMap<String, ()> = BTreeMap::new();
        for item in channel.children_named("item") {
            let post_type = item.child_text("wp:post_type").unwrap_or_default();
            let id = item.child_text("wp:post_id").unwrap_or_default();
            let link = item.child_text("link").unwrap_or_default();
            let meta = postmeta(item);
            let evidence =
                |detail: String| FeatureEvidence { url: format!("wxr:{post_type}:{id}"), detail };
            match post_type.as_str() {
                "post" | "page" | "product" => {
                    let status = match item.child_text("wp:status").as_deref() {
                        Some("publish") => ItemStatus::Published,
                        Some("draft") | Some("auto-draft") => ItemStatus::Draft,
                        Some("pending") => ItemStatus::Pending,
                        Some("future") => ItemStatus::Scheduled,
                        Some("private") => ItemStatus::Private,
                        _ => continue,
                    };
                    let kind = match post_type.as_str() {
                        "post" => ContentKind::Post,
                        "product" => ContentKind::Product,
                        _ => ContentKind::Page,
                    };
                    let key = format!("wxr:{post_type}:{id}");
                    let path = if link.is_empty() {
                        format!("/?p={id}")
                    } else {
                        path_of(&link, origin.as_ref())
                    };
                    post_paths.insert(id.clone(), path.clone());
                    let content = item.child_text("content:encoded").unwrap_or_default();
                    for feature in detect_shortcodes(&content, &key) {
                        inventory.add_feature(feature);
                    }
                    let mut item_categories = Vec::new();
                    let mut item_tags = Vec::new();
                    for category in item.children_named("category") {
                        let name = category.text.trim().to_string();
                        match category.attribute("domain") {
                            Some("category" | "product_cat") => item_categories.push(name),
                            Some("post_tag" | "product_tag") => item_tags.push(name),
                            _ => {}
                        }
                    }
                    let mut fields = BTreeMap::new();
                    if kind == ContentKind::Product {
                        inventory.add_feature(DetectedFeature {
                            kind: FeatureKind::Shop,
                            evidence: vec![evidence("WooCommerce product".into())],
                            fields: Vec::new(),
                        });
                        if let Some(sku) = meta.get("_sku").filter(|s| !s.is_empty()) {
                            fields.insert("sku".into(), Value::String(sku.clone()));
                        }
                        let price = meta.get("_price").or_else(|| meta.get("_regular_price"));
                        if let Some(price) = price
                            .and_then(|p| p.parse::<f64>().ok())
                            .and_then(serde_json::Number::from_f64)
                        {
                            fields.insert("price".into(), Value::Number(price));
                            match &self.currency {
                                Some(currency) => {
                                    fields
                                        .insert("currency".into(), Value::String(currency.clone()));
                                }
                                None => inventory.notes.push(format!(
                                    "{key}: price without a currency; the export does not carry it"
                                )),
                            }
                        }
                        if let Some(stock) = meta.get("_stock").and_then(|s| s.parse::<f64>().ok())
                        {
                            fields.insert("stock".into(), Value::from(stock as i64));
                        }
                    }
                    let open_comments =
                        item.child_text("wp:comment_status").as_deref() == Some("open");
                    let comments = item.children_named("wp:comment").count();
                    if (open_comments && kind == ContentKind::Post) || comments > 0 {
                        inventory.add_feature(DetectedFeature {
                            kind: FeatureKind::Comments,
                            evidence: vec![evidence(format!(
                                "comments {}, {comments} stored",
                                if open_comments { "open" } else { "closed" }
                            ))],
                            fields: Vec::new(),
                        });
                    }
                    let protected = item.child_text("wp:post_password").is_some();
                    if protected {
                        inventory.add_feature(DetectedFeature {
                            kind: FeatureKind::MemberArea,
                            evidence: vec![evidence("password-protected post".into())],
                            fields: Vec::new(),
                        });
                    }
                    if status == ItemStatus::Published && !link.is_empty() {
                        inventory.urls.push(InventoryUrl {
                            url: link.clone(),
                            path: path.clone(),
                            status: 200,
                            redirect_to: None,
                            content_type: Some("text/html".into()),
                            sha256: None,
                            item: Some(key.clone()),
                            page: None,
                            inbound_links: 0,
                            body: None,
                        });
                    }
                    inventory.items.push(InventoryItem {
                        key,
                        kind,
                        slug: item.child_text("wp:post_name").unwrap_or_else(|| id.clone()),
                        url: (!link.is_empty()).then_some(link),
                        path,
                        title: item.child_text("title").unwrap_or_default(),
                        html: if protected { String::new() } else { content },
                        excerpt: item.child_text("excerpt:encoded"),
                        status,
                        published_at: wp_date(item.child_text("wp:post_date_gmt")),
                        modified_at: wp_date(item.child_text("wp:post_modified_gmt")),
                        language: None,
                        categories: item_categories,
                        tags: item_tags,
                        fields,
                        media: Vec::new(),
                        author: item.child_text("dc:creator"),
                    });
                }
                "attachment" => {
                    let Some(url) = item.child_text("wp:attachment_url") else { continue };
                    let path = path_of(&url, origin.as_ref());
                    attachments.insert(path.clone(), ());
                    let mut media = InventoryMedia::new(url.clone(), path.clone());
                    media.declared_type = item.child_text("wp:post_mime_type");
                    media.alt =
                        meta.get("_wp_attachment_image_alt").filter(|a| !a.is_empty()).cloned();
                    if let Some(bytes) = self.read_upload(&path) {
                        media = InventoryMedia { bytes: None, ..media }.with_bytes(bytes);
                    }
                    inventory.urls.push(InventoryUrl {
                        url,
                        path,
                        status: 200,
                        redirect_to: None,
                        content_type: media.declared_type.clone(),
                        sha256: media.sha256.clone(),
                        item: None,
                        page: None,
                        inbound_links: 0,
                        body: None,
                    });
                    inventory.media.push(media);
                }
                "nav_menu_item" => {
                    let menu = item
                        .children_named("category")
                        .find(|c| c.attribute("domain") == Some("nav_menu"))
                        .map(|c| c.text.trim().to_string())
                        .unwrap_or_else(|| "menu".into());
                    let order =
                        item.child_text("wp:menu_order").and_then(|o| o.parse().ok()).unwrap_or(0);
                    let target = match meta.get("_menu_item_type").map(String::as_str) {
                        Some("custom") => meta.get("_menu_item_url").cloned().unwrap_or_default(),
                        Some("taxonomy") => format!(
                            "term:{}",
                            meta.get("_menu_item_object_id").cloned().unwrap_or_default()
                        ),
                        _ => format!(
                            "post:{}",
                            meta.get("_menu_item_object_id").cloned().unwrap_or_default()
                        ),
                    };
                    let parent = meta
                        .get("_menu_item_menu_item_parent")
                        .filter(|p| p.as_str() != "0")
                        .cloned();
                    menus.entry(menu).or_default().push(MenuItemRecord {
                        order,
                        id: id.clone(),
                        title: item.child_text("title").unwrap_or_default(),
                        parent,
                        target,
                    });
                }
                "wpcf7_contact_form" | "wpforms" | "fluentform" => {
                    inventory.add_feature(DetectedFeature {
                        kind: FeatureKind::ContactForm,
                        evidence: vec![evidence(format!("form plugin post type {post_type}"))],
                        fields: Vec::new(),
                    })
                }
                other if BOOKING_TYPES.contains(&other) => inventory.add_feature(DetectedFeature {
                    kind: FeatureKind::Bookings,
                    evidence: vec![evidence(format!("booking plugin post type {other}"))],
                    fields: Vec::new(),
                }),
                _ => {}
            }
        }
        for (name, mut entries) in menus {
            entries.sort_by_key(|e| e.order);
            let positions: BTreeMap<String, usize> =
                entries.iter().enumerate().map(|(i, e)| (e.id.clone(), i)).collect();
            let entries = entries
                .iter()
                .map(|MenuItemRecord { title, parent, target, .. }| {
                    let target = if let Some(post) = target.strip_prefix("post:") {
                        post_paths.get(post).cloned().unwrap_or_default()
                    } else if let Some(term) = target.strip_prefix("term:") {
                        category_ids.get(term).cloned().unwrap_or_default()
                    } else {
                        let origin_text = inventory.origin.clone().unwrap_or_default();
                        target
                            .strip_prefix(&origin_text)
                            .map(str::to_string)
                            .unwrap_or_else(|| target.clone())
                    };
                    MenuEntry {
                        title: title.clone(),
                        target,
                        parent: parent.as_ref().and_then(|p| positions.get(p).copied()),
                    }
                })
                .collect();
            inventory.menus.push(Menu { name, entries });
        }
        if let Some(dir) = &self.uploads {
            for (relative, bytes) in list_uploads(dir) {
                let path = format!("/wp-content/uploads/{relative}");
                if attachments.contains_key(&path) {
                    continue;
                }
                let url = inventory
                    .origin
                    .clone()
                    .map(|o| format!("{o}{path}"))
                    .unwrap_or_else(|| path.clone());
                inventory
                    .notes
                    .push(format!("{path}: in the uploads directory, named by no attachment"));
                inventory.media.push(InventoryMedia::new(url, path).with_bytes(bytes));
            }
        }
        Ok(())
    }

    fn read_upload(&self, path: &str) -> Option<Bytes> {
        let dir = self.uploads.as_ref()?;
        let relative = path.split("/wp-content/uploads/").nth(1)?;
        if relative.split('/').any(|s| s == ".." || s.is_empty()) {
            return None;
        }
        std::fs::read(dir.join(relative)).ok().map(Bytes::from)
    }
}

/// Every file under `dir`, by `/`-separated relative path.
pub(crate) fn list_uploads(dir: &Path) -> Vec<(String, Bytes)> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file()
                && let (Ok(relative), Ok(bytes)) = (path.strip_prefix(dir), std::fs::read(&path))
            {
                found.push((relative.to_string_lossy().replace('\\', "/"), Bytes::from(bytes)));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

#[async_trait]
impl SiteSource for Wxr {
    async fn inventory(&self, ctx: &SourceContext) -> Result<SiteInventory> {
        let mut inventory = SiteInventory::new(SourceKind::Wxr, ctx.clock.now());
        let source = self.clone();
        let inventory = tokio::task::spawn_blocking(move || {
            source.parse_inventory(&mut inventory).map(|()| inventory)
        })
        .await
        .map_err(|e| MigrateError::Service(format!("WXR task failed: {e}")))??;
        tracing::info!(
            items = inventory.items.len(),
            media = inventory.media.len(),
            "WXR export read"
        );
        Ok(inventory)
    }
}
