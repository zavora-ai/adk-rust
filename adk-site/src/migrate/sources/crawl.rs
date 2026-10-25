//! `Crawl`: any site, from its sitemaps and links (spec 3.5 §7.8 "Sources").

use std::collections::{BTreeMap, HashSet, VecDeque};

use async_trait::async_trait;
use url::Url;

use super::super::error::{MigrateError, Result};
use super::super::html::{analyse, css_urls, site_path};
use super::super::http::{FetchOptions, Fetched, origin_of};
use super::super::inventory::{
    InventoryMedia, InventoryUrl, Menu, MenuEntry, SiteInventory, SourceKind, Taxonomy, Term,
};
use super::super::{SiteSource, SourceContext};
use super::{page_item, parse_sitemap};
use crate::release::file_digest;

/// Reads any site by following its sitemaps and links: robots-respecting, rate-limited and
/// signed through the context's [`HttpClient`](super::super::http::HttpClient), same-origin
/// only, within the context's limits.
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::migrate::{SiteSource, SourceContext, sources::Crawl};
///
/// # async fn demo(ctx: &SourceContext) -> adk_site::migrate::Result<()> {
/// let inventory = Crawl::new("https://example.com/".parse().unwrap()).inventory(ctx).await?;
/// println!("{} pages", inventory.urls.len());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Crawl {
    start: Url,
    seeds: Vec<Url>,
    sitemaps: bool,
    media: bool,
    assets: bool,
    kind: SourceKind,
}

/// Paths of non-page resources worth reading: uploads, documents, scripts.
fn is_resource(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.contains("/wp-content/uploads/")
        || [
            ".pdf", ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".mp4", ".mp3", ".zip",
            ".doc", ".docx", ".xls", ".xlsx",
        ]
        .iter()
        .any(|e| path.ends_with(e))
}

impl Crawl {
    /// A crawl from `start` that reads sitemaps and media.
    pub fn new(start: Url) -> Self {
        Self {
            start,
            seeds: Vec::new(),
            sitemaps: true,
            media: true,
            assets: false,
            kind: SourceKind::Crawl,
        }
    }

    /// Adds pages to start from.
    #[must_use]
    pub fn with_seeds(mut self, seeds: impl IntoIterator<Item = Url>) -> Self {
        self.seeds.extend(seeds);
        self
    }

    /// Ignores sitemaps.
    #[must_use]
    pub fn without_sitemaps(mut self) -> Self {
        self.sitemaps = false;
        self
    }

    /// Does not download media.
    #[must_use]
    pub fn without_media(mut self) -> Self {
        self.media = false;
        self
    }

    /// Also reads every same-origin stylesheet, icon, font, image candidate and script, and
    /// keeps every body: what a freeze needs to serve the site as it was.
    #[must_use]
    pub fn with_assets(mut self) -> Self {
        self.assets = true;
        self
    }

    /// Reports the inventory as coming from `kind`, for sources built on a crawl.
    #[must_use]
    pub(crate) fn reporting_as(mut self, kind: SourceKind) -> Self {
        self.kind = kind;
        self
    }

    fn record(&self, fetched: &Fetched, url: &Url, origin: &str) -> InventoryUrl {
        let keep_body = self.assets
            || fetched.is_html()
            || fetched.content_type().is_some_and(|t| t.contains("javascript"))
            || url.path().ends_with(".js");
        InventoryUrl {
            url: url.to_string(),
            path: site_path(url, origin),
            status: fetched.status,
            redirect_to: fetched.location.as_ref().map(|l| site_path(l, origin)),
            content_type: fetched.content_type(),
            sha256: (fetched.status == 200).then(|| file_digest(&fetched.body)),
            item: None,
            page: None,
            inbound_links: 0,
            body: keep_body.then(|| fetched.body.clone()),
        }
    }

