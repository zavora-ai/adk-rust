//! Token counting API for Anthropic.
//!
//! Provides [`TokenCount`] and the `count_tokens` method on [`AnthropicClient`],
//! wrapping the `POST /v1/messages/count_tokens` endpoint.

use super::client::{AnthropicClient, convert_claudius_error};
use adk_core::{AdkError, LlmRequest};
use claudius::{MessageCountTokensParams, Model};

/// Result of a token counting request.
///
/// Contains the input token count for a given request without generating
/// a response.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
/// use adk_core::LlmRequest;
///
/// let client = AnthropicClient::new(AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5-20250929"))?;
/// let request = LlmRequest::default();
/// let count = client.count_tokens(&request).await?;
/// println!("Input tokens: {}", count.input_tokens);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCount {
    /// The total number of input tokens across messages, system prompt, and tools.
    pub input_tokens: u32,
}

impl AnthropicClient {
    /// Count tokens for a request without generating a response.
    ///
    /// Calls `POST /v1/messages/count_tokens` using the same request
    /// construction as `generate_content`, so system prompt extraction,
    /// multimodal mapping, and tool conversion all apply.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` with structured error context if the
    /// API returns an error, consistent with the messages API.
    pub async fn count_tokens(&self, request: &LlmRequest) -> Result<TokenCount, AdkError> {
        let params = Self::build_message_params(
            &self.model,
            self.max_tokens,
            request,
            self.config.prompt_caching,
            self.config.thinking.as_ref(),
        )?;

        // Build MessageCountTokensParams from the already-constructed message params.
        let mut count_params =
            MessageCountTokensParams::new(params.messages, Model::Custom(self.model.clone()));

        if let Some(system) = params.system {
            count_params = count_params.with_system(system);
        }

        if let Some(tools) = params.tools {
            count_params = count_params.with_tools(tools);
        }

        if let Some(thinking) = params.thinking {
            count_params = count_params.with_thinking(thinking);
        }

        let result =
            self.client.count_tokens(count_params).await.map_err(convert_claudius_error)?;

        Ok(TokenCount { input_tokens: result.input_tokens })
    }
}
