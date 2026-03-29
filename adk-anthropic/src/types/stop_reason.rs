use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Reasons why the model stopped generating a response.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model reached the end of a generated turn
    EndTurn,

    /// The response reached the maximum token limit for the response
    MaxTokens,

    /// The model reached a specified stop sequence
    StopSequence,

    /// The model indicated it wants to use a tool
    ToolUse,

    /// The model paused in the middle of a turn
    PauseTurn,

    /// The model refused to respond due to safety or other considerations
    Refusal,

    /// The run was paused (e.g. for human-in-the-loop approval)
    PauseRun,

    /// The model's context window was exceeded
    ModelContextWindowExceeded,
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopReason::EndTurn => write!(f, "end_turn"),
            StopReason::MaxTokens => write!(f, "max_tokens"),
            StopReason::StopSequence => write!(f, "stop_sequence"),
            StopReason::ToolUse => write!(f, "tool_use"),
            StopReason::PauseTurn => write!(f, "pause_turn"),
            StopReason::Refusal => write!(f, "refusal"),
            StopReason::PauseRun => write!(f, "pause_run"),
            StopReason::ModelContextWindowExceeded => {
                write!(f, "model_context_window_exceeded")
            }
        }
    }
}

/// Error returned when parsing an invalid stop reason string.
///
/// This error contains the invalid string value that could not be parsed
/// into a valid `StopReason` variant.
#[derive(Debug)]
pub struct StopReasonParseError {
    /// The invalid string value that could not be parsed.
    pub invalid_value: String,
}

impl fmt::Display for StopReasonParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Unknown stop reason: {}", self.invalid_value)
    }
}

impl std::error::Error for StopReasonParseError {}

impl FromStr for StopReason {
    type Err = StopReasonParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "end_turn" => Ok(StopReason::EndTurn),
            "max_tokens" => Ok(StopReason::MaxTokens),
            "stop_sequence" => Ok(StopReason::StopSequence),
            "tool_use" => Ok(StopReason::ToolUse),
            "pause_turn" => Ok(StopReason::PauseTurn),
            "refusal" => Ok(StopReason::Refusal),
            "pause_run" => Ok(StopReason::PauseRun),
            "model_context_window_exceeded" => Ok(StopReason::ModelContextWindowExceeded),
            _ => Err(StopReasonParseError { invalid_value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let reason = StopReason::EndTurn;
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(json, r#""end_turn""#);

        let reason = StopReason::MaxTokens;
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(json, r#""max_tokens""#);
    }

    #[test]
    fn deserialization() {
        let json = r#""end_turn""#;
        let reason: StopReason = serde_json::from_str(json).unwrap();
        assert_eq!(reason, StopReason::EndTurn);

        let json = r#""stop_sequence""#;
        let reason: StopReason = serde_json::from_str(json).unwrap();
        assert_eq!(reason, StopReason::StopSequence);
    }

    #[test]
    fn display() {
        let reason = StopReason::EndTurn;
        assert_eq!(reason.to_string(), "end_turn");

        let reason = StopReason::MaxTokens;
        assert_eq!(reason.to_string(), "max_tokens");
    }
}
