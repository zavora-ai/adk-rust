//! The sanitiser: HTML to Markdown through an allowlist (spec 3.5 §7.8 "Sanitiser").
//!
//! | Input | Output |
//! |-------|--------|
//! | `p`, `h1`–`h6`, `ul`, `ol`, `li`, `blockquote`, `pre`, `hr`, `table` | Markdown blocks |
//! | `a` (`http`, `https`, `mailto`, `tel` or a path), `img`, `em`, `strong`, `code`, `del`, `br` | Markdown inline syntax |
//! | `iframe` from a listed host | A link to the embedded page; the built-in renderer escapes raw HTML |
//! | `div`, `span`, `section`, `figure` and other containers | Their content |
//! | `script`, `style`, `noscript`, `noembed`, `object`, `embed`, `applet`, unlisted `iframe`, forms and their controls | Dropped with their content, recorded; an element's raw text never becomes page text |
//! | Elements hidden by a style (`display:none`, off-screen, zero size) or the `hidden` attribute | Dropped with their content, recorded |
//! | Inline handlers (`on*`), `style`, `class` and every other attribute | Dropped, handlers recorded |
//!
//! Text is escaped so it can never become Markdown syntax or raw HTML: the output parses to
//! no HTML events, and rendered it holds only the elements listed in [`ALLOWED_ELEMENTS`].

use std::collections::BTreeSet;

use ego_tree::NodeRef;
use scraper::{Html, Node};
use serde::{Deserialize, Serialize};
use url::Url;

use super::html::HiddenRules;

/// The elements a sanitised body renders to through the built-in renderer.
pub const ALLOWED_ELEMENTS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "a",
    "img",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "code",
    "em",
    "strong",
    "del",
    "br",
    "hr",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
];

/// Elements dropped with their content.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "template", "object", "embed", "applet", "form", "input",
    "button", "select", "textarea", "option", "svg", "math", "canvas", "head", "meta", "link",
    "base", "frame", "frameset", "title", "dialog", "portal", "noembed", "noframes",
];

/// Iframe hosts kept by default: privacy-enhanced video and map embeds.
pub const DEFAULT_IFRAME_HOSTS: &[&str] =
    &["www.youtube-nocookie.com", "player.vimeo.com", "www.google.com", "www.openstreetmap.org"];

/// Why something was dropped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
#[non_exhaustive]
pub enum DropReason {
    /// A `<script>`.
    Script,
    /// An inline event handler attribute.
    InlineHandler {
        /// The attribute.
        attribute: String,
    },
    /// `<object>`, `<embed>` or `<applet>`.
    Plugin,
    /// An `<iframe>` whose host is not listed.
    UnlistedIframe {
        /// Its `src`.
        src: String,
    },
    /// An element hidden by a style or the `hidden` attribute.
    Hidden {
        /// How it was hidden.
        how: String,
    },
    /// A form or form control.
    Form,
    /// A URL with a scheme other than `http`, `https`, `mailto` or `tel`.
    UnsafeUrl {
        /// The URL.
        url: String,
    },
    /// Any other element outside the allowlist that is dropped with its content.
    Other,
    /// A link or image to a page of the site that the crawl could not read, such as one
    /// `robots.txt` disallows: the link becomes its text, the image is left out.
    UnreadTarget {
        /// The target, as a path on the site.
        target: String,
    },
}

/// One dropped element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dropped {
    /// The element's name.
    pub element: String,
    /// Why.
    #[serde(flatten)]
    pub reason: DropReason,
    /// A short excerpt of what it held, as text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excerpt: String,
}

/// How a URL is used, for [`Sanitiser::sanitise_mapped`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlUse {
    /// A link.
    Link,
    /// An image.
    Image,
}

/// A sanitised body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sanitised {
    /// The Markdown.
    pub markdown: String,
    /// What was dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<Dropped>,
    /// The link targets written, after mapping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// The image sources written, after mapping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// HTML to Markdown through an allowlist (spec 3.5 §7.8 "Sanitiser").
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::sanitize::Sanitiser;
///
/// let out = Sanitiser::new().sanitise(
///     r#"<p onclick="x()">Hello <b>world</b><script>steal()</script></p>
///        <div style="display:none"><a href="https://spam.example">pills</a></div>"#,
///     None,
/// );
/// assert_eq!(out.markdown, "Hello **world**\n");
/// assert_eq!(out.dropped.len(), 3);
/// ```
#[derive(Debug, Clone)]
pub struct Sanitiser {
    iframe_hosts: BTreeSet<String>,
}

