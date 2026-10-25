//! A small element tree over quick-xml for WXR exports and sitemaps.

use quick_xml::Reader;
use quick_xml::events::Event;

use super::error::{MigrateError, Result};

/// An XML element: its qualified name as written (`wp:post_id`), attributes, text and
/// children. Text is the concatenation of the element's own text and CDATA.
#[derive(Debug, Clone, Default)]
pub(crate) struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The trimmed text of the first child named `name`, when not empty.
    pub fn child_text(&self, name: &str) -> Option<String> {
        self.child(name).map(|c| c.text.trim().to_string()).filter(|t| !t.is_empty())
    }

    /// An attribute's value.
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

/// Parses a document into its root element.
pub(crate) fn parse(text: &str, what: &str) -> Result<Element> {
    let err = |e: &dyn std::fmt::Display| MigrateError::parse(what, e);
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Element> = vec![Element::default()];
    loop {
        let event = reader.read_event().map_err(|e| err(&e))?;
        match event {
            Event::Start(start) => {
                let element = open(&start).map_err(|e| err(&e))?;
                stack.push(element);
            }
            Event::Empty(start) => {
                let element = open(&start).map_err(|e| err(&e))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err(MigrateError::parse(what, "unbalanced end tag"));
                }
                let element = stack.pop().unwrap_or_default();
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::Text(text) => {
                let decoded = text.xml10_content().map_err(|e| err(&e))?;
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&decoded);
                }
            }
            Event::CData(data) => {
                let decoded = data.decode().map_err(|e| err(&e))?;
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&decoded);
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = if reference.is_char_ref() {
                    reference.resolve_char_ref().map_err(|e| err(&e))?
                } else {
                    let name = reference.decode().map_err(|e| err(&e))?;
                    entity(&name)
                };
                if let Some(current) = stack.last_mut()
                    && let Some(c) = resolved
                {
                    current.text.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut document = stack.into_iter().next().unwrap_or_default();
    document
        .children
        .pop()
        .filter(|_| document.children.is_empty())
        .ok_or_else(|| MigrateError::parse(what, "expected exactly one root element"))
}

fn open(start: &quick_xml::events::BytesStart<'_>) -> std::result::Result<Element, String> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| e.to_string())?
            .into_owned();
        attributes.push((key, value));
    }
    Ok(Element { name, attributes, text: String::new(), children: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cdata_and_entities_are_joined() {
        let root = parse(
            r#"<?xml version="1.0"?><rss xmlns:wp="x"><channel><title>A &amp; B</title>
            <item><wp:post_id>7</wp:post_id><content:encoded><![CDATA[<p>Hi</p>]]></content:encoded>
            <category domain="post_tag" nicename="t">T&#233;</category></item></channel></rss>"#,
            "test",
        )
        .unwrap();
        assert_eq!(root.name, "rss");
        let channel = root.child("channel").unwrap();
        assert_eq!(channel.child_text("title").as_deref(), Some("A & B"));
        let item = channel.child("item").unwrap();
        assert_eq!(item.child_text("wp:post_id").as_deref(), Some("7"));
        assert_eq!(item.child_text("content:encoded").as_deref(), Some("<p>Hi</p>"));
        let category = item.child("category").unwrap();
        assert_eq!(category.attribute("domain"), Some("post_tag"));
        assert_eq!(category.text, "Té");
    }

    #[test]
    fn malformed_documents_are_errors() {
        assert!(parse("<a><b></a>", "test").is_err());
        assert!(parse("", "test").is_err());
    }
}
