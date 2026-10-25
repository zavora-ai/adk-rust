//! HTML reading shared by the sources, the sanitiser, the detectors and the parity check:
//! page metadata, main content, links, forms, scripts, hiding styles and feature detection.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::Value;
use url::Url;

use super::inventory::{DetectedFeature, FeatureEvidence, FeatureKind, FormField, PageMeta};

/// Main-content selectors, most specific first: WordPress's entry content, schema.org's
/// article body, the built-in renderer's body, then the page's main landmark.
const MAIN_SELECTORS: &[&str] = &[
    ".entry-content",
    "[itemprop=articleBody]",
    "article .body",
    "main",
    "[role=main]",
    "article",
    "#content",
    "body",
];

/// Elements whose text is never page text.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "template", "nav", "header", "footer", "aside", "form",
    "iframe", "object", "embed", "svg", "button", "select", "option",
];

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("static selector parses")
}

/// A script on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScriptRef {
    /// Its `src`, resolved.
    pub src: Option<Url>,
    /// Its inline text.
    pub inline: Option<String>,
}

/// A form on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormInfo {
    /// Its action, resolved; the page itself when absent.
    pub action: Url,
    /// `get` or `post`.
    pub method: String,
    /// Its id and class attributes, for evidence.
    pub marker: String,
    /// Its fields.
    pub fields: Vec<FormField>,
    /// What it is, when recognised.
    pub kind: Option<FeatureKind>,
}

/// What a page holds.
#[derive(Debug, Clone, Default)]
pub(crate) struct PageAnalysis {
    pub meta: PageMeta,
    pub links: Vec<Url>,
    pub media: Vec<(Url, Option<String>)>,
    pub scripts: Vec<ScriptRef>,
    pub forms: Vec<FormInfo>,
    pub main_html: String,
    pub features: Vec<DetectedFeature>,
    pub json_ld: Vec<Value>,
    pub body_class: String,
    /// The links of the page's first `<nav>`: label and target.
    pub nav: Vec<(String, Url)>,
    /// Stylesheets, icons, responsive image candidates, video and audio sources.
    pub assets: Vec<Url>,
    /// Links marked `rel="category tag"` (categories) and `rel="tag"` (tags): name and kind.
    pub terms: Vec<(String, TermLink)>,
}

/// What a `rel` term link names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TermLink {
    Category,
    Tag,
}

/// Classes and ids a page's `<style>` blocks hide, for the hidden-element detector.
#[derive(Debug, Clone, Default)]
pub(crate) struct HiddenRules {
    pub classes: BTreeSet<String>,
    pub ids: BTreeSet<String>,
}

impl HiddenRules {
    /// Reads `.class { … }` and `#id { … }` rules that hide from every `<style>` in `html`.
    pub fn from_document(document: &Html) -> Self {
        static RULE: OnceLock<Regex> = OnceLock::new();
        let rule = RULE.get_or_init(|| {
            Regex::new(r"([.#])([A-Za-z0-9_-]+)\s*\{([^}]*)\}").expect("static regex")
        });
        let mut rules = Self::default();
        for style in document.select(&selector("style")) {
            let css: String = style.text().collect();
            for capture in rule.captures_iter(&css) {
                if hiding_style(&capture[3]).is_some() {
                    let set = if &capture[1] == "." { &mut rules.classes } else { &mut rules.ids };
                    set.insert(capture[2].to_string());
                }
            }
        }
        rules
    }

    /// Why `element` is hidden, when it is.
    pub fn hides(&self, element: &scraper::node::Element) -> Option<String> {
        if element.attr("hidden").is_some() {
            return Some("hidden attribute".into());
        }
        if let Some(style) = element.attr("style")
            && let Some(reason) = hiding_style(style)
        {
            return Some(format!("style: {reason}"));
        }
        if let Some(id) = element.id()
            && self.ids.contains(id)
        {
            return Some(format!("#{id} hidden by a style block"));
        }
        element
            .classes()
            .find(|class| self.classes.contains(*class))
            .map(|class| format!(".{class} hidden by a style block"))
    }
}

