//! The citations of an agent's final answer.

use adk_core::{CitationMetadata, Content, GroundingSource, Part, bind_markdown_citations};

/// The citations of a final answer: the provider's, then each markdown link of the answer's
/// text to a source the run retrieved, with character offsets over the answer's text parts
/// joined in order.
///
/// `None` when there are neither.
pub(crate) fn answer_citations(
    content: &Content,
    retrieved: &[GroundingSource],
    provider: Option<&CitationMetadata>,
) -> Option<CitationMetadata> {
    let mut citations =
        provider.map(|metadata| metadata.citation_sources.clone()).unwrap_or_default();
    if !retrieved.is_empty() {
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        for citation in bind_markdown_citations(&text, retrieved) {
            if !citations.contains(&citation) {
                citations.push(citation);
            }
        }
    }
    (!citations.is_empty()).then_some(CitationMetadata { citation_sources: citations })
}

#[cfg(test)]
mod tests {
    use adk_core::CitationSource;

    use super::*;

    #[test]
    fn keeps_provider_citations_and_adds_links_to_retrieved_sources() {
        let retrieved = [GroundingSource::new("rag_search", "https://example.com/a")];
        let provider = CitationMetadata {
            citation_sources: vec![CitationSource {
                uri: Some("https://search.example/x".to_string()),
                title: None,
                start_index: None,
                end_index: None,
                license: None,
                publication_date: None,
            }],
        };
        let content = Content::new("model")
            .with_text("See ")
            .with_text("[a](https://example.com/a) and [b](https://example.com/b).");

        let citations = answer_citations(&content, &retrieved, Some(&provider)).expect("citations");
        let uris: Vec<_> =
            citations.citation_sources.iter().map(|c| c.uri.as_deref().unwrap()).collect();
        assert_eq!(uris, ["https://search.example/x", "https://example.com/a"]);
        assert_eq!(citations.citation_sources[1].start_index, Some(4));
        assert_eq!(
            answer_citations(&Content::new("model").with_text("none"), &retrieved, None),
            None
        );
    }
}
