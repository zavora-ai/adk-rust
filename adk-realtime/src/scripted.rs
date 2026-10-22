//! A provider-free realtime model that answers each client action from a script
//! (feature `scripted`).
//!
//! [`ScriptedRealtimeModel`] plays the part of a provider for tests of the runners, the
//! integration layer and voice surfaces, the way `adk_model::ScriptedLlm` stands in for a
//! chat model: no key, no network, and the same events a provider would send. A script is
//! a list of [`ScriptedStep`]s consumed in order; each names the client action that
//! triggers it and the [`ServerEvent`]s the session then emits. An action with no matching
//! step at the front of the script emits nothing, so a script states exactly what the
//! session says and when.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedStep};
//! use adk_realtime::{RealtimeModel, RealtimeSession};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let model = ScriptedRealtimeModel::new(vec![
//!     ScriptedStep::Text { events: scripted::spoken("Hello there.") },
//! ]);
//! let session = model.connect(Default::default()).await.unwrap();
//! session.send_text("hi").await.unwrap();
//! let mut transcript = String::new();
//! while let Some(Ok(event)) = session.next_event().await {
//!     if let adk_realtime::ServerEvent::TranscriptDelta { delta, .. } = event {
//!         transcript.push_str(&delta);
//!     }
//! }
//! assert_eq!(transcript, "Hello there.");
//! assert_eq!(model.log().texts(), vec!["hi".to_string()]);
//! # });
//! ```

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::audio::{AudioChunk, AudioFormat};
use crate::config::RealtimeConfig;
use crate::error::Result;
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::model::RealtimeModel;
use crate::session::{BoxedSession, ContextMutationOutcome, RealtimeSession};

/// How long a session with nothing queued waits for a client action to trigger the next
/// step before it reports the end of the session.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_millis(500);

/// One step of a script: the client action that triggers it and the events it emits.
///
/// Serializes with an `on` tag, so a script can be written as JSON:
/// `[{"on": "text", "events": [{"type": "response.done", "event_id": "e", "response": {}}]}]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum ScriptedStep {
    /// Emitted when the session connects, before any client action.
    Connect {
        /// The events.
        events: Vec<ServerEvent>,
    },
    /// Emitted when the client sends text.
    Text {
        /// The events.
        events: Vec<ServerEvent>,
    },
    /// Emitted when the client sends audio, on the first chunk after the step is reached.
    Audio {
        /// The events.
        events: Vec<ServerEvent>,
    },
    /// Emitted when the client sends a tool output or tool response.
    ToolOutput {
        /// The events.
        events: Vec<ServerEvent>,
    },
    /// Emitted when the client requests a response.
    Response {
        /// The events.
        events: Vec<ServerEvent>,
    },
}

impl ScriptedStep {
    fn trigger(&self) -> Trigger {
        match self {
            Self::Connect { .. } => Trigger::Connect,
            Self::Text { .. } => Trigger::Text,
            Self::Audio { .. } => Trigger::Audio,
            Self::ToolOutput { .. } => Trigger::ToolOutput,
            Self::Response { .. } => Trigger::Response,
        }
    }

    fn into_events(self) -> Vec<ServerEvent> {
        match self {
            Self::Connect { events }
            | Self::Text { events }
            | Self::Audio { events }
            | Self::ToolOutput { events }
            | Self::Response { events } => events,
        }
    }
}

/// A reply, for scripts built in code: the events one step emits.
///
/// # Example
///
/// ```rust
/// use adk_realtime::scripted::{self, ScriptedReply, ScriptedStep};
///
/// let step: ScriptedStep = ScriptedReply::new(scripted::spoken("ok")).on_text();
/// # let _ = step;
/// ```
#[derive(Debug, Clone, Default)]
pub struct ScriptedReply {
    /// The events the step emits.
    pub events: Vec<ServerEvent>,
}

impl ScriptedReply {
    /// A reply of `events`.
    pub fn new(events: Vec<ServerEvent>) -> Self {
        Self { events }
    }

