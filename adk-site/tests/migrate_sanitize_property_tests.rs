//! The sanitiser never emits an element outside its allowlist (spec 3.5 §11 "Property tests",
//! §7.8 "Sanitiser"): for any HTML, its Markdown parses to no raw HTML, renders through the
//! built-in renderer to allowlisted elements only, carries no handler, style or script URL,
//! and keeps nothing from scripts or hidden elements.
#![cfg(feature = "migrate")]

use adk_site::generator::markdown_to_html;
use adk_site::migrate::sanitize::{ALLOWED_ELEMENTS, Sanitiser};
use proptest::prelude::*;
use pulldown_cmark::{Event, Options, Parser};
use scraper::{Html, Node};
use url::Url;

const MARKER: &str = "DROPPEDMARKER";

const TAGS: &[&str] = &[
    "p",
    "div",
    "span",
    "a",
    "img",
    "ul",
    "ol",
    "li",
    "h1",
    "h2",
    "h4",
    "blockquote",
    "pre",
    "code",
    "em",
    "strong",
    "b",
    "i",
    "br",
    "hr",
    "table",
    "tr",
    "td",
    "th",
    "iframe",
    "object",
    "embed",
    "form",
    "input",
    "section",
    "figure",
    "figcaption",
    "del",
    "video",
    "small",
    "sup",
];

const ATTRIBUTES: &[&str] = &[
    r#"href="javascript:alert(1)""#,
    r#"href=" JaVaScRiPt:alert(1)""#,
    r#"href="data:text/html,<script>x</script>""#,
    r#"href="vbscript:msgbox""#,
    r#"href="https://x.test/a b(c)""#,
    r#"href="/path/""#,
    r##"href="#frag""##,
    r#"href="mailto:a@b.test""#,
    r#"src="javascript:alert(1)""#,
    r#"src="https://www.youtube-nocookie.com/embed/x""#,
    r#"src="https://evil.test/x""#,
    r#"src="/img.png""#,
    r#"onclick="steal()""#,
    r#"onerror="steal()""#,
    r#"style="color:red""#,
    r#"class="x""#,
    r#"alt="a *b* [c]""#,
    r#"title="t""#,
];

const WORDS: &[&str] = &[
    "hello",
    "*star*",
    "_under_",
    "[link](x)",
    "<b>",
    "&lt;script&gt;",
    "&amp;",
    "# head",
    "1. item",
    "- dash",
    "> quote",
    "`tick`",
    "|pipe|",
    "~~del~~",
    "!",
    "\\",
    "{brace}",
    "---",
    "[^1]",
    "- [ ] task",
    "http://bare.test",
    "&#60;img&#62;",
];

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(WORDS), 1..4).prop_map(|w| w.join(" "))
}

fn html() -> impl Strategy<Value = String> {
    let leaf = text();
    leaf.prop_recursive(4, 48, 4, |inner| {
        prop_oneof![
            4 => (
                prop::sample::select(TAGS),
                prop::collection::vec(prop::sample::select(ATTRIBUTES), 0..3),
                prop::collection::vec(inner.clone(), 0..4),
            )
                .prop_map(|(tag, attributes, children)| {
                    format!("<{tag} {}>{}</{tag}>", attributes.join(" "), children.concat())
                }),
            1 => inner.clone().prop_map(|_| format!("<script>var m = '{MARKER}';</script>")),
            1 => inner.clone().prop_map(|_| format!("<style>.m::after{{content:'{MARKER}'}}</style>")),
            1 => inner.clone().prop_map(|child| format!("<div style=\"display:none\">{MARKER} {child}</div>")),
            1 => inner.clone().prop_map(|child| format!("<span hidden>{MARKER}{child}</span>")),
            1 => inner.prop_map(|child| format!("<p style=\"position:absolute;left:-9999px\"><a href=\"https://spam.test\">{MARKER}</a>{child}</p>")),
        ]
    })
}

fn check(markdown: &str) -> Result<(), TestCaseError> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        prop_assert!(
            !matches!(event, Event::Html(_) | Event::InlineHtml(_)),
            "raw HTML in {markdown:?}"
        );
    }
    prop_assert!(!markdown.contains(MARKER), "dropped content kept: {markdown:?}");
    let rendered = markdown_to_html(markdown);
    let fragment = Html::parse_fragment(&rendered);
    for node in fragment.tree.nodes() {
        let Node::Element(element) = node.value() else { continue };
        let name = element.name();
        prop_assert!(
            ALLOWED_ELEMENTS.contains(&name) || matches!(name, "html" | "head" | "body"),
            "<{name}> outside the allowlist in {rendered:?} from {markdown:?}"
        );
        for (attribute, value) in element.attrs() {
            prop_assert!(
                !attribute.starts_with("on") && attribute != "style",
                "{attribute} in {rendered:?}"
            );
            let value = value.trim().to_ascii_lowercase();
            for scheme in ["javascript:", "data:", "vbscript:"] {
                prop_assert!(!value.starts_with(scheme), "{attribute}={value} in {rendered:?}");
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn sanitised_markdown_stays_inside_the_allowlist(input in html()) {
        let page = Url::parse("https://solar.test/posts/a/").unwrap();
        check(&Sanitiser::new().sanitise(&input, Some(&page)).markdown)?;
        check(&Sanitiser::new().sanitise(&input, None).markdown)?;
    }

    #[test]
    fn plain_text_survives_as_text(words in prop::collection::vec(prop::sample::select(WORDS), 1..6)) {
        let text = words.join(" ");
        let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let markdown = Sanitiser::new().sanitise(&format!("<p>{escaped}</p>"), None).markdown;
        check(&markdown)?;
        let rendered = Html::parse_fragment(&markdown_to_html(&markdown));
        let shown: String = rendered.root_element().text().collect();
        let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let decoded = Html::parse_fragment(&escaped).root_element().text().collect::<String>();
        prop_assert_eq!(collapse(&shown), collapse(&decoded));
    }
}
