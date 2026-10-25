//! What a build produces and what each output depends on (spec 3.5 §7.8.2 step 5).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use awp_types::{ContentId, ContentItem, ContentKind, ContentState, LanguageTag, Redirect};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{SiteProject, kind_directory, kind_name};
use crate::error::{Result, SiteError};
use crate::redirects::GONE_PAGE;

/// What an output depends on. An item's footprint is the set of keys it touches; an output
/// is rebuilt when a changed item's old or new footprint meets its keys.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "key", rename_all = "snake_case", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum DepKey {
    /// One item.
    Item {
        /// The item.
        id: ContentId,
    },
    /// The language versions of one item, linked to each other as alternates.
    Group {
        /// The kind.
        kind: ContentKind,
        /// The slug.
        slug: String,
    },
    /// The items of one kind in one language: listing pages and feeds.
    Kind {
        /// The kind.
        kind: ContentKind,
        /// The language.
        language: String,
    },
    /// The items with one tag in one language.
    Tag {
        /// The tag.
        tag: String,
        /// The language.
        language: String,
    },
    /// The home page sections of one language.
    Home {
        /// The language.
        language: String,
    },
    /// The navigation of one language, on every page.
    Nav {
        /// The language.
        language: String,
    },
    /// Every item: sitemaps, `llms.txt`, redirects.
    All,
    /// Configuration, theme and terms only.
    Site,
}

/// How an item appears in a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Visibility {
    /// Rendered as a page.
    Live,
    /// Withdrawn and replaced by a notice with this text.
    Notice(String),
    /// Withdrawn or expired: answers 410 Gone.
    Gone,
    /// Not published at the build time.
    Hidden,
}

pub(crate) fn field_str<'a>(item: &'a ContentItem, key: &str) -> Option<&'a str> {
    item.fields.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

pub(crate) fn visibility(item: &ContentItem, as_of: DateTime<Utc>, preview: bool) -> Visibility {
    if item.state == ContentState::Withdrawn {
        let withdrawal = item.fields.get("withdrawal");
        let mode = withdrawal.and_then(|w| w.get("mode")).and_then(Value::as_str).unwrap_or("gone");
        let text =
            withdrawal.and_then(|w| w.get("text")).and_then(Value::as_str).unwrap_or_default();
        return if mode == "notice" {
            Visibility::Notice(text.to_string())
        } else {
            Visibility::Gone
        };
    }
    let schedule = item.schedule.clone().unwrap_or_default();
    if schedule.expires_at.is_some_and(|at| at <= as_of) && !preview {
        return if item.kind == ContentKind::Notice { Visibility::Live } else { Visibility::Gone };
    }
    if preview {
        return Visibility::Live;
    }
    let embargoed = schedule.embargo_until.is_some_and(|at| at > as_of);
    let due = schedule.publish_at.is_none_or(|at| at <= as_of);
    match item.state {
        ContentState::Published | ContentState::Corrected | ContentState::Approved
            if !embargoed && due =>
        {
            Visibility::Live
        }
        ContentState::Scheduled if !embargoed && due && schedule.publish_at.is_some() => {
            Visibility::Live
        }
        _ => Visibility::Hidden,
    }
}

/// The page title of an item.
pub(crate) fn title(item: &ContentItem) -> String {
    field_str(item, "title").map_or_else(
        || item.slug.rsplit('/').next().unwrap_or(&item.slug).replace('-', " "),
        str::to_string,
    )
}

/// The item's date for listings and feeds.
pub(crate) fn item_date(item: &ContentItem, key: &str) -> Option<DateTime<Utc>> {
    let parse = |text: &str| {
        DateTime::parse_from_rfc3339(text).map(|d| d.with_timezone(&Utc)).ok().or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc())
        })
    };
    field_str(item, key).and_then(parse)
}

pub(crate) fn published_at(item: &ContentItem) -> Option<DateTime<Utc>> {
    item_date(item, "publishedAt").or_else(|| item.schedule.as_ref().and_then(|s| s.publish_at))
}

pub(crate) fn modified_at(item: &ContentItem) -> Option<DateTime<Utc>> {
    item_date(item, "updatedAt").or_else(|| published_at(item))
}