    /// The reply as a step on connect.
    pub fn on_connect(self) -> ScriptedStep {
        ScriptedStep::Connect { events: self.events }
    }

    /// The reply as a step on text.
    pub fn on_text(self) -> ScriptedStep {
        ScriptedStep::Text { events: self.events }
    }

    /// The reply as a step on audio.
    pub fn on_audio(self) -> ScriptedStep {
        ScriptedStep::Audio { events: self.events }
    }

    /// The reply as a step on a tool output.
    pub fn on_tool_output(self) -> ScriptedStep {
        ScriptedStep::ToolOutput { events: self.events }
    }

    /// The reply as a step on a response request.
    pub fn on_response(self) -> ScriptedStep {
        ScriptedStep::Response { events: self.events }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Connect,
    Text,
    Audio,
    ToolOutput,
    Response,
}

/// What the client sent to a scripted session, for assertions.
#[derive(Debug, Default)]
pub struct ScriptedLog {
    texts: Mutex<Vec<String>>,
    tool_outputs: Mutex<Vec<ToolResponse>>,
    audio_chunks: Mutex<usize>,
    responses_requested: Mutex<usize>,
    interrupts: Mutex<usize>,
    configs: Mutex<Vec<RealtimeConfig>>,
}

impl ScriptedLog {
    /// Every text the client sent, in order.
    pub fn texts(&self) -> Vec<String> {
        self.texts.lock().clone()
    }

    /// Every tool output or response the client sent, in order.
    pub fn tool_outputs(&self) -> Vec<ToolResponse> {
        self.tool_outputs.lock().clone()
    }

    /// How many audio chunks the client sent.
    pub fn audio_chunks(&self) -> usize {
        *self.audio_chunks.lock()
    }

    /// How many responses the client requested.
    pub fn responses_requested(&self) -> usize {
        *self.responses_requested.lock()
    }

    /// How many times the client interrupted a response.
    pub fn interrupts(&self) -> usize {
        *self.interrupts.lock()
    }

    /// The configuration of each session the model connected, in order.
    pub fn configs(&self) -> Vec<RealtimeConfig> {
        self.configs.lock().clone()
    }
}

/// A realtime model that answers from a script instead of a provider.
#[derive(Debug, Clone)]
pub struct ScriptedRealtimeModel {
    script: Arc<Vec<ScriptedStep>>,
    model_id: String,
    idle_timeout: Duration,
    log: Arc<ScriptedLog>,
}

impl ScriptedRealtimeModel {
    /// A model that plays `script`, as `scripted-realtime`.
    pub fn new(script: Vec<ScriptedStep>) -> Self {
        Self {
            script: Arc::new(script),
            model_id: "scripted-realtime".to_string(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            log: Arc::new(ScriptedLog::default()),
        }
    }

    /// Reports `model_id` as the model, so spend is attributed to its vendor.
    #[must_use]
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = model_id.into();
        self
    }

    /// How long a session with nothing queued waits for the next client action before
    /// `next_event` reports the end of the session. Defaults to [`DEFAULT_IDLE_TIMEOUT`].
    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// What the client has sent so far.
    pub fn log(&self) -> Arc<ScriptedLog> {
        self.log.clone()
    }
}

#[async_trait]
impl RealtimeModel for ScriptedRealtimeModel {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn supports_realtime(&self) -> bool {
        true
    }

    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_16khz(), AudioFormat::pcm16_24khz()]
    }

    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }

    fn available_voices(&self) -> Vec<&str> {
        vec!["default"]
    }

    async fn connect(&self, config: RealtimeConfig) -> Result<BoxedSession> {
        self.log.configs.lock().push(config);
        let session = ScriptedSession {
            id: format!("scripted-{}", uuid::Uuid::new_v4()),
            script: Mutex::new(self.script.iter().cloned().collect()),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            closed: Mutex::new(false),
            idle_timeout: self.idle_timeout,
            log: self.log.clone(),
        };
        session.fire(Trigger::Connect);
        Ok(Box::new(session))
    }
}

