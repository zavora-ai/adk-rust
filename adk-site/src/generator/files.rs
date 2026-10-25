//! Site-wide files: sitemaps, feeds, `llms.txt`, `robots.txt`, `security.txt`, the public
//! charter terms, the stylesheet and redirects (spec 3.5 §4.4, §7.6, §7.8 "Generator").

use std::collections::BTreeSet;

use awp_types::ContentKind;
use chrono::{DateTime, SecondsFormat, Utc};

use super::plan::{
    DepKey, PageKind, Plan, field_str, lang_key, modified_at, page_files, published_at, string,
    title,
};
use crate::content::{SiteProject, kind_name};
use crate::error::{Result, SiteError};

/// URLs per sitemap file (sitemaps.org).
pub(crate) const SITEMAP_LIMIT: usize = 50_000;
/// The public charter terms (spec 3.5 §7.6).
pub const CHARTER_PATH: &str = ".well-known/awp/charter.json";

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn deps(keys: &[DepKey]) -> BTreeSet<DepKey> {
    keys.iter().cloned().collect()
}

/// The paths of the site-wide files a plan produces, with their dependencies, without
/// rendering them.
pub(crate) fn site_file_paths(
    project: &SiteProject,
    plan: &Plan,
) -> Vec<(String, BTreeSet<DepKey>)> {
    let mut out = vec![
        ("assets/site.css".to_string(), deps(&[DepKey::Site])),
        ("robots.txt".to_string(), deps(&[DepKey::Site])),
        ("llms.txt".to_string(), deps(&[DepKey::All])),
    ];
    let urls = sitemap_entries(project, plan).len();
    if urls <= SITEMAP_LIMIT {
        out.push(("sitemap.xml".into(), deps(&[DepKey::All])));
    } else {
        out.push(("sitemap.xml".into(), deps(&[DepKey::All])));
        for n in 1..=urls.div_ceil(SITEMAP_LIMIT) {
            out.push((format!("sitemap-{n}.xml"), deps(&[DepKey::All])));
        }
    }
    if !security_contacts(project).is_empty() {
        out.push((".well-known/security.txt".into(), deps(&[DepKey::Site])));
    }
    if project.public_terms.is_some() {
        out.push((CHARTER_PATH.into(), deps(&[DepKey::Site])));
    }
    for language in &project.config.languages {
        let key = lang_key(language);
        if feed_members(project, plan, &key).is_empty() {
            continue;
        }
        let prefix = project.config.language_prefix(language).trim_start_matches('/').to_string();
        let path =
            if prefix.is_empty() { "feed.xml".to_string() } else { format!("{prefix}/feed.xml") };
        out.push((
            path,
            deps(&[
                DepKey::Kind { kind: ContentKind::Post, language: key.clone() },
                DepKey::Kind { kind: ContentKind::Article, language: key },
            ]),
        ));
    }
    for (path, _) in project.redirects_format.render(&redirects(project, plan)) {
        out.push((path, deps(&[DepKey::All, DepKey::Site])));
    }
    for path in project.documents.keys() {
        out.push((path.trim_start_matches('/').to_string(), deps(&[DepKey::Site])));
    }
    out
}

/// The full redirect set: configured redirects, then 410 rules for withdrawn pages.
pub(crate) fn redirects(project: &SiteProject, plan: &Plan) -> Vec<awp_types::Redirect> {
    let mut all = project.config.redirects.clone();
    all.extend(plan.gone.iter().cloned());
    all
}

fn security_contacts(project: &SiteProject) -> Vec<String> {
    let mut contacts = project.config.security.contact.clone();
    if contacts.is_empty()
        && let Some(email) = project.public_terms.as_ref().and_then(|t| t.contact.email.clone())
    {
        contacts.push(format!("mailto:{email}"));
    }
    contacts
}

fn feed_members(project: &SiteProject, plan: &Plan, language: &str) -> Vec<usize> {
    let mut members: Vec<usize> = plan
        .live
        .iter()
        .copied()
        .filter(|&i| {
            let item = &project.items[i];
            matches!(item.kind, ContentKind::Post | ContentKind::Article)
                && lang_key(&item.language) == language
        })
        .collect();
    members.sort_by_key(|&i| {
        (std::cmp::Reverse(published_at(&project.items[i])), project.items[i].id.clone())
    });
    members.truncate(50);
    members
}

struct SitemapEntry {
    loc: String,
    lastmod: Option<DateTime<Utc>>,
    alternates: Vec<(String, String)>,
}