pub(crate) fn tags(item: &ContentItem) -> Vec<String> {
    let mut tags: Vec<String> = item
        .fields
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    tags.sort();
    tags.dedup();
    tags
}

/// A URL-safe slug for a tag.
pub(crate) fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "tag".into() } else { out }
}

/// The key of a translation group in a [`DependencyGraph`](super::DependencyGraph).
pub(crate) fn group_key(kind: ContentKind, slug: &str) -> String {
    format!("{}/{slug}", kind_name(kind))
}

pub(crate) fn lang_key(language: &LanguageTag) -> String {
    language.as_str().to_ascii_lowercase()
}

/// The URL path of an item's page, with leading and trailing slashes.
pub(crate) fn item_path(project: &SiteProject, item: &ContentItem) -> String {
    if let Some(path) = field_str(item, "path") {
        return path.to_string();
    }
    let prefix = project.config.language_prefix(&item.language);
    match item.kind {
        ContentKind::Page if matches!(item.slug.as_str(), "index" | "home") => format!("{prefix}/"),
        ContentKind::Page | ContentKind::Collection => format!("{prefix}/{}/", item.slug),
        kind => format!("{prefix}/{}/{}/", kind_directory(kind), item.slug),
    }
}

/// The files of a page at URL `path`: HTML, Markdown twin, machine view.
pub(crate) fn page_files(path: &str) -> (String, Option<String>, Option<String>) {
    let trimmed = path.trim_start_matches('/');
    if trimmed.ends_with(".html") {
        return (trimmed.to_string(), None, None);
    }
    (
        format!("{trimmed}index.html"),
        Some(format!("{trimmed}index.md")),
        Some(format!("{trimmed}index.awp.json")),
    )
}

/// What a page shows.
#[derive(Debug, Clone)]
pub(crate) enum PageKind {
    /// An item's page.
    Item(usize),
    /// A withdrawn item replaced by a notice.
    Notice(usize, String),
    /// A listing of items: a kind, a tag or a collection, one page of it.
    Listing {
        title: String,
        kind: Option<ContentKind>,
        collection: Option<usize>,
        members: Vec<usize>,
        page: usize,
        pages: usize,
        base: String,
    },
    /// A language's home page.
    Home { item: Option<usize>, sections: Vec<(ContentKind, Vec<usize>)> },
    /// `/404.html`.
    NotFound,
    /// `/410.html`.
    Gone,
}

#[derive(Debug, Clone)]
pub(crate) struct Page {
    pub path: String,
    pub language: LanguageTag,
    pub kind: PageKind,
    pub deps: BTreeSet<DepKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NavLink {
    pub title: String,
    pub url: String,
}

/// Everything a build will produce, before rendering.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub pages: Vec<Page>,
    /// `(output path, repository path, item index)`.
    pub media: Vec<(String, String, usize)>,
    pub gone: Vec<Redirect>,
    pub footprints: BTreeMap<ContentId, BTreeSet<DepKey>>,
    pub nav: BTreeMap<String, Vec<NavLink>>,
    pub legal: BTreeMap<String, Vec<NavLink>>,
    /// Live items by index.
    pub live: Vec<usize>,
    pub paths: BTreeMap<usize, String>,
    /// Translation groups: live item indexes by (kind, slug).
    pub groups: BTreeMap<(ContentKind, String), Vec<usize>>,
}

impl Plan {
    /// The navigation signature of a language: a change rebuilds every page in it.
    pub fn nav_signature(&self, language: &str) -> String {
        serde_json::to_string(&(self.nav.get(language), self.legal.get(language)))
            .unwrap_or_default()
    }

    /// The alternates signature of every translation group: its members' languages and
    /// paths. A change rebuilds the group's pages, which link each other.
    pub fn group_signatures(&self, items: &[ContentItem]) -> BTreeMap<String, String> {
        self.groups
            .iter()
            .map(|((kind, slug), members)| {
                let alternates: Vec<(&str, &str)> = members
                    .iter()
                    .map(|&i| (items[i].language.as_str(), self.paths[&i].as_str()))
                    .collect();
                (group_key(*kind, slug), serde_json::to_string(&alternates).unwrap_or_default())
            })
            .collect()
    }
}

