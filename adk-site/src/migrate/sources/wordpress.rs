//! `WordPressRest`: the public `wp-json/wp/v2` collections (spec 3.5 §7.8 "Sources").

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::CredentialBroker;
use async_trait::async_trait;
use awp_types::ContentKind;
use base64::Engine as _;
use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::Value;
use url::Url;

use super::super::error::{MigrateError, Result};
use super::super::html::site_path;
use super::super::http::{FetchOptions, origin_of};
use super::super::inventory::{
    DetectedFeature, FeatureEvidence, FeatureKind, InventoryItem, InventoryMedia, InventoryUrl,
    ItemStatus, Menu, MenuEntry, SiteInventory, SourceKind, Taxonomy, Term,
};
use super::super::{SiteSource, SourceContext};

/// The scope a draft credential may carry, and the only one.
pub const DRAFT_SCOPE: &str = "read";

/// REST namespaces that name a feature.
const NAMESPACES: &[(&str, FeatureKind)] = &[
    ("contact-form-7/v1", FeatureKind::ContactForm),
    ("gf/v2", FeatureKind::ContactForm),
    ("wpforms/v1", FeatureKind::ContactForm),
    ("wc/store/v1", FeatureKind::Shop),
    ("wc/store", FeatureKind::Shop),
    ("wc/v3", FeatureKind::Shop),
    ("bookly/v1", FeatureKind::Bookings),
    ("mphb/v1", FeatureKind::Bookings),
    ("amelia/v1", FeatureKind::Bookings),
    ("mailpoet/v1", FeatureKind::Newsletter),
    ("mc4wp/v1", FeatureKind::Newsletter),
    ("pmpro/v1", FeatureKind::MemberArea),
    ("wp-members/v1", FeatureKind::MemberArea),
];

/// Reads a WordPress site through its REST API: posts, pages, media, categories, tags and,
/// when WooCommerce's Store API is there, products. Drafts are read only with a read-scoped
/// application password the [`CredentialBroker`] issues to the site's acting identity, on a
/// proven domain (A-I9, A-I19).
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::migrate::{SiteSource, SourceContext, sources::WordPressRest};
///
/// # async fn demo(ctx: &SourceContext) -> adk_site::migrate::Result<()> {
/// let inventory = WordPressRest::new("https://example.com/".parse().unwrap()).inventory(ctx).await?;
/// println!("{} items", inventory.items.len());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct WordPressRest {
    base: Url,
    drafts: Option<Arc<dyn CredentialBroker>>,
    media: bool,
}

impl std::fmt::Debug for WordPressRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WordPressRest")
            .field("base", &self.base.as_str())
            .field("drafts", &self.drafts.is_some())
            .field("media", &self.media)
            .finish()
    }
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty())
}

fn gmt(value: &Value, pointer: &str) -> Option<DateTime<Utc>> {
    let text = text(value, pointer)?;
    NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S").ok().map(|d| d.and_utc())
}

fn status(value: &Value) -> Option<ItemStatus> {
    Some(match value.get("status").and_then(Value::as_str) {
        Some("publish") | None => ItemStatus::Published,
        Some("draft") => ItemStatus::Draft,
        Some("pending") => ItemStatus::Pending,
        Some("future") => ItemStatus::Scheduled,
        Some("private") => ItemStatus::Private,
        Some(_) => return None,
    })
}

impl WordPressRest {
    /// A source for the site at `base` (its home URL).
    pub fn new(base: Url) -> Self {
        Self { base, drafts: None, media: true }
    }

    /// Reads drafts too, with a read-scoped credential from `broker`.
    #[must_use]
    pub fn with_drafts(mut self, broker: Arc<dyn CredentialBroker>) -> Self {
        self.drafts = Some(broker);
        self
    }

    /// Does not download media bytes.
    #[must_use]
    pub fn without_media(mut self) -> Self {
        self.media = false;
        self
    }

    fn api(&self, route: &str) -> Result<Url> {
        self.base.join(&format!("wp-json/{route}")).map_err(|e| MigrateError::parse("REST URL", e))
    }

    async fn get(
        &self,
        ctx: &SourceContext,
        url: &Url,
        auth: Option<&HeaderValue>,
    ) -> Result<(Value, Option<u32>)> {
        let mut options = FetchOptions::crawler().following(3);
        if let Some(auth) = auth {
            options = options.header(AUTHORIZATION, auth.clone());
        }
        let fetched = ctx.http.fetch(url, &options).await?;
        if fetched.status != 200 {
            return Err(MigrateError::Status { url: url.to_string(), status: fetched.status });
        }
        let total_pages = fetched.header("x-wp-totalpages").and_then(|v| v.parse().ok());
        let value = serde_json::from_slice(&fetched.body)
            .map_err(|e| MigrateError::parse(url.to_string(), e))?;
        Ok((value, total_pages))
    }

