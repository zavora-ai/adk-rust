//! # Transcript Aggregator
//!
//! State machine that collects streaming deltas (`TextDelta`, `TranscriptDelta`)
//! into complete turns, emitting `AggregatedEvent` values when a turn finalizes
//! (either via `ResponseDone` or interruption by a new `ResponseCreated`).

use std::collections::VecDeque;

use crate::events::ServerEvent;

use super::{AggregatedEvent, CompletedToolCall};

/// Internal accumulator for an in-progress assistant turn.
///
/// Collects text deltas, audio transcript deltas, and tool calls
/// until the turn is finalized (either normally or via interruption).
struct TurnAccumulator {
    /// Provider item ID for delta attribution.
    item_id: String,
    /// Accumulated text output from `TextDelta` events.
    text: String,
    /// Accumulated audio transcript from `TranscriptDelta` events.
    audio_transcript: String,
    /// Tool calls executed during this turn.
    tool_calls: Vec<CompletedToolCall>,
}

impl TurnAccumulator {
    /// Creates a new empty accumulator.
    fn new() -> Self {
        Self {
            item_id: String::new(),
            text: String::new(),
            audio_transcript: String::new(),
            tool_calls: Vec::new(),
        }
    }
}

/// Collects streaming deltas into complete turns.
///
/// The `TranscriptAggregator` is a state machine that processes `ServerEvent`
/// values and emits `AggregatedEvent` values when a turn completes. It handles:
///
/// - Starting a new turn on `ResponseCreated`, or on the first text or transcript delta
///   of a provider that sends no `ResponseCreated` (Gemini Live)
/// - Accumulating text via `TextDelta`
/// - Accumulating audio transcript via `TranscriptDelta`
/// - Finalizing a turn on `ResponseDone`
/// - Handling interrupted turns (new `ResponseCreated` during accumulation)
/// - Collecting the user's input transcript: `InputTranscriptCompleted` completes it, and
///   on a provider that streams only `InputTranscriptDelta`s it completes when the model's
///   next turn starts or ends
///
/// [`process`](Self::process) returns the first event an input produces;
/// [`take_pending`](Self::take_pending) returns the rest, in order.
///
/// # Example
///
/// ```rust,ignore
/// let mut aggregator = TranscriptAggregator::new();
///
/// for event in server_events {
///     if let Some(aggregated) = aggregator.process(&event) {
///         // Handle the completed turn
///     }
///     for aggregated in aggregator.take_pending() {
///         // A user utterance that completed with the same event
///     }
/// }
/// ```
pub struct TranscriptAggregator {
    /// Current accumulating turn (None if idle between turns).
    current_turn: Option<TurnAccumulator>,
    /// The user's input transcript streamed so far, not yet completed.
    user_transcript: String,
    /// Events an input produced beyond the one `process` returned.
    pending: VecDeque<AggregatedEvent>,
}

impl TranscriptAggregator {
    /// Creates a new `TranscriptAggregator` in idle state.
    pub fn new() -> Self {
        Self { current_turn: None, user_transcript: String::new(), pending: VecDeque::new() }
    }

    /// Process a `ServerEvent` and optionally emit an `AggregatedEvent`.
    ///
    /// Returns `Some(AggregatedEvent)` when a turn completes (either normally
    /// via `ResponseDone` or via interruption by a new `ResponseCreated`) or a user
    /// utterance completes. Returns `None` for intermediate events that only accumulate
    /// state. An input that completes more than one event queues the rest for
    /// [`take_pending`](Self::take_pending).
    pub fn process(&mut self, event: &ServerEvent) -> Option<AggregatedEvent> {
        match event {
            ServerEvent::ResponseCreated { .. } => {
                // Finalize previous turn as interrupted if one was in progress
                let interrupted = self.finalize_current(true);
                // The user's utterance ended before the model's turn starts.
                let utterance = self.finalize_user_transcript();
                // Start accumulating a new turn
                self.current_turn = Some(TurnAccumulator::new());
                self.first_of(interrupted, utterance)
            }
            ServerEvent::TextDelta { delta, item_id, .. } => {
                let utterance = self.open_turn_for_delta();
                if let Some(ref mut turn) = self.current_turn {
                    turn.item_id = item_id.clone();
                    turn.text.push_str(delta);
                }
                utterance
            }
            ServerEvent::TranscriptDelta { delta, item_id, .. } => {
                let utterance = self.open_turn_for_delta();
                if let Some(ref mut turn) = self.current_turn {
                    turn.item_id = item_id.clone();
                    turn.audio_transcript.push_str(delta);
                }
                utterance
            }
            ServerEvent::InputTranscriptDelta { delta, .. } => {
                self.user_transcript.push_str(delta);
                None
            }
            ServerEvent::InputTranscriptCompleted { transcript, .. } => {
                self.user_transcript.clear();
                Some(AggregatedEvent::UserUtteranceComplete { transcript: transcript.clone() })
            }
            ServerEvent::ResponseDone { .. } => {
                let turn = self.finalize_current(false);
                let utterance = self.finalize_user_transcript();
                self.first_of(turn, utterance)
            }
            _ => None,
        }
    }

    /// The events the last [`process`](Self::process) produced beyond the one it returned.
    pub fn take_pending(&mut self) -> Vec<AggregatedEvent> {
        self.pending.drain(..).collect()
    }

