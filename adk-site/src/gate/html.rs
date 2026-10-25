//! HTML analysis for the launch gate: links, automated WCAG 2.2 AA rules, render-blocking
//! resources and alternates (spec 3.5 §7.8.1).

use std::collections::BTreeSet;

use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};

/// How serious an accessibility violation is; the gate fails on `Serious` and `Critical`
/// (spec 3.5 §7.8.1 "zero serious violations").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Advice.
    Minor,
    /// A barrier for some users.
    Moderate,
    /// A barrier that blocks tasks for some users; fails the gate.
    Serious,
    /// A barrier that blocks content outright; fails the gate.
    Critical,
}

impl Severity {
    /// Whether a violation of this severity fails the gate.
    pub fn fails_gate(self) -> bool {
        matches!(self, Self::Serious | Self::Critical)
    }
}

/// One automated accessibility violation in a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A11yViolation {
    /// The rule id, such as `image-alt`.
    pub rule: &'static str,
    /// The WCAG 2.2 success criterion, such as `1.1.1`.
    pub wcag: &'static str,
    /// How serious it is.
    pub severity: Severity,
    /// A short description of the offending element.
    pub element: String,
    /// What to do about it.
    pub help: &'static str,
}

/// A URL a page refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkRef {
    pub tag: String,
    pub url: String,
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("static selector is valid")
}

fn describe(element: &ElementRef<'_>) -> String {
    let value = element.value();
    let mut text = format!("<{}", value.name());
    for attr in ["id", "name", "type", "href", "src"] {
        if let Some(v) = value.attr(attr) {
            let v: String = v.chars().take(60).collect();
            text.push_str(&format!(" {attr}=\"{v}\""));
        }
    }
    text.push('>');
    text
}

fn is_hidden(element: &ElementRef<'_>) -> bool {
    std::iter::once(*element).chain(element.ancestors().filter_map(ElementRef::wrap)).any(|e| {
        e.value().attr("aria-hidden") == Some("true") || e.value().attr("hidden").is_some()
    })
}

