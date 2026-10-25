//! The Hugo layout: `hugo.toml`, one content file per page with JSON front matter carrying
//! the context in `params.ctx`, and a Go-template page layout (spec 3.5 §7.8 "Generator").
//!
//! JSON front matter keeps every value a string or number: YAML would turn dates into
//! timestamps.

use std::path::Path;

use serde_json::json;

use super::{PageContext, write_file, write_starter};
use crate::error::{Result, SiteError};

const CONFIG: &str = r#"# Written by adk-site for Hugo. adk-site writes the home and list pages, sitemaps, feeds
# and robots.txt itself, so Hugo renders single pages only.
baseURL = "BASE_URL/"
disableKinds = ["home", "section", "taxonomy", "term", "rss", "sitemap", "robotstxt", "404"]
disableHugoGeneratorInject = true

[markup.goldmark.renderer]
unsafe = false
"#;

/// The built-in default templates in Go templates, with the context in `$c`.
const LAYOUT: &str = r##"{{- $c := .Params.ctx -}}{{- $s := $c.site -}}{{- $p := $c.page -}}{{- $t := $c.t -}}
<!doctype html>
<html lang="{{ $p.language }}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{ with $p.title }}{{ if ne . $s.name }}{{ . }} · {{ end }}{{ end }}{{ $s.name }}</title>
{{ with $p.description }}<meta name="description" content="{{ . }}">
{{ end }}{{ if $p.noindex }}<meta name="robots" content="noindex">
{{ end }}<link rel="canonical" href="{{ $p.url }}">
{{ range $p.translations }}<link rel="alternate" hreflang="{{ .language }}" href="{{ .url }}">
{{ end }}{{ with $s.contract }}<link rel="awp" href="{{ . }}">
{{ end }}{{ with $p.markdown }}<link rel="alternate" type="text/markdown" href="{{ . }}">
{{ end }}{{ with $p.machine_view }}<link rel="alternate" type="application/awp+json" href="{{ . }}">
{{ end }}<link rel="stylesheet" href="{{ $s.stylesheet }}">
{{ with $p.json_ld }}<script type="application/ld+json">{{ . | safeJS }}</script>
{{ end }}</head>
<body>
<a class="skip-link" href="#content">{{ $t.skip_to_content }}</a>
<header class="site-header">
<a class="site-name" href="{{ $s.home }}">{{ $s.name }}</a>
{{ with $s.nav }}<nav aria-label="{{ $t.main_navigation }}"><ul>{{ range . }}<li><a href="{{ .url }}"{{ if .current }} aria-current="page"{{ end }}>{{ .title }}</a></li>{{ end }}</ul></nav>
{{ end }}</header>
<main id="content">
{{ if eq $c.template "item" }}<article>
<header>
<h1>{{ $p.title }}</h1>
{{ if or $p.byline $p.date }}<p class="meta">{{ with $p.byline }}<span class="byline">{{ $t.by }} {{ . }}</span> {{ end }}{{ with $p.date }}<time datetime="{{ . }}">{{ . }}</time>{{ end }}</p>
{{ end }}</header>
{{ range $p.notices }}<aside class="notice notice-{{ .class }}" aria-label="{{ .title }}"><p><strong>{{ .title }}</strong>{{ with .date }} <time datetime="{{ . }}">{{ . }}</time>{{ end }}</p>{{ with .text }}<p>{{ . }}</p>{{ end }}</aside>
{{ end }}{{ range $i, $f := $p.figures }}<figure><img src="{{ $f.src }}" alt="{{ $f.alt }}"{{ with $f.width }} width="{{ . }}" height="{{ $f.height }}"{{ end }}{{ if gt $i 0 }} loading="lazy"{{ end }}>{{ with $f.caption }}<figcaption>{{ . }}</figcaption>{{ end }}</figure>
{{ end }}{{ with $p.fields }}<dl class="facts">{{ range . }}<dt>{{ .label }}</dt><dd>{{ .value }}</dd>{{ end }}</dl>
{{ end }}{{ with $p.live }}<dl class="live">{{ range $field := . }}<dt>{{ $field.label }}</dt><dd data-awp-live="{{ $field.name }}">{{ if $field.value }}{{ $field.value }} <small>({{ $t.as_of }} <time datetime="{{ $field.as_of }}">{{ $field.as_of_date }}</time>)</small>{{ else }}{{ $t.check_availability }}{{ end }}</dd>{{ end }}</dl>
{{ end }}{{ with $p.body }}<div class="body">
{{ . | safeHTML }}</div>
{{ end }}{{ with $p.stream }}<section class="stream" data-awp-stream="{{ .id }}" aria-label="{{ $t.live_updates }}"><h2>{{ $t.live_updates }}</h2><ol reversed>{{ range .entries }}<li id="entry-{{ .n }}"><time datetime="{{ .at }}">{{ .time }}</time>{{ with .corrects }} <small>({{ $t.corrects }} #{{ . }})</small>{{ end }}{{ .html | safeHTML }}</li>{{ end }}</ol></section>
{{ end }}{{ with $p.tags }}<p class="tags">{{ $t.tags }}: {{ range $i, $tag := . }}{{ if $i }}, {{ end }}<a href="{{ $tag.url }}">{{ $tag.name }}</a>{{ end }}</p>
{{ end }}{{ range $i, $section := $p.sections }}<section aria-labelledby="section-{{ add $i 1 }}">
<h2 id="section-{{ add $i 1 }}">{{ $section.title }}</h2>
<ul class="items">{{ range $section.items }}<li><a href="{{ .url }}">{{ .title }}</a>{{ with .date }} <time datetime="{{ . }}">{{ . }}</time>{{ end }}{{ with .summary }}<p>{{ . }}</p>{{ end }}</li>{{ end }}</ul>
{{ with $section.more }}<p><a href="{{ . }}">{{ $t.all }} {{ $section.title }}</a></p>{{ end }}
</section>
{{ end }}</article>
{{ else if eq $c.template "list" }}<h1>{{ $p.title }}</h1>
{{ with $p.body }}<div class="body">
{{ . | safeHTML }}</div>
{{ end }}<ul class="items">{{ range $p.items }}<li><h2><a href="{{ .url }}">{{ .title }}</a></h2>{{ with .date }}<p class="meta"><time datetime="{{ . }}">{{ . }}</time></p>{{ end }}{{ with .summary }}<p>{{ . }}</p>{{ end }}</li>
{{ end }}</ul>
{{ with $p.pagination }}<nav aria-label="{{ $t.pagination }}"><p>{{ $t.page }} {{ .current }} {{ $t.of }} {{ .total }}</p><ul>{{ with .prev }}<li><a href="{{ . }}" rel="prev">{{ $t.previous }}</a></li>{{ end }}{{ with .next }}<li><a href="{{ . }}" rel="next">{{ $t.next }}</a></li>{{ end }}</ul></nav>
{{ end }}{{ else }}<h1>{{ $p.title }}</h1>
{{ range $p.notices }}<div class="notice notice-{{ .class }}"><p><strong>{{ .title }}</strong>{{ with .date }} <time datetime="{{ . }}">{{ . }}</time>{{ end }}</p>{{ with .text }}<p>{{ . }}</p>{{ end }}</div>
{{ end }}<p><a href="{{ $s.home }}">{{ $t.home }}</a></p>
{{ end }}</main>
<footer class="site-footer">
{{ with $s.legal }}<nav aria-label="{{ $t.legal_navigation }}"><ul>{{ range . }}<li><a href="{{ .url }}">{{ .title }}</a></li>{{ end }}</ul></nav>
{{ end }}{{ with $p.translations }}<nav aria-label="{{ $t.languages }}"><ul>{{ range . }}<li><a href="{{ .url }}" hreflang="{{ .language }}" lang="{{ .language }}">{{ .name }}</a></li>{{ end }}</ul></nav>
{{ end }}<p>{{ $s.name }}</p>
</footer>
</body>
</html>
"##;

/// Writes the project: the starter files the project lacks, then one page per context.
pub(super) fn write_project(dir: &Path, base_url: &str, pages: &[PageContext]) -> Result<()> {
    write_starter(dir, "hugo.toml", &CONFIG.replace("BASE_URL", base_url))?;
    write_starter(dir, "layouts/adk-page.html", LAYOUT)?;
    for page in pages {
        let matter = json!({
            "title": page.title(),
            "url": format!("/{}/{}/", super::PAGES_DIR, page.n),
            "layout": "adk-page",
            "params": { "ctx": page.value() },
        });
        let mut bytes =
            serde_json::to_vec_pretty(&matter).map_err(|e| SiteError::serde("front matter", e))?;
        bytes.push(b'\n');
        write_file(dir, &format!("content/{}/{}.md", super::PAGES_DIR, page.n), &bytes)?;
    }
    Ok(())
}
