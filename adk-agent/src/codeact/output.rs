//! The typed value a script returns to the host.
//!
//! A CodeAct script communicates its result by *returning a tagged value* (the
//! runtime's completion value, decoded to JSON). The host classifies it into
//! one of three outcomes:
//!
//! - [`ScriptOutput::Observation`] — fed back to the model for the next turn,
//! - [`ScriptOutput::Error`] — fed back to the model (an opaque message string),
//! - [`ScriptOutput::FinalResult`] — returned to the caller; ends the loop,
//! - [`ScriptOutput::TransferToAgent`] — hands control to another agent; ends
//!   the loop. Only offered to the model when transfer targets exist.
//!
//! Errors are just strings. The framework is language-agnostic: it does not
//! model exception types, tracebacks, or "kinds". Whatever the runtime engine
//! produces — a Python traceback, a JS stack, a shell error — is passed through
//! verbatim, because that is what the LLM expects for that engine.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The typed value a CodeAct script must return.
///
/// Encoded as an internally tagged object keyed on `type`, e.g.
/// `{"type": "final_result", "value": ...}`. This wire shape is
/// language-agnostic; how a script *constructs* it is described to the model by
/// the runtime (see [`RuntimeCapabilities::prompt`](crate::codeact::RuntimeCapabilities)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScriptOutput {
    /// Intermediate information the model wants to inspect before continuing.
    Observation {
        /// Arbitrary JSON payload surfaced back to the model.
        value: Value,
    },
    /// An error the model should react to. Just a message string.
    Error {
        /// The error text, in whatever form the model expects.
        message: String,
    },
    /// The terminal answer. Ends the CodeAct loop.
    FinalResult {
        /// Arbitrary JSON payload returned to the caller.
        value: Value,
    },
    /// Hand control to another agent (a sub-agent, or a target the Runner
    /// supplied via `RunConfig::transfer_targets`). Ends this agent's loop; the
    /// named agent takes over, exactly as a [`LlmAgent`](crate::LlmAgent)
    /// `transfer_to_agent` call would.
    ///
    /// This variant is only described to the model when transfer targets are
    /// available. If the model emits it with an unknown target, the host feeds
    /// the error back instead of transferring.
    TransferToAgent {
        /// The name of the agent to transfer control to.
        agent_name: String,
    },
}

impl ScriptOutput {
    /// Decode a completed script's returned value into a [`ScriptOutput`].
    ///
    /// A value that does not conform to the tagged contract becomes an
    /// [`ScriptOutput::Error`] telling the model to return a proper value next
    /// turn, rather than crashing the run.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_agent::codeact::ScriptOutput;
    /// use serde_json::json;
    ///
    /// let out = ScriptOutput::decode(json!({"type": "final_result", "value": 42}));
    /// assert!(matches!(out, ScriptOutput::FinalResult { .. }));
    /// ```
    pub fn decode(value: Value) -> Self {
        match serde_json::from_value::<ScriptOutput>(value.clone()) {
            Ok(output) => output,
            Err(err) => ScriptOutput::Error {
                message: format!(
                    "script must return an observation/error/final_result value; got {value} ({err})"
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn script_output_round_trips() {
        let cases = [
            ScriptOutput::Observation { value: json!({"rows": 3}) },
            ScriptOutput::FinalResult { value: json!("done") },
            ScriptOutput::Error { message: "boom".to_string() },
            ScriptOutput::TransferToAgent { agent_name: "billing".to_string() },
        ];
        for case in cases {
            let encoded = serde_json::to_value(&case).unwrap();
            let decoded: ScriptOutput = serde_json::from_value(encoded).unwrap();
            assert_eq!(case, decoded);
        }
    }

    #[test]
    fn final_result_wire_shape_is_tagged() {
        let encoded = serde_json::to_value(ScriptOutput::FinalResult { value: json!(7) }).unwrap();
        assert_eq!(encoded, json!({"type": "final_result", "value": 7}));
    }

    #[test]
    fn transfer_wire_shape_is_tagged() {
        let encoded = serde_json::to_value(ScriptOutput::TransferToAgent {
            agent_name: "billing".to_string(),
        })
        .unwrap();
        assert_eq!(encoded, json!({"type": "transfer_to_agent", "agent_name": "billing"}));
    }

    #[test]
    fn decode_rejects_non_variant() {
        let out = ScriptOutput::decode(json!({"not": "a variant"}));
        assert!(matches!(out, ScriptOutput::Error { .. }));
    }
}