impl Default for Sanitiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Sanitiser {
    /// A sanitiser that keeps embeds from [`DEFAULT_IFRAME_HOSTS`].
    pub fn new() -> Self {
        Self { iframe_hosts: DEFAULT_IFRAME_HOSTS.iter().map(|h| (*h).to_string()).collect() }
    }

    /// Keeps embeds from exactly `hosts`.
    #[must_use]
    pub fn with_iframe_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.iframe_hosts = hosts.into_iter().map(|h| h.into().to_ascii_lowercase()).collect();
        self
    }

    /// Sanitises `html`; links and images on `page`'s origin become paths.
    pub fn sanitise(&self, html: &str, page: Option<&Url>) -> Sanitised {
        let origin = page.map(super::http::origin_of);
        self.sanitise_mapped(html, page, &mut |_, url: &Url| match &origin {
            Some(origin) => super::html::site_path(url, origin),
            None => url.to_string(),
        })
    }

    /// Sanitises `html`, writing every resolved `http` and `https` URL as `map` returns it. A
    /// link `map` writes as an empty string becomes its text, and such an image is left out.
    pub fn sanitise_mapped(
        &self,
        html: &str,
        page: Option<&Url>,
        map: &mut dyn FnMut(UrlUse, &Url) -> String,
    ) -> Sanitised {
        let fragment = Html::parse_fragment(&autop(html));
        let hidden = HiddenRules::from_document(&fragment);
        let mut converter =
            Converter { sanitiser: self, page, map, hidden, out: Sanitised::default() };
        let blocks = converter.blocks(fragment.tree.root());
        let mut markdown = blocks.join("\n\n");
        if !markdown.is_empty() {
            markdown.push('\n');
        }
        converter.out.markdown = markdown;
        converter.out
    }
}

/// WordPress stores classic-editor posts without paragraph tags; blank lines separate
/// paragraphs. Text with no block element gets `<p>`s, as WordPress's `wpautop` would.
fn autop(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let has_blocks = [
        "<p",
        "<div",
        "<h1",
        "<h2",
        "<h3",
        "<ul",
        "<ol",
        "<table",
        "<blockquote",
        "<pre",
        "<figure",
        "<section",
        "<article",
    ]
    .iter()
    .any(|tag| lower.contains(tag));
    if has_blocks || !html.contains("\n\n") {
        return html.to_string();
    }
    html.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("<p>{}</p>", p.replace('\n', "<br>")))
        .collect::<Vec<_>>()
        .join("\n")
}

struct Converter<'a, 'm> {
    sanitiser: &'a Sanitiser,
    page: Option<&'a Url>,
    map: &'m mut dyn FnMut(UrlUse, &Url) -> String,
    hidden: HiddenRules,
    out: Sanitised,
}

/// Ends a run of inline content as a paragraph block.
fn flush(inline: &mut String, blocks: &mut Vec<String>) {
    let text = inline.trim().to_string();
    if !text.is_empty() {
        blocks.push(escape_line_starts(&text));
    }
    inline.clear();
}

fn excerpt(node: NodeRef<'_, Node>) -> String {
    let mut text = String::new();
    for descendant in node.descendants() {
        if let Node::Text(t) = descendant.value() {
            text.push_str(t);
            text.push(' ');
        }
        if text.len() > 160 {
            break;
        }
    }
    let collapsed = super::html::collapse(&text);
    collapsed.chars().take(120).collect()
}

/// Escapes text so it renders as itself: every character with Markdown or HTML meaning.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '&' | '!' | '#' | '{'
            | '}' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Escapes what would start a list, heading, quote or thematic break at the start of a line.
