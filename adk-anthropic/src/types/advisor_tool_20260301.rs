use serde::{Deserialize, Serialize};

use crate::types::CacheControlEphemeral;

/// The `anthropic-beta` value a request that declares the advisor tool, or replays its
/// results, must send.
pub const ADVISOR_TOOL_BETA: &str = "advisor-tool-2026-03-01";

/// Claude's advisor tool (`advisor_20260301`, beta): the executor model consults a stronger
/// advisor model mid-generation, inside the same request.
///
/// The advisor sees the executor's whole transcript and runs with no tools. Its result comes
/// back as an `advisor_tool_result` block, and its tokens are reported in `usage.iterations`
/// as `advisor_message` entries billed at the advisor model's rates, not in the top-level
/// usage. The advisor must be at least as capable as the executor; an invalid pair returns
/// `400 invalid_request_error`.
///
/// # Example
///
/// ```
/// use adk_anthropic::AdvisorTool20260301;
///
/// let tool = AdvisorTool20260301::new("claude-sonnet-5-5").with_max_uses(2).with_max_tokens(2_048);
/// let json = serde_json::to_value(&tool).unwrap();
/// assert_eq!(json["name"], "advisor");
/// assert_eq!(json["model"], "claude-sonnet-5-5");
/// assert_eq!(json["max_uses"], 2);
/// assert_eq!(json["max_tokens"], 2_048);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdvisorTool20260301 {
    /// Name of the tool; the API requires `advisor`.
    #[serde(default = "default_name")]
    pub name: String,

    /// The advisor model, billed at its own rates for each consultation.
    pub model: String,

    /// Most advisor calls in one turn; past it a call returns a `max_uses_exceeded` result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,

    /// Most output tokens (thinking and text) per advisor call; at least 1,024.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Caches the advisor's own prompt across calls in a conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caching: Option<CacheControlEphemeral>,

    /// Create a cache control breakpoint at this tool definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

fn default_name() -> String {
    "advisor".to_string()
}

impl AdvisorTool20260301 {
    /// An advisor tool that consults `model`, with no caps.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            name: default_name(),
            model: model.into(),
            max_uses: None,
            max_tokens: None,
            caching: None,
            cache_control: None,
        }
    }

    /// Caps the advisor calls in one turn.
    pub fn with_max_uses(mut self, max_uses: u32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    /// Caps each advisor call's output tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Caches the advisor's prompt across calls, for conversations with several consultations.
    pub fn with_caching(mut self, caching: CacheControlEphemeral) -> Self {
        self.caching = Some(caching);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CacheTtl, ToolUnionParam};
    use serde_json::json;

    #[test]
    fn the_declaration_round_trips_through_the_tool_union() {
        let tool = ToolUnionParam::Advisor20260301(
            AdvisorTool20260301::new("claude-opus-5-5")
                .with_max_uses(3)
                .with_max_tokens(4_096)
                .with_caching(CacheControlEphemeral::new().with_ttl(CacheTtl::one_hour())),
        );
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "type": "advisor_20260301",
                "name": "advisor",
                "model": "claude-opus-5-5",
                "max_uses": 3,
                "max_tokens": 4_096,
                "caching": { "type": "ephemeral", "ttl": "1h" },
            })
        );
        assert_eq!(serde_json::from_value::<ToolUnionParam>(json).unwrap(), tool);
        assert!(!tool.is_strict());
    }

    fn betas(params: &crate::types::MessageCreateParams) -> String {
        let client = crate::Anthropic::new(Some("api-key".into())).unwrap();
        let headers = client.message_headers_for_request(params, false).unwrap();
        headers
            .get("anthropic-beta")
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn the_beta_is_sent_while_the_tool_is_declared_or_its_results_are_replayed() {
        use crate::types::{ContentBlock, MessageCreateParams, MessageParam, MessageRole};

        let mut params = MessageCreateParams::default();
        assert!(!betas(&params).contains(ADVISOR_TOOL_BETA));

        params.tools = Some(vec![ToolUnionParam::new_advisor_tool("claude-sonnet-5-5")]);
        assert!(betas(&params).contains(ADVISOR_TOOL_BETA));

        // The tool removed, its result still in the history.
        params.tools = None;
        let result: ContentBlock = serde_json::from_value(json!({
            "type": "advisor_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": { "type": "advisor_redacted_result", "encrypted_content": "abc" }
        }))
        .unwrap();
        params.messages = vec![
            MessageParam::user("hello"),
            MessageParam::new_with_blocks(vec![result], MessageRole::Assistant),
        ];
        assert!(params.requires_advisor_beta());
        assert!(betas(&params).contains(ADVISOR_TOOL_BETA));
    }
}
