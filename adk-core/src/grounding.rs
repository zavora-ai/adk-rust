//! Sources a run retrieved, and the citations an answer makes to them.
//!
//! A retrieval tool records each source it returned to the model as a [`GroundingSource`] on
//! its event's [`EventActions::grounding_sources`](crate::EventActions::grounding_sources),
//! with [`record_grounding_sources`]. An agent collects the sources its run recorded and binds
//! the markdown links of its final answer that point at one of them into the answer's
//! [`CitationMetadata`](crate::CitationMetadata), with [`bind_markdown_citations`], so a
//! citation is checkable: it names a source the run actually retrieved.

use serde::{Deserialize, Serialize};

use crate::model::CitationSource;
use crate::tool::ToolContext;

/// A source a tool retrieved and gave the model during a run.
///
/// # Example
///
/// ```rust
/// use adk_core::GroundingSource;
///
/// let source = GroundingSource::new("rag_search", "https://example.com/refunds")
///     .with_title("Refund policy")
///     .with_document_id("refunds")
///     .with_chunk_id("refunds_0");
/// assert_eq!(source.uri, "https://example.com/refunds");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GroundingSource {
    /// URI of the source, which a citation names.
    pub uri: String,
    /// Human-readable title of the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Name of the tool that retrieved it.
    pub tool: String,
    /// ID of the document the passage came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    /// ID of the retrieved passage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
}

impl GroundingSource {
    /// A source at `uri` that `tool` retrieved.
    pub fn new(tool: impl Into<String>, uri: impl Into<String>) -> Self {
        Self { uri: uri.into(), title: None, tool: tool.into(), document_id: None, chunk_id: None }
    }

    /// Sets the source's title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the ID of the document the passage came from.
    #[must_use]
    pub fn with_document_id(mut self, document_id: impl Into<String>) -> Self {
        self.document_id = Some(document_id.into());
        self
    }

    /// Sets the ID of the retrieved passage.
    #[must_use]
    pub fn with_chunk_id(mut self, chunk_id: impl Into<String>) -> Self {
        self.chunk_id = Some(chunk_id.into());
        self
    }
}

/// Records `sources` on the actions of the tool call `ctx` belongs to.
///
/// The agent puts them on the call's event, where they stay in the session, and counts them
/// as retrieved by the run when it binds the final answer's citations.
///
/// # Example
///
/// ```rust,ignore
/// use adk_core::{GroundingSource, record_grounding_sources};
///
/// record_grounding_sources(
///     ctx.as_ref(),
///     [GroundingSource::new("rag_search", "https://example.com/refunds")],
/// );
/// ```
pub fn record_grounding_sources(
    ctx: &dyn ToolContext,
    sources: impl IntoIterator<Item = GroundingSource>,
) {
    let mut actions = ctx.actions();
    for source in sources {
        if !actions.grounding_sources.contains(&source) {
            actions.grounding_sources.push(source);
        }
    }
    ctx.set_actions(actions);
}

/// The markdown links in `text` whose target is the URI of one of `sources`, as citations.
///
/// A link is `[label](uri)` or `[label](uri "title")`. Each citation carries the source's
/// title and the link's character offsets in `text`, the convention of
/// [`CitationSource::start_index`]. A link to any other URI is not a citation of a retrieved
/// source and is left out, as is a link whose URI only resembles a source's: URIs match
/// exactly.
///
/// # Example
///
/// ```rust
/// use adk_core::{GroundingSource, bind_markdown_citations};
///
/// let sources = [GroundingSource::new("rag_search", "https://example.com/refunds")
///     .with_title("Refund policy")];
/// let answer = "Refunds take 30 days ([policy](https://example.com/refunds)). \
///               See [elsewhere](https://other.example.com).";
///
/// let citations = bind_markdown_citations(answer, &sources);
/// assert_eq!(citations.len(), 1);
/// assert_eq!(citations[0].uri.as_deref(), Some("https://example.com/refunds"));
/// assert_eq!(citations[0].title.as_deref(), Some("Refund policy"));
/// ```
pub fn bind_markdown_citations(text: &str, sources: &[GroundingSource]) -> Vec<CitationSource> {
    let mut citations = Vec::new();
    let mut search_from = 0;
    while let Some(open) = text[search_from..].find('[').map(|i| search_from + i) {
        search_from = open + 1;
        let Some(close) = text[open + 1..].find("](").map(|i| open + 1 + i) else {
            break;
        };
        // A label holds no bracket, so `[a [b](u)` binds the inner link.
        if text[open + 1..close].contains(['[', ']']) {
            continue;
        }
        let target_start = close + 2;
        let Some(end) = text[target_start..].find(')').map(|i| target_start + i) else {
            continue;
        };
        let target = text[target_start..end].trim();
        let uri = target.split_once(char::is_whitespace).map_or(target, |(uri, _title)| uri);
        let uri = uri.strip_prefix('<').and_then(|uri| uri.strip_suffix('>')).unwrap_or(uri);
        if let Some(source) = sources.iter().find(|source| source.uri == uri) {
            citations.push(CitationSource {
                uri: Some(source.uri.clone()),
                title: source.title.clone(),
                start_index: i32::try_from(text[..open].chars().count()).ok(),
                end_index: i32::try_from(text[..=end].chars().count()).ok(),
                license: None,
                publication_date: None,
            });
            search_from = end + 1;
        }
    }
    citations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<GroundingSource> {
        vec![
            GroundingSource::new("rag_search", "https://example.com/a").with_title("A"),
            GroundingSource::new("rag_search", "https://example.com/b?x=1&y=2"),
        ]
    }

    #[test]
    fn binds_only_links_to_recorded_sources_with_character_offsets() {
        let text = "日本 [first](https://example.com/a) and [no](https://example.com/z) \
                    [second](<https://example.com/a> \"Title\").";
        let citations = bind_markdown_citations(text, &sources());
        let offsets: Vec<(Option<i32>, Option<i32>)> =
            citations.iter().map(|c| (c.start_index, c.end_index)).collect();
        assert_eq!(citations.len(), 2);
        let first_start = "日本 ".chars().count() as i32;
        let first_end = first_start + "[first](https://example.com/a)".chars().count() as i32;
        assert_eq!(offsets[0], (Some(first_start), Some(first_end)));
        assert!(citations.iter().all(|c| c.uri.as_deref() == Some("https://example.com/a")));
        assert!(citations.iter().all(|c| c.title.as_deref() == Some("A")));
    }

    #[test]
    fn a_uri_that_only_resembles_a_source_is_not_bound() {
        for text in [
            "[x](https://example.com/a/)",
            "[x](https://example.com/aa)",
            "[x](HTTPS://example.com/a)",
            "[x] (https://example.com/a)",
            "[x](https://example.com/a",
            "https://example.com/a",
        ] {
            assert!(bind_markdown_citations(text, &sources()).is_empty(), "{text}");
        }
    }

    #[test]
    fn a_nested_bracket_binds_the_inner_link() {
        let citations = bind_markdown_citations("[a [b](https://example.com/a)", &sources());
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].start_index, Some(3));
    }

    #[test]
    fn grounding_sources_serialize_in_camel_case_without_empty_fields() {
        let source = GroundingSource::new("rag_search", "https://example.com/a")
            .with_document_id("doc")
            .with_chunk_id("doc_0");
        assert_eq!(
            serde_json::to_value(&source).unwrap(),
            serde_json::json!({
                "uri": "https://example.com/a",
                "tool": "rag_search",
                "documentId": "doc",
                "chunkId": "doc_0",
            })
        );
    }
}