/// A session over a script.
struct ScriptedSession {
    id: String,
    script: Mutex<VecDeque<ScriptedStep>>,
    queue: Mutex<VecDeque<ServerEvent>>,
    notify: Notify,
    closed: Mutex<bool>,
    idle_timeout: Duration,
    log: Arc<ScriptedLog>,
}

impl ScriptedSession {
    /// Consumes the front step when `trigger` is what it waits for, queueing its events.
    fn fire(&self, trigger: Trigger) {
        let step = {
            let mut script = self.script.lock();
            match script.front() {
                Some(step) if step.trigger() == trigger => script.pop_front(),
                _ => None,
            }
        };
        if let Some(step) = step {
            self.queue.lock().extend(step.into_events());
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }
}

#[async_trait]
impl RealtimeSession for ScriptedSession {
    fn session_id(&self) -> &str {
        &self.id
    }

    fn is_connected(&self) -> bool {
        !*self.closed.lock()
    }

    async fn send_audio(&self, _audio: &AudioChunk) -> Result<()> {
        *self.log.audio_chunks.lock() += 1;
        self.fire(Trigger::Audio);
        Ok(())
    }

    async fn send_audio_base64(&self, _audio_base64: &str) -> Result<()> {
        *self.log.audio_chunks.lock() += 1;
        self.fire(Trigger::Audio);
        Ok(())
    }

    async fn send_text(&self, text: &str) -> Result<()> {
        self.log.texts.lock().push(text.to_string());
        self.fire(Trigger::Text);
        Ok(())
    }

    async fn send_tool_response(&self, response: ToolResponse) -> Result<()> {
        self.log.tool_outputs.lock().push(response);
        self.fire(Trigger::ToolOutput);
        Ok(())
    }

    async fn send_tool_output(&self, response: ToolResponse) -> Result<()> {
        self.log.tool_outputs.lock().push(response);
        self.fire(Trigger::ToolOutput);
        Ok(())
    }

    async fn commit_audio(&self) -> Result<()> {
        self.fire(Trigger::Audio);
        Ok(())
    }

    async fn clear_audio(&self) -> Result<()> {
        Ok(())
    }

    async fn create_response(&self) -> Result<()> {
        *self.log.responses_requested.lock() += 1;
        self.fire(Trigger::Response);
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        *self.log.interrupts.lock() += 1;
        Ok(())
    }

    async fn send_event(&self, _event: ClientEvent) -> Result<()> {
        Ok(())
    }

    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        loop {
            if let Some(event) = self.queue.lock().pop_front() {
                return Some(Ok(event));
            }
            if *self.closed.lock() {
                return None;
            }
            // A scripted session only speaks when a client action triggers a step, so a
            // quiet queue means the client has nothing more to do for now: give an in-flight
            // tool dispatch the idle timeout to answer, then report the end of the session.
            // The session stays open — a later client action still fires its step, and the
            // next call returns what it queued.
            let waited = tokio::time::timeout(self.idle_timeout, self.notify.notified()).await;
            if waited.is_err() && self.queue.lock().is_empty() {
                return None;
            }
        }
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        Box::pin(async_stream::stream! {
            while let Some(event) = self.next_event().await {
                yield event;
            }
        })
    }

    async fn close(&self) -> Result<()> {
        *self.closed.lock() = true;
        self.notify.notify_waiters();
        self.notify.notify_one();
        Ok(())
    }

    async fn mutate_context(&self, _config: RealtimeConfig) -> Result<ContextMutationOutcome> {
        Ok(ContextMutationOutcome::Applied)
    }
}

// ─── Event helpers for scripts ───────────────────────────────────────────────

/// A `ResponseCreated` event.
pub fn response_created() -> ServerEvent {
    ServerEvent::ResponseCreated {
        event_id: format!("evt-{}", uuid::Uuid::new_v4()),
        response: json!({ "id": "resp-scripted" }),
    }
}