/// The declaration in a CSS declaration block that hides an element, when one does.
pub(crate) fn hiding_style(style: &str) -> Option<String> {
    let mut declarations: Vec<(String, String)> = Vec::new();
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else { continue };
        let property = property.trim().to_ascii_lowercase();
        let value = value.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
        declarations.push((property, value));
    }
    let get = |name: &str| declarations.iter().find(|(p, _)| p == name).map(|(_, v)| v.as_str());
    let zero = |v: &str| {
        let number: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        !number.is_empty() && number.parse::<f64>().is_ok_and(|n| n == 0.0)
    };
    let far_negative = |v: &str| {
        v.strip_prefix('-')
            .map(|rest| rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
            .and_then(|digits| digits.parse::<u64>().ok())
            .is_some_and(|n| n >= 999)
    };
    if get("display") == Some("none") {
        return Some("display:none".into());
    }
    if matches!(get("visibility"), Some("hidden" | "collapse")) {
        return Some("visibility:hidden".into());
    }
    if get("opacity").is_some_and(zero) {
        return Some("opacity:0".into());
    }
    if get("font-size").is_some_and(zero) {
        return Some("font-size:0".into());
    }
    let clipped = get("overflow").is_some_and(|v| v == "hidden");
    if clipped && (get("height").is_some_and(zero) || get("width").is_some_and(zero)) {
        return Some("zero size with overflow:hidden".into());
    }
    if matches!(get("position"), Some("absolute" | "fixed")) {
        for side in ["left", "top", "right", "bottom"] {
            if get(side).is_some_and(far_negative) {
                return Some(format!("positioned off-screen ({side})"));
            }
        }
        if get("clip").is_some_and(|v| v.replace(' ', "").starts_with("rect(0")) {
            return Some("clipped to nothing".into());
        }
    }
    if get("text-indent").is_some_and(far_negative) {
        return Some("text-indent off-screen".into());
    }
    None
}

/// The element holding a page's main content.
pub(crate) fn main_element(document: &Html) -> Option<ElementRef<'_>> {
    MAIN_SELECTORS.iter().find_map(|css| document.select(&selector(css)).next())
}

/// The visible text under `element`: skipped elements and hidden ones left out, whitespace
/// collapsed.
pub(crate) fn visible_text(element: ElementRef<'_>, hidden: &HiddenRules) -> String {
    fn walk(node: ego_tree::NodeRef<'_, Node>, hidden: &HiddenRules, out: &mut String) {
        for child in node.children() {
            match child.value() {
                Node::Text(text) => {
                    out.push_str(text);
                    out.push(' ');
                }
                Node::Element(element) => {
                    let name = element.name();
                    if SKIPPED.contains(&name) || hidden.hides(element).is_some() {
                        continue;
                    }
                    if matches!(name, "br" | "p" | "div" | "li" | "h1" | "h2" | "h3" | "td") {
                        out.push(' ');
                    }
                    walk(child, hidden, out);
                }
                _ => {}
            }
        }
    }
    let mut out = String::new();
    walk(*element, hidden, &mut out);
    collapse(&out)
}

/// Collapses runs of whitespace into single spaces and trims.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The main text of an HTML document, for parity.
pub(crate) fn main_text(html: &str) -> String {
    let document = Html::parse_document(html);
    let hidden = HiddenRules::from_document(&document);
    main_element(&document).map(|main| visible_text(main, &hidden)).unwrap_or_default()
}

/// The path of a same-site URL, or the URL itself.
pub(crate) fn site_path(url: &Url, origin: &str) -> String {
    if super::http::origin_of(url) == origin {
        match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        }
    } else {
        url.to_string()
    }
}

