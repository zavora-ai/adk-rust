//! Few-shot retrieval provider that injects Example Store hits into requests.

use super::client::{
    ExampleStoreClient, ExamplesArrayFilter, SearchExampleResult, SearchExamplesRequest,
};
use adk_core::{BeforeModelCallback, BeforeModelResult, Content, Part, Result};
use std::sync::Arc;

const DEFAULT_TOP_K: i64 = 5;

/// Retrieves top-k similar examples for the incoming user message and injects
/// them into the request preamble as dynamic few-shot instructions.
///
/// Package the provider as a
/// [`BeforeModelCallback`](adk_core::BeforeModelCallback) via
/// [`into_before_model_callback`](Self::into_before_model_callback) and
/// register it on an agent builder. On every model call the callback takes the
/// text of the most recent `user` content as the search query, calls
/// [`ExampleStoreClient::search_examples`], and prepends the formatted results
/// as a leading `user` content — the same position `LlmAgentBuilder`
/// instructions occupy, so the examples land alongside the system instruction.
///
/// # Example
///
/// ```no_run
/// use adk_tool::example_store::{
///     ExampleStoreClient, ExampleStoreConfig, ExampleStoreProvider,
/// };
/// use std::sync::Arc;
///
/// # fn demo() -> adk_core::Result<()> {
/// let client = Arc::new(ExampleStoreClient::new_with_adc(
///     ExampleStoreConfig::new("my-project", "us-central1", "my-store"),
/// )?);
/// let provider = ExampleStoreProvider::new(client).with_top_k(3);
///
/// // Register on any agent builder that accepts a BeforeModelCallback:
/// // LlmAgentBuilder::new("assistant")
/// //     .model(model)
/// //     .instruction("You are a helpful assistant.")
/// //     .before_model_callback(provider.into_before_model_callback())
/// //     .build()?;
/// # let _callback = provider.into_before_model_callback();
/// # Ok(())
/// # }
/// ```
pub struct ExampleStoreProvider {
    client: Arc<ExampleStoreClient>,
    top_k: i64,
    function_names: Option<ExamplesArrayFilter>,
    fail_open: bool,
}

impl ExampleStoreProvider {
    /// Creates a provider retrieving the default top-5 examples per request.
    pub fn new(client: Arc<ExampleStoreClient>) -> Self {
        Self { client, top_k: DEFAULT_TOP_K, function_names: None, fail_open: false }
    }

    /// Sets how many examples are retrieved per request.
    #[must_use]
    pub fn with_top_k(mut self, top_k: i64) -> Self {
        self.top_k = top_k;
        self
    }

    /// Restricts retrieval to examples matching the function-name filter.
    #[must_use]
    pub fn with_function_names(mut self, function_names: ExamplesArrayFilter) -> Self {
        self.function_names = Some(function_names);
        self
    }

    /// Controls failure behavior inside the callback.
    ///
    /// When `true`, a failed retrieval logs a warning and the model call
    /// proceeds without examples. When `false` (the default), the retrieval
    /// error propagates and fails the model call.
    #[must_use]
    pub fn fail_open(mut self, fail_open: bool) -> Self {
        self.fail_open = fail_open;
        self
    }

    /// Retrieves the top-k examples most similar to `query`.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying
    /// [`ExampleStoreClient::search_examples`] call fails.
    pub async fn retrieve(&self, query: &str) -> Result<Vec<SearchExampleResult>> {
        let mut request = SearchExamplesRequest::by_search_key(query, self.top_k);
        if let Some(function_names) = &self.function_names {
            request = request.with_function_names(function_names.clone());
        }
        Ok(self.client.search_examples(request).await?.results)
    }

    /// Formats search results as a few-shot instruction block.
    ///
    /// Each example renders its conversation and expected responses as
    /// `role: text` lines. Non-text parts are skipped.
    pub fn format_examples(results: &[SearchExampleResult]) -> String {
        let mut block = String::from(
            "The following retrieved examples show how to respond to similar requests:\n",
        );
        for (index, result) in results.iter().enumerate() {
            block.push_str(&format!("\nExample {}:\n", index + 1));
            let contents_example = &result.example.stored_contents_example.contents_example;
            for content in &contents_example.contents {
                append_content_lines(&mut block, content);
            }
            for expected in &contents_example.expected_contents {
                append_content_lines(&mut block, &expected.content);
            }
        }
        block
    }

    /// Packages this provider as a [`BeforeModelCallback`].
    ///
    /// The callback searches with the text of the most recent `user` content
    /// and prepends the formatted results as a leading `user` content. When the
    /// request has no user text or retrieval returns no results, the request
    /// passes through unmodified.
    pub fn into_before_model_callback(self) -> BeforeModelCallback {
        let provider = Arc::new(self);
        Box::new(move |_ctx, mut request| {
            let provider = provider.clone();
            Box::pin(async move {
                let Some(query) = last_user_text(&request.contents) else {
                    return Ok(BeforeModelResult::Continue(request));
                };
                match provider.retrieve(&query).await {
                    Ok(results) => {
                        if !results.is_empty() {
                            let block = Self::format_examples(&results);
                            request.contents.insert(0, Content::new("user").with_text(block));
                        }
                        Ok(BeforeModelResult::Continue(request))
                    }
                    Err(error) if provider.fail_open => {
                        tracing::warn!(
                            error = %error,
                            "example store retrieval failed — continuing without examples"
                        );
                        Ok(BeforeModelResult::Continue(request))
                    }
                    Err(error) => Err(error),
                }
            })
        })
    }
}

fn append_content_lines(block: &mut String, content: &Content) {
    for part in &content.parts {
        if let Part::Text { text } = part {
            block.push_str(&format!("  {}: {text}\n", content.role));
        }
    }
}

fn last_user_text(contents: &[Content]) -> Option<String> {
    contents.iter().rev().find(|content| content.role == "user").and_then(|content| {
        content
            .parts
            .iter()
            .find_map(|part| if let Part::Text { text } = part { Some(text.clone()) } else { None })
    })
}

#[cfg(test)]
mod tests {
    use super::super::client::{ContentsExample, Example, StoredContentsExample};
    use super::*;

    fn result_with_turns(user: &str, model: &str) -> SearchExampleResult {
        SearchExampleResult {
            example: Example::new(StoredContentsExample::new(ContentsExample::new(
                vec![Content::new("user").with_text(user)],
                vec![Content::new("model").with_text(model)],
            ))),
            similarity_score: Some(0.9),
        }
    }

    #[test]
    fn test_format_examples_renders_role_prefixed_turns() {
        let results =
            vec![result_with_turns("What is 2+2?", "4"), result_with_turns("Capital?", "Paris")];
        let block = ExampleStoreProvider::format_examples(&results);
        assert_eq!(
            block,
            "The following retrieved examples show how to respond to similar requests:\n\
             \nExample 1:\n  user: What is 2+2?\n  model: 4\n\
             \nExample 2:\n  user: Capital?\n  model: Paris\n",
        );
    }

    #[test]
    fn test_last_user_text_picks_the_most_recent_user_content() {
        let contents = vec![
            Content::new("user").with_text("instruction preamble"),
            Content::new("model").with_text("hello"),
            Content::new("user").with_text("actual question"),
        ];
        assert_eq!(last_user_text(&contents), Some("actual question".to_string()));
        assert_eq!(last_user_text(&[]), None);
    }
}
