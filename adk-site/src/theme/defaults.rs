//! The default theme's templates, stylesheet and interface strings.
//!
//! The templates carry no script: the only `<script>` element is JSON-LD data. They meet the
//! launch gate's automated WCAG 2.2 AA rules by construction: `lang`, a title, one `<main>`,
//! one `<h1>`, ordered headings, labelled navigation and alt text on every image.

pub(super) const BASE: &str = r##"<!doctype html>
<html lang="{{ page.language }}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{% if page.title and page.title != site.name %}{{ page.title }} · {% endif %}{{ site.name }}</title>
{% if page.description %}<meta name="description" content="{{ page.description }}">
{% endif %}{% if page.noindex %}<meta name="robots" content="noindex">
{% endif %}<link rel="canonical" href="{{ page.url }}">
{% for alt in page.translations %}<link rel="alternate" hreflang="{{ alt.language }}" href="{{ alt.url }}">
{% endfor %}{% if site.contract %}<link rel="awp" href="{{ site.contract }}">
{% endif %}{% if page.markdown %}<link rel="alternate" type="text/markdown" href="{{ page.markdown }}">
{% endif %}{% if page.machine_view %}<link rel="alternate" type="application/awp+json" href="{{ page.machine_view }}">
{% endif %}<link rel="stylesheet" href="{{ site.stylesheet }}">
{% if page.json_ld %}<script type="application/ld+json">{{ page.json_ld|safe }}</script>
{% endif %}</head>
<body>
<a class="skip-link" href="#content">{{ t.skip_to_content }}</a>
<header class="site-header">
<a class="site-name" href="{{ site.home }}">{{ site.name }}</a>
{% if site.nav %}<nav aria-label="{{ t.main_navigation }}"><ul>{% for link in site.nav %}<li><a href="{{ link.url }}"{% if link.current %} aria-current="page"{% endif %}>{{ link.title }}</a></li>{% endfor %}</ul></nav>
{% endif %}</header>
<main id="content">
{% block content %}{% endblock %}
</main>
<footer class="site-footer">
{% if site.legal %}<nav aria-label="{{ t.legal_navigation }}"><ul>{% for link in site.legal %}<li><a href="{{ link.url }}">{{ link.title }}</a></li>{% endfor %}</ul></nav>
{% endif %}{% if page.translations %}<nav aria-label="{{ t.languages }}"><ul>{% for alt in page.translations %}<li><a href="{{ alt.url }}" hreflang="{{ alt.language }}" lang="{{ alt.language }}">{{ alt.name }}</a></li>{% endfor %}</ul></nav>
{% endif %}<p>{{ site.name }}</p>
</footer>
</body>
</html>
"##;