/// A localised interface string.
pub(crate) fn string(project: &SiteProject, language: &str, key: &str) -> String {
    project
        .config
        .strings
        .get(language)
        .and_then(|s| s.get(key))
        .cloned()
        .unwrap_or_else(|| project.theme.string(key).to_string())
}

fn listing_order(kind: Option<ContentKind>, items: &[ContentItem], members: &mut [usize]) {
    let chronological =
        matches!(kind, Some(ContentKind::Post | ContentKind::Article | ContentKind::Notice) | None);
    if kind == Some(ContentKind::Event) {
        members.sort_by_key(|&i| {
            (item_date(&items[i], "startsAt"), title(&items[i]), items[i].id.clone())
        });
    } else if chronological {
        members.sort_by_key(|&i| {
            (Reverse(published_at(&items[i])), title(&items[i]), items[i].id.clone())
        });
    } else {
        members.sort_by_key(|&i| (title(&items[i]), items[i].id.clone()));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_listing(
    pages: &mut Vec<Page>,
    project: &SiteProject,
    language: &LanguageTag,
    base: String,
    title: String,
    kind: Option<ContentKind>,
    collection: Option<usize>,
    members: Vec<usize>,
    deps: BTreeSet<DepKey>,
) {
    let size = project.config.page_size.max(1);
    let pages_total = members.len().div_ceil(size).max(1);
    for page in 1..=pages_total {
        let path = if page == 1 { base.clone() } else { format!("{base}page/{page}/") };
        let slice = members.iter().skip((page - 1) * size).take(size).copied().collect();
        pages.push(Page {
            path,
            language: language.clone(),
            kind: PageKind::Listing {
                title: title.clone(),
                kind,
                collection,
                members: slice,
                page,
                pages: pages_total,
                base: base.clone(),
            },
            deps: deps.clone(),
        });
    }
}

const LISTED_KINDS: &[ContentKind] = &[
    ContentKind::Post,
    ContentKind::Article,
    ContentKind::Product,
    ContentKind::Service,
    ContentKind::Event,
    ContentKind::Notice,
    ContentKind::Person,
];

/// Plans a build of `project`.
pub(crate) fn plan(project: &SiteProject) -> Result<Plan> {
    let items = &project.items;
    let config = &project.config;
    let mut pages = Vec::new();
    let mut media = Vec::new();
    let mut gone = Vec::new();
    let mut footprints = BTreeMap::new();
    let mut live = Vec::new();
    let mut paths = BTreeMap::new();
    let mut groups: BTreeMap<(ContentKind, String), Vec<usize>> = BTreeMap::new();
    let mut by_kind: BTreeMap<(ContentKind, String), Vec<usize>> = BTreeMap::new();
    let mut by_tag: BTreeMap<(String, String), (String, Vec<usize>)> = BTreeMap::new();
    let mut homes: BTreeMap<String, usize> = BTreeMap::new();
    let mut collections = Vec::new();

    for (index, item) in items.iter().enumerate() {
        let language = lang_key(&item.language);
        let visibility = visibility(item, project.as_of, project.preview);
        let mut footprint = BTreeSet::from([DepKey::Item { id: item.id.clone() }, DepKey::All]);
        if visibility != Visibility::Hidden {
            footprint.insert(DepKey::Kind { kind: item.kind, language: language.clone() });
            if config.home_sections.contains(&item.kind) {
                footprint.insert(DepKey::Home { language: language.clone() });
            }
            for tag in tags(item) {
                footprint.insert(DepKey::Tag { tag: slugify(&tag), language: language.clone() });
            }
        }
        footprints.insert(item.id.clone(), footprint);
        let path = item_path(project, item);
        match visibility {
            Visibility::Hidden => continue,
            Visibility::Gone => {
                gone.push(Redirect { from: path, to: GONE_PAGE.into(), status: 410 });
                continue;
            }
            Visibility::Notice(text) => {
                let deps = BTreeSet::from([
                    DepKey::Item { id: item.id.clone() },
                    DepKey::Nav { language: language.clone() },
                ]);
                pages.push(Page {
                    path: path.clone(),
                    language: item.language.clone(),
                    kind: PageKind::Notice(index, text),
                    deps,
                });
                paths.insert(index, path);
                continue;
            }
            Visibility::Live => {}
        }
        live.push(index);
        paths.insert(index, path.clone());
        groups.entry((item.kind, item.slug.clone())).or_default().push(index);
        for tag in tags(item) {
            by_tag
                .entry((slugify(&tag), language.clone()))
                .or_insert_with(|| (tag.clone(), Vec::new()))
                .1
                .push(index);
        }
        let (html, _, _) = page_files(&path);
        let dir = html.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
        for reference in &item.media {
            let name = reference.path.rsplit('/').next().unwrap_or(&reference.path);
            media.push((format!("{dir}{name}"), reference.path.clone(), index));
        }
        let prefix = config.language_prefix(&item.language);
        let is_home = item.kind == ContentKind::Page && path == format!("{prefix}/");
        if is_home {
            homes.insert(language.clone(), index);
            continue;
        }
        if item.kind == ContentKind::Collection {
            collections.push(index);
            continue;
        }
        by_kind.entry((item.kind, language.clone())).or_default().push(index);
        let deps = BTreeSet::from([
            DepKey::Item { id: item.id.clone() },
            DepKey::Group { kind: item.kind, slug: item.slug.clone() },
            DepKey::Nav { language: language.clone() },
        ]);
        pages.push(Page {
            path,
            language: item.language.clone(),
            kind: PageKind::Item(index),
            deps,
        });
    }

    // Collections: listings of one kind, optionally one tag, at their own path.
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for &index in &collections {
        let item = &items[index];
        let language = lang_key(&item.language);
        let of = field_str(item, "of")
            .and_then(|k| serde_json::from_value::<ContentKind>(Value::String(k.into())).ok());
        let tag = field_str(item, "tag").map(slugify);
        let mut members: Vec<usize> = by_kind
            .iter()
            .filter(|((kind, lang), _)| lang == &language && of.is_none_or(|of| of == *kind))
            .flat_map(|(_, v)| v.iter().copied())
            .filter(|&i| {
                tag.as_ref().is_none_or(|t| tags(&items[i]).iter().any(|x| &slugify(x) == t))
            })
            .collect();
        listing_order(of, items, &mut members);
        let mut deps = BTreeSet::from([
            DepKey::Item { id: item.id.clone() },
            DepKey::Group { kind: item.kind, slug: item.slug.clone() },
            DepKey::Nav { language: language.clone() },
        ]);
        match of {
            Some(kind) => {
                deps.insert(DepKey::Kind { kind, language: language.clone() });
            }
            None => {
                deps.insert(DepKey::All);
            }
        }
        if let Some(tag) = &tag {
            deps.insert(DepKey::Tag { tag: tag.clone(), language: language.clone() });
        }
        let base = paths[&index].clone();
        covered.insert(base.clone());
        push_listing(
            &mut pages,
            project,
            &item.language,
            base,
            title(item),
            of,
            Some(index),
            members,
            deps,
        );
    }

    // Kind listings and navigation.
    let mut nav: BTreeMap<String, Vec<NavLink>> = BTreeMap::new();
    let mut legal: BTreeMap<String, Vec<NavLink>> = BTreeMap::new();
    for language in &config.languages {
        let key = lang_key(language);
        let prefix = config.language_prefix(language);
        let mut links: Vec<(f64, String, String)> = live
            .iter()
            .map(|&i| &items[i])
            .filter(|item| item.kind == ContentKind::Page && lang_key(&item.language) == key)
            .filter_map(|item| {
                item.fields
                    .get("navOrder")
                    .and_then(Value::as_f64)
                    .map(|order| (order, title(item), item_path(project, item)))
            })
            .collect();
        links.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut entries: Vec<NavLink> =
            links.into_iter().map(|(_, title, url)| NavLink { title, url }).collect();
        for kind in LISTED_KINDS {
            let Some(members) = by_kind.get(&(*kind, key.clone())) else { continue };
            let base = format!("{prefix}/{}/", kind_directory(*kind));
            let label = string(project, &key, &format!("kind_{}", kind_name(*kind)));
            if !covered.contains(&base) {
                let mut members = members.clone();
                listing_order(Some(*kind), items, &mut members);
                let deps = BTreeSet::from([
                    DepKey::Kind { kind: *kind, language: key.clone() },
                    DepKey::Nav { language: key.clone() },
                ]);
                push_listing(
                    &mut pages,
                    project,
                    language,
                    base.clone(),
                    label.clone(),
                    Some(*kind),
                    None,
                    members,
                    deps,
                );
            }
            entries.push(NavLink { title: label, url: base });
        }
        nav.insert(key.clone(), entries);
        let mut legal_links: Vec<(String, NavLink)> = live
            .iter()
            .map(|&i| &items[i])
            .filter(|item| lang_key(&item.language) == key)
            .filter_map(|item| {
                field_str(item, "legal").map(|kind| {
                    (
                        kind.to_string(),
                        NavLink { title: title(item), url: item_path(project, item) },
                    )
                })
            })
            .collect();
        legal_links.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.url.cmp(&b.1.url)));
        legal.insert(key.clone(), legal_links.into_iter().map(|(_, link)| link).collect());

        // Home.
        let mut sections = Vec::new();
        for kind in &config.home_sections {
            if let Some(members) = by_kind.get(&(*kind, key.clone())) {
                let mut members = members.clone();
                listing_order(Some(*kind), items, &mut members);
                members.truncate(5);
                sections.push((*kind, members));
            }
        }
        let home_item = homes.get(&key).copied();
        let mut deps = BTreeSet::from([
            DepKey::Home { language: key.clone() },
            DepKey::Nav { language: key.clone() },
        ]);
        if let Some(index) = home_item {
            deps.insert(DepKey::Item { id: items[index].id.clone() });
            deps.insert(DepKey::Group { kind: items[index].kind, slug: items[index].slug.clone() });
        }
        pages.push(Page {
            path: format!("{prefix}/"),
            language: language.clone(),
            kind: PageKind::Home { item: home_item, sections },
            deps,
        });
    }

    // Tag pages.
    for ((tag_slug, language), (tag, mut members)) in by_tag {
        let Some(tag_language) = config.languages.iter().find(|l| lang_key(l) == language) else {
            continue;
        };
        listing_order(None, items, &mut members);
        let prefix = config.language_prefix(tag_language);
        let deps = BTreeSet::from([
            DepKey::Tag { tag: tag_slug.clone(), language: language.clone() },
            DepKey::Nav { language: language.clone() },
        ]);
        let title = format!("{}: {tag}", string(project, &language, "tag"));
        push_listing(
            &mut pages,
            project,
            tag_language,
            format!("{prefix}/tags/{tag_slug}/"),
            title,
            None,
            None,
            members,
            deps,
        );
    }

    let default_language = config.default_language().clone();
    let default_key = lang_key(&default_language);
    for (path, kind) in [("/404.html", PageKind::NotFound), (GONE_PAGE, PageKind::Gone)] {
        let deps = BTreeSet::from([DepKey::Nav { language: default_key.clone() }]);
        pages.push(Page { path: path.into(), language: default_language.clone(), kind, deps });
    }

    pages.sort_by(|a, b| a.path.cmp(&b.path));
    for pair in pages.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(SiteError::InvalidContent {
                path: pair[0].path.clone(),
                message: "two pages have this URL; change a slug or a `path` field".into(),
            });
        }
    }
    gone.sort_by(|a, b| a.from.cmp(&b.from));
    media.sort();
    media.dedup_by(|a, b| a.0 == b.0);
    for members in groups.values_mut() {
        members.sort_by_key(|&i| {
            let position =
                config.languages.iter().position(|l| lang_key(l) == lang_key(&items[i].language));
            (position.unwrap_or(usize::MAX), items[i].language.clone())
        });
    }
    Ok(Plan { pages, media, gone, footprints, nav, legal, live, paths, groups })
}
