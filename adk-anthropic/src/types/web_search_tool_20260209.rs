use serde::{Deserialize, Serialize};

use crate::types::cache_control_ephemeral::CacheControlEphemeral;
use crate::types::web_search_tool_20250305::UserLocation;

/// Parameters for the web search tool with dynamic filtering (version 20260209).
///
/// Claude filters search results with code before they enter the context window.
/// Supported on Claude Opus 5.5, Opus 5, Opus 4.8, Opus 4.7, Opus 4.6, Sonnet 5.5,
/// Sonnet 5, and Sonnet 4.6; other models use [`crate::WebSearchTool20250305`].
/// Do not declare a code execution tool in the same request.
///
/// Responses can contain `server_tool_use` blocks named `code_execution` and
/// `code_execution_tool_result` blocks alongside the `web_search_tool_result` blocks.
/// **Important:** [`crate::ContentBlock`] has no `code_execution_tool_result`
/// variant, so [`crate::Message`] deserialization fails on those responses.
///
/// # Example
///
/// ```
/// use adk_anthropic::{ToolUnionParam, WebSearchTool20260209};
///
/// let tool = ToolUnionParam::WebSearch20260209(WebSearchTool20260209::new().with_max_uses(3));
/// let json = serde_json::to_value(&tool).unwrap();
/// assert_eq!(json["type"], "web_search_20260209");
/// assert_eq!(json["max_uses"], 3);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchTool20260209 {
    /// Name of the tool. This is how the tool will be called by the model and in `tool_use` blocks.
    #[serde(default = "default_name")]
    pub name: String,

    /// If provided, only these domains will be included in results.
    ///
    /// Cannot be used alongside `blocked_domains`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,

    /// If provided, these domains will never appear in results.
    ///
    /// Cannot be used alongside `allowed_domains`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_domains: Option<Vec<String>>,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Maximum number of times the tool can be used in the API request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,

    /// Parameters for the user's location. Used to provide more relevant search results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<UserLocation>,
}

fn default_name() -> String {
    "web_search".to_string()
}

impl WebSearchTool20260209 {
    /// Creates a new `WebSearchTool20260209` with default values.
    pub fn new() -> Self {
        Self {
            name: default_name(),
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            max_uses: None,
            user_location: None,
        }
    }

    /// Restricts results to the given domains and clears `blocked_domains`.
    pub fn with_allowed_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_domains = Some(domains);
        self.blocked_domains = None;
        self
    }

    /// Excludes results from the given domains and clears `allowed_domains`.
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

    /// Sets the maximum number of searches in the API request.
    pub fn with_max_uses(mut self, max_uses: i32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    /// Sets the approximate user location for localized results.
    pub fn with_user_location(mut self, user_location: UserLocation) -> Self {
        self.user_location = Some(user_location);
        self
    }
}

impl Default for WebSearchTool20260209 {
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
        let tool = WebSearchTool20260209::new()
            .with_allowed_domains(vec!["example.com".to_string()])
            .with_max_uses(2)
            .with_user_location(UserLocation::new().with_city("Nairobi").with_country("KE"))
            .with_cache_control(CacheControlEphemeral::new());

        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            json!({
                "name": "web_search",
                "allowed_domains": ["example.com"],
                "cache_control": {"type": "ephemeral"},
                "max_uses": 2,
                "user_location": {"type": "approximate", "city": "Nairobi", "country": "KE"}
            })
        );
    }

    #[test]
    fn deserializes_with_default_name() {
        let tool: WebSearchTool20260209 =
            serde_json::from_value(json!({"blocked_domains": ["spam.example"]})).unwrap();

        assert_eq!(
            tool,
            WebSearchTool20260209::new().with_blocked_domains(vec!["spam.example".to_string()])
        );
    }

    #[test]
    fn allowed_and_blocked_domains_are_mutually_exclusive() {
        let tool = WebSearchTool20260209::new()
            .with_blocked_domains(vec!["blocked.example".to_string()])
            .with_allowed_domains(vec!["allowed.example".to_string()]);
        assert_eq!(tool.allowed_domains, Some(vec!["allowed.example".to_string()]));
        assert_eq!(tool.blocked_domains, None);

        let tool = tool.with_blocked_domains(vec!["blocked.example".to_string()]);
        assert_eq!(tool.allowed_domains, None);
        assert_eq!(tool.blocked_domains, Some(vec!["blocked.example".to_string()]));
    }
}
