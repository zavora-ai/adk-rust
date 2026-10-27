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

/// The elements a page's `<style>` blocks hide, for the hidden-element detector.
///
/// Only a rule that hides an element by itself counts: its selector is a tag, classes and an
/// id with nothing around them (`.ad`, `div.links`, `#promo`), outside any `@media`,
/// `@supports` or `@container` block. A selector with context (`.menu .item`, `ul > li`),
/// a pseudo-class (`.panel:not(.open)`) or a condition hides some elements only, and a class or
/// id that another rule shows again — a responsive menu, a dropdown, a dialog — is a state of
/// the page, not hidden content.
#[derive(Debug, Clone, Default)]
pub(crate) struct HiddenRules {
    rules: Vec<HidingRule>,
}

/// A context-free selector whose rule hides what it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HidingRule {
    selector: String,
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl HidingRule {
    /// `selector` when it is a compound of a tag, an id and classes only.
    fn parse(selector: &str) -> Option<Self> {
        static COMPOUND: OnceLock<Regex> = OnceLock::new();
        static PART: OnceLock<Regex> = OnceLock::new();
        let compound = COMPOUND.get_or_init(|| {
            Regex::new(r"^(?:[A-Za-z][A-Za-z0-9-]*)?(?:[.#][A-Za-z0-9_-]+)+$")
                .expect("static regex")
        });
        let part =
            PART.get_or_init(|| Regex::new(r"([.#])([A-Za-z0-9_-]+)").expect("static regex"));
        if !compound.is_match(selector) {
            return None;
        }
        let tag_end = selector.find(['.', '#']).unwrap_or(selector.len());
        let mut rule = Self {
            selector: selector.to_string(),
            tag: (tag_end > 0).then(|| selector[..tag_end].to_ascii_lowercase()),
            id: None,
            classes: Vec::new(),
        };
        for capture in part.captures_iter(&selector[tag_end..]) {
            if &capture[1] == "#" {
                rule.id = Some(capture[2].to_string());
            } else {
                rule.classes.push(capture[2].to_string());
            }
        }
        Some(rule)
    }

    fn matches(&self, element: &scraper::node::Element) -> bool {
        self.tag.as_deref().is_none_or(|tag| element.name().eq_ignore_ascii_case(tag))
            && self.id.as_deref().is_none_or(|id| element.id() == Some(id))
            && self.classes.iter().all(|class| element.classes().any(|c| c == class))
    }
}

/// What kind of block a `{` opens in a style sheet.
#[derive(Debug)]
enum CssBlock {
    /// A style rule: its selector list, whether a condition or a parent rule scopes it, and its
    /// declarations so far.
    Style { selectors: String, scoped: bool, declarations: String },
    /// `@media`, `@supports`, `@container`, `@scope`: the rules inside apply on a condition.
    Conditional,
    /// `@layer` and other grouping at-rules: the rules inside apply as they stand.
    Group,
    /// `@keyframes`, `@font-face` and the like: no element rules inside.
    Ignored,
}

impl HiddenRules {
    /// Reads the hiding rules of every `<style>` in `document`.
    pub fn from_document(document: &Html) -> Self {
        let mut hiding: Vec<HidingRule> = Vec::new();
        let mut shown: BTreeSet<String> = BTreeSet::new();
        for style in document.select(&selector("style")) {
            let css: String = style.text().collect();
            read_rules(&css, &mut |selectors, scoped, declarations| {
                let hides = hiding_style(declarations).is_some();
                let shows = showing_style(declarations);
                for item in split_selectors(selectors) {
                    if hides
                        && !scoped
                        && let Some(rule) = HidingRule::parse(item)
                    {
                        hiding.push(rule);
                    }
                    if shows {
                        shown.extend(subject_names(item));
                    }
                }
            });
        }
        hiding.retain(|rule| {
            let mut names = rule.classes.iter().map(|c| format!(".{c}"));
            let id = rule.id.as_ref().map(|i| format!("#{i}"));
            !names.any(|name| shown.contains(&name)) && id.is_none_or(|id| !shown.contains(&id))
        });
        Self { rules: hiding }
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
        self.rules
            .iter()
            .find(|rule| rule.matches(element))
            .map(|rule| format!("{} hidden by a style block", rule.selector))
    }
}