    /// Crawls into `inventory`.
    pub(crate) async fn crawl_into(
        &self,
        ctx: &SourceContext,
        inventory: &mut SiteInventory,
    ) -> Result<()> {
        let origin = origin_of(&self.start);
        inventory.origin = Some(origin.clone());
        let robots = ctx.http.robots_for(&self.start).await;
        let mut queue: VecDeque<(Url, usize)> = VecDeque::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut enqueue = |url: Url, depth: usize, queue: &mut VecDeque<(Url, usize)>| {
            let mut url = url;
            url.set_fragment(None);
            if origin_of(&url) == origin && seen.insert(url.to_string()) {
                queue.push_back((url, depth));
            }
        };
        enqueue(self.start.clone(), 0, &mut queue);
        for seed in &self.seeds {
            enqueue(seed.clone(), 0, &mut queue);
        }
        if self.sitemaps {
            let mut sitemaps: VecDeque<String> = robots.sitemaps().iter().cloned().collect();
            for default in ["/sitemap.xml", "/wp-sitemap.xml"] {
                if let Ok(url) = self.start.join(default) {
                    sitemaps.push_back(url.to_string());
                }
            }
            let mut read = HashSet::new();
            while let Some(sitemap) = sitemaps.pop_front() {
                if read.len() >= 50 || !read.insert(sitemap.clone()) {
                    continue;
                }
                let Ok(url) = Url::parse(&sitemap) else { continue };
                if origin_of(&url) != origin {
                    continue;
                }
                match ctx.http.fetch(&url, &FetchOptions::crawler().following(3)).await {
                    Ok(fetched) if fetched.status == 200 => {
                        let (pages, nested) = parse_sitemap(&fetched.text());
                        sitemaps.extend(nested);
                        for page in pages.iter().filter_map(|p| Url::parse(p).ok()) {
                            enqueue(page, 1, &mut queue);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => inventory.notes.push(format!("sitemap {sitemap}: {error}")),
                }
            }
        }
        let mut inbound: BTreeMap<String, u32> = BTreeMap::new();
        let mut media: Vec<(Url, Option<String>)> = Vec::new();
        let mut scripts: Vec<Url> = Vec::new();
        let mut terms: BTreeMap<String, Vec<Term>> = BTreeMap::new();
        let mut pages = 0usize;
        while let Some((url, depth)) = queue.pop_front() {
            if pages >= ctx.limits.max_pages {
                inventory.notes.push(format!(
                    "crawl stopped at {} pages; {} queued URLs not read",
                    ctx.limits.max_pages,
                    queue.len() + 1
                ));
                break;
            }
            if is_resource(&url) {
                if !media.iter().any(|(u, _)| u == &url) {
                    media.push((url, None));
                }
                continue;
            }
            let fetched = match ctx.http.fetch(&url, &FetchOptions::crawler()).await {
                Ok(fetched) => fetched,
                Err(MigrateError::RobotsDisallowed { url }) => {
                    inventory.notes.push(format!("{url}: disallowed by robots.txt, not read"));
                    continue;
                }
                Err(error) => {
                    inventory.notes.push(format!("{url}: {error}"));
                    continue;
                }
            };
            pages += 1;
            let mut record = self.record(&fetched, &url, &origin);
            if let Some(next) = fetched.location.clone().filter(|_| fetched.is_redirect()) {
                enqueue(next, depth, &mut queue);
            }
            if fetched.status == 200 && fetched.is_html() {
                let page = analyse(&fetched.text(), &url);
                for feature in page.features.clone() {
                    inventory.add_feature(feature);
                }
                for link in &page.links {
                    if origin_of(link) == origin {
                        *inbound.entry(site_path(link, &origin)).or_default() += 1;
                        if depth < ctx.limits.max_depth {
                            enqueue(link.clone(), depth + 1, &mut queue);
                        }
                    }
                }
                if self.media {
                    for (image, alt) in &page.media {
                        if origin_of(image) == origin && !media.iter().any(|(u, _)| u == image) {
                            media.push((image.clone(), alt.clone()));
                        }
                    }
                    for script in page.scripts.iter().filter_map(|s| s.src.clone()) {
                        if origin_of(&script) == origin && !scripts.contains(&script) {
                            scripts.push(script);
                        }
                    }
                    if self.assets {
                        for asset in &page.assets {
                            if origin_of(asset) == origin && !scripts.contains(asset) {
                                scripts.push(asset.clone());
                            }
                        }
                    }
                }
                if record.path == "/" {
                    inventory.site_name = page
                        .meta
                        .open_graph
                        .get("og:site_name")
                        .cloned()
                        .or_else(|| page.meta.title.clone());
                    inventory.description = page.meta.description.clone();
                    inventory.language = page.meta.language.clone();
                    if !page.nav.is_empty() && inventory.menus.is_empty() {
                        inventory.menus.push(Menu {
                            name: "primary".into(),
                            entries: page
                                .nav
                                .iter()
                                .map(|(title, target)| MenuEntry {
                                    title: title.clone(),
                                    target: site_path(target, &origin),
                                    parent: None,
                                })
                                .collect(),
                        });
                    }
                }
                for (taxonomy, prefix) in [("category", "/category/"), ("post_tag", "/tag/")] {
                    if let Some(rest) = record.path.strip_prefix(prefix) {
                        let slug = rest.trim_matches('/').to_string();
                        let title = page.meta.title.clone().unwrap_or_else(|| slug.clone());
                        let name = [" – ", " - ", " | "]
                            .iter()
                            .fold(title.as_str(), |t, sep| t.split(sep).next().unwrap_or(t))
                            .trim_start_matches("Category: ")
                            .trim_start_matches("Tag: ")
                            .trim_end_matches(" Archives")
                            .trim()
                            .to_string();
                        terms.entry(taxonomy.to_string()).or_default().push(Term {
                            slug,
                            name,
                            parent: None,
                            path: Some(record.path.clone()),
                        });
                    }
                }
                if let Some(item) = page_item(&url, &record.path, &page, &page.body_class) {
                    record.item = Some(item.key.clone());
                    inventory.items.push(item);
                }
                record.page = Some(page.meta);
            }
            inventory.urls.push(record);
        }
        for (url, alt) in media.into_iter().take(ctx.limits.max_media) {
            match ctx.http.fetch(&url, &FetchOptions::crawler()).await {
                Ok(fetched) => {
                    let record = self.record(&fetched, &url, &origin);
                    if fetched.status == 200 {
                        let mut entry = InventoryMedia::new(url.to_string(), record.path.clone())
                            .with_bytes(fetched.body.clone());
                        entry.declared_type = fetched.content_type();
                        entry.alt = alt;
                        inventory.media.push(entry);
                    }
                    inventory.urls.push(record);
                }
                Err(error) => inventory.notes.push(format!("{url}: {error}")),
            }
        }
        let mut resources: VecDeque<Url> = scripts.into();
        let mut read = HashSet::new();
        while let Some(url) = resources.pop_front() {
            if read.len() >= ctx.limits.max_media || !read.insert(url.to_string()) {
                continue;
            }
            match ctx.http.fetch(&url, &FetchOptions::crawler()).await {
                Ok(fetched) => {
                    if self.assets
                        && fetched.status == 200
                        && fetched.content_type().is_some_and(|t| t == "text/css")
                    {
                        for nested in css_urls(&fetched.text(), &url) {
                            if origin_of(&nested) == origin {
                                resources.push_back(nested);
                            }
                        }
                    }
                    inventory.urls.push(self.record(&fetched, &url, &origin));
                }
                Err(error) => inventory.notes.push(format!("{url}: {error}")),
            }
        }
        for url in &mut inventory.urls {
            url.inbound_links = inbound.get(&url.path).copied().unwrap_or_default();
        }
        let mut seen_paths = HashSet::new();
        inventory.urls.retain(|u| seen_paths.insert(u.path.clone()));
        for (name, terms) in terms {
            inventory.taxonomies.push(Taxonomy { name, terms });
        }
        tracing::info!(
            origin = %origin,
            urls = inventory.urls.len(),
            items = inventory.items.len(),
            media = inventory.media.len(),
            "crawl finished"
        );
        Ok(())
    }
}

#[async_trait]
impl SiteSource for Crawl {
    async fn inventory(&self, ctx: &SourceContext) -> Result<SiteInventory> {
        let mut inventory = SiteInventory::new(self.kind, ctx.clock.now());
        self.crawl_into(ctx, &mut inventory).await?;
        Ok(inventory)
    }
}
