//! Content items as files: front matter plus a Markdown body (spec 3.5 §7.8.2).
//!
//! YAML front matter sits between `---` lines, TOML between `+++` lines. The keys `id`,
//! `kind`, `slug`, `language`, `state`, `volatile`, `sources`, `schedule`, `media` and
//! `fields` are the item's own; every other key is a field. Files are written with YAML
//! front matter and the fields under `fields`, so a field may share a name with an item key.

use std::collections::BTreeMap;

use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, FactRef, LanguageTag, Markdown, MediaRef,
    Schedule,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, SiteError};

const ITEM_KEYS: &[&str] = &[
    "id", "kind", "slug", "language", "state", "volatile", "sources", "schedule", "media", "fields",
];

/// The item keys of the front matter.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemKeys {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<ContentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<ContentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<LanguageTag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<ContentState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    volatile: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sources: Vec<FactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    media: Vec<MediaRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, Value>,
}

/// What the file's location says about an item, used when its front matter does not.
#[derive(Debug, Clone)]
pub(crate) struct ItemDefaults {
    pub id: ContentId,
    pub kind: ContentKind,
    pub slug: String,
    pub language: LanguageTag,
}

fn split(text: &str) -> Option<(char, &str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for (fence, kind) in [("---", 'y'), ("+++", 't')] {
        let Some(rest) = text.strip_prefix(fence) else { continue };
        let Some(rest) = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n")) else {
            continue;
        };
        let mut offset = 0;
        for line in rest.split_inclusive('\n') {
            if line.trim_end_matches(['\r', '\n']) == fence {
                let body = &rest[offset + line.len()..];
                return Some((kind, &rest[..offset], body));
            }
            offset += line.len();
        }
    }
    None
}

/// Parses an item file. `origin` names the file in errors.
pub(crate) fn parse_item(origin: &str, text: &str, defaults: ItemDefaults) -> Result<ContentItem> {
    let invalid = |message: String| SiteError::InvalidContent { path: origin.to_string(), message };
    let (map, body): (serde_json::Map<String, Value>, &str) = match split(text) {
        None => (serde_json::Map::new(), text),
        Some(('y', front, body)) => {
            let value: Value = if front.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                serde_yaml::from_str(front)
                    .map_err(|e| invalid(format!("front matter is not YAML: {e}")))?
            };
            match value {
                Value::Object(map) => (map, body),
                Value::Null => (serde_json::Map::new(), body),
                _ => return Err(invalid("front matter must be a mapping".into())),
            }
        }
        Some((_, front, body)) => {
            let table: toml::Table = toml::from_str(front)
                .map_err(|e| invalid(format!("front matter is not TOML: {e}")))?;
            let value = serde_json::to_value(table).map_err(|e| invalid(e.to_string()))?;
            match value {
                Value::Object(map) => (map, body),
                _ => return Err(invalid("front matter must be a table".into())),
            }
        }
    };
    let mut own = serde_json::Map::new();
    let mut extra = BTreeMap::new();
    for (key, value) in map {
        if ITEM_KEYS.contains(&key.as_str()) {
            own.insert(key, value);
        } else {
            extra.insert(key, value);
        }
    }
    let keys: ItemKeys = serde_json::from_value(Value::Object(own))
        .map_err(|e| invalid(format!("front matter: {e}")))?;
    let mut fields = extra;
    fields.extend(keys.fields);
    let body = body.trim_start_matches(['\r', '\n']);
    Ok(ContentItem {
        id: keys.id.unwrap_or(defaults.id),
        kind: keys.kind.unwrap_or(defaults.kind),
        slug: keys.slug.unwrap_or(defaults.slug),
        language: keys.language.unwrap_or(defaults.language),
        fields,
        volatile: keys.volatile,
        body: (!body.trim().is_empty()).then(|| Markdown(body.to_string())),
        media: keys.media,
        sources: keys.sources,
        schedule: keys.schedule,
        state: keys.state.unwrap_or(ContentState::Draft),
    })
}

/// Writes an item as a file: YAML front matter, then the body.
pub(crate) fn render_item(item: &ContentItem) -> Result<String> {
    let keys = ItemKeys {
        id: Some(item.id.clone()),
        kind: Some(item.kind),
        slug: Some(item.slug.clone()),
        language: Some(item.language.clone()),
        state: Some(item.state),
        volatile: item.volatile.clone(),
        sources: item.sources.clone(),
        schedule: item.schedule.clone(),
        media: item.media.clone(),
        fields: item.fields.clone(),
    };
    let yaml = serde_yaml::to_string(&keys).map_err(|e| SiteError::serde("front matter", e))?;
    let mut text = format!("---\n{yaml}---\n");
    if let Some(Markdown(body)) = &item.body {
        text.push('\n');
        text.push_str(body);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> ItemDefaults {
        ItemDefaults {
            id: "posts/hello/en".into(),
            kind: ContentKind::Post,
            slug: "hello".into(),
            language: "en".parse().unwrap(),
        }
    }

    #[test]
    fn yaml_front_matter_splits_item_keys_from_fields() {
        let text = "---\ntitle: Hello\nstate: published\nvolatile: [stock]\nprice: 10\nfields:\n  sku: A1\n---\n\n# Body\n";
        let item = parse_item("posts/hello/en.md", text, defaults()).unwrap();
        assert_eq!(item.state, ContentState::Published);
        assert_eq!(item.fields["title"], "Hello");
        assert_eq!(item.fields["sku"], "A1");
        assert_eq!(item.fields["price"], 10);
        assert_eq!(item.volatile, vec!["stock".to_string()]);
        assert_eq!(item.body, Some(Markdown("# Body\n".into())));
        assert_eq!(item.id, ContentId::from("posts/hello/en"));
    }

    #[test]
    fn toml_front_matter_and_no_front_matter_parse() {
        let item = parse_item("x.md", "+++\ntitle = \"T\"\n+++\nbody", defaults()).unwrap();
        assert_eq!(item.fields["title"], "T");
        let bare = parse_item("x.md", "just text", defaults()).unwrap();
        assert_eq!(bare.state, ContentState::Draft);
        assert_eq!(bare.body, Some(Markdown("just text".into())));
    }

    #[test]
    fn rendering_round_trips() {
        let text = "---\ntitle: Hello\nstate: published\nsources: [fact-1]\nschedule:\n  publishAt: 2026-10-01T09:00:00Z\n---\nBody text\n";
        let item = parse_item("x.md", text, defaults()).unwrap();
        let again = parse_item("x.md", &render_item(&item).unwrap(), defaults()).unwrap();
        assert_eq!(again, item);
    }

    #[test]
    fn malformed_front_matter_names_the_file() {
        let error = parse_item("posts/bad/en.md", "---\n: : :\n---\n", defaults()).unwrap_err();
        assert!(error.to_string().contains("posts/bad/en.md"));
        let error = parse_item("p.md", "---\nstate: nonsense\n---\n", defaults()).unwrap_err();
        assert!(error.to_string().contains("front matter"));
    }
}
