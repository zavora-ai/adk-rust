use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Built-in server tool types with forward-compatible Unknown catch-all.
///
/// Each variant maps to a specific wire name string used by the Anthropic API.
/// The `Unknown(String)` variant captures any unrecognised tool name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTool {
    /// Bash tool version 20241022.
    BashTool20241022,
    /// Bash tool version 20250124.
    BashTool20250124,
    /// Text editor version 20250124.
    TextEditor20250124,
    /// Text editor version 20250429.
    TextEditor20250429,
    /// Text editor version 20250728.
    TextEditor20250728,
    /// Web search version 20250305.
    WebSearch20250305,
    /// Web fetch version 20250124.
    WebFetch20250124,
    /// Code execution version 20250522.
    CodeExecution20250522,
    /// Computer use version 20241022.
    ComputerUse20241022,
    /// Computer use version 20250124.
    ComputerUse20250124,
    /// Memory tool.
    Memory,
    /// Unknown tool type (forward compatibility).
    Unknown(String),
}

impl ServerTool {
    /// Returns the wire name string for this tool.
    pub fn as_str(&self) -> &str {
        match self {
            Self::BashTool20241022 => "bash_20241022",
            Self::BashTool20250124 => "bash_20250124",
            Self::TextEditor20250124 => "text_editor_20250124",
            Self::TextEditor20250429 => "text_editor_20250429",
            Self::TextEditor20250728 => "text_editor_20250728",
            Self::WebSearch20250305 => "web_search_20250305",
            Self::WebFetch20250124 => "web_fetch_20250124",
            Self::CodeExecution20250522 => "code_execution_20250522",
            Self::ComputerUse20241022 => "computer_20241022",
            Self::ComputerUse20250124 => "computer_20250124",
            Self::Memory => "memory",
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl From<&str> for ServerTool {
    fn from(s: &str) -> Self {
        match s {
            "bash_20241022" => Self::BashTool20241022,
            "bash_20250124" => Self::BashTool20250124,
            "text_editor_20250124" => Self::TextEditor20250124,
            "text_editor_20250429" => Self::TextEditor20250429,
            "text_editor_20250728" => Self::TextEditor20250728,
            "web_search_20250305" => Self::WebSearch20250305,
            "web_fetch_20250124" => Self::WebFetch20250124,
            "code_execution_20250522" => Self::CodeExecution20250522,
            "computer_20241022" => Self::ComputerUse20241022,
            "computer_20250124" => Self::ComputerUse20250124,
            "memory" => Self::Memory,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl Serialize for ServerTool {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ServerTool {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from(s.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_known_variants() {
        assert_eq!(
            serde_json::to_string(&ServerTool::BashTool20241022).unwrap(),
            r#""bash_20241022""#
        );
        assert_eq!(serde_json::to_string(&ServerTool::Memory).unwrap(), r#""memory""#);
        assert_eq!(
            serde_json::to_string(&ServerTool::CodeExecution20250522).unwrap(),
            r#""code_execution_20250522""#
        );
    }

    #[test]
    fn serialization_unknown() {
        let tool = ServerTool::Unknown("future_tool_20260101".to_string());
        assert_eq!(serde_json::to_string(&tool).unwrap(), r#""future_tool_20260101""#);
    }

    #[test]
    fn deserialization_known() {
        let tool: ServerTool = serde_json::from_str(r#""web_search_20250305""#).unwrap();
        assert_eq!(tool, ServerTool::WebSearch20250305);
    }

    #[test]
    fn deserialization_unknown() {
        let tool: ServerTool = serde_json::from_str(r#""new_tool_20270101""#).unwrap();
        assert_eq!(tool, ServerTool::Unknown("new_tool_20270101".to_string()));
    }

    #[test]
    fn roundtrip_all_known() {
        let variants = vec![
            ServerTool::BashTool20241022,
            ServerTool::BashTool20250124,
            ServerTool::TextEditor20250124,
            ServerTool::TextEditor20250429,
            ServerTool::TextEditor20250728,
            ServerTool::WebSearch20250305,
            ServerTool::WebFetch20250124,
            ServerTool::CodeExecution20250522,
            ServerTool::ComputerUse20241022,
            ServerTool::ComputerUse20250124,
            ServerTool::Memory,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: ServerTool = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized);
        }
    }
}
