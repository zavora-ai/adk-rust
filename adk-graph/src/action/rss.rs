//! RSS/Feed action node executor (requires `action-rss` feature).
//!
//! Fetches a feed URL via `reqwest`, performs basic RSS/Atom XML parsing
//! to extract items (title, link, description), applies keyword filters,
//! and implements seen-item tracking in state.
//!
//! Note: The `feed-rs` crate is not yet a dependency, so we do manual
//! XML string parsing for basic RSS and Atom feed structures.

use std::collections::HashSet;

use adk_action::{RssNodeConfig, interpolate_variables};
use serde_json::{Value, json};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute an RSS action node.
pub async fn execute_rss(config: &RssNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;
    let state = &ctx.state;

    // Interpolate feed URL
    let feed_url = interpolate_variables(&config.feed_url, state);
    tracing::debug!(node = %node_id, feed_url = %feed_url, "fetching RSS feed");

    // Fetch the feed
    let client = reqwest::Client::new();
    let response =
        client.get(&feed_url).header("User-Agent", "adk-graph-rss/1.0").send().await.map_err(
            |e| GraphError::NodeExecutionFailed {
                node: node_id.clone(),
                message: format!("failed to fetch feed: {e}"),
            },
        )?;

    if !response.status().is_success() {
        return Err(GraphError::NodeExecutionFailed {
            node: node_id.clone(),
            message: format!("feed returned HTTP {}", response.status().as_u16()),
        });
    }

    let body = response.text().await.map_err(|e| GraphError::NodeExecutionFailed {
        node: node_id.clone(),
        message: format!("failed to read feed body: {e}"),
    })?;

    // Parse feed items from XML
    let items = parse_feed_items(&body);

    // Load seen items from state for tracking
    let mut seen_ids = load_seen_ids(config, state);

    // Filter items
    let filtered: Vec<Value> = items
        .into_iter()
        .filter(|item| {
            // Skip seen items
            if let Some(link) = item["link"].as_str() {
                if seen_ids.contains(link) {
                    return false;
                }
            }
            // Apply keyword filter
            if let Some(filter) = &config.filter {
                if !filter.keywords.is_empty() {
                    let title = item["title"].as_str().unwrap_or("");
                    let description = item["description"].as_str().unwrap_or("");
                    let text = format!("{title} {description}").to_lowercase();
                    if !filter.keywords.iter().any(|kw| text.contains(&kw.to_lowercase())) {
                        return false;
                    }
                }
            }
            true
        })
        .collect();

    // Update seen items
    let mut output = NodeOutput::new();
    if let Some(tracking) = &config.seen_tracking {
        if tracking.enabled {
            for item in &filtered {
                if let Some(link) = item["link"].as_str() {
                    seen_ids.insert(link.to_string());
                }
            }
            // Cap at max_items
            let max = tracking.max_items.unwrap_or(1000) as usize;
            let seen_vec: Vec<String> = if seen_ids.len() > max {
                seen_ids.into_iter().take(max).collect()
            } else {
                seen_ids.into_iter().collect()
            };
            let state_key = tracking.state_key.as_deref().unwrap_or("rss_seen_items");
            output = output.with_update(state_key, json!(seen_vec));
        }
    }

    let result = json!({
        "feed_url": feed_url,
        "item_count": filtered.len(),
        "items": filtered,
    });

    output = output.with_update(output_key, result);
    Ok(output)
}

/// Load previously seen item IDs from state.
fn load_seen_ids(
    config: &RssNodeConfig,
    state: &std::collections::HashMap<String, Value>,
) -> HashSet<String> {
    let mut seen = HashSet::new();
    if let Some(tracking) = &config.seen_tracking {
        if tracking.enabled {
            let state_key = tracking.state_key.as_deref().unwrap_or("rss_seen_items");
            if let Some(Value::Array(arr)) = state.get(state_key) {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        seen.insert(s.to_string());
                    }
                }
            }
        }
    }
    seen
}