/// Reads a page's metadata, links, media, scripts, forms, main content and features.
pub(crate) fn analyse(html: &str, page: &Url) -> PageAnalysis {
    let document = Html::parse_document(html);
    let hidden = HiddenRules::from_document(&document);
    let origin = super::http::origin_of(page);
    let resolve = |href: &str| -> Option<Url> {
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') {
            return None;
        }
        let mut url = page.join(href).ok()?;
        url.set_fragment(None);
        matches!(url.scheme(), "http" | "https").then_some(url)
    };
    let attr_of = |css: &str, attr: &str| -> Option<String> {
        document
            .select(&selector(css))
            .next()
            .and_then(|e| e.value().attr(attr))
            .map(collapse)
            .filter(|v| !v.is_empty())
    };
    let mut analysis = PageAnalysis::default();
    analysis.meta.title = document
        .select(&selector("title"))
        .next()
        .map(|t| collapse(&t.text().collect::<String>()))
        .filter(|t| !t.is_empty());
    analysis.meta.description = attr_of("meta[name=description]", "content");
    analysis.meta.canonical = attr_of("link[rel=canonical]", "href")
        .and_then(|href| resolve(&href))
        .map(|url| site_path(&url, &origin));
    analysis.meta.language = attr_of("html", "lang");
    for meta in document.select(&selector("meta[property^='og:']")) {
        if let (Some(property), Some(content)) =
            (meta.value().attr("property"), meta.value().attr("content"))
        {
            analysis.meta.open_graph.insert(property.to_string(), collapse(content));
        }
    }
    let main = main_element(&document);
    analysis.meta.main_text = main.map(|m| visible_text(m, &hidden)).unwrap_or_default();
    analysis.main_html = main.map(|m| m.inner_html()).unwrap_or_default();
    for link in document.select(&selector("a[href], area[href]")) {
        if let Some(url) = link.value().attr("href").and_then(resolve)
            && !analysis.links.contains(&url)
        {
            analysis.links.push(url);
        }
    }
    for image in document.select(&selector("img[src]")) {
        if let Some(url) = image.value().attr("src").and_then(resolve)
            && !analysis.media.iter().any(|(u, _)| u == &url)
        {
            analysis.media.push((url, image.value().attr("alt").map(str::to_string)));
        }
    }
    for script in document.select(&selector("script")) {
        let kind = script.value().attr("type").unwrap_or("text/javascript").to_ascii_lowercase();
        if kind == "application/ld+json" {
            let text: String = script.text().collect();
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                analysis.json_ld.push(value);
            }
            continue;
        }
        let src = script.value().attr("src").and_then(resolve);
        let inline: String = script.text().collect();
        analysis
            .scripts
            .push(ScriptRef { src, inline: (!inline.trim().is_empty()).then_some(inline) });
    }
    for form in document.select(&selector("form")) {
        analysis.forms.push(read_form(form, page));
    }
    let mut assets: Vec<Url> = Vec::new();
    for link in document.select(&selector("link[href]")) {
        let rel = link.value().attr("rel").unwrap_or_default().to_ascii_lowercase();
        if ["stylesheet", "icon", "preload", "apple-touch-icon", "manifest"]
            .iter()
            .any(|r| rel.contains(r))
            && let Some(url) = link.value().attr("href").and_then(resolve)
        {
            assets.push(url);
        }
    }
    for element in document.select(&selector("img[srcset], source[srcset]")) {
        for candidate in element.value().attr("srcset").unwrap_or_default().split(',') {
            if let Some(url) = candidate.split_whitespace().next().and_then(resolve) {
                assets.push(url);
            }
        }
    }
    for element in document.select(&selector("video[src], audio[src], source[src], video[poster]"))
    {
        for attribute in ["src", "poster"] {
            if let Some(url) = element.value().attr(attribute).and_then(resolve) {
                assets.push(url);
            }
        }
    }
    assets.dedup();
    analysis.assets = assets;
    for link in document.select(&selector("a[rel~=tag]")) {
        let name = collapse(&link.text().collect::<String>());
        if name.is_empty() {
            continue;
        }
        let rel = link.value().attr("rel").unwrap_or_default();
        let kind = if rel.split_whitespace().any(|r| r == "category") {
            TermLink::Category
        } else {
            TermLink::Tag
        };
        analysis.terms.push((name, kind));
    }
    if let Some(nav) = document.select(&selector("nav")).next() {
        for link in nav.select(&selector("a[href]")) {
            let label = collapse(&link.text().collect::<String>());
            if let Some(url) = link.value().attr("href").and_then(resolve)
                && !label.is_empty()
            {
                analysis.nav.push((label, url));
            }
        }
    }
    analysis.body_class = document
        .select(&selector("body"))
        .next()
        .and_then(|b| b.value().attr("class"))
        .unwrap_or_default()
        .to_string();
    analysis.features = detect_features(&analysis, page);
    analysis
}

