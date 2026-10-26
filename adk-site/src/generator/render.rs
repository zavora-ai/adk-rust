//! Rendering pages and their twins (spec 3.5 §7.4 "Machine view of every page", §7.8
//! "Generator").

use std::collections::BTreeMap;

use awp_types::{ContentItem, ContentKind, MachineView, Markdown};
use minijinja::Environment;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use serde_json::{Map, Value, json};

use super::plan::{
    Page, PageKind, Plan, field_str, item_date, lang_key, modified_at, page_files, published_at,
    slugify, string, tags, title,
};
use crate::content::SiteProject;
use crate::error::{Result, SiteError};

/// The stylesheet's path.
pub(crate) const STYLESHEET: &str = "/assets/site.css";

/// Fields the item page lists, in order.
const DISPLAY_FIELDS: &[&str] =
    &["price", "sku", "startsAt", "endsAt", "location", "validUntil", "role"];
/// Fields that steer the build and are not resource data.
const INTERNAL_FIELDS: &[&str] = &["navOrder", "path", "withdrawal"];

fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    let scheme = url.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
    if url.contains(':') && matches!(scheme.as_str(), "javascript" | "vbscript" | "data" | "file") {
        CowStr::Borrowed("#")
    } else {
        url
    }
}

fn demote(level: HeadingLevel) -> HeadingLevel {
    match level {
        HeadingLevel::H1 => HeadingLevel::H2,
        HeadingLevel::H2 => HeadingLevel::H3,
        HeadingLevel::H3 => HeadingLevel::H4,
        HeadingLevel::H4 => HeadingLevel::H5,
        HeadingLevel::H5 | HeadingLevel::H6 => HeadingLevel::H6,
    }
}

/// Renders Markdown with raw HTML escaped, unsafe URL schemes removed and headings moved
/// below the page's `<h1>`.
///
/// # Example
///
/// ```rust
/// use adk_site::generator::markdown_to_html;
///
/// let html = markdown_to_html("# Intro\n\n<script>x()</script>\n\n[a](javascript:alert(1))");
/// assert!(html.contains("<h2>Intro</h2>"));
/// assert!(!html.contains("<script>"));
/// assert!(html.contains("href=\"#\""));
/// ```
pub fn markdown_to_html(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        Event::Start(Tag::Heading { level, id, classes, attrs }) => {
            Event::Start(Tag::Heading { level: demote(level), id, classes, attrs })
        }
        Event::End(TagEnd::Heading(level)) => Event::End(TagEnd::Heading(demote(level))),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Link { link_type, dest_url: safe_url(dest_url), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Image { link_type, dest_url: safe_url(dest_url), title, id })
        }
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// `value` with every object's keys in sorted order, so a page's bytes are the same whether
/// or not `serde_json` keeps insertion order: another crate in a build can turn on its
/// `preserve_order` feature, and a release's digests must not depend on that.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(key, value)| (key, sorted(value))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// JSON for a `<script type="application/ld+json">` block: `<`, `>` and `&` escaped so the
/// data cannot close the element.
fn script_json(value: &Value) -> String {
    serde_json::to_string(&sorted(value.clone()))
        .unwrap_or_default()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn date_only(at: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    at.map(|d| d.format("%Y-%m-%d").to_string())
}

fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) struct Renderer<'a> {
    project: &'a SiteProject,
    plan: &'a Plan,
    env: Environment<'static>,
    base_url: String,
}

