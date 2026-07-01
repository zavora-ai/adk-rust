//! The suspend/resume payload, persisted in **session state**.
//!
//! There is no side store. When the agent suspends (a confirmation-gated or
//! long-running tool call), it serializes the live interpreter continuation
//! (Monty snapshot) plus the loop transcript into a [`CodeActCheckpoint`] and
//! writes it to session state under [`PENDING_STATE_KEY`] via an event's
//! `state_delta`. On the next invocation `CodeActAgent::run` reads it back and
//! resumes — the same "save to session, rebuild, continue" model as
//! [`LlmAgent`](crate::LlmAgent), with the snapshot standing in for the part of
//! the continuation that can't be reconstructed from message history.

use adk_core::Content;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Session-state key under which the pending checkpoint is stored.
pub const PENDING_STATE_KEY: &str = "codeact_pending";

/// The tool call a suspended run is waiting on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingToolCall {
    /// The interpreter call id (for correlating the eventual response).
    pub call_id: u64,
    /// The tool the script invoked.
    pub tool: String,
    /// The arguments the tool was called with.
    pub args: Value,
}

/// The result to feed back into a resumed continuation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResolutionRecord {
    /// Return this value from the tool call.
    Value(Value),
    /// Raise an error at the tool call site, with this message.
    Raise(String),
}

/// Why a checkpoint was written, and how to resume from it.
///
/// The first two bracket every inline tool call (a write-ahead log); the last
/// two are external pauses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Disposition {
    /// Written **before** an inline tool runs. On recovery the tool is re-run to
    /// produce the result (safe: only pure script execution preceded it).
    PendingResult,
    /// Written **after** an inline tool returns. On recovery the continuation is
    /// resumed with the stored result — the tool is **not** re-run.
    Resolved(ResolutionRecord),
    /// A confirmation-gated tool is awaiting a human decision (delivered via
    /// `RunConfig::tool_confirmation_decisions` on the next invocation).
    AwaitingConfirmation,
    /// A long-running tool was started; its result arrives out-of-band as a
    /// `FunctionResponse` in the next invocation's message.
    AwaitingCompletion {
        /// The pending handle the tool's `execute()` returned (e.g. a task id).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_handle: Option<Value>,
    },
}

/// A serializable snapshot of a suspended CodeAct run, stored in session state.
///
/// Does not derive `PartialEq` because [`Content`] does not; compare via the
/// serialized form when equality is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeActCheckpoint {
    /// The model turn this run was on.
    pub iteration: u32,
    /// The model <-> observation transcript accumulated so far.
    pub transcript: Vec<Content>,
    /// The serialized interpreter continuation, paused at [`call`](Self::call).
    pub snapshot: Vec<u8>,
    /// The tool call being awaited.
    pub call: PendingToolCall,
    /// Why it suspended / how to resume.
    pub disposition: Disposition,
    /// The tool roster at suspend time, to detect a mismatched resume (the
    /// external-function set is baked into the snapshot).
    pub tool_roster: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checkpoint_round_trips_through_json() {
        let cp = CodeActCheckpoint {
            iteration: 2,
            transcript: vec![Content::new("user").with_text("hi")],
            snapshot: vec![1, 2, 3],
            call: PendingToolCall { call_id: 7, tool: "slow".into(), args: json!({"x": 1}) },
            disposition: Disposition::AwaitingCompletion {
                pending_handle: Some(json!({"task": "t1"})),
            },
            tool_roster: vec!["slow".into()],
        };
        let value = serde_json::to_value(&cp).unwrap();
        let back: CodeActCheckpoint = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(value, serde_json::to_value(&back).unwrap());
    }

    #[test]
    fn dispositions_round_trip() {
        for case in [
            Disposition::PendingResult,
            Disposition::Resolved(ResolutionRecord::Value(json!(1))),
            Disposition::Resolved(ResolutionRecord::Raise("boom".into())),
            Disposition::AwaitingConfirmation,
            Disposition::AwaitingCompletion { pending_handle: Some(json!("h")) },
            Disposition::AwaitingCompletion { pending_handle: None },
        ] {
            let v = serde_json::to_value(&case).unwrap();
            assert_eq!(case, serde_json::from_value(v).unwrap());
        }
    }
}
