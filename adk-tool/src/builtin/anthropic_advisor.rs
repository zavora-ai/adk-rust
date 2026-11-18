use adk_core::{AdkError, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Claude's advisor tool (`advisor_20260301`, beta), which Anthropic runs inside the request.
///
/// The executor model (the agent's `AnthropicClient`) calls the advisor when it decides to;
/// Anthropic runs the advisor model on the executor's transcript, with no tools, and the
/// executor continues with the advice. Nothing runs locally.
///
/// | Guarantee | How |
/// |-----------|-----|
/// | Governed | A built-in tool: the run's tool policy decides before the request whether it is sent, and a denied or held tool is withheld |
/// | Metered | Each advisor call is billed at the advisor model's rates; `AnthropicClient` adds them to the response's cost, so the run budget and the spend ledger count them |
/// | Bounded | Under a spend or cost cap, set [`max_uses`](Self::with_max_uses): an advisor without it cannot be bounded, so a capped run refuses the request |
///
/// The advisor must be at least as capable as the executor (for example `claude-haiku-5-5`
/// with a `claude-sonnet-5-5` advisor); current advisor models return their advice encrypted,
/// readable only by the executor. For an advisor of any provider whose advice the agent reads,
/// use the provider-neutral `AdvisorTool`.
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
/// use adk_tool::AnthropicAdvisorTool;
///
/// let advisor = AnthropicAdvisorTool::new("claude-sonnet-5-5").with_max_uses(2).with_max_tokens(2_048);
/// let declaration = advisor.declaration();
/// assert_eq!(declaration["x-adk-anthropic-tool"]["type"], "advisor_20260301");
/// assert_eq!(declaration["x-adk-anthropic-tool"]["model"], "claude-sonnet-5-5");
/// assert_eq!(declaration["x-adk-anthropic-tool"]["max_uses"], 2);
/// ```
#[derive(Debug, Clone)]
pub struct AnthropicAdvisorTool {
    model: String,
    max_uses: Option<u32>,
    max_tokens: Option<u32>,
    caching_ttl: Option<&'static str>,
}

impl AnthropicAdvisorTool {
    /// An advisor tool that consults `model`.
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into(), max_uses: None, max_tokens: None, caching_ttl: None }
    }

    /// Caps the advisor calls in one turn.
    pub fn with_max_uses(mut self, max_uses: u32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    /// Caps each advisor call's output tokens (thinking and text); at least 1,024.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Caches the advisor's prompt across calls for an hour (`true`) or five minutes
    /// (`false`); worth it from about three consultations per conversation.
    pub fn with_caching(mut self, one_hour: bool) -> Self {
        self.caching_ttl = Some(if one_hour { "1h" } else { "5m" });
        self
    }
}

#[async_trait]
impl Tool for AnthropicAdvisorTool {
    fn name(&self) -> &str {
        "advisor"
    }

    fn description(&self) -> &str {
        "Consults a stronger Claude model for guidance mid-task (server-side)."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    // Runs on the provider; a policy sees this effect before the tool is attached.
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn declaration(&self) -> Value {
        let mut tool = json!({
            "type": "advisor_20260301",
            "name": "advisor",
            "model": self.model,
        });
        if let Some(max_uses) = self.max_uses {
            tool["max_uses"] = json!(max_uses);
        }
        if let Some(max_tokens) = self.max_tokens {
            tool["max_tokens"] = json!(max_tokens);
        }
        if let Some(ttl) = self.caching_ttl {
            tool["caching"] = json!({ "type": "ephemeral", "ttl": ttl });
        }
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-anthropic-tool": tool,
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(AdkError::tool(
            "the advisor tool runs on Anthropic's servers, inside the model request",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declaration_carries_every_configured_field() {
        let tool = AnthropicAdvisorTool::new("claude-opus-5-5")
            .with_max_uses(3)
            .with_max_tokens(4_096)
            .with_caching(true);
        assert_eq!(
            tool.declaration(),
            json!({
                "name": "advisor",
                "description": "Consults a stronger Claude model for guidance mid-task (server-side).",
                "x-adk-anthropic-tool": {
                    "type": "advisor_20260301",
                    "name": "advisor",
                    "model": "claude-opus-5-5",
                    "max_uses": 3,
                    "max_tokens": 4_096,
                    "caching": { "type": "ephemeral", "ttl": "1h" },
                },
            })
        );
        assert!(tool.is_builtin());
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
    }
}