fn read_form(form: ElementRef<'_>, page: &Url) -> FormInfo {
    let element = form.value();
    let action = element
        .attr("action")
        .filter(|a| !a.trim().is_empty())
        .and_then(|a| page.join(a.trim()).ok())
        .unwrap_or_else(|| page.clone());
    let method = element.attr("method").unwrap_or("get").to_ascii_lowercase();
    let marker = format!(
        "{} {} {}",
        element.attr("id").unwrap_or_default(),
        element.attr("class").unwrap_or_default(),
        element.attr("role").unwrap_or_default()
    )
    .to_ascii_lowercase();
    let mut fields = Vec::new();
    for input in form.select(&selector("input, textarea, select")) {
        let value = input.value();
        let input_type = match value.name() {
            "textarea" => "textarea".to_string(),
            "select" => "select".to_string(),
            _ => value.attr("type").unwrap_or("text").to_ascii_lowercase(),
        };
        if matches!(input_type.as_str(), "hidden" | "submit" | "button" | "image" | "reset") {
            continue;
        }
        let Some(name) = value.attr("name").filter(|n| !n.is_empty()) else { continue };
        fields.push(FormField {
            name: name.to_string(),
            input_type,
            required: value.attr("required").is_some()
                || value.attr("aria-required") == Some("true"),
        });
    }
    let kind = classify_form(&action, &method, &marker, &fields);
    FormInfo { action, method, marker, fields, kind }
}

fn classify_form(
    action: &Url,
    method: &str,
    marker: &str,
    fields: &[FormField],
) -> Option<FeatureKind> {
    let path = action.path().to_ascii_lowercase();
    let has_type = |t: &str| fields.iter().any(|f| f.input_type == t);
    let has_name = |names: &[&str]| fields.iter().any(|f| names.contains(&f.name.as_str()));
    let marked = |words: &[&str]| words.iter().any(|w| marker.contains(w) || path.contains(w));
    if has_type("password") || path.ends_with("wp-login.php") || marked(&["login", "signin"]) {
        return Some(FeatureKind::Login);
    }
    if path.ends_with("wp-comments-post.php") || marked(&["commentform", "comment-form"]) {
        return Some(FeatureKind::Comments);
    }
    if marked(&["booking", "appointment", "reservation", "book-now"])
        || (has_type("date") || has_type("datetime-local")) && marked(&["book"])
    {
        return Some(FeatureKind::Bookings);
    }
    if marked(&["add-to-cart", "cart", "checkout", "woocommerce"]) || has_name(&["add-to-cart"]) {
        return Some(FeatureKind::Shop);
    }
    if marker.contains("search") || (method == "get" && has_name(&["s", "q", "search", "query"])) {
        return Some(FeatureKind::Search);
    }
    let email = has_type("email") || has_name(&["email", "your-email"]);
    if email && (has_type("textarea") || marked(&["contact", "wpcf7", "wpforms", "gform"])) {
        return Some(FeatureKind::ContactForm);
    }
    if email && marked(&["newsletter", "subscribe", "mailchimp", "list-manage", "mc4wp"]) {
        return Some(FeatureKind::Newsletter);
    }
    if email && fields.len() <= 2 {
        return Some(FeatureKind::Newsletter);
    }
    None
}