pub(super) const ITEM: &str = r##"{% extends "base.html" %}{% block content %}<article>
<header>
<h1>{{ page.title }}</h1>
{% if page.byline or page.date %}<p class="meta">{% if page.byline %}<span class="byline">{{ t.by }} {{ page.byline }}</span> {% endif %}{% if page.date %}<time datetime="{{ page.date }}">{{ page.date }}</time>{% endif %}</p>
{% endif %}</header>
{% for notice in page.notices %}<aside class="notice notice-{{ notice.class }}" aria-label="{{ notice.title }}"><p><strong>{{ notice.title }}</strong>{% if notice.date %} <time datetime="{{ notice.date }}">{{ notice.date }}</time>{% endif %}</p>{% if notice.text %}<p>{{ notice.text }}</p>{% endif %}</aside>
{% endfor %}{% for figure in page.figures %}<figure><img src="{{ figure.src }}" alt="{{ figure.alt }}"{% if figure.width %} width="{{ figure.width }}" height="{{ figure.height }}"{% endif %}{% if not loop.first %} loading="lazy"{% endif %}>{% if figure.caption %}<figcaption>{{ figure.caption }}</figcaption>{% endif %}</figure>
{% endfor %}{% if page.fields %}<dl class="facts">{% for field in page.fields %}<dt>{{ field.label }}</dt><dd>{{ field.value }}</dd>{% endfor %}</dl>
{% endif %}{% if page.live %}<dl class="live">{% for field in page.live %}<dt>{{ field.label }}</dt><dd data-awp-live="{{ field.name }}">{% if field.value %}{{ field.value }} <small>({{ t.as_of }} <time datetime="{{ field.as_of }}">{{ field.as_of_date }}</time>)</small>{% else %}{{ t.check_availability }}{% endif %}</dd>{% endfor %}</dl>
{% endif %}{% if page.body %}<div class="body">
{{ page.body|safe }}</div>
{% endif %}{% if page.stream %}<section class="stream" data-awp-stream="{{ page.stream.id }}" aria-label="{{ t.live_updates }}"><h2>{{ t.live_updates }}</h2><ol reversed>{% for entry in page.stream.entries %}<li id="entry-{{ entry.n }}"><time datetime="{{ entry.at }}">{{ entry.time }}</time>{% if entry.corrects %} <small>({{ t.corrects }} #{{ entry.corrects }})</small>{% endif %}{{ entry.html|safe }}</li>{% endfor %}</ol></section>
{% endif %}{% if page.tags %}<p class="tags">{{ t.tags }}: {% for tag in page.tags %}<a href="{{ tag.url }}">{{ tag.name }}</a>{% if not loop.last %}, {% endif %}{% endfor %}</p>
{% endif %}{% for section in page.sections %}<section aria-labelledby="section-{{ loop.index }}">
<h2 id="section-{{ loop.index }}">{{ section.title }}</h2>
<ul class="items">{% for entry in section.items %}<li><a href="{{ entry.url }}">{{ entry.title }}</a>{% if entry.date %} <time datetime="{{ entry.date }}">{{ entry.date }}</time>{% endif %}{% if entry.summary %}<p>{{ entry.summary }}</p>{% endif %}</li>{% endfor %}</ul>
{% if section.more %}<p><a href="{{ section.more }}">{{ t.all }} {{ section.title }}</a></p>{% endif %}
</section>
{% endfor %}</article>
{% endblock %}"##;

pub(super) const LIST: &str = r##"{% extends "base.html" %}{% block content %}<h1>{{ page.title }}</h1>
{% if page.body %}<div class="body">
{{ page.body|safe }}</div>
{% endif %}<ul class="items">{% for entry in page.items %}<li><h2><a href="{{ entry.url }}">{{ entry.title }}</a></h2>{% if entry.date %}<p class="meta"><time datetime="{{ entry.date }}">{{ entry.date }}</time></p>{% endif %}{% if entry.summary %}<p>{{ entry.summary }}</p>{% endif %}</li>
{% endfor %}</ul>
{% if page.pagination %}<nav aria-label="{{ t.pagination }}"><p>{{ t.page }} {{ page.pagination.current }} {{ t.of }} {{ page.pagination.total }}</p><ul>{% if page.pagination.prev %}<li><a href="{{ page.pagination.prev }}" rel="prev">{{ t.previous }}</a></li>{% endif %}{% if page.pagination.next %}<li><a href="{{ page.pagination.next }}" rel="next">{{ t.next }}</a></li>{% endif %}</ul></nav>
{% endif %}{% endblock %}"##;

pub(super) const NOTICE: &str = r##"{% extends "base.html" %}{% block content %}<h1>{{ page.title }}</h1>
{% for notice in page.notices %}<div class="notice notice-{{ notice.class }}"><p><strong>{{ notice.title }}</strong>{% if notice.date %} <time datetime="{{ notice.date }}">{{ notice.date }}</time>{% endif %}</p>{% if notice.text %}<p>{{ notice.text }}</p>{% endif %}</div>
{% endfor %}<p><a href="{{ site.home }}">{{ t.home }}</a></p>
{% endblock %}"##;

pub(super) const STYLESHEET: &str = r##"*, *::before, *::after { box-sizing: border-box; }
html { color: var(--color-text); background: var(--color-background); font-family: var(--font-body); line-height: 1.6; }
body { margin: 0; }
h1, h2, h3, h4 { font-family: var(--font-heading); line-height: 1.25; }
a { color: var(--color-link); text-underline-offset: 0.15em; }
a:focus-visible { outline: 3px solid var(--color-accent); outline-offset: 2px; }
code, pre { font-family: var(--font-mono); }
pre { overflow-x: auto; padding: 1rem; border: 1px solid var(--color-border); border-radius: var(--radius); }
img { max-width: 100%; height: auto; }
.skip-link { position: absolute; left: -9999px; }
.skip-link:focus { position: static; display: inline-block; padding: 0.5rem 1rem; background: var(--color-accent); color: var(--color-accent-text); }
.site-header, main, .site-footer { max-width: var(--measure); margin: 0 auto; padding: 1rem; }
.site-header { display: flex; flex-wrap: wrap; gap: 1rem; align-items: baseline; justify-content: space-between; border-bottom: 1px solid var(--color-border); }
.site-name { font-weight: 700; text-decoration: none; color: var(--color-text); }
nav ul { list-style: none; display: flex; flex-wrap: wrap; gap: 1rem; margin: 0; padding: 0; }
.site-footer { border-top: 1px solid var(--color-border); color: var(--color-muted); }
.meta, time { color: var(--color-muted); }
.notice { background: var(--color-notice-background); color: var(--color-notice-text); padding: 0.75rem 1rem; border-radius: var(--radius); margin: 1rem 0; }
.items { list-style: none; padding: 0; }
.items li { margin: 0 0 1.25rem; }
.items h2 { font-size: 1.25rem; margin: 0; }
dl.facts, dl.live { display: grid; grid-template-columns: max-content 1fr; gap: 0.25rem 1rem; }
dt { font-weight: 600; }
dd { margin: 0; }
figure { margin: 1rem 0; }
figcaption { color: var(--color-muted); font-size: 0.9rem; }
table { border-collapse: collapse; }
th, td { border: 1px solid var(--color-border); padding: 0.25rem 0.5rem; }
"##;

/// English interface strings; a site overrides them per language in `site.toml`.
pub(super) const STRINGS: &[(&str, &str)] = &[
    ("skip_to_content", "Skip to content"),
    ("main_navigation", "Main"),
    ("legal_navigation", "Legal"),
    ("languages", "Languages"),
    ("check_availability", "Check availability"),
    ("as_of", "as of"),
    ("live_updates", "Live updates"),
    ("corrects", "corrects"),
    ("tags", "Tags"),
    ("tag", "Tag"),
    ("by", "By"),
    ("all", "All"),
    ("pagination", "Pages"),
    ("page", "Page"),
    ("of", "of"),
    ("previous", "Previous page"),
    ("next", "Next page"),
    ("home", "Go to the home page"),
    ("correction", "Correction"),
    ("withdrawn", "Withdrawn"),
    ("expired", "Expired"),
    ("illustration", "Illustration"),
    ("not_found_title", "Page not found"),
    ("not_found_text", "The page you asked for does not exist."),
    ("gone_title", "Page removed"),
    ("gone_text", "This page was withdrawn and is no longer available."),
    ("kind_page", "Pages"),
    ("kind_post", "Posts"),
    ("kind_article", "News"),
    ("kind_product", "Products"),
    ("kind_event", "Events"),
    ("kind_notice", "Notices"),
    ("kind_service", "Services"),
    ("kind_person", "People"),
    ("kind_collection", "Collections"),
    ("field_price", "Price"),
    ("field_sku", "SKU"),
    ("field_startsAt", "Starts"),
    ("field_endsAt", "Ends"),
    ("field_location", "Location"),
    ("field_validUntil", "Valid until"),
    ("field_role", "Role"),
    ("field_publishedAt", "Published"),
    ("field_updatedAt", "Updated"),
    ("field_expiresAt", "Expires"),
];