/// Walks the style rules of a style sheet, calling `rule` with each rule's selector list,
/// whether a condition or a parent rule scopes it, and its declarations.
fn read_rules(css: &str, rule: &mut impl FnMut(&str, bool, &str)) {
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    let comment = COMMENT.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").expect("static regex"));
    let css = comment.replace_all(css, " ");
    let mut stack: Vec<CssBlock> = Vec::new();
    let mut segment = String::new();
    for c in css.chars() {
        match c {
            '{' => {
                let prelude = segment.trim().to_string();
                segment.clear();
                let ignored = stack.iter().any(|b| matches!(b, CssBlock::Ignored));
                let scoped = stack
                    .iter()
                    .any(|b| matches!(b, CssBlock::Conditional | CssBlock::Style { .. }));
                let block = if ignored {
                    CssBlock::Ignored
                } else if let Some(at) = prelude.strip_prefix('@') {
                    let name = at.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').next();
                    match name.map(str::to_ascii_lowercase).as_deref() {
                        Some("media" | "supports" | "container" | "scope" | "document") => {
                            CssBlock::Conditional
                        }
                        Some("layer" | "starting-style") => CssBlock::Group,
                        _ => CssBlock::Ignored,
                    }
                } else {
                    CssBlock::Style { selectors: prelude, scoped, declarations: String::new() }
                };
                stack.push(block);
            }
            ';' => {
                if let Some(CssBlock::Style { declarations, .. }) = stack.last_mut() {
                    declarations.push_str(&segment);
                    declarations.push(';');
                }
                segment.clear();
            }
            '}' => {
                match stack.pop() {
                    Some(CssBlock::Style { selectors, scoped, mut declarations }) => {
                        declarations.push_str(&segment);
                        rule(&selectors, scoped, &declarations);
                    }
                    Some(CssBlock::Conditional | CssBlock::Group | CssBlock::Ignored) | None => {}
                }
                segment.clear();
            }
            _ => segment.push(c),
        }
    }
}

/// The selectors of a selector list, split at the commas outside parentheses and brackets.
fn split_selectors(list: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in list.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                items.push(list[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(list[start..].trim());
    items.retain(|item| !item.is_empty());
    items
}

/// The `.class` and `#id` names of a selector's subject — its last compound — outside
/// functional pseudo-classes such as `:not(…)`.
fn subject_names(selector: &str) -> Vec<String> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let name = NAME.get_or_init(|| Regex::new(r"[.#][A-Za-z0-9_-]+").expect("static regex"));
    let mut outside = String::new();
    let mut depth = 0i32;
    for c in selector.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth == 0 => outside.push(c),
            _ => {}
        }
    }
    let subject = outside
        .rsplit(|c: char| c.is_whitespace() || matches!(c, '>' | '+' | '~'))
        .find(|part| !part.is_empty())
        .unwrap_or_default();
    name.find_iter(subject).map(|m| m.as_str().to_string()).collect()
}

/// Whether a declaration block shows an element another rule may hide: a `display` other than
/// `none`, or `visibility: visible`.
fn showing_style(style: &str) -> bool {
    style.split(';').filter_map(|d| d.split_once(':')).any(|(property, value)| {
        let value = value.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
        match property.trim().to_ascii_lowercase().as_str() {
            "display" => !value.is_empty() && value != "none",
            "visibility" => value == "visible",
            _ => false,
        }
    })
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

    /// Only rules that hide an element by themselves count: a selector with context, a
    /// pseudo-class or a media condition hides some elements only, and a class another rule
    /// shows again is a state of the page (spec 3.5 §7.8 "Sanitiser").
    #[test]
    fn style_blocks_hide_by_context_free_selectors_only() {
        let document = Html::parse_document(
            r#"<style>
            .a{display:none} div.b, #c { visibility : hidden !important } .d.e{opacity:0}
            .menu .item{display:none} ul>li.f{display:none} .g:not(.open){display:none}
            @media (max-width:600px){.h{display:none}} @supports (display:grid){.i{display:none}}
            @layer base{.j{display:none}} @font-face{font-family:x;src:url(x.woff)}
            .k{display:none} .k.open{display:block} .l{display:none}
            @media (min-width:600px){.wrap .l:not(.closed){display:flex}}
            .m{display:none;/* } */} .n { color: red; .o { display:none } }
            </style>"#,
        );
        let rules = HiddenRules::from_document(&document);
        let hidden = |html: &str| {
            let fragment = Html::parse_fragment(html);
            let element = fragment.root_element().first_child().unwrap();
            rules.hides(element.value().as_element().unwrap())
        };
        assert_eq!(hidden(r#"<p class="a"></p>"#).as_deref(), Some(".a hidden by a style block"));
        assert_eq!(
            hidden(r#"<div class="b"></div>"#).as_deref(),
            Some("div.b hidden by a style block")
        );
        assert!(hidden(r#"<span class="b"></span>"#).is_none(), "div.b names the tag");
        assert!(hidden(r#"<p id="c"></p>"#).is_some());
        assert!(hidden(r#"<p class="d e"></p>"#).is_some());
        assert!(hidden(r#"<p class="d"></p>"#).is_none(), ".d.e needs both classes");
        assert!(hidden(r#"<p class="j"></p>"#).is_some(), "a layer applies unconditionally");
        assert!(hidden(r#"<p class="m"></p>"#).is_some(), "comments are not CSS");
        for html in [
            r#"<p class="item"></p>"#,
            r#"<li class="f"></li>"#,
            r#"<p class="g"></p>"#,
            r#"<p class="h"></p>"#,
            r#"<p class="i"></p>"#,
            r#"<p class="k"></p>"#,
            r#"<p class="l"></p>"#,
            r#"<p class="o"></p>"#,
            r#"<p class="n"></p>"#,
        ] {
            assert!(hidden(html).is_none(), "{html}");
        }
        assert_eq!(hidden(r#"<p hidden></p>"#).as_deref(), Some("hidden attribute"));
        assert_eq!(
            hidden(r#"<p style="display:none"></p>"#).as_deref(),
            Some("style: display:none")
        );
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