fn visible_text(element: &ElementRef<'_>) -> String {
    let mut text = String::new();
    for node in element.descendants() {
        if let Some(t) = node.value().as_text() {
            let hidden = node
                .ancestors()
                .filter_map(ElementRef::wrap)
                .take_while(|a| a.id() != element.id())
                .any(|a| a.value().attr("aria-hidden") == Some("true"));
            if !hidden {
                text.push_str(t);
            }
        }
        if let Some(e) = ElementRef::wrap(node)
            && e.id() != element.id()
            && e.value().name() == "img"
            && let Some(alt) = e.value().attr("alt")
        {
            text.push(' ');
            text.push_str(alt);
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

fn labelledby_text(document: &Html, ids: &str) -> String {
    ids.split_whitespace()
        .filter_map(|id| {
            let css = format!("[id=\"{}\"]", id.replace('"', ""));
            Selector::parse(&css).ok().and_then(|s| document.select(&s).next())
        })
        .map(|e| visible_text(&e))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The accessible name from ARIA attributes, or an empty string.
fn aria_name(document: &Html, element: &ElementRef<'_>) -> String {
    let value = element.value();
    if let Some(ids) = value.attr("aria-labelledby") {
        let text = labelledby_text(document, ids);
        if !text.trim().is_empty() {
            return text;
        }
    }
    value.attr("aria-label").unwrap_or_default().trim().to_string()
}

fn valid_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or_default();
    let primary_ok = (2..=3).contains(&primary.len())
        && primary.chars().all(|c| c.is_ascii_alphabetic())
        || primary.eq_ignore_ascii_case("x")
        || primary.eq_ignore_ascii_case("i");
    primary_ok
        && parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

const GENERIC_LINK_TEXT: &[&str] =
    &["click here", "here", "read more", "more", "link", "this link", "click", "learn more"];

/// Runs the automated WCAG 2.2 AA rules over one page.
///
/// | Rule | WCAG | Severity |
/// |------|------|----------|
/// | `html-has-lang`, `html-lang-valid` | 3.1.1 | Serious |
/// | `document-title` | 2.4.2 | Serious |
/// | `image-alt`, `input-image-alt` | 1.1.1 | Critical |
/// | `svg-img-alt` | 1.1.1 | Serious |
/// | `label` | 1.3.1, 4.1.2 | Critical |
/// | `button-name` | 4.1.2 | Critical |
/// | `link-name` | 2.4.4, 4.1.2 | Serious |
/// | `frame-title` | 4.1.2 | Serious |
/// | `landmark-one-main` | 1.3.1 | Serious |
/// | `meta-viewport` | 1.4.4 | Critical |
/// | `heading-order`, `page-has-heading-one` | 1.3.1 | Moderate |
/// | `link-text-generic` | 2.4.4 | Moderate |
pub(crate) fn accessibility(document: &Html) -> Vec<A11yViolation> {
    let mut out = Vec::new();
    let mut push = |rule, wcag, severity, element: String, help| {
        out.push(A11yViolation { rule, wcag, severity, element, help });
    };

    match document.select(&selector("html")).next() {
        Some(html) => match html.value().attr("lang").map(str::trim) {
            None | Some("") => push(
                "html-has-lang",
                "3.1.1",
                Severity::Serious,
                "<html>".into(),
                "Give the <html> element a lang attribute naming the page's language.",
            ),
            Some(lang) if !valid_language_tag(lang) => push(
                "html-lang-valid",
                "3.1.1",
                Severity::Serious,
                format!("<html lang=\"{lang}\">"),
                "Use a valid BCP 47 language tag in the lang attribute.",
            ),
            Some(_) => {}
        },
        None => push(
            "html-has-lang",
            "3.1.1",
            Severity::Serious,
            "document".into(),
            "Give the <html> element a lang attribute naming the page's language.",
        ),
    }

    let title = document.select(&selector("head > title, title")).next();
    if title.is_none_or(|t| visible_text(&t).is_empty()) {
        push(
            "document-title",
            "2.4.2",
            Severity::Serious,
            "<title>".into(),
            "Give the page a non-empty <title>.",
        );
    }

    for img in document.select(&selector("img")) {
        let v = img.value();
        let presentational = matches!(v.attr("role"), Some("presentation" | "none"));
        if v.attr("alt").is_none()
            && !presentational
            && !is_hidden(&img)
            && aria_name(document, &img).is_empty()
            && !non_empty(v.attr("title"))
        {
            push(
                "image-alt",
                "1.1.1",
                Severity::Critical,
                describe(&img),
                "Give the image alt text, or alt=\"\" when it is decorative.",
            );
        }
    }
    for input in document.select(&selector("input[type=image]")) {
        let v = input.value();
        if !non_empty(v.attr("alt")) && aria_name(document, &input).is_empty() {
            push(
                "input-image-alt",
                "1.1.1",
                Severity::Critical,
                describe(&input),
                "Give the image button alt text that names its action.",
            );
        }
    }
    for svg in document.select(&selector("svg[role=img]")) {
        let has_title =
            svg.select(&selector("title")).next().is_some_and(|t| !visible_text(&t).is_empty());
        if !has_title && aria_name(document, &svg).is_empty() {
            push(
                "svg-img-alt",
                "1.1.1",
                Severity::Serious,
                describe(&svg),
                "Give the SVG image a <title> or an aria-label.",
            );
        }
    }

    for control in document.select(&selector("input, select, textarea")) {
        let v = control.value();
        let kind = v.attr("type").unwrap_or("text").to_ascii_lowercase();
        if v.name() == "input" && matches!(kind.as_str(), "hidden" | "image") {
            continue;
        }
        if v.name() == "input" && matches!(kind.as_str(), "submit" | "reset" | "button") {
            let named = non_empty(v.attr("value"))
                || !aria_name(document, &control).is_empty()
                || non_empty(v.attr("title"))
                || kind != "button";
            if !named {
                push(
                    "button-name",
                    "4.1.2",
                    Severity::Critical,
                    describe(&control),
                    "Give the button a value or an aria-label.",
                );
            }
            continue;
        }
        let by_for = v.attr("id").is_some_and(|id| {
            let css = format!("label[for=\"{}\"]", id.replace('"', ""));
            Selector::parse(&css)
                .ok()
                .and_then(|s| document.select(&s).next())
                .is_some_and(|label| !visible_text(&label).is_empty())
        });
        let wrapped = control
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|a| a.value().name() == "label" && !visible_text(&a).is_empty());
        if !by_for
            && !wrapped
            && aria_name(document, &control).is_empty()
            && !non_empty(v.attr("title"))
        {
            push(
                "label",
                "1.3.1",
                Severity::Critical,
                describe(&control),
                "Associate the form control with a <label>, or give it an aria-label.",
            );
        }
    }

    for button in document.select(&selector("button")) {
        if visible_text(&button).is_empty()
            && aria_name(document, &button).is_empty()
            && !non_empty(button.value().attr("title"))
        {
            push(
                "button-name",
                "4.1.2",
                Severity::Critical,
                describe(&button),
                "Give the button text or an aria-label.",
            );
        }
    }

    for link in document.select(&selector("a[href]")) {
        if is_hidden(&link) {
            continue;
        }
        let text = visible_text(&link);
        let aria = aria_name(document, &link);
        if text.is_empty() && aria.is_empty() && !non_empty(link.value().attr("title")) {
            push(
                "link-name",
                "2.4.4",
                Severity::Serious,
                describe(&link),
                "Give the link text, an image with alt text, or an aria-label.",
            );
        } else if aria.is_empty() && GENERIC_LINK_TEXT.contains(&text.to_lowercase().as_str()) {
            push(
                "link-text-generic",
                "2.4.4",
                Severity::Moderate,
                describe(&link),
                "Say where the link goes instead of a generic phrase.",
            );
        }
    }

    for frame in document.select(&selector("iframe, frame")) {
        if !non_empty(frame.value().attr("title")) && aria_name(document, &frame).is_empty() {
            push(
                "frame-title",
                "4.1.2",
                Severity::Serious,
                describe(&frame),
                "Give the frame a title that describes its content.",
            );
        }
    }

    let mains = document.select(&selector("main, [role=main]")).count();
    if mains != 1 {
        push(
            "landmark-one-main",
            "1.3.1",
            Severity::Serious,
            format!("{mains} main landmarks"),
            "Put the page's content in exactly one <main> landmark.",
        );
    }

    for meta in document.select(&selector("meta[name=viewport]")) {
        let content =
            meta.value().attr("content").unwrap_or_default().replace(' ', "").to_lowercase();
        let blocks_zoom = content.split(',').any(|part| match part.split_once('=') {
            Some(("user-scalable", v)) => v == "no" || v == "0",
            Some(("maximum-scale", v)) => v.parse::<f64>().is_ok_and(|s| s < 2.0),
            _ => false,
        });
        if blocks_zoom {
            push(
                "meta-viewport",
                "1.4.4",
                Severity::Critical,
                describe(&meta),
                "Let people zoom: drop user-scalable=no and keep maximum-scale at 2 or more.",
            );
        }
    }

    let mut previous: Option<u8> = None;
    let mut has_h1 = false;
    for heading in document.select(&selector("h1, h2, h3, h4, h5, h6")) {
        let level = heading.value().name()[1..].parse::<u8>().unwrap_or(1);
        has_h1 |= level == 1;
        if let Some(prev) = previous
            && level > prev + 1
        {
            push(
                "heading-order",
                "1.3.1",
                Severity::Moderate,
                describe(&heading),
                "Increase heading levels one at a time.",
            );
        }
        previous = Some(level);
    }
    if !has_h1 {
        push(
            "page-has-heading-one",
            "1.3.1",
            Severity::Moderate,
            "document".into(),
            "Give the page one <h1> naming its content.",
        );
    }
    out
}

/// Every URL the page refers to: links, images, scripts, styles, frames, media and forms.
pub(crate) fn links(document: &Html) -> Vec<LinkRef> {
    let mut out = Vec::new();
    let attrs = [
        ("a[href]", "href"),
        ("area[href]", "href"),
        ("link[href]", "href"),
        ("img[src]", "src"),
        ("script[src]", "src"),
        ("iframe[src]", "src"),
        ("source[src]", "src"),
        ("video[src]", "src"),
        ("video[poster]", "poster"),
        ("audio[src]", "src"),
        ("track[src]", "src"),
        ("embed[src]", "src"),
        ("object[data]", "data"),
        ("form[action]", "action"),
    ];
    for (css, attr) in attrs {
        for element in document.select(&selector(css)) {
            // Canonical and hreflang alternates name public URLs, checked as pages like any link.
            if let Some(url) = element.value().attr(attr) {
                out.push(LinkRef {
                    tag: element.value().name().to_string(),
                    url: url.trim().to_string(),
                });
            }
        }
    }
    for css in ["img[srcset]", "source[srcset]"] {
        for element in document.select(&selector(css)) {
            for candidate in element.value().attr("srcset").unwrap_or_default().split(',') {
                if let Some(url) = candidate.split_whitespace().next() {
                    out.push(LinkRef {
                        tag: element.value().name().to_string(),
                        url: url.to_string(),
                    });
                }
            }
        }
    }
    out
}

/// The resources that block first render: stylesheets, synchronous scripts, preloads and
/// the first eagerly loaded image in the main landmark (the likely LCP element).
pub(crate) fn critical_resources(document: &Html) -> Vec<String> {
    let mut out = BTreeSet::new();
    for link in document.select(&selector("link[href]")) {
        let rel = link.value().attr("rel").unwrap_or_default().to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_whitespace().collect();
        if rels.contains(&"stylesheet") || rels.contains(&"preload") {
            out.insert(link.value().attr("href").unwrap_or_default().to_string());
        }
    }
    for script in document.select(&selector("script[src]")) {
        let v = script.value();
        if v.attr("async").is_none()
            && v.attr("defer").is_none()
            && v.attr("type") != Some("module")
        {
            out.insert(v.attr("src").unwrap_or_default().to_string());
        }
    }
    let scope = document
        .select(&selector("main img[src], [role=main] img[src]"))
        .chain(document.select(&selector("img[src]")))
        .find(|img| img.value().attr("loading") != Some("lazy"));
    if let Some(img) = scope {
        out.insert(img.value().attr("src").unwrap_or_default().to_string());
    }
    out.into_iter().filter(|u| !u.is_empty()).collect()
}

/// The page's `<link rel="alternate">` targets as `(type, href)`.
pub(crate) fn alternates(document: &Html) -> Vec<(String, String)> {
    document
        .select(&selector("link[rel~=alternate][href]"))
        .map(|link| {
            (
                link.value().attr("type").unwrap_or_default().to_string(),
                link.value().attr("href").unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The JSON-LD blocks of the page.
pub(crate) fn json_ld(document: &Html) -> Vec<String> {
    document
        .select(&selector("script[type=\"application/ld+json\"]"))
        .map(|s| s.text().collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(html: &str) -> Vec<&'static str> {
        accessibility(&Html::parse_document(html)).into_iter().map(|v| v.rule).collect()
    }

    const GOOD: &str = r#"<!doctype html><html lang="en"><head><title>Home</title>
        <meta name="viewport" content="width=device-width, initial-scale=1"></head>
        <body><header><a href="/">Example</a></header><main><h1>Welcome</h1>
        <img src="a.png" alt="A solar panel"><img src="line.png" alt="">
        <form><label for="e">Email</label><input id="e" type="email">
        <label>Name <input name="n"></label><input type="submit" value="Send"></form>
        <h2>More</h2><a href="/about/">About us</a></main></body></html>"#;

    #[test]
    fn a_good_page_has_no_violations() {
        assert!(rules(GOOD).is_empty(), "{:?}", rules(GOOD));
    }

    #[test]
    fn each_rule_fires_on_its_fixture() {
        assert!(
            rules("<html><head><title>x</title></head><body><main><h1>x</h1></main></body></html>")
                .contains(&"html-has-lang")
        );
        assert!(
            rules(&GOOD.replace("lang=\"en\"", "lang=\"english language\""))
                .contains(&"html-lang-valid")
        );
        assert!(rules(&GOOD.replace("<title>Home</title>", "")).contains(&"document-title"));
        assert!(rules(&GOOD.replace(" alt=\"A solar panel\"", "")).contains(&"image-alt"));
        assert!(rules(&GOOD.replace("<label for=\"e\">Email</label>", "")).contains(&"label"));
        assert!(rules(&GOOD.replace(">About us<", "><")).contains(&"link-name"));
        assert!(rules(&GOOD.replace(">About us<", ">click here<")).contains(&"link-text-generic"));
        assert!(
            rules(&GOOD.replace("<main>", "<div>").replace("</main>", "</div>"))
                .contains(&"landmark-one-main")
        );
        assert!(rules(&GOOD.replace("<h2>More</h2>", "<h4>More</h4>")).contains(&"heading-order"));
        assert!(rules(&GOOD.replace("<h1>Welcome</h1>", "")).contains(&"page-has-heading-one"));
        assert!(
            rules(&GOOD.replace("initial-scale=1", "initial-scale=1, user-scalable=no"))
                .contains(&"meta-viewport")
        );
        assert!(
            rules(&GOOD.replace("</main>", "<iframe src=\"/m\"></iframe><button></button></main>"))
                .contains(&"frame-title")
        );
        assert!(
            rules(&GOOD.replace("</main>", "<button></button></main>")).contains(&"button-name")
        );
    }

    #[test]
    fn severities_split_gate_failures_from_advice() {
        let violations =
            accessibility(&Html::parse_document(&GOOD.replace(">About us<", ">here<")));
        assert!(violations.iter().all(|v| !v.severity.fails_gate()));
    }

    #[test]
    fn critical_path_and_links_are_extracted() {
        let page = Html::parse_document(
            r#"<html><head><link rel="stylesheet" href="/s.css"><script src="/a.js"></script>
            <script src="/b.js" defer></script><link rel="alternate" type="text/markdown" href="index.md"></head>
            <body><main><img src="/hero.png" srcset="/hero-2x.png 2x" alt="h"></main><a href="/x/">x</a></body></html>"#,
        );
        assert_eq!(critical_resources(&page), vec!["/a.js", "/hero.png", "/s.css"]);
        let urls: Vec<String> = links(&page).into_iter().map(|l| l.url).collect();
        assert!(urls.contains(&"/hero-2x.png".to_string()) && urls.contains(&"/x/".to_string()));
        assert_eq!(alternates(&page), vec![("text/markdown".to_string(), "index.md".to_string())]);
    }
}
