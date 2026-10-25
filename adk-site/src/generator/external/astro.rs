//! The Astro layout: `astro.config.mjs`, the contexts as JSON, and one dynamic page route
//! that renders them (spec 3.5 §7.8 "Generator").

use std::path::Path;

use serde_json::Value;

use super::{PageContext, json_file, write_file, write_starter};
use crate::error::{Result, SiteError};

const PACKAGE: &str = r#"{ "name": "adk-site-astro", "private": true, "type": "module" }
"#;

const CONFIG: &str = r#"// Written by adk-site for Astro. The output directory comes from the sandboxed run.
export default {
  site: "BASE_URL",
  outDir: process.env.ADK_SITE_OUT,
  build: { format: "directory" },
  compressHTML: false,
  devToolbar: { enabled: false },
  vite: { cacheDir: ".vite-cache", logLevel: "warn" },
};
"#;

/// The built-in default templates as an Astro page.
const PAGE: &str = r##"---
import pages from "../../adk/pages.json";

export function getStaticPaths() {
  return pages.map((c) => ({ params: { n: c.n }, props: { c } }));
}

const { c } = Astro.props;
const s = c.site;
const p = c.page;
const t = c.t;
---
<!doctype html>
<html lang={p.language}>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{p.title && p.title !== s.name ? `${p.title} · ` : ""}{s.name}</title>
{p.description && <meta name="description" content={p.description}>}
{p.noindex && <meta name="robots" content="noindex">}
<link rel="canonical" href={p.url}>
{(p.translations ?? []).map((alt) => <link rel="alternate" hreflang={alt.language} href={alt.url}>)}
{s.contract && <link rel="awp" href={s.contract}>}
{p.markdown && <link rel="alternate" type="text/markdown" href={p.markdown}>}
{p.machine_view && <link rel="alternate" type="application/awp+json" href={p.machine_view}>}
<link rel="stylesheet" href={s.stylesheet}>
{p.json_ld && <script type="application/ld+json" is:inline set:html={p.json_ld}></script>}
</head>
<body>
<a class="skip-link" href="#content">{t.skip_to_content}</a>
<header class="site-header">
<a class="site-name" href={s.home}>{s.name}</a>
{s.nav && s.nav.length > 0 && <nav aria-label={t.main_navigation}><ul>{s.nav.map((link) => <li><a href={link.url} aria-current={link.current ? "page" : undefined}>{link.title}</a></li>)}</ul></nav>}
</header>
<main id="content">
{c.template === "item" && (
<article>
<header>
<h1>{p.title}</h1>
{(p.byline || p.date) && <p class="meta">{p.byline && <><span class="byline">{t.by} {p.byline}</span> </>}{p.date && <time datetime={p.date}>{p.date}</time>}</p>}
</header>
{(p.notices ?? []).map((notice) => <aside class={`notice notice-${notice.class}`} aria-label={notice.title}><p><strong>{notice.title}</strong>{notice.date && <> <time datetime={notice.date}>{notice.date}</time></>}</p>{notice.text && <p>{notice.text}</p>}</aside>)}
{(p.figures ?? []).map((figure, i) => <figure><img src={figure.src} alt={figure.alt} width={figure.width} height={figure.width ? figure.height : undefined} loading={i > 0 ? "lazy" : undefined}>{figure.caption && <figcaption>{figure.caption}</figcaption>}</figure>)}
{p.fields && p.fields.length > 0 && <dl class="facts">{p.fields.map((field) => <><dt>{field.label}</dt><dd>{field.value}</dd></>)}</dl>}
{p.live && p.live.length > 0 && <dl class="live">{p.live.map((field) => <><dt>{field.label}</dt><dd data-awp-live={field.name}>{field.value ? <>{field.value} <small>({t.as_of} <time datetime={field.as_of}>{field.as_of_date}</time>)</small></> : t.check_availability}</dd></>)}</dl>}
{p.body && <div class="body"><Fragment set:html={p.body} /></div>}
{p.stream && <section class="stream" data-awp-stream={p.stream.id} aria-label={t.live_updates}><h2>{t.live_updates}</h2><ol reversed>{(p.stream.entries ?? []).map((entry) => <li id={`entry-${entry.n}`}><time datetime={entry.at}>{entry.time}</time>{entry.corrects && <> <small>({t.corrects} #{entry.corrects})</small></>}<Fragment set:html={entry.html} /></li>)}</ol></section>}
{p.tags && p.tags.length > 0 && <p class="tags">{t.tags}: {p.tags.map((tag, i) => <>{i > 0 && ", "}<a href={tag.url}>{tag.name}</a></>)}</p>}
{(p.sections ?? []).map((section, i) => (
<section aria-labelledby={`section-${i + 1}`}>
<h2 id={`section-${i + 1}`}>{section.title}</h2>
<ul class="items">{(section.items ?? []).map((entry) => <li><a href={entry.url}>{entry.title}</a>{entry.date && <> <time datetime={entry.date}>{entry.date}</time></>}{entry.summary && <p>{entry.summary}</p>}</li>)}</ul>
{section.more && <p><a href={section.more}>{t.all} {section.title}</a></p>}
</section>
))}
</article>
)}
{c.template === "list" && (
<>
<h1>{p.title}</h1>
{p.body && <div class="body"><Fragment set:html={p.body} /></div>}
<ul class="items">{(p.items ?? []).map((entry) => <li><h2><a href={entry.url}>{entry.title}</a></h2>{entry.date && <p class="meta"><time datetime={entry.date}>{entry.date}</time></p>}{entry.summary && <p>{entry.summary}</p>}</li>)}</ul>
{p.pagination && <nav aria-label={t.pagination}><p>{t.page} {p.pagination.current} {t.of} {p.pagination.total}</p><ul>{p.pagination.prev && <li><a href={p.pagination.prev} rel="prev">{t.previous}</a></li>}{p.pagination.next && <li><a href={p.pagination.next} rel="next">{t.next}</a></li>}</ul></nav>}
</>
)}
{c.template === "notice" && (
<>
<h1>{p.title}</h1>
{(p.notices ?? []).map((notice) => <div class={`notice notice-${notice.class}`}><p><strong>{notice.title}</strong>{notice.date && <> <time datetime={notice.date}>{notice.date}</time></>}</p>{notice.text && <p>{notice.text}</p>}</div>)}
<p><a href={s.home}>{t.home}</a></p>
</>
)}
</main>
<footer class="site-footer">
{s.legal && s.legal.length > 0 && <nav aria-label={t.legal_navigation}><ul>{s.legal.map((link) => <li><a href={link.url}>{link.title}</a></li>)}</ul></nav>}
{p.translations && p.translations.length > 0 && <nav aria-label={t.languages}><ul>{p.translations.map((alt) => <li><a href={alt.url} hreflang={alt.language} lang={alt.language}>{alt.name}</a></li>)}</ul></nav>}
<p>{s.name}</p>
</footer>
</body>
</html>
"##;

/// Writes the project: the starter files the project lacks, then the contexts.
pub(super) fn write_project(dir: &Path, base_url: &str, pages: &[PageContext]) -> Result<()> {
    write_starter(dir, "package.json", PACKAGE)?;
    write_starter(dir, "astro.config.mjs", &CONFIG.replace("BASE_URL", base_url))?;
    write_starter(dir, &format!("src/pages/{}/[n].astro", super::PAGES_DIR), PAGE)?;
    let data = Value::Array(pages.iter().map(PageContext::value).collect());
    write_file(dir, "src/adk/pages.json", &json_file(&data)?)
}

/// Gives the project a writable `node_modules` whose packages link to the toolchain's, so
/// Astro and its bundler resolve packages and write their caches (`node_modules/.astro`,
/// `.vite`) without installing anything or writing into the toolchain.
pub(super) fn link_toolchain(project: &Path, toolchain: Option<&Path>) -> Result<()> {
    let toolchain = toolchain.ok_or_else(|| SiteError::Generator {
        generator: "astro".into(),
        message: "no node_modules directory was given".into(),
    })?;
    let modules = project.join("node_modules");
    std::fs::create_dir_all(&modules).map_err(|e| SiteError::io(&modules, e))?;
    for entry in std::fs::read_dir(toolchain).map_err(|e| SiteError::io(toolchain, e))? {
        let entry = entry.map_err(|e| SiteError::io(toolchain, e))?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".astro" | ".vite" | ".cache" | ".package-lock.json")) {
            continue;
        }
        let link = modules.join(&name);
        if link.symlink_metadata().is_ok() {
            continue;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), &link).map_err(|e| SiteError::io(&link, e))?;
        #[cfg(not(unix))]
        return Err(SiteError::Generator {
            generator: "astro".into(),
            message: "Astro builds need a Unix host for the sandbox".into(),
        });
    }
    Ok(())
}