impl<'a> Renderer<'a> {
    pub fn new(project: &'a SiteProject, plan: &'a Plan) -> Result<Self> {
        Ok(Self {
            project,
            plan,
            env: project.theme.environment()?,
            base_url: project.config.base_url(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn strings(&self, language: &str) -> BTreeMap<String, String> {
        let mut strings = self.project.theme.strings.clone();
        if let Some(own) = self.project.config.strings.get(language) {
            strings.extend(own.clone());
        }
        strings
    }

    fn site(&self, page: &Page) -> Value {
        let language = lang_key(&page.language);
        let prefix = self.project.config.language_prefix(&page.language);
        let nav: Vec<Value> = self
            .plan
            .nav
            .get(&language)
            .into_iter()
            .flatten()
            .map(|link| json!({ "title": link.title, "url": link.url, "current": page.path.starts_with(&link.url) }))
            .collect();
        let legal: Vec<Value> = self
            .plan
            .legal
            .get(&language)
            .into_iter()
            .flatten()
            .map(|link| json!({ "title": link.title, "url": link.url }))
            .collect();
        json!({
            "name": self.project.config.name,
            "home": format!("{prefix}/"),
            "stylesheet": STYLESHEET,
            "language": page.language.as_str(),
            "nav": nav,
            "legal": legal,
            "contract": self.project.documents.contains_key(".well-known/awp.json").then_some("/.well-known/awp.json"),
        })
    }

    fn translations(&self, item: &ContentItem) -> Vec<Value> {
        let Some(group) = self.plan.groups.get(&(item.kind, item.slug.clone())) else {
            return Vec::new();
        };
        if group.len() < 2 {
            return Vec::new();
        }
        group
            .iter()
            .map(|&i| {
                let other = &self.project.items[i];
                json!({
                    "language": other.language.as_str(),
                    "url": self.url(&self.plan.paths[&i]),
                    "name": other.language.as_str().to_uppercase(),
                })
            })
            .collect()
    }

    fn entry(&self, index: usize) -> Value {
        let item = &self.project.items[index];
        json!({
            "title": title(item),
            "url": self.plan.paths[&index],
            "summary": field_str(item, "summary").or_else(|| field_str(item, "description")),
            "date": date_only(published_at(item)),
        })
    }

    fn json_ld(&self, page: &Page) -> Value {
        let config = &self.project.config;
        let url = self.url(&page.path);
        let base = |kind: &str, name: &str| {
            let mut map = Map::new();
            map.insert("@context".into(), "https://schema.org".into());
            map.insert("@type".into(), kind.into());
            map.insert("name".into(), name.into());
            map.insert("url".into(), url.clone().into());
            map.insert("inLanguage".into(), page.language.as_str().into());
            map
        };
        match &page.kind {
            PageKind::Item(index) | PageKind::Notice(index, _) => {
                let item = &self.project.items[*index];
                let name = title(item);
                let kind = match item.kind {
                    ContentKind::Post => "BlogPosting",
                    ContentKind::Article => "NewsArticle",
                    ContentKind::Product => "Product",
                    ContentKind::Event => "Event",
                    ContentKind::Notice => "SpecialAnnouncement",
                    ContentKind::Service => "Service",
                    ContentKind::Person => "Person",
                    ContentKind::Collection => "CollectionPage",
                    _ => "WebPage",
                };
                let mut map = base(kind, &name);
                let released =
                    |key: &str| (!self.is_live(item, key)).then(|| item.fields.get(key)).flatten();
                if let Some(summary) =
                    field_str(item, "summary").or_else(|| field_str(item, "description"))
                {
                    map.insert("description".into(), summary.into());
                }
                let published = published_at(item)
                    .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                let modified = item_date(item, "updatedAt")
                    .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                let images: Vec<Value> = item
                    .media
                    .iter()
                    .filter(|m| m.content_type.starts_with("image/"))
                    .map(|m| {
                        let file = m.path.rsplit('/').next().unwrap_or(&m.path);
                        Value::String(self.url(&format!("{}{file}", page.path)))
                    })
                    .collect();
                match item.kind {
                    ContentKind::Post | ContentKind::Article => {
                        map.insert("headline".into(), name.clone().into());
                        if let Some(p) = &published {
                            map.insert("datePublished".into(), p.clone().into());
                        }
                        if let Some(m) = modified {
                            map.insert("dateModified".into(), m.into());
                        }
                        if let Some(byline) = field_str(item, "byline") {
                            map.insert(
                                "author".into(),
                                json!({ "@type": "Person", "name": byline }),
                            );
                        }
                    }
                    ContentKind::Product => {
                        if let Some(sku) = released("sku") {
                            map.insert("sku".into(), sku.clone());
                        }
                        if let (Some(price), Some(currency)) =
                            (released("price"), released("currency"))
                        {
                            let mut offer = Map::new();
                            offer.insert("@type".into(), "Offer".into());
                            offer.insert("price".into(), Value::String(display(price)));
                            offer.insert("priceCurrency".into(), currency.clone());
                            offer.insert("url".into(), url.clone().into());
                            if let Some(until) = released("validUntil") {
                                offer.insert("priceValidUntil".into(), until.clone());
                            }
                            map.insert("offers".into(), Value::Object(offer));
                        }
                    }
                    ContentKind::Event => {
                        for (key, field) in [("startDate", "startsAt"), ("endDate", "endsAt")] {
                            if let Some(value) = released(field) {
                                map.insert(key.into(), value.clone());
                            }
                        }
                        if let Some(location) = field_str(item, "location") {
                            map.insert(
                                "location".into(),
                                json!({ "@type": "Place", "name": location }),
                            );
                        }
                    }
                    ContentKind::Notice => {
                        if let Some(p) = &published {
                            map.insert("datePosted".into(), p.clone().into());
                        }
                        if let Some(expires) = item.schedule.as_ref().and_then(|s| s.expires_at) {
                            map.insert(
                                "expires".into(),
                                expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true).into(),
                            );
                        }
                    }
                    ContentKind::Service => {
                        map.insert("provider".into(), json!({ "@type": "Organization", "name": config.name, "url": self.base_url }));
                    }
                    ContentKind::Person => {
                        if let Some(role) = field_str(item, "role") {
                            map.insert("jobTitle".into(), role.into());
                        }
                    }
                    _ => {}
                }
                if !images.is_empty() {
                    map.insert("image".into(), Value::Array(images));
                }
                Value::Object(map)
            }
            PageKind::Listing { title, .. } => Value::Object(base("CollectionPage", title)),
            PageKind::Home { item, .. } => {
                let name =
                    item.map_or_else(|| config.name.clone(), |i| title(&self.project.items[i]));
                let mut page_ld = base("WebPage", &name);
                page_ld.remove("@context");
                let mut site = base("WebSite", &config.name);
                site.insert("url".into(), format!("{}/", self.base_url).into());
                if let Some(description) = &config.description {
                    site.insert("description".into(), description.clone().into());
                }
                site.insert("mainEntityOfPage".into(), Value::Object(page_ld));
                Value::Object(site)
            }
            PageKind::NotFound | PageKind::Gone => Value::Null,
        }
    }

    fn capabilities_for(&self, kind: Option<ContentKind>) -> Vec<String> {
        let config = &self.project.config;
        let mut names: Vec<String> = config.site_capabilities.clone();
        if let Some(kind) = kind {
            names.extend(config.kind_capabilities.get(&kind).cloned().unwrap_or_default());
        }
        names.sort();
        names.dedup();
        names
    }

    fn resource(&self, item: &ContentItem, path: &str) -> BTreeMap<String, Value> {
        let mut resource: BTreeMap<String, Value> = item
            .fields
            .iter()
            .filter(|(key, _)| !self.is_live(item, key) && !INTERNAL_FIELDS.contains(&key.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        resource.insert("id".into(), item.id.0.clone().into());
        resource.insert("url".into(), self.url(path).into());
        for field in crate::lanes::live_fields(&self.project.config, item) {
            let Some(value) = item.fields.get(field) else { continue };
            let mut live = Map::new();
            live.insert("value".into(), value.clone());
            live.insert(
                "asOf".into(),
                self.live_as_of(item).to_rfc3339_opts(chrono::SecondsFormat::Secs, true).into(),
            );
            if let Some(capability) = self.project.config.live_capabilities.get(field) {
                live.insert("capability".into(), capability.clone().into());
            }
            resource.insert(field.to_string(), Value::Object(live));
        }
        resource
    }

    /// Whether `key` is one of `item`'s live fields: its own `volatile` list or its kind's
    /// (spec 3.5 §7.8.3).
    fn is_live(&self, item: &ContentItem, key: &str) -> bool {
        crate::lanes::item_lane(&self.project.config, item, key) == crate::lanes::Lane::Live
    }

    /// The folded entries of `item`'s stream fields, newest first, for the page and its
    /// Markdown twin (spec 3.5 §7.8.3 "Stream").
    fn stream_entries(&self, item: &ContentItem, markdown: &mut String) -> Vec<Value> {
        let mut entries = Vec::new();
        for field in crate::lanes::stream_fields(&self.project.config, item.kind) {
            let Some(list) = item.fields.get(field).and_then(Value::as_array) else { continue };
            let mut folded: Vec<&Value> = list.iter().collect();
            folded.sort_by_key(|entry| std::cmp::Reverse(entry.get("n").and_then(Value::as_u64)));
            for entry in folded {
                let text = entry.get("markdown").and_then(Value::as_str).unwrap_or_default();
                let at = entry.get("publishedAt").and_then(Value::as_str).unwrap_or_default();
                let n = entry.get("n").and_then(Value::as_u64).unwrap_or_default();
                let corrects = entry.get("corrects").and_then(Value::as_u64);
                markdown.push_str(&format!("- **{at}** {}\n", text.replace('\n', " ")));
                entries.push(json!({
                    "n": n,
                    "at": at,
                    "time": at.get(11..16).unwrap_or(at),
                    "html": markdown_to_html(text),
                    "corrects": corrects,
                }));
            }
        }
        if !entries.is_empty() {
            markdown.push('\n');
        }
        entries
    }

    /// When an item's live values were last released: its update time, else the build's.
    fn live_as_of(&self, item: &ContentItem) -> chrono::DateTime<chrono::Utc> {
        modified_at(item).unwrap_or(self.project.as_of)
    }

    fn notices(&self, item: &ContentItem, language: &str) -> Vec<Value> {
        let mut notices = Vec::new();
        if let Some(corrections) = item.fields.get("corrections").and_then(Value::as_array) {
            for correction in corrections {
                notices.push(json!({
                    "class": "correction",
                    "title": string(self.project, language, "correction"),
                    "date": correction.get("at").and_then(Value::as_str).map(|d| d.chars().take(10).collect::<String>()),
                    "text": correction.get("note").and_then(Value::as_str).unwrap_or_default(),
                }));
            }
        }
        if let Some(expires) = item.schedule.as_ref().and_then(|s| s.expires_at)
            && expires <= self.project.as_of
        {
            notices.push(json!({
                "class": "expired",
                "title": string(self.project, language, "expired"),
                "date": date_only(Some(expires)),
            }));
        }
        notices
    }

    /// Renders one page into its files: HTML, then the Markdown twin and the machine view.
    pub fn page(&self, page: &Page) -> Result<Vec<(String, Vec<u8>)>> {
        let language = lang_key(&page.language);
        let (html_path, md_path, view_path) = page_files(&page.path);
        let md_url = md_path.as_ref().map(|_| format!("{}index.md", page.path));
        let md_absolute = md_url.as_ref().map(|u| self.url(u));
        let view_url = view_path.as_ref().map(|_| format!("{}index.awp.json", page.path));
        let json_ld = self.json_ld(page);
        let mut ctx = Map::new();
        ctx.insert("language".into(), page.language.as_str().into());
        ctx.insert("path".into(), page.path.clone().into());
        ctx.insert("url".into(), self.url(&page.path).into());
        ctx.insert("markdown".into(), md_url.clone().into());
        ctx.insert("machine_view".into(), view_url.clone().into());
        if !json_ld.is_null() {
            ctx.insert("json_ld".into(), script_json(&json_ld).into());
        }
        let mut markdown = String::new();
        let mut view: Option<MachineView> = None;
        let template = match &page.kind {
            PageKind::Item(index) | PageKind::Notice(index, _) => {
                let item = &self.project.items[*index];
                let name = title(item);
                ctx.insert("title".into(), name.clone().into());
                let summary = field_str(item, "summary").or_else(|| field_str(item, "description"));
                ctx.insert("description".into(), summary.into());
                ctx.insert("translations".into(), self.translations(item).into());
                let mut resource = self.resource(item, &page.path);
                markdown.push_str(&format!("# {name}\n\n"));
                let template = if let PageKind::Notice(_, text) = &page.kind {
                    let notice = json!({ "class": "withdrawn", "title": string(self.project, &language, "withdrawn"), "text": text });
                    ctx.insert("notices".into(), vec![notice].into());
                    ctx.insert("noindex".into(), true.into());
                    markdown.push_str(&format!(
                        "**{}.** {text}\n",
                        string(self.project, &language, "withdrawn")
                    ));
                    resource = BTreeMap::from([
                        ("id".to_string(), item.id.0.clone().into()),
                        ("url".to_string(), self.url(&page.path).into()),
                        ("withdrawn".to_string(), true.into()),
                    ]);
                    "notice.html"
                } else {
                    self.item_context(item, page, &language, &mut ctx, &mut markdown);
                    "item.html"
                };
                view = Some(MachineView {
                    path: page.path.clone(),
                    release_seq: self.project.release_seq,
                    kind: Some(item.kind),
                    language: Some(item.language.clone()),
                    title: Some(name),
                    resource,
                    capabilities: self.capabilities_for(Some(item.kind)),
                    json_ld: (!json_ld.is_null()).then(|| json_ld.clone()),
                    markdown: md_absolute.clone(),
                });
                template
            }
            PageKind::Listing {
                title: list_title,
                kind,
                collection,
                members,
                page: number,
                pages,
                base,
                ..
            } => {
                ctx.insert("title".into(), list_title.clone().into());
                markdown.push_str(&format!("# {list_title}\n\n"));
                if let Some(index) = collection {
                    let item = &self.project.items[*index];
                    if let Some(Markdown(body)) = &item.body {
                        ctx.insert("body".into(), markdown_to_html(body).into());
                        markdown.push_str(body.trim_end());
                        markdown.push_str("\n\n");
                    }
                    ctx.insert("translations".into(), self.translations(item).into());
                }
                let entries: Vec<Value> = members.iter().map(|&i| self.entry(i)).collect();
                for entry in &entries {
                    let title = entry["title"].as_str().unwrap_or_default();
                    let url = entry["url"].as_str().unwrap_or_default();
                    markdown.push_str(&format!("- [{title}]({}{url}index.md)", self.base_url));
                    if let Some(summary) = entry["summary"].as_str() {
                        markdown.push_str(&format!(": {summary}"));
                    }
                    markdown.push('\n');
                }
                if *pages > 1 {
                    let link =
                        |n: usize| if n == 1 { base.clone() } else { format!("{base}page/{n}/") };
                    ctx.insert(
                        "pagination".into(),
                        json!({
                            "current": number,
                            "total": pages,
                            "prev": (*number > 1).then(|| link(number - 1)),
                            "next": (number < pages).then(|| link(number + 1)),
                        }),
                    );
                }
                let resource = BTreeMap::from([
                    ("items".to_string(), Value::Array(entries.clone())),
                    ("page".to_string(), json!(number)),
                    ("pages".to_string(), json!(pages)),
                ]);
                ctx.insert("items".into(), entries.into());
                view = Some(MachineView {
                    path: page.path.clone(),
                    release_seq: self.project.release_seq,
                    kind: Some(ContentKind::Collection),
                    language: Some(page.language.clone()),
                    title: Some(list_title.clone()),
                    resource,
                    capabilities: self.capabilities_for(*kind),
                    json_ld: Some(json_ld.clone()),
                    markdown: md_absolute.clone(),
                });
                "list.html"
            }
            PageKind::Home { item, sections } => {
                let name = item.map_or_else(
                    || self.project.config.name.clone(),
                    |i| title(&self.project.items[i]),
                );
                ctx.insert("title".into(), name.clone().into());
                markdown.push_str(&format!("# {name}\n\n"));
                let mut resource = BTreeMap::new();
                if let Some(index) = item {
                    let home = &self.project.items[*index];
                    ctx.insert("translations".into(), self.translations(home).into());
                    resource = self.resource(home, &page.path);
                    if let Some(Markdown(body)) = &home.body {
                        ctx.insert("body".into(), markdown_to_html(body).into());
                        markdown.push_str(body.trim_end());
                        markdown.push_str("\n\n");
                    }
                    let summary =
                        field_str(home, "summary").or_else(|| field_str(home, "description"));
                    ctx.insert(
                        "description".into(),
                        summary.or(self.project.config.description.as_deref()).into(),
                    );
                } else if let Some(description) = &self.project.config.description {
                    ctx.insert("description".into(), description.clone().into());
                    ctx.insert("body".into(), markdown_to_html(description).into());
                    markdown.push_str(&format!("{description}\n\n"));
                }
                let prefix = self.project.config.language_prefix(&page.language);
                let sections: Vec<Value> = sections
                    .iter()
                    .map(|(kind, members)| {
                        let label = string(
                            self.project,
                            &language,
                            &format!("kind_{}", crate::content::kind_name(*kind)),
                        );
                        let entries: Vec<Value> = members.iter().map(|&i| self.entry(i)).collect();
                        markdown.push_str(&format!("## {label}\n\n"));
                        for entry in &entries {
                            markdown.push_str(&format!(
                                "- [{}]({}{}index.md)\n",
                                entry["title"].as_str().unwrap_or_default(),
                                self.base_url,
                                entry["url"].as_str().unwrap_or_default()
                            ));
                        }
                        markdown.push('\n');
                        json!({
                            "title": label,
                            "items": entries,
                            "more": format!("{prefix}/{}/", crate::content::kind_directory(*kind)),
                        })
                    })
                    .collect();
                ctx.insert("sections".into(), sections.into());
                view = Some(MachineView {
                    path: page.path.clone(),
                    release_seq: self.project.release_seq,
                    kind: Some(ContentKind::Page),
                    language: Some(page.language.clone()),
                    title: Some(name),
                    resource,
                    capabilities: self.capabilities_for(None),
                    json_ld: Some(json_ld.clone()),
                    markdown: md_absolute.clone(),
                });
                "item.html"
            }
            PageKind::NotFound | PageKind::Gone => {
                let (title_key, text_key) = if matches!(page.kind, PageKind::NotFound) {
                    ("not_found_title", "not_found_text")
                } else {
                    ("gone_title", "gone_text")
                };
                ctx.insert("title".into(), string(self.project, &language, title_key).into());
                ctx.insert("noindex".into(), true.into());
                ctx.insert(
                    "notices".into(),
                    vec![json!({ "class": "info", "title": "", "text": string(self.project, &language, text_key) })].into(),
                );
                "notice.html"
            }
        };
        let context = json!({ "site": self.site(page), "page": Value::Object(ctx), "t": self.strings(&language) });
        let html = self
            .env
            .get_template(template)
            .and_then(|t| t.render(minijinja::Value::from_serialize(&context)))
            .map_err(|e| SiteError::Template {
                template: template.into(),
                message: format!("{e:#}"),
            })?;
        let mut files = vec![(html_path, html.into_bytes())];
        if let (Some(md_path), Some(view_path), Some(view)) = (md_path, view_path, view) {
            if !markdown.ends_with('\n') {
                markdown.push('\n');
            }
            files.push((md_path, markdown.into_bytes()));
            let mut view = view;
            view.json_ld = view.json_ld.map(sorted);
            view.resource =
                view.resource.into_iter().map(|(key, value)| (key, sorted(value))).collect();
            let mut json = serde_json::to_vec_pretty(&view)
                .map_err(|e| SiteError::serde("machine view", e))?;
            json.push(b'\n');
            files.push((view_path, json));
        }
        Ok(files)
    }

    fn item_context(
        &self,
        item: &ContentItem,
        page: &Page,
        language: &str,
        ctx: &mut Map<String, Value>,
        markdown: &mut String,
    ) {
        let body = item.body.as_ref().map(|Markdown(b)| b.as_str()).unwrap_or_default();
        if let Some(summary) = field_str(item, "summary") {
            markdown.push_str(&format!("> {summary}\n\n"));
        }
        ctx.insert("byline".into(), field_str(item, "byline").into());
        ctx.insert("date".into(), date_only(published_at(item)).into());
        let notices = self.notices(item, language);
        for notice in &notices {
            markdown.push_str(&format!(
                "> **{}** {} {}\n\n",
                notice["title"].as_str().unwrap_or_default(),
                notice["date"].as_str().unwrap_or_default(),
                notice["text"].as_str().unwrap_or_default()
            ));
        }
        ctx.insert("notices".into(), notices.into());
        let figures: Vec<Value> = item
            .media
            .iter()
            .filter(|m| m.content_type.starts_with("image/"))
            .filter_map(|m| {
                let file = m.path.rsplit('/').next().unwrap_or(&m.path);
                (!body.contains(file)).then(|| {
                    json!({
                        "src": file,
                        "alt": m.alt.clone().unwrap_or_default(),
                        "caption": m.generated.then(|| string(self.project, language, "illustration")),
                    })
                })
            })
            .collect();
        ctx.insert("figures".into(), figures.into());
        let mut rows = Vec::new();
        for key in DISPLAY_FIELDS {
            if self.is_live(item, key) {
                continue;
            }
            let Some(value) = item.fields.get(*key).filter(|v| !v.is_null()) else { continue };
            let mut text = display(value);
            if *key == "price"
                && let Some(currency) = field_str(item, "currency")
            {
                text = format!("{text} {currency}");
            }
            let label = string(self.project, language, &format!("field_{key}"));
            markdown.push_str(&format!("- **{label}:** {text}\n"));
            rows.push(json!({ "label": label, "value": text }));
        }
        if item.kind == ContentKind::Notice {
            let dates = [
                ("field_publishedAt", date_only(published_at(item))),
                ("field_expiresAt", date_only(item.schedule.as_ref().and_then(|s| s.expires_at))),
            ];
            for (key, date) in dates {
                if let Some(date) = date {
                    let label = string(self.project, language, key);
                    markdown.push_str(&format!("- **{label}:** {date}\n"));
                    rows.push(json!({ "label": label, "value": date }));
                }
            }
        }
        if !rows.is_empty() {
            markdown.push('\n');
        }
        ctx.insert("fields".into(), rows.into());
        let as_of = self.live_as_of(item);
        let live: Vec<Value> = crate::lanes::live_fields(&self.project.config, item)
            .into_iter()
            .map(|name| {
                json!({
                    "name": name,
                    "label": string(self.project, language, &format!("field_{name}")),
                    "value": item.fields.get(name).map(display),
                    "as_of": as_of.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "as_of_date": date_only(Some(as_of)),
                })
            })
            .collect();
        ctx.insert("live".into(), live.into());
        if !body.trim().is_empty() {
            ctx.insert("body".into(), markdown_to_html(body).into());
            markdown.push_str(body.trim_end());
            markdown.push('\n');
        }
        let stream = self.stream_entries(item, markdown);
        if !stream.is_empty() {
            ctx.insert("stream".into(), json!({ "id": item.id.0, "entries": stream }));
        }
        let prefix = self.project.config.language_prefix(&page.language);
        let tag_links: Vec<Value> = tags(item)
            .into_iter()
            .map(|tag| json!({ "name": tag, "url": format!("{prefix}/tags/{}/", slugify(&tag)) }))
            .collect();
        ctx.insert("tags".into(), tag_links.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_keys_are_sorted_whatever_serde_json_keeps() {
        let value = json!({ "url": "u", "@type": "WebPage", "a": [{ "z": 1, "b": 2 }] });
        assert_eq!(
            serde_json::to_string(&sorted(value)).unwrap(),
            r#"{"@type":"WebPage","a":[{"b":2,"z":1}],"url":"u"}"#
        );
    }

    #[test]
    fn markdown_is_safe_and_demoted() {
        let html = markdown_to_html(
            "# A\n\n## B\n\n<img src=x onerror=alert(1)>\n\n![p](data:image/png;base64,AA)",
        );
        assert!(html.contains("<h2>A</h2>") && html.contains("<h3>B</h3>"));
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(html.contains("src=\"#\""));
    }

    #[test]
    fn script_json_cannot_close_the_element() {
        let text = script_json(&json!({ "name": "</script><script>alert(1)</script>" }));
        assert!(!text.contains("</script>"));
    }
}