    async fn collection(
        &self,
        ctx: &SourceContext,
        route: &str,
        params: &str,
        auth: Option<&HeaderValue>,
    ) -> Result<Vec<Value>> {
        let per_page = ctx.limits.per_page.clamp(1, 100);
        let mut all = Vec::new();
        for page in 1u32.. {
            let url = self.api(&format!("{route}?per_page={per_page}&page={page}{params}"))?;
            let (value, total) = match self.get(ctx, &url, auth).await {
                Ok(found) => found,
                Err(MigrateError::Status { status: 400, .. }) if page > 1 => break,
                Err(error) => return Err(error),
            };
            let Value::Array(items) = value else {
                return Err(MigrateError::parse(url.to_string(), "expected a JSON array"));
            };
            let empty = items.is_empty();
            all.extend(items);
            if empty || total.is_none_or(|t| page >= t) || all.len() >= ctx.limits.max_pages {
                break;
            }
        }
        Ok(all)
    }

    /// The `Authorization` header for drafts: a broker-issued credential whose only scope is
    /// `read`, valid for the API (A-I19: never typed into a conversation).
    async fn draft_authorization(&self, ctx: &SourceContext) -> Result<Option<HeaderValue>> {
        let Some(broker) = &self.drafts else { return Ok(None) };
        ctx.require_proven(self.base.host_str().unwrap_or_default(), "reading drafts")?;
        let resource = self.api("wp/v2/")?;
        let token = broker
            .token_for(&ctx.acting, &resource, &[DRAFT_SCOPE.to_string()])
            .await
            .map_err(|e| MigrateError::Credential(e.to_string()))?;
        if token.scopes.iter().any(|s| s != DRAFT_SCOPE) {
            return Err(MigrateError::Credential(format!(
                "the credential carries scopes {:?}; drafts are read with '{DRAFT_SCOPE}' only",
                token.scopes
            )));
        }
        if !token.covers(&resource) {
            return Err(MigrateError::Credential(format!("the credential is not for {resource}")));
        }
        if !token.is_live(ctx.clock.now()) {
            return Err(MigrateError::Credential("the credential has expired".into()));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(token.secret.expose());
        let mut value = HeaderValue::from_str(&format!("Basic {encoded}"))
            .map_err(|e| MigrateError::Credential(e.to_string()))?;
        value.set_sensitive(true);
        Ok(Some(value))
    }

    fn item(
        &self,
        inventory: &mut SiteInventory,
        value: &Value,
        kind: ContentKind,
        terms: &BTreeMap<i64, String>,
        origin: &str,
    ) {
        let Some(status) = status(value) else { return };
        let id = value.get("id").and_then(Value::as_i64).unwrap_or_default();
        let type_name = match kind {
            ContentKind::Post => "post",
            ContentKind::Product => "product",
            _ => "page",
        };
        let key = format!("wp:{type_name}:{id}");
        if inventory.item(&key).is_some() {
            return;
        }
        let link = text(value, "/link").unwrap_or_default();
        let path = Url::parse(&link)
            .map(|u| site_path(&u, origin))
            .unwrap_or_else(|_| format!("/?p={id}"));
        let protected =
            value.pointer("/content/protected").and_then(Value::as_bool).unwrap_or(false);
        if protected {
            inventory.add_feature(DetectedFeature {
                kind: FeatureKind::MemberArea,
                evidence: vec![FeatureEvidence {
                    url: link.clone(),
                    detail: "password-protected content".into(),
                }],
                fields: Vec::new(),
            });
        }
        if value.get("comment_status").and_then(Value::as_str) == Some("open")
            && kind == ContentKind::Post
        {
            inventory.add_feature(DetectedFeature {
                kind: FeatureKind::Comments,
                evidence: vec![FeatureEvidence {
                    url: link.clone(),
                    detail: "comment_status open".into(),
                }],
                fields: Vec::new(),
            });
        }
        let names = |field: &str| -> Vec<String> {
            value
                .get(field)
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_i64)
                        .filter_map(|i| terms.get(&i).cloned())
                        .collect()
                })
                .unwrap_or_default()
        };
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
            slug: text(value, "/slug").unwrap_or_else(|| id.to_string()),
            url: (!link.is_empty()).then_some(link),
            path,
            title: text(value, "/title/rendered")
                .map(|t| super::super::html::collapse(&t))
                .unwrap_or_default(),
            html: if protected {
                String::new()
            } else {
                text(value, "/content/rendered").unwrap_or_default()
            },
            excerpt: text(value, "/excerpt/rendered"),
            status,
            published_at: gmt(value, "/date_gmt"),
            modified_at: gmt(value, "/modified_gmt"),
            language: None,
            categories: names("categories"),
            tags: names("tags"),
            fields: BTreeMap::new(),
            media: Vec::new(),
            author: text(value, "/_embedded/author/0/name"),
        });
    }
}

