use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

/// Reasons why the model stopped generating a response.
///
/// A reason this client does not know deserializes to
/// [`StopReason::Unknown`] instead of failing the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// A stop reason this client does not know, as the API sent it.
    #[serde(untagged)]
    Unknown(String),
}

impl StopReason {
    /// Returns the wire value, such as `end_turn`.
    pub fn as_str(&self) -> &str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::StopSequence => "stop_sequence",
            StopReason::ToolUse => "tool_use",
            StopReason::PauseTurn => "pause_turn",
            StopReason::Refusal => "refusal",
            StopReason::PauseRun => "pause_run",
            StopReason::ModelContextWindowExceeded => "model_context_window_exceeded",
            StopReason::Unknown(value) => value,
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StopReason {
    type Err = Infallible;

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
            other => Ok(StopReason::Unknown(other.to_string())),
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
    fn unknown_reason_round_trips() {
        let reason: StopReason = serde_json::from_str(r#""future_reason""#).unwrap();
        assert_eq!(reason, StopReason::Unknown("future_reason".to_string()));
        assert_eq!(serde_json::to_string(&reason).unwrap(), r#""future_reason""#);
        assert_eq!(reason.to_string(), "future_reason");
        assert_eq!("future_reason".parse::<StopReason>(), Ok(reason));
    }

    #[test]
    fn as_str_matches_serde_for_known_reasons() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::ToolUse,
            StopReason::PauseTurn,
            StopReason::Refusal,
            StopReason::PauseRun,
            StopReason::ModelContextWindowExceeded,
        ] {
            let wire = serde_json::to_value(&reason).unwrap();
            assert_eq!(wire, reason.as_str());
            assert_eq!(reason.as_str().parse::<StopReason>(), Ok(reason));
        }
    }

    #[test]
    fn display() {
        let reason = StopReason::EndTurn;
        assert_eq!(reason.to_string(), "end_turn");

        let reason = StopReason::MaxTokens;
        assert_eq!(reason.to_string(), "max_tokens");
    }
}
