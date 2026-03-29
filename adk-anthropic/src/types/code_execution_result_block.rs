use serde::{Deserialize, Serialize};

/// Code execution tool output block.
///
/// Returned when the code execution tool completes, carrying the output
/// and return code from the execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeExecutionResultBlock {
    /// The output produced by the code execution.
    pub output: String,
    /// The return code from the code execution.
    pub return_code: i32,
}

impl CodeExecutionResultBlock {
    /// Create a new `CodeExecutionResultBlock`.
    pub fn new(output: impl Into<String>, return_code: i32) -> Self {
        Self { output: output.into(), return_code }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization() {
        let block = CodeExecutionResultBlock::new("Hello, world!", 0);
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json, json!({"output": "Hello, world!", "return_code": 0}));
    }

    #[test]
    fn deserialization() {
        let json = json!({"output": "error occurred", "return_code": 1});
        let block: CodeExecutionResultBlock = serde_json::from_value(json).unwrap();
        assert_eq!(block.output, "error occurred");
        assert_eq!(block.return_code, 1);
    }
}