#[async_trait]
impl SiteSource for WordPressRest {
    async fn inventory(&self, ctx: &SourceContext) -> Result<SiteInventory> {
        let mut inventory = SiteInventory::new(SourceKind::WordPressRest, ctx.clock.now());
        let origin = origin_of(&self.base);
        inventory.origin = Some(origin.clone());
        let index_url = self.api("")?;
        let (index, _) = self.get(ctx, &index_url, None).await?;
        inventory.site_name = text(&index, "/name");
        inventory.description = text(&index, "/description");
        let namespaces: Vec<String> = index
            .get("namespaces")
            .and_then(Value::as_array)
            .map(|n| n.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        for (namespace, kind) in NAMESPACES {
            if namespaces.iter().any(|n| n == namespace) {
                inventory.add_feature(DetectedFeature {
                    kind: *kind,
                    evidence: vec![FeatureEvidence {
                        url: index_url.to_string(),
                        detail: format!("REST namespace {namespace}"),
                    }],
                    fields: Vec::new(),
                });
            }
        }
        inventory.add_feature(DetectedFeature {
            kind: FeatureKind::Search,
            evidence: vec![FeatureEvidence {
                url: index_url.to_string(),
                detail: "WordPress site search".into(),
            }],
            fields: Vec::new(),
        });
        let auth = self.draft_authorization(ctx).await?;
        let mut terms: BTreeMap<i64, String> = BTreeMap::new();
        for (route, taxonomy, prefix) in
            [("wp/v2/categories", "category", "/category/"), ("wp/v2/tags", "post_tag", "/tag/")]
        {
            let values = match self.collection(ctx, route, "", None).await {
                Ok(values) => values,
                Err(error) => {
                    inventory.notes.push(format!("{route}: {error}"));
                    continue;
                }
            };
            let mut list = Vec::new();
            for value in values {
                let (Some(id), Some(slug)) =
                    (value.get("id").and_then(Value::as_i64), text(&value, "/slug"))
                else {
                    continue;
                };
                let name = text(&value, "/name").unwrap_or_else(|| slug.clone());
                terms.insert(id, name.clone());
                let path = text(&value, "/link")
                    .and_then(|l| Url::parse(&l).ok())
                    .map(|u| site_path(&u, &origin))
                    .unwrap_or_else(|| format!("{prefix}{slug}/"));
                list.push(Term { slug, name, parent: None, path: Some(path) });
            }
            if !list.is_empty() {
                inventory.taxonomies.push(Taxonomy { name: taxonomy.into(), terms: list });
            }
        }
        for (route, kind) in
            [("wp/v2/pages", ContentKind::Page), ("wp/v2/posts", ContentKind::Post)]
        {
            for value in self.collection(ctx, route, "&_embed=author", None).await? {
                self.item(&mut inventory, &value, kind, &terms, &origin);
            }
            if let Some(auth) = &auth {
                let drafts = self
                    .collection(
                        ctx,
                        route,
                        "&status=draft,pending,future,private&context=edit",
                        Some(auth),
                    )
                    .await?;
                for value in drafts {
                    self.item(&mut inventory, &value, kind, &terms, &origin);
                }
            }
        }
        if namespaces.iter().any(|n| n == "wc/store/v1") {
            match self.collection(ctx, "wc/store/v1/products", "", None).await {
                Ok(products) => {
                    for product in products {
                        let id = product.get("id").and_then(Value::as_i64).unwrap_or_default();
                        let key = format!("wp:product:{id}");
                        let link = text(&product, "/permalink").unwrap_or_default();
                        let path = Url::parse(&link)
                            .map(|u| site_path(&u, &origin))
                            .unwrap_or_else(|_| format!("/?p={id}"));
                        let mut fields = BTreeMap::new();
                        if let Some(sku) = text(&product, "/sku") {
                            fields.insert("sku".into(), Value::String(sku));
                        }
                        let minor = product
                            .pointer("/prices/currency_minor_unit")
                            .and_then(Value::as_i64)
                            .unwrap_or(2);
                        let price =
                            text(&product, "/prices/price").and_then(|p| p.parse::<f64>().ok());
                        if let Some(price) = price.and_then(|p| {
                            serde_json::Number::from_f64(p / 10f64.powi(minor as i32))
                        }) {
                            fields.insert("price".into(), Value::Number(price));
                        }
                        if let Some(currency) = text(&product, "/prices/currency_code") {
                            fields.insert("currency".into(), Value::String(currency));
                        }
                        let categories: Vec<String> = product
                            .get("categories")
                            .and_then(Value::as_array)
                            .map(|c| c.iter().filter_map(|c| text(c, "/name")).collect())
                            .unwrap_or_default();
                        let media: Vec<String> = product
                            .get("images")
                            .and_then(Value::as_array)
                            .map(|i| i.iter().filter_map(|i| text(i, "/src")).collect())
                            .unwrap_or_default();
                        if !link.is_empty() {
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
                            kind: ContentKind::Product,
                            slug: text(&product, "/slug").unwrap_or_else(|| id.to_string()),
                            url: (!link.is_empty()).then_some(link),
                            path,
                            title: text(&product, "/name").unwrap_or_default(),
                            html: text(&product, "/description").unwrap_or_default(),
                            excerpt: text(&product, "/short_description"),
                            status: ItemStatus::Published,
                            published_at: None,
                            modified_at: None,
                            language: None,
                            categories,
                            tags: Vec::new(),
                            fields,
                            media,
                            author: None,
                        });
                    }
                }
                Err(error) => inventory.notes.push(format!("wc/store/v1/products: {error}")),
            }
        }
        if let Some(auth) = &auth {
            match self.collection(ctx, "wp/v2/menu-items", "", Some(auth)).await {
                Ok(items) => {
                    let mut menu = Menu { name: "primary".into(), entries: Vec::new() };
                    for item in items {
                        let target = text(&item, "/url")
                            .and_then(|u| Url::parse(&u).ok())
                            .map(|u| site_path(&u, &origin))
                            .unwrap_or_default();
                        menu.entries.push(MenuEntry {
                            title: text(&item, "/title/rendered").unwrap_or_default(),
                            target,
                            parent: None,
                        });
                    }
                    if !menu.entries.is_empty() {
                        inventory.menus.push(menu);
                    }
                }
                Err(error) => inventory.notes.push(format!("wp/v2/menu-items: {error}")),
            }
        } else {
            inventory.notes.push("menus need an authenticated read; none read".into());
        }
        match self.collection(ctx, "wp/v2/media", "", None).await {
            Ok(values) => {
                let mut fetched = 0usize;
                for value in values {
                    let Some(source) = text(&value, "/source_url") else { continue };
                    let mut urls = vec![source];
                    if let Some(sizes) =
                        value.pointer("/media_details/sizes").and_then(Value::as_object)
                    {
                        urls.extend(sizes.values().filter_map(|s| text(s, "/source_url")));
                    }
                    urls.sort();
                    urls.dedup();
                    for url_text in urls {
                        let Ok(url) = Url::parse(&url_text) else { continue };
                        let path = site_path(&url, &origin);
                        if inventory.media.iter().any(|m| m.path == path) {
                            continue;
                        }
                        let mut media = InventoryMedia::new(url_text.clone(), path.clone());
                        media.declared_type = text(&value, "/mime_type");
                        media.alt = text(&value, "/alt_text");
                        if self.media && fetched < ctx.limits.max_media && origin_of(&url) == origin
                        {
                            fetched += 1;
                            match ctx.http.fetch(&url, &FetchOptions::crawler()).await {
                                Ok(response) if response.status == 200 => {
                                    media = media.with_bytes(response.body.clone());
                                }
                                Ok(response) => {
                                    inventory.notes.push(format!("{url}: HTTP {}", response.status))
                                }
                                Err(error) => inventory.notes.push(format!("{url}: {error}")),
                            }
                        }
                        inventory.urls.push(InventoryUrl {
                            url: url_text,
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
                }
            }
            Err(error) => inventory.notes.push(format!("wp/v2/media: {error}")),
        }
        let mut seen = std::collections::HashSet::new();
        inventory.urls.retain(|u| seen.insert(u.path.clone()));
        tracing::info!(origin = %origin, items = inventory.items.len(), media = inventory.media.len(), "WordPress REST read");
        Ok(inventory)
    }
}