    /// Opens a turn for a model delta that arrived without `ResponseCreated`, completing
    /// the user's streamed transcript first.
    fn open_turn_for_delta(&mut self) -> Option<AggregatedEvent> {
        if self.current_turn.is_some() {
            return None;
        }
        self.current_turn = Some(TurnAccumulator::new());
        self.finalize_user_transcript()
    }

    fn finalize_user_transcript(&mut self) -> Option<AggregatedEvent> {
        if self.user_transcript.trim().is_empty() {
            self.user_transcript.clear();
            return None;
        }
        let transcript = std::mem::take(&mut self.user_transcript);
        Some(AggregatedEvent::UserUtteranceComplete { transcript })
    }

    /// Returns the first of two events and queues the second.
    fn first_of(
        &mut self,
        first: Option<AggregatedEvent>,
        second: Option<AggregatedEvent>,
    ) -> Option<AggregatedEvent> {
        match (first, second) {
            (Some(first), Some(second)) => {
                self.pending.push_back(second);
                Some(first)
            }
            (first, second) => first.or(second),
        }
    }

    /// Finalize the current turn accumulator and emit a `TurnComplete` event.
    ///
    /// Returns `None` if no turn was in progress.
    fn finalize_current(&mut self, interrupted: bool) -> Option<AggregatedEvent> {
        self.current_turn.take().map(|turn| AggregatedEvent::TurnComplete {
            text: turn.text,
            audio_transcript: turn.audio_transcript,
            tool_calls: turn.tool_calls,
            item_id: turn.item_id,
            interrupted,
        })
    }

    /// Record a completed tool call in the current turn.
    ///
    /// If no turn is currently in progress, the call is silently dropped.
    pub fn record_tool_call(&mut self, call: CompletedToolCall) {
        if let Some(ref mut turn) = self.current_turn {
            turn.tool_calls.push(call);
        }
    }

    /// Process a finalized user transcript from speech recognition.
    ///
    /// Returns a `UserUtteranceComplete` event containing the full transcript.
    pub fn process_user_transcript(&mut self, transcript: &str) -> AggregatedEvent {
        AggregatedEvent::UserUtteranceComplete { transcript: transcript.to_string() }
    }
}

impl Default for TranscriptAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(delta: &str) -> ServerEvent {
        ServerEvent::TextDelta {
            event_id: "e".into(),
            response_id: "r".into(),
            item_id: "item".into(),
            output_index: 0,
            content_index: 0,
            delta: delta.into(),
        }
    }

    fn done() -> ServerEvent {
        ServerEvent::ResponseDone { event_id: "e".into(), response: serde_json::json!({}) }
    }

    fn input(delta: &str) -> ServerEvent {
        ServerEvent::InputTranscriptDelta {
            item_id: "in".into(),
            content_index: 0,
            delta: delta.into(),
        }
    }

    #[test]
    fn a_turn_opens_on_the_first_delta_without_response_created() {
        let mut aggregator = TranscriptAggregator::new();
        assert!(aggregator.process(&text("hel")).is_none());
        assert!(aggregator.process(&text("lo")).is_none());
        match aggregator.process(&done()) {
            Some(AggregatedEvent::TurnComplete { text, interrupted, .. }) => {
                assert_eq!(text, "hello");
                assert!(!interrupted);
            }
            other => panic!("expected a completed turn, got {other:?}"),
        }
    }

    #[test]
    fn streamed_input_transcript_completes_when_the_model_turn_starts() {
        let mut aggregator = TranscriptAggregator::new();
        assert!(aggregator.process(&input("yes ")).is_none());
        assert!(aggregator.process(&input("please")).is_none());
        match aggregator.process(&text("ok")) {
            Some(AggregatedEvent::UserUtteranceComplete { transcript }) => {
                assert_eq!(transcript, "yes please");
            }
            other => panic!("expected the user utterance, got {other:?}"),
        }
        assert!(aggregator.take_pending().is_empty());
        assert!(matches!(aggregator.process(&done()), Some(AggregatedEvent::TurnComplete { .. })));
    }

    #[test]
    fn response_created_queues_the_utterance_behind_the_interrupted_turn() {
        let mut aggregator = TranscriptAggregator::new();
        let created =
            ServerEvent::ResponseCreated { event_id: "e".into(), response: serde_json::json!({}) };
        assert!(aggregator.process(&created).is_none());
        assert!(aggregator.process(&text("partial")).is_none());
        assert!(aggregator.process(&input("stop")).is_none());
        match aggregator.process(&created) {
            Some(AggregatedEvent::TurnComplete { text, interrupted: true, .. }) => {
                assert_eq!(text, "partial")
            }
            other => panic!("expected the interrupted turn, got {other:?}"),
        }
        let pending = aggregator.take_pending();
        assert!(matches!(
            pending.as_slice(),
            [AggregatedEvent::UserUtteranceComplete { transcript }] if transcript == "stop"
        ));
    }

    #[test]
    fn input_transcript_completed_replaces_the_streamed_text() {
        let mut aggregator = TranscriptAggregator::new();
        aggregator.process(&input("ye"));
        let completed = ServerEvent::InputTranscriptCompleted {
            item_id: "in".into(),
            content_index: 0,
            transcript: "yes".into(),
        };
        assert!(matches!(
            aggregator.process(&completed),
            Some(AggregatedEvent::UserUtteranceComplete { transcript }) if transcript == "yes"
        ));
        // The streamed prefix is not reported a second time.
        assert!(aggregator.process(&done()).is_none());
    }
}
