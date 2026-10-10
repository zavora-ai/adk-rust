use serde::{Deserialize, Serialize};

use crate::types::cache_control_ephemeral::CacheControlEphemeral;
use crate::types::citations_config::CitationsConfig;

/// Parameters for the web fetch tool with dynamic filtering (version 20260209).
///
/// Claude filters fetched content with code before it enters the context window.
/// Supported on Claude Opus 5.5, Opus 5, Opus 4.8, Opus 4.7, Opus 4.6, Sonnet 5.5,
/// Sonnet 5, and Sonnet 4.6; other models use [`crate::WebFetchTool20250910`].
/// Do not declare a code execution tool in the same request. Web fetch only
/// retrieves URLs that already appear in the conversation.
///
/// Responses can contain `server_tool_use` blocks named `code_execution` and
/// `code_execution_tool_result` blocks alongside the `web_fetch_tool_result` blocks.
/// Set [`allowed_callers`](Self::allowed_callers) to `["direct"]` to call the tool
/// directly, without dynamic filtering.
///
/// # Example
///
/// ```
/// use adk_anthropic::{CitationsConfig, ToolUnionParam, WebFetchTool20260209};
///
/// let tool = ToolUnionParam::WebFetch20260209(
///     WebFetchTool20260209::new()
///         .with_citations(CitationsConfig::enabled())
///         .with_max_content_tokens(4_000),
/// );
/// let json = serde_json::to_value(&tool).unwrap();
/// assert_eq!(json["type"], "web_fetch_20260209");
/// assert_eq!(json["citations"]["enabled"], true);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebFetchTool20260209 {
    /// Name of the tool. This is how the tool will be called by the model and in `tool_use` blocks.
    #[serde(default = "default_name")]
    pub name: String,

    /// If provided, only these domains may be fetched.
    ///
    /// Cannot be used alongside `blocked_domains`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,

    /// If provided, these domains will never be fetched.
    ///
    /// Cannot be used alongside `allowed_domains`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_domains: Option<Vec<String>>,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Whether fetched documents carry citations in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<CitationsConfig>,

    /// Maximum number of times the tool can be used in the API request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,

    /// Maximum number of tokens to return from the fetched content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_content_tokens: Option<i32>,

    /// Who may call the tool: `"direct"` for Claude itself, or a code execution tool
    /// version such as `"code_execution_20260120"`.
    ///
    /// The API defaults this version to `["code_execution_20260120"]`, which runs
    /// fetches through dynamic filtering. `["direct"]` turns dynamic filtering off,
    /// which Zero Data Retention and models without programmatic tool calling require.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_callers: Option<Vec<String>>,
}

fn default_name() -> String {
    "web_fetch".to_string()
}

impl WebFetchTool20260209 {
    /// Creates a new `WebFetchTool20260209` with default values.
    pub fn new() -> Self {
        Self {
            name: default_name(),
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            citations: None,
            max_uses: None,
            max_content_tokens: None,
            allowed_callers: None,
        }
    }

    /// Restricts fetches to the given domains and clears `blocked_domains`.
    pub fn with_allowed_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_domains = Some(domains);
        self.blocked_domains = None;
        self
    }

    /// Excludes the given domains from fetches and clears `allowed_domains`.
    pub fn with_blocked_domains(mut self, domains: Vec<String>) -> Self {
        self.blocked_domains = Some(domains);
        self.allowed_domains = None;
        self
    }

    /// Sets the cache control breakpoint for the tool definition.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Enables or disables citations for fetched documents.
    pub fn with_citations(mut self, citations: CitationsConfig) -> Self {
        self.citations = Some(citations);
        self
    }

    /// Sets the maximum number of fetches in the API request.
    pub fn with_max_uses(mut self, max_uses: i32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    /// Sets the maximum number of tokens returned from fetched content.
    pub fn with_max_content_tokens(mut self, max_content_tokens: i32) -> Self {
        self.max_content_tokens = Some(max_content_tokens);
        self
    }

    /// Sets who may call the tool. `["direct"]` turns dynamic filtering off.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_anthropic::WebFetchTool20260209;
    ///
    /// let tool = WebFetchTool20260209::new().with_allowed_callers(["direct"]);
    /// let json = serde_json::to_value(&tool).unwrap();
    /// assert_eq!(json["allowed_callers"], serde_json::json!(["direct"]));
    /// ```
    pub fn with_allowed_callers(
        mut self,
        callers: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_callers = Some(callers.into_iter().map(Into::into).collect());
        self
    }
}

impl Default for WebFetchTool20260209 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_all_parameters() {
        let tool = WebFetchTool20260209::new()
            .with_allowed_domains(vec!["example.com".to_string()])
            .with_citations(CitationsConfig::enabled())
            .with_max_uses(1)
            .with_max_content_tokens(4_000)
            .with_cache_control(CacheControlEphemeral::new());

        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            json!({
                "name": "web_fetch",
                "allowed_domains": ["example.com"],
                "cache_control": {"type": "ephemeral"},
                "citations": {"enabled": true},
                "max_uses": 1,
                "max_content_tokens": 4000
            })
        );
    }

    #[test]
    fn direct_callers_round_trip() {
        let tool = WebFetchTool20260209::new().with_allowed_callers(["direct"]);
        let wire = json!({"name": "web_fetch", "allowed_callers": ["direct"]});
        assert_eq!(serde_json::to_value(&tool).unwrap(), wire);
        assert_eq!(serde_json::from_value::<WebFetchTool20260209>(wire).unwrap(), tool);
    }

    #[test]
    fn deserializes_with_default_name() {
        let tool: WebFetchTool20260209 = serde_json::from_value(json!({
            "blocked_domains": ["spam.example"],
            "citations": {"enabled": false}
        }))
        .unwrap();

        assert_eq!(
            tool,
            WebFetchTool20260209::new()
                .with_blocked_domains(vec!["spam.example".to_string()])
                .with_citations(CitationsConfig::disabled())
        );
    }

    #[test]
    fn allowed_and_blocked_domains_are_mutually_exclusive() {
        let tool = WebFetchTool20260209::new()
            .with_blocked_domains(vec!["blocked.example".to_string()])
            .with_allowed_domains(vec!["allowed.example".to_string()]);
        assert_eq!(tool.allowed_domains, Some(vec!["allowed.example".to_string()]));
        assert_eq!(tool.blocked_domains, None);

        let tool = tool.with_blocked_domains(vec!["blocked.example".to_string()]);
        assert_eq!(tool.allowed_domains, None);
        assert_eq!(tool.blocked_domains, Some(vec!["blocked.example".to_string()]));
    }
}
