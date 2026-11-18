//! Mid-conversation tool changes (beta `inline-tools-2026-09-15`): `tool_addition` and
//! `tool_removal` blocks in a `role: "system"` message change the tools a conversation offers
//! from that point on, while the `tools` array, and so the cached prefix, stays as first sent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The beta header a request that carries a tool change, or a compaction block that records
/// tool changes, sends.
pub const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

/// The tool a [`ToolAdditionBlock`] or [`ToolRemovalBlock`] names.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::ToolChangeTarget;
///
/// let target = ToolChangeTarget::definition(serde_json::json!({
///     "name": "db_query",
///     "description": "Run a read-only SQL query.",
///     "input_schema": {"type": "object", "properties": {"sql": {"type": "string"}}}
/// }));
/// assert_eq!(serde_json::to_value(&target).unwrap()["type"], "tool_definition");
/// assert_eq!(
///     serde_json::to_value(ToolChangeTarget::reference("db_query")).unwrap(),
///     serde_json::json!({"type": "tool_reference", "name": "db_query"})
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolChangeTarget {
    /// A tool declared in the request's `tools` array, by name.
    ToolReference {
        /// The tool's name.
        name: String,
    },
    /// A tool defined in the message itself: an entry of the `tools` array, as sent there.
    ToolDefinition {
        /// The tool's definition.
        definition: Value,
    },
}

impl ToolChangeTarget {
    /// Names a tool the request's `tools` array declares.
    pub fn reference(name: impl Into<String>) -> Self {
        Self::ToolReference { name: name.into() }
    }

    /// Carries a tool's full definition.
    pub fn definition(definition: Value) -> Self {
        Self::ToolDefinition { definition }
    }
}

/// Offers a tool from this point in the conversation on, or replaces the definition of the tool
/// of the same name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolAdditionBlock {
    /// The tool offered.
    pub tool: ToolChangeTarget,
}

/// Withdraws a tool from this point in the conversation on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRemovalBlock {
    /// The tool withdrawn, by reference.
    pub tool: ToolChangeTarget,
}

#[cfg(test)]
mod tests {
    use crate::types::{ContentBlock, MessageParam, MessageRole};
    use serde_json::json;

    use super::*;

    #[test]
    fn a_tool_change_message_matches_the_documented_shape() {
        let definition = json!({
            "name": "db_query",
            "description": "Run a read-only SQL query against the analytics database.",
            "input_schema": {"type": "object", "properties": {"sql": {"type": "string"}}, "required": ["sql"]}
        });
        let message = MessageParam::new_with_blocks(
            vec![
                ContentBlock::ToolAddition(ToolAdditionBlock {
                    tool: ToolChangeTarget::definition(definition.clone()),
                }),
                ContentBlock::ToolRemoval(ToolRemovalBlock {
                    tool: ToolChangeTarget::reference("get_weather"),
                }),
            ],
            MessageRole::System,
        );
        let wire = json!({
            "role": "system",
            "content": [
                {"type": "tool_addition", "tool": {"type": "tool_definition", "definition": definition}},
                {"type": "tool_removal", "tool": {"type": "tool_reference", "name": "get_weather"}}
            ]
        });
        assert_eq!(serde_json::to_value(&message).unwrap(), wire);
        assert_eq!(serde_json::from_value::<MessageParam>(wire).unwrap(), message);
    }
}