fn escape_line_starts(block: &str) -> String {
    block
        .split('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            let indent = &line[..line.len() - trimmed.len()];
            let first = trimmed.chars().next();
            if matches!(first, Some('-' | '+' | '=' | '>' | '#')) {
                return format!("{indent}\\{trimmed}");
            }
            let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                let rest = &trimmed[digits.len()..];
                if rest.starts_with('.') || rest.starts_with(')') {
                    return format!("{indent}{digits}\\{rest}");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A URL written into Markdown: characters that end a destination are percent-encoded.
fn destination(url: &str) -> String {
    url.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('\n', "")
}

impl Converter<'_, '_> {
    fn record(&mut self, element: &str, reason: DropReason, node: NodeRef<'_, Node>) {
        self.out.dropped.push(Dropped {
            element: element.to_string(),
            reason,
            excerpt: excerpt(node),
        });
    }

    /// Records inline handlers on an element that is kept or unwrapped.
    fn check_handlers(&mut self, element: &scraper::node::Element) {
        for (name, _) in element.attrs() {
            if name.to_ascii_lowercase().starts_with("on") {
                self.out.dropped.push(Dropped {
                    element: element.name().to_string(),
                    reason: DropReason::InlineHandler { attribute: name.to_string() },
                    excerpt: String::new(),
                });
            }
        }
    }

    /// Resolves an `href` or `src`; `None` drops it.
    fn url(&mut self, raw: &str, kind: UrlUse) -> Option<String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if raw.starts_with('#') {
            return (kind == UrlUse::Link).then(|| destination(raw));
        }
        let lower = raw.to_ascii_lowercase();
        if kind == UrlUse::Link && (lower.starts_with("mailto:") || lower.starts_with("tel:")) {
            return Some(destination(raw));
        }
        let resolved = match self.page {
            Some(page) => page.join(raw).ok(),
            None => Url::parse(raw).ok().or_else(|| {
                Url::parse("https://site.invalid/").ok().and_then(|base| base.join(raw).ok())
            }),
        };
        match resolved {
            Some(url) if matches!(url.scheme(), "http" | "https") => {
                let written = if url.host_str() == Some("site.invalid") && self.page.is_none() {
                    url[url::Position::BeforePath..].to_string()
                } else {
                    (self.map)(kind, &url)
                };
                Some(destination(&written))
            }
            _ => {
                self.out.dropped.push(Dropped {
                    element: if kind == UrlUse::Link { "a" } else { "img" }.into(),
                    reason: DropReason::UnsafeUrl { url: raw.chars().take(200).collect() },
                    excerpt: String::new(),
                });
                None
            }
        }
    }

    /// The text under `node` as written, for code: dropped and hidden elements left out.
    fn raw_text(&mut self, node: NodeRef<'_, Node>) -> String {
        let mut text = String::new();
        for child in node.children() {
            match child.value() {
                Node::Text(t) => text.push_str(t),
                Node::Element(element) => {
                    let name = element.name().to_ascii_lowercase();
                    if self.dropped(&name, element, child) {
                        continue;
                    }
                    self.check_handlers(element);
                    // Code holds no embed: an iframe there is dropped with its raw text.
                    if name == "iframe" {
                        if self.iframe(element, child).is_some() {
                            self.record("iframe", DropReason::Other, child);
                        }
                        continue;
                    }
                    text.push_str(&self.raw_text(child));
                }
                _ => {}
            }
        }
        text
    }

    /// Converts children in block context: block elements become blocks, runs of inline
    /// content become paragraphs.
    fn blocks(&mut self, node: NodeRef<'_, Node>) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut inline = String::new();
        for child in node.children() {
            self.block_child(child, &mut inline, &mut blocks);
        }
        flush(&mut inline, &mut blocks);
        blocks
    }

    /// Converts one node in block context: a block element flushes `inline` and adds its
    /// blocks, inline content joins `inline`, and a dropped element leaves nothing — the raw
    /// text of a `script` or `iframe` included.
    fn block_child(
        &mut self,
        child: NodeRef<'_, Node>,
        inline: &mut String,
        blocks: &mut Vec<String>,
    ) {
        match child.value() {
            Node::Text(text) => inline.push_str(&escape(&collapse_ws(text))),
            Node::Element(element) => {
                let name = element.name().to_ascii_lowercase();
                if self.dropped(&name, element, child) {
                    return;
                }
                self.check_handlers(element);
                match name.as_str() {
                    "p" | "address" | "figcaption" | "summary" | "dt" | "dd" | "caption" => {
                        flush(inline, blocks);
                        let text = self.inline_children(child);
                        let text = text.trim();
                        if !text.is_empty() {
                            blocks.push(escape_line_starts(text));
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush(inline, blocks);
                        let level: usize = name[1..].parse().unwrap_or(2);
                        let text = self.inline_children(child).replace('\n', " ");
                        let text = text.trim();
                        if !text.is_empty() {
                            blocks.push(format!("{} {text}", "#".repeat(level)));
                        }
                    }
                    "ul" | "ol" => {
                        flush(inline, blocks);
                        let list = self.list(child, name == "ol");
                        if !list.is_empty() {
                            blocks.push(list);
                        }
                    }
                    "blockquote" => {
                        flush(inline, blocks);
                        let inner = self.blocks(child).join("\n\n");
                        if !inner.trim().is_empty() {
                            blocks.push(
                                inner
                                    .lines()
                                    .map(
                                        |l| {
                                            if l.is_empty() { ">".into() } else { format!("> {l}") }
                                        },
                                    )
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                    }
                    "pre" => {
                        flush(inline, blocks);
                        let code = self.raw_text(child);
                        if !code.trim().is_empty() {
                            let longest = longest_run(&code, '`').max(2);
                            let fence = "`".repeat(longest + 1);
                            blocks
                                .push(format!("{fence}\n{}\n{fence}", code.trim_end_matches('\n')));
                        }
                    }
                    "hr" => {
                        flush(inline, blocks);
                        blocks.push("---".into());
                    }
                    "table" => {
                        flush(inline, blocks);
                        if let Some(table) = self.table(child) {
                            blocks.push(table);
                        }
                    }
                    "iframe" => {
                        flush(inline, blocks);
                        if let Some(link) = self.iframe(element, child) {
                            blocks.push(link);
                        }
                    }
                    "div" | "section" | "article" | "main" | "header" | "footer" | "aside"
                    | "nav" | "figure" | "details" | "dl" | "center" | "body" | "html"
                    | "hgroup" | "picture" => {
                        flush(inline, blocks);
                        blocks.extend(self.blocks(child));
                    }
                    _ => inline.push_str(&self.inline_element(&name, element, child)),
                }
            }
            _ => {}
        }
    }

    /// Whether an element is dropped with its content; records why.
    fn dropped(
        &mut self,
        name: &str,
        element: &scraper::node::Element,
        node: NodeRef<'_, Node>,
    ) -> bool {
        if let Some(how) = self.hidden.hides(element) {
            self.record(name, DropReason::Hidden { how }, node);
            return true;
        }
        if !DROPPED.contains(&name) {
            return false;
        }
        let reason = match name {
            "script" => DropReason::Script,
            "object" | "embed" | "applet" => DropReason::Plugin,
            "form" | "input" | "button" | "select" | "textarea" | "option" => DropReason::Form,
            _ => DropReason::Other,
        };
        self.record(name, reason, node);
        true
    }

    fn iframe(
        &mut self,
        element: &scraper::node::Element,
        node: NodeRef<'_, Node>,
    ) -> Option<String> {
        let src = element.attr("src").unwrap_or_default().to_string();
        let parsed = self.page.map_or_else(|| Url::parse(&src).ok(), |p| p.join(&src).ok());
        let listed = parsed.as_ref().is_some_and(|url| {
            url.scheme() == "https"
                && url
                    .host_str()
                    .is_some_and(|h| self.sanitiser.iframe_hosts.contains(&h.to_ascii_lowercase()))
        });
        if !listed {
            self.record(
                "iframe",
                DropReason::UnlistedIframe { src: src.chars().take(200).collect() },
                node,
            );
            return None;
        }
        let url = parsed?;
        let title = element.attr("title").map(super::html::collapse).filter(|t| !t.is_empty());
        let label = escape(&title.unwrap_or_else(|| "Embedded content".into()));
        Some(format!("[{label}]({})", destination(url.as_str())))
    }

    fn list(&mut self, node: NodeRef<'_, Node>, ordered: bool) -> String {
        let mut lines = Vec::new();
        let mut number = 1;
        for child in node.children() {
            let Node::Element(element) = child.value() else { continue };
            let name = element.name().to_ascii_lowercase();
            let body = if name == "li" {
                if self.dropped(&name, element, child) {
                    continue;
                }
                self.check_handlers(element);
                self.blocks(child)
            } else {
                self.blocks_of_one(child)
            };
            let marker = if ordered { format!("{number}. ") } else { "- ".into() };
            let indent = " ".repeat(marker.len());
            let body = body.join("\n\n");
            let body = body.trim();
            if body.is_empty() {
                continue;
            }
            number += 1;
            let mut item = String::new();
            for (i, line) in body.lines().enumerate() {
                if i == 0 {
                    item.push_str(&marker);
                    item.push_str(line);
                } else if line.is_empty() {
                    item.push('\n');
                    continue;
                } else {
                    item.push('\n');
                    item.push_str(&indent);
                    item.push_str(line);
                }
            }
            lines.push(item);
        }
        lines.join("\n")
    }

    /// Converts a single non-`li` element found inside a list as a block: the element itself,
    /// so an `iframe` or `script` there is dropped or linked as it is anywhere else.
    fn blocks_of_one(&mut self, node: NodeRef<'_, Node>) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut inline = String::new();
        self.block_child(node, &mut inline, &mut blocks);
        flush(&mut inline, &mut blocks);
        blocks
    }

    fn table(&mut self, node: NodeRef<'_, Node>) -> Option<String> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for row in node.descendants() {
            let Node::Element(element) = row.value() else { continue };
            if element.name() != "tr" {
                continue;
            }
            let mut cells = Vec::new();
            for cell in row.children() {
                let Node::Element(cell_element) = cell.value() else { continue };
                if matches!(cell_element.name(), "td" | "th") {
                    self.check_handlers(cell_element);
                    let text = self.inline_children(cell).replace('\n', " ");
                    cells.push(text.trim().to_string());
                }
            }
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
        let width = rows.iter().map(Vec::len).max()?;
        let line = |cells: &[String]| {
            let mut padded: Vec<String> = cells.to_vec();
            padded.resize(width, String::new());
            format!("| {} |", padded.join(" | "))
        };
        let mut out = vec![line(&rows[0]), format!("|{}", " --- |".repeat(width))];
        out.extend(rows[1..].iter().map(|r| line(r)));
        Some(out.join("\n"))
    }

    fn inline_children(&mut self, node: NodeRef<'_, Node>) -> String {
        let mut out = String::new();
        for child in node.children() {
            match child.value() {
                Node::Text(text) => out.push_str(&escape(&collapse_ws(text))),
                Node::Element(element) => {
                    let name = element.name().to_ascii_lowercase();
                    if self.dropped(&name, element, child) {
                        continue;
                    }
                    self.check_handlers(element);
                    out.push_str(&self.inline_element(&name, element, child));
                }
                _ => {}
            }
        }
        out
    }

    fn inline_element(
        &mut self,
        name: &str,
        element: &scraper::node::Element,
        node: NodeRef<'_, Node>,
    ) -> String {
        let wrap = |inner: String, mark: &str| {
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                inner
            } else {
                let lead = if inner.starts_with(' ') { " " } else { "" };
                let tail = if inner.ends_with(' ') { " " } else { "" };
                format!("{lead}{mark}{trimmed}{mark}{tail}")
            }
        };
        match name {
            "strong" | "b" => wrap(self.inline_children(node), "**"),
            "em" | "i" | "cite" => wrap(self.inline_children(node), "*"),
            "del" | "s" | "strike" => wrap(self.inline_children(node), "~~"),
            "code" | "kbd" | "samp" | "tt" => {
                let text = collapse_ws(&self.raw_text(node));
                if text.trim().is_empty() {
                    return String::new();
                }
                let fence = "`".repeat(longest_run(&text, '`') + 1);
                let pad = if text.starts_with('`') || text.ends_with('`') { " " } else { "" };
                format!("{fence}{pad}{text}{pad}{fence}")
            }
            "br" => "\\\n".into(),
            "a" => {
                let text = self.inline_children(node);
                let href = element.attr("href").and_then(|h| self.url(h, UrlUse::Link));
                match href {
                    Some(href) if !href.is_empty() && !text.trim().is_empty() => {
                        self.out.links.push(href.clone());
                        format!("[{}]({href})", text.trim())
                    }
                    _ => text,
                }
            }
            "img" => {
                let src = element.attr("src").and_then(|s| self.url(s, UrlUse::Image));
                match src {
                    Some(src) if !src.is_empty() => {
                        self.out.images.push(src.clone());
                        let alt =
                            escape(&super::html::collapse(element.attr("alt").unwrap_or_default()));
                        format!("![{alt}]({src})")
                    }
                    _ => String::new(),
                }
            }
            "iframe" => self.iframe(element, node).unwrap_or_default(),
            "p" | "div" | "li" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "blockquote" | "pre" | "table" | "section" | "figure" => {
                // Block content in an inline position: keep its text on its own line.
                let inner = self.blocks(node).join(" ");
                format!(" {} ", inner.replace('\n', " "))
            }
            "video" | "audio" | "source" => {
                let src = element.attr("src").and_then(|s| self.url(s, UrlUse::Link));
                match src {
                    Some(src) if !src.is_empty() => {
                        format!("[{}]({src})", if name == "audio" { "Audio" } else { "Video" })
                    }
                    _ => String::new(),
                }
            }
            _ => self.inline_children(node),
        }
    }
}

fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn longest_run(text: &str, c: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch == c {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(html: &str) -> String {
        Sanitiser::new()
            .sanitise(html, Some(&Url::parse("https://old.test/post/").unwrap()))
            .markdown
    }

    #[test]
    fn structure_converts() {
        assert_eq!(
            md("<h2>Title</h2><p>One <em>two</em> <strong>three</strong>.</p>"),
            "## Title\n\nOne *two* **three**.\n"
        );
        assert_eq!(
            md("<ul><li>a</li><li>b <a href=\"/x/\">x</a></li></ul>"),
            "- a\n- b [x](/x/)\n"
        );
        assert_eq!(md("<ol><li>a</li><li>b</li></ol>"), "1. a\n2. b\n");
        assert_eq!(md("<blockquote><p>q</p></blockquote>"), "> q\n");
        assert_eq!(md("<pre><code>let x = `1`;</code></pre>"), "```\nlet x = `1`;\n```\n");
        assert_eq!(md("<p>a<br>b</p>"), "a\\\nb\n");
        assert_eq!(
            md("<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>"),
            "| A | B |\n| --- | --- |\n| 1 | 2 |\n"
        );
        assert_eq!(
            md("<p><img src=\"https://old.test/a b.jpg\" alt=\"x\"></p>"),
            "![x](/a%20b.jpg)\n"
        );
    }

    #[test]
    fn dangerous_content_is_dropped_and_recorded() {
        let out = Sanitiser::new().sanitise(
            r#"<p>Hi<script>alert(1)</script></p><object data="x.swf"></object><embed src="y">
            <iframe src="https://evil.test/frame"></iframe>
            <iframe src="https://www.youtube-nocookie.com/embed/abc" title="Tour"></iframe>
            <a href="javascript:alert(1)">click</a><img src="x" onerror="steal()">
            <span style="font-size:0">hidden</span>"#,
            None,
        );
        assert!(!out.markdown.contains("alert"));
        assert!(!out.markdown.contains("evil.test"));
        assert!(out.markdown.contains("[Tour](https://www.youtube-nocookie.com/embed/abc)"));
        let reasons: Vec<&DropReason> = out.dropped.iter().map(|d| &d.reason).collect();
        assert!(reasons.contains(&&DropReason::Script));
        assert!(reasons.contains(&&DropReason::Plugin));
        assert!(reasons.iter().any(|r| matches!(r, DropReason::UnlistedIframe { .. })));
        assert!(reasons.iter().any(|r| matches!(r, DropReason::UnsafeUrl { .. })));
        assert!(reasons.iter().any(|r| matches!(r, DropReason::InlineHandler { .. })));
        assert!(reasons.iter().any(|r| matches!(r, DropReason::Hidden { .. })));
    }

    #[test]
    fn text_never_becomes_syntax() {
        assert_eq!(
            md("<p>&lt;script&gt;x&lt;/script&gt; *a* [b](c)</p>"),
            "\\<script\\>x\\</script\\> \\*a\\* \\[b\\](c)\n"
        );
        assert_eq!(
            md("<p>1. not a list</p><p>- nor this</p><p># nor this</p>"),
            "1\\. not a list\n\n\\- nor this\n\n\\# nor this\n"
        );
    }

    #[test]
    fn classic_editor_text_gets_paragraphs() {
        assert_eq!(
            md("First line\n\nSecond [contact-form-7]"),
            "First line\n\nSecond \\[contact-form-7\\]\n"
        );
    }
}