fn sitemap_entries(project: &SiteProject, plan: &Plan) -> Vec<SitemapEntry> {
    let base = project.config.base_url();
    let mut entries = Vec::new();
    for page in &plan.pages {
        let (lastmod, alternates) = match &page.kind {
            PageKind::Item(index) => {
                let item = &project.items[*index];
                let alternates = plan
                    .groups
                    .get(&(item.kind, item.slug.clone()))
                    .filter(|g| g.len() > 1)
                    .map(|group| {
                        group
                            .iter()
                            .map(|&i| {
                                (
                                    project.items[i].language.as_str().to_string(),
                                    format!("{base}{}", plan.paths[&i]),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (modified_at(item), alternates)
            }
            PageKind::Listing { members, .. } => {
                (members.iter().filter_map(|&i| modified_at(&project.items[i])).max(), Vec::new())
            }
            PageKind::Home { item, sections } => {
                let own = item.and_then(|i| modified_at(&project.items[i]));
                let latest = sections
                    .iter()
                    .flat_map(|(_, m)| m.iter())
                    .filter_map(|&i| modified_at(&project.items[i]))
                    .max();
                (own.max(latest), Vec::new())
            }
            PageKind::Notice(..) | PageKind::NotFound | PageKind::Gone => continue,
        };
        entries.push(SitemapEntry { loc: format!("{base}{}", page.path), lastmod, alternates });
    }
    entries
}

fn urlset(entries: &[SitemapEntry]) -> String {
    let mut xml_text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" \
         xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );
    for entry in entries {
        xml_text.push_str(&format!("<url><loc>{}</loc>", xml(&entry.loc)));
        if let Some(lastmod) = entry.lastmod {
            xml_text.push_str(&format!("<lastmod>{}</lastmod>", stamp(lastmod)));
        }
        for (language, href) in &entry.alternates {
            xml_text.push_str(&format!(
                "<xhtml:link rel=\"alternate\" hreflang=\"{}\" href=\"{}\"/>",
                xml(language),
                xml(href)
            ));
        }
        xml_text.push_str("</url>\n");
    }
    xml_text.push_str("</urlset>\n");
    xml_text
}

/// Renders one site-wide file.
pub(crate) fn render(project: &SiteProject, plan: &Plan, path: &str) -> Result<Vec<u8>> {
    let config = &project.config;
    let base = config.base_url();
    let text = match path {
        "assets/site.css" => project.theme.stylesheet(),
        "robots.txt" => {
            let mut text = String::from("User-agent: *\n");
            if let Some(usage) = project.public_terms.as_ref().and_then(|t| t.usage.as_ref()) {
                text.push_str(&format!("Content-Usage: {usage}\n"));
            }
            text.push_str(&format!("Allow: /\n\nSitemap: {base}/sitemap.xml\n"));
            text
        }
        "llms.txt" => {
            let mut text = format!("# {}\n\n", config.name);
            if let Some(description) = &config.description {
                text.push_str(&format!("> {description}\n\n"));
            }
            text.push_str(&format!(
                "Every page has a Markdown version at `<page>/index.md` and a structured version at \
                 `<page>/index.awp.json`.\n\n- [Home]({base}/index.md)\n"
            ));
            let default_language = lang_key(config.default_language());
            let mut kinds: Vec<ContentKind> =
                plan.live.iter().map(|&i| project.items[i].kind).collect();
            kinds.sort();
            kinds.dedup();
            for kind in kinds {
                let label =
                    string(project, &default_language, &format!("kind_{}", kind_name(kind)));
                text.push_str(&format!("\n## {label}\n\n"));
                for &index in &plan.live {
                    let item = &project.items[index];
                    let home = format!("{}/", config.language_prefix(&item.language));
                    if item.kind != kind || plan.paths.get(&index).is_none_or(|p| *p == home) {
                        continue;
                    }
                    let (_, md, _) = page_files(&plan.paths[&index]);
                    let Some(md) = md else { continue };
                    let suffix = if lang_key(&item.language) == default_language {
                        String::new()
                    } else {
                        format!(" ({})", item.language)
                    };
                    text.push_str(&format!("- [{}{suffix}]({base}/{md})", title(item)));
                    if let Some(summary) =
                        field_str(item, "summary").or_else(|| field_str(item, "description"))
                    {
                        text.push_str(&format!(": {summary}"));
                    }
                    text.push('\n');
                }
            }
            text
        }
        ".well-known/security.txt" => {
            let mut text = String::new();
            for contact in security_contacts(project) {
                text.push_str(&format!("Contact: {contact}\n"));
            }
            let expires =
                config.security.expires.unwrap_or(project.as_of + chrono::Duration::days(365));
            text.push_str(&format!("Expires: {}\n", stamp(expires)));
            let languages: Vec<&str> = config.languages.iter().map(|l| l.as_str()).collect();
            text.push_str(&format!("Preferred-Languages: {}\n", languages.join(", ")));
            text.push_str(&format!("Canonical: {base}/.well-known/security.txt\n"));
            if let Some(policy) = &config.security.policy {
                text.push_str(&format!("Policy: {policy}\n"));
            }
            if let Some(ack) = &config.security.acknowledgments {
                text.push_str(&format!("Acknowledgments: {ack}\n"));
            }
            text
        }
        CHARTER_PATH => {
            let terms = project
                .public_terms
                .as_ref()
                .ok_or_else(|| SiteError::Pipeline("no public terms".into()))?;
            let value =
                serde_json::to_value(terms).map_err(|e| SiteError::serde("public terms", e))?;
            adk_auth::signed_audit::canonicalize(&value)
                .map_err(|e| SiteError::serde("public terms", e))?
        }
        _ if path.starts_with("sitemap") && path.ends_with(".xml") => {
            let entries = sitemap_entries(project, plan);
            if entries.len() <= SITEMAP_LIMIT {
                urlset(&entries)
            } else if path == "sitemap.xml" {
                let mut text = String::from(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
                );
                for (n, chunk) in entries.chunks(SITEMAP_LIMIT).enumerate() {
                    text.push_str(&format!("<sitemap><loc>{base}/sitemap-{}.xml</loc>", n + 1));
                    if let Some(lastmod) = chunk.iter().filter_map(|e| e.lastmod).max() {
                        text.push_str(&format!("<lastmod>{}</lastmod>", stamp(lastmod)));
                    }
                    text.push_str("</sitemap>\n");
                }
                text.push_str("</sitemapindex>\n");
                text
            } else {
                let n: usize = path
                    .trim_start_matches("sitemap-")
                    .trim_end_matches(".xml")
                    .parse()
                    .map_err(|_| SiteError::Pipeline(format!("unknown sitemap '{path}'")))?;
                urlset(entries.chunks(SITEMAP_LIMIT).nth(n - 1).unwrap_or_default())
            }
        }
        _ if path.ends_with("feed.xml") => {
            let language = config
                .languages
                .iter()
                .find(|l| {
                    let prefix = config.language_prefix(l).trim_start_matches('/').to_string();
                    path == if prefix.is_empty() {
                        "feed.xml".to_string()
                    } else {
                        format!("{prefix}/feed.xml")
                    }
                })
                .ok_or_else(|| SiteError::Pipeline(format!("unknown feed '{path}'")))?;
            let key = lang_key(language);
            let members = feed_members(project, plan, &key);
            let prefix = config.language_prefix(language);
            let updated = members
                .iter()
                .filter_map(|&i| modified_at(&project.items[i]))
                .max()
                .unwrap_or(project.as_of);
            let mut text = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\" xml:lang=\"{}\">\n\
                 <title>{}</title>\n<id>{base}{prefix}/</id>\n<link rel=\"self\" href=\"{base}/{path}\"/>\n\
                 <link href=\"{base}{prefix}/\"/>\n<updated>{}</updated>\n",
                xml(language.as_str()),
                xml(&config.name),
                stamp(updated)
            );
            for index in members {
                let item = &project.items[index];
                let url = format!("{base}{}", plan.paths[&index]);
                let at = modified_at(item).unwrap_or(updated);
                text.push_str(&format!(
                    "<entry><title>{}</title><link href=\"{}\"/><id>{}</id><updated>{}</updated>",
                    xml(&title(item)),
                    xml(&url),
                    xml(&url),
                    stamp(at)
                ));
                if let Some(summary) = field_str(item, "summary") {
                    text.push_str(&format!("<summary>{}</summary>", xml(summary)));
                }
                text.push_str("</entry>\n");
            }
            text.push_str("</feed>\n");
            text
        }
        _ if project.documents.contains_key(path) => return Ok(project.documents[path].to_vec()),
        _ if project.documents.contains_key(&format!("/{path}")) => {
            return Ok(project.documents[&format!("/{path}")].to_vec());
        }
        _ => {
            let files = project.redirects_format.render(&redirects(project, plan));
            return files
                .into_iter()
                .find(|(p, _)| p == path)
                .map(|(_, bytes)| bytes)
                .ok_or_else(|| SiteError::Pipeline(format!("the generator has no file '{path}'")));
        }
    };
    Ok(text.into_bytes())
}