/// Parse feed items from raw XML content.
///
/// Supports basic RSS 2.0 (`<item>`) and Atom (`<entry>`) structures.
/// Extracts title, link, and description/summary from each item.
fn parse_feed_items(xml: &str) -> Vec<Value> {
    let mut items = Vec::new();

    // Try RSS 2.0 format first (<item> tags)
    let rss_items = extract_elements(xml, "item");
    if !rss_items.is_empty() {
        for item_xml in rss_items {
            let title = extract_text(&item_xml, "title").unwrap_or_default();
            let link = extract_text(&item_xml, "link").unwrap_or_default();
            let description = extract_text(&item_xml, "description").unwrap_or_default();
            items.push(json!({
                "title": title,
                "link": link,
                "description": description,
            }));
        }
        return items;
    }

    // Try Atom format (<entry> tags)
    let atom_entries = extract_elements(xml, "entry");
    for entry_xml in atom_entries {
        let title = extract_text(&entry_xml, "title").unwrap_or_default();
        // Atom uses <link href="..."/> attribute
        let link = extract_link_href(&entry_xml).unwrap_or_default();
        let description = extract_text(&entry_xml, "summary")
            .or_else(|| extract_text(&entry_xml, "content"))
            .unwrap_or_default();
        items.push(json!({
            "title": title,
            "link": link,
            "description": description,
        }));
    }

    items
}

/// Extract all occurrences of `<tag>...</tag>` from XML.
fn extract_elements(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut results = Vec::new();
    let mut search_from = 0;

    while let Some(start) = xml[search_from..].find(&open) {
        let abs_start = search_from + start;
        if let Some(end) = xml[abs_start..].find(&close) {
            let abs_end = abs_start + end + close.len();
            results.push(xml[abs_start..abs_end].to_string());
            search_from = abs_end;
        } else {
            break;
        }
    }

    results
}

/// Extract text content from `<tag>text</tag>`.
fn extract_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let open_with_attrs = format!("<{tag} ");
    let close = format!("</{tag}>");

    // Try simple <tag>content</tag>
    if let Some(start) = xml.find(&open) {
        let content_start = start + open.len();
        if let Some(end) = xml[content_start..].find(&close) {
            let text = &xml[content_start..content_start + end];
            return Some(decode_xml_entities(text.trim()));
        }
    }

    // Try <tag attr="...">content</tag>
    if let Some(start) = xml.find(&open_with_attrs) {
        let after_open = &xml[start..];
        if let Some(gt) = after_open.find('>') {
            let content_start = start + gt + 1;
            if let Some(end) = xml[content_start..].find(&close) {
                let text = &xml[content_start..content_start + end];
                return Some(decode_xml_entities(text.trim()));
            }
        }
    }

    None
}

/// Extract href from `<link href="..." .../>` (Atom format).
fn extract_link_href(xml: &str) -> Option<String> {
    let link_start = xml.find("<link ")?;
    let after_link = &xml[link_start..];
    let tag_end = after_link.find('>')?;
    let tag = &after_link[..tag_end];

    // Find href="..."
    let href_start = tag.find("href=\"")?;
    let value_start = href_start + 6;
    let value_end = tag[value_start..].find('"')?;
    Some(tag[value_start..value_start + value_end].to_string())
}

/// Decode common XML entities.
fn decode_xml_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rss_items() {
        let xml = r#"
        <rss version="2.0">
          <channel>
            <title>Test Feed</title>
            <item>
              <title>First Post</title>
              <link>https://example.com/1</link>
              <description>First description</description>
            </item>
            <item>
              <title>Second Post</title>
              <link>https://example.com/2</link>
              <description>Second description</description>
            </item>
          </channel>
        </rss>"#;

        let items = parse_feed_items(xml);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["title"], "First Post");
        assert_eq!(items[0]["link"], "https://example.com/1");
        assert_eq!(items[1]["title"], "Second Post");
    }

    #[test]
    fn test_parse_atom_entries() {
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom">
          <title>Test Feed</title>
          <entry>
            <title>Atom Entry</title>
            <link href="https://example.com/atom/1" rel="alternate"/>
            <summary>Atom summary</summary>
          </entry>
        </feed>"#;

        let items = parse_feed_items(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["title"], "Atom Entry");
        assert_eq!(items[0]["link"], "https://example.com/atom/1");
        assert_eq!(items[0]["description"], "Atom summary");
    }

    #[test]
    fn test_decode_xml_entities() {
        assert_eq!(decode_xml_entities("a &amp; b"), "a & b");
        assert_eq!(decode_xml_entities("&lt;tag&gt;"), "<tag>");
    }

    #[test]
    fn test_extract_text_with_cdata() {
        let xml = "<item><title>Hello &amp; World</title></item>";
        assert_eq!(extract_text(xml, "title"), Some("Hello & World".to_string()));
    }
}
