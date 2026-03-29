use serde::{Deserialize, Serialize};

/// Programmatic tool invocation from code execution.
///
/// This block carries tool invocations issued by Claude from inside a code
/// execution block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgrammaticToolUseBlock {
    /// Unique identifier for this tool use.
    pub id: String,
    /// Name of the tool being invoked.
    pub name: String,
    /// Input parameters for the tool.
    pub input: serde_json::Value,
}

impl ProgrammaticToolUseBlock {
    /// Create a new `ProgrammaticToolUseBlock`.
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        Self { id: id.into(), name: name.into(), input }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization() {
        let block =
            ProgrammaticToolUseBlock::new("tool_1", "calculator", json!({"expression": "2+2"}));
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(
            json,
            json!({"id": "tool_1", "name": "calculator", "input": {"expression": "2+2"}})
        );
    }

    #[test]
    fn deserialization() {
        let json = json!({"id": "tool_2", "name": "search", "input": {"query": "rust"}});
        let block: ProgrammaticToolUseBlock = serde_json::from_value(json).unwrap();
        assert_eq!(block.id, "tool_2");
        assert_eq!(block.name, "search");
    }
}