/// The URLs a stylesheet references with `url(…)`, resolved against `base`.
pub(crate) fn css_urls(css: &str, base: &Url) -> Vec<Url> {
    static URL: OnceLock<Regex> = OnceLock::new();
    let pattern = URL
        .get_or_init(|| Regex::new(r#"url\(\s*['"]?([^'")\s]+)['"]?\s*\)"#).expect("static regex"));
    let mut found = Vec::new();
    for capture in pattern.captures_iter(css) {
        let raw = &capture[1];
        if raw.starts_with("data:") || raw.starts_with('#') {
            continue;
        }
        if let Ok(mut url) = base.join(raw) {
            url.set_fragment(None);
            if !found.contains(&url) {
                found.push(url);
            }
        }
    }
    found
}

/// Shortcodes and markers of form, shop and booking plugins in stored post content.
pub(crate) fn detect_shortcodes(content: &str, url: &str) -> Vec<DetectedFeature> {
    static SHORTCODE: OnceLock<Regex> = OnceLock::new();
    let shortcode =
        SHORTCODE.get_or_init(|| Regex::new(r"\[([a-z][a-z0-9_-]*)[\s\]]").expect("static regex"));
    let mut found: Vec<DetectedFeature> = Vec::new();
    let mut push = |kind: FeatureKind, detail: String| {
        found.push(DetectedFeature {
            kind,
            evidence: vec![FeatureEvidence { url: url.to_string(), detail }],
            fields: Vec::new(),
        });
    };
    for capture in shortcode.captures_iter(content) {
        let name = &capture[1];
        let kind = match name {
            "contact-form-7" | "contact-form" | "wpforms" | "gravityform" | "ninja_form"
            | "formidable" | "caldera_form" | "fluentform" => FeatureKind::ContactForm,
            "mc4wp_form" | "mailpoet_form" | "newsletter" => FeatureKind::Newsletter,
            "booked-calendar" | "bookly-form" | "ea_bootstrap" | "amelia" | "simply_schedule"
            | "booking" => FeatureKind::Bookings,
            name if name.starts_with("woocommerce_")
                || name == "products"
                || name == "product_page" =>
            {
                FeatureKind::Shop
            }
            "wpmem_form" | "swpm_login_form" | "pmpro_account" | "members_access" => {
                FeatureKind::MemberArea
            }
            _ => continue,
        };
        push(kind, format!("shortcode [{name}]"));
    }
    for (marker, kind) in [
        ("<!-- wp:search", FeatureKind::Search),
        ("<!-- wp:woocommerce/", FeatureKind::Shop),
        ("<!-- wp:comments", FeatureKind::Comments),
        ("<!-- wp:loginout", FeatureKind::Login),
    ] {
        if content.contains(marker) {
            push(kind, format!("block {}", marker.trim_start_matches("<!-- ")));
        }
    }
    found
}

fn detect_features(analysis: &PageAnalysis, page: &Url) -> Vec<DetectedFeature> {
    let mut features: Vec<DetectedFeature> = Vec::new();
    let mut add = |kind: FeatureKind, detail: String, fields: Vec<FormField>| {
        let evidence = FeatureEvidence { url: page.to_string(), detail };
        match features.iter_mut().find(|f| f.kind == kind) {
            Some(feature) => feature.evidence.push(evidence),
            None => features.push(DetectedFeature { kind, evidence: vec![evidence], fields }),
        }
    };
    for form in &analysis.forms {
        if let Some(kind) = form.kind {
            add(
                kind,
                format!("form action={} method={}", form.action.path(), form.method),
                form.fields.clone(),
            );
        }
    }
    let body_class = analysis.body_class.to_ascii_lowercase();
    let product_ld = analysis.json_ld.iter().any(|v| json_ld_has_type(v, "Product"));
    if body_class.contains("woocommerce") || product_ld {
        add(FeatureKind::Shop, "WooCommerce page or schema.org Product".into(), Vec::new());
    }
    for link in &analysis.links {
        let path = link.path().to_ascii_lowercase();
        if path.contains("/my-account")
            || path.contains("/members/")
            || link.query().is_some_and(|q| q.contains("action=register"))
        {
            add(FeatureKind::MemberArea, format!("link to {path}"), Vec::new());
        }
        if path == "/cart/"
            || path == "/checkout/"
            || link.query().is_some_and(|q| q.contains("add-to-cart="))
        {
            add(FeatureKind::Shop, format!("link to {}", link.as_str()), Vec::new());
        }
    }
    features
}

/// Whether a JSON-LD value (an object, an array or a `@graph`) has an item of `@type` `kind`.
pub(crate) fn json_ld_has_type(value: &Value, kind: &str) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|v| json_ld_has_type(v, kind)),
        Value::Object(map) => {
            let typed = match map.get("@type") {
                Some(Value::String(t)) => t == kind,
                Some(Value::Array(ts)) => ts.iter().any(|t| t.as_str() == Some(kind)),
                _ => false,
            };
            typed || map.get("@graph").is_some_and(|g| json_ld_has_type(g, kind))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_styles_are_recognised() {
        for style in [
            "display: none",
            "visibility:hidden",
            "opacity: 0",
            "font-size:0px",
            "height:0;overflow:hidden",
            "position:absolute;left:-9999px",
            "position: fixed; top: -5000px",
            "text-indent:-9999px",
            "DISPLAY:NONE !important",
        ] {
            assert!(hiding_style(style).is_some(), "{style}");
        }
        for style in ["color:red", "display:block", "opacity:0.5", "position:absolute;left:-10px"] {
            assert!(hiding_style(style).is_none(), "{style}");
        }
    }

    #[test]
    fn a_page_is_read_whole() {
        let url = Url::parse("https://example.test/contact/").unwrap();
        let html = r#"<!doctype html><html lang="en"><head><title> Contact  us </title>
            <meta name="description" content="Write to us">
            <link rel="canonical" href="https://example.test/contact/">
            <meta property="og:title" content="Contact">
            <style>.sneaky { display:none }</style></head>
            <body class="page"><nav><a href="/">Home</a></nav>
            <main><div class="entry-content"><p>Call us today.</p>
            <div class="sneaky">hidden words <a href="https://spam.test/">spam</a></div>
            <img src="/wp-content/uploads/a.jpg" alt="A">
            <form class="wpcf7-form" method="post" action="/contact/#wpcf7">
              <input name="your-name" required><input type="email" name="your-email">
              <textarea name="your-message"></textarea><input type="submit" value="Send"></form>
            </div></main>
            <form role="search" action="/" method="get"><input name="s"></form>
            <script>console.log(1)</script></body></html>"#;
        let page = analyse(html, &url);
        assert_eq!(page.meta.title.as_deref(), Some("Contact us"));
        assert_eq!(page.meta.description.as_deref(), Some("Write to us"));
        assert_eq!(page.meta.canonical.as_deref(), Some("/contact/"));
        assert_eq!(page.meta.language.as_deref(), Some("en"));
        assert_eq!(page.meta.main_text, "Call us today.");
        assert_eq!(page.meta.open_graph["og:title"], "Contact");
        assert!(page.links.iter().any(|l| l.as_str() == "https://spam.test/"));
        assert_eq!(page.media[0].0.path(), "/wp-content/uploads/a.jpg");
        assert_eq!(page.scripts.len(), 1);
        let kinds: Vec<FeatureKind> = page.features.iter().map(|f| f.kind).collect();
        assert!(kinds.contains(&FeatureKind::ContactForm), "{kinds:?}");
        assert!(kinds.contains(&FeatureKind::Search), "{kinds:?}");
        let contact = page.features.iter().find(|f| f.kind == FeatureKind::ContactForm).unwrap();
        assert_eq!(contact.fields.len(), 3);
    }

    #[test]
    fn shortcodes_and_blocks_name_features() {
        let found = detect_shortcodes(
            "[contact-form-7 id=\"5\"] text [woocommerce_cart] <!-- wp:search /-->",
            "wxr:post:1",
        );
        let kinds: Vec<FeatureKind> = found.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FeatureKind::ContactForm, FeatureKind::Shop, FeatureKind::Search]);
    }
}