/// A `ResponseDone` event carrying `response`, such as a provider usage report.
pub fn response_done(response: Value) -> ServerEvent {
    ServerEvent::ResponseDone { event_id: format!("evt-{}", uuid::Uuid::new_v4()), response }
}

/// A completed function call to `name` with `args`, under `call_id`.
pub fn function_call(call_id: &str, name: &str, args: Value) -> ServerEvent {
    ServerEvent::FunctionCallDone {
        event_id: format!("evt-{}", uuid::Uuid::new_v4()),
        response_id: "resp-scripted".to_string(),
        item_id: format!("item-{call_id}"),
        output_index: 0,
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments: args.to_string(),
    }
}

/// A transcript delta of the model's speech.
pub fn transcript_delta(text: &str) -> ServerEvent {
    ServerEvent::TranscriptDelta {
        event_id: format!("evt-{}", uuid::Uuid::new_v4()),
        response_id: "resp-scripted".to_string(),
        item_id: "item-scripted".to_string(),
        output_index: 0,
        content_index: 0,
        delta: text.to_string(),
    }
}

/// A text delta of the model's answer.
pub fn text_delta(text: &str) -> ServerEvent {
    ServerEvent::TextDelta {
        event_id: format!("evt-{}", uuid::Uuid::new_v4()),
        response_id: "resp-scripted".to_string(),
        item_id: "item-scripted".to_string(),
        output_index: 0,
        content_index: 0,
        delta: text.to_string(),
    }
}

/// The user's completed input transcript.
pub fn input_transcript(text: &str) -> ServerEvent {
    ServerEvent::InputTranscriptCompleted {
        item_id: format!("input-{}", uuid::Uuid::new_v4()),
        content_index: 0,
        transcript: text.to_string(),
    }
}

/// One spoken turn: a response that transcribes as `text` and completes.
pub fn spoken(text: &str) -> Vec<ServerEvent> {
    vec![
        response_created(),
        transcript_delta(text),
        response_done(json!({ "id": "resp-scripted" })),
    ]
}

/// One turn in which the model calls `name` with `args` under `call_id` and the response
/// closes, as a provider does before the tool output arrives.
pub fn calls(call_id: &str, name: &str, args: Value) -> Vec<ServerEvent> {
    vec![
        response_created(),
        function_call(call_id, name, args),
        response_done(json!({ "id": "resp-scripted" })),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn steps_fire_in_order_on_their_triggers() {
        let model = ScriptedRealtimeModel::new(vec![
            ScriptedReply::new(vec![response_created()]).on_connect(),
            ScriptedReply::new(spoken("one")).on_text(),
            ScriptedReply::new(spoken("two")).on_tool_output(),
        ])
        .with_idle_timeout(Duration::from_millis(20));
        let session = model.connect(RealtimeConfig::default()).await.unwrap();
        // A tool output before its turn in the script emits nothing.
        session
            .send_tool_output(ToolResponse { call_id: "c".into(), output: json!({}) })
            .await
            .unwrap();
        session.send_text("go").await.unwrap();
        session
            .send_tool_output(ToolResponse { call_id: "c".into(), output: json!({}) })
            .await
            .unwrap();
        let mut transcript = String::new();
        let mut count = 0;
        while let Some(Ok(event)) = session.next_event().await {
            count += 1;
            if let ServerEvent::TranscriptDelta { delta, .. } = event {
                transcript.push_str(&delta);
            }
        }
        assert_eq!(transcript, "onetwo");
        assert_eq!(count, 1 + 3 + 3);
        assert_eq!(model.log().tool_outputs().len(), 2);
        assert_eq!(model.log().texts(), vec!["go".to_string()]);
    }

    #[test]
    fn a_script_round_trips_through_json() {
        let step =
            ScriptedReply::new(calls("call-1", "bash", json!({ "command": "ls" }))).on_text();
        let text = serde_json::to_string(&[step]).unwrap();
        let parsed: Vec<ScriptedStep> = serde_json::from_str(&text).unwrap();
        assert!(matches!(&parsed[0], ScriptedStep::Text { events } if events.len() == 3));
    }
}
