//! Turns an agent run's events into A2A task updates, as adk-go's A2A executor does
//! (`server/adka2a/v2`: `processor.go`, `task_artifact.go`, `metadata.go`,
//! `input_required.go`).
//!
//! Every event with content becomes one artifact update carrying all of its parts: text,
//! thoughts (`adk_thought`), files, function calls and responses (`adk_type`), and anything
//! else as JSON data. Each update carries the event's `adk_*` metadata. Long-running calls
//! move out of the artifacts into the `INPUT_REQUIRED` status message, and the first model
//! error fails the task. [`OutputMode`] decides how updates map to artifacts.

use std::collections::{HashMap, HashSet};

use a2a_protocol_types::Part as WirePart;
use a2a_protocol_types::artifact::{Artifact, ArtifactId};
use a2a_protocol_types::task::TaskState;
use adk_core::{Event, Part};
use serde_json::{Map, Value, json};

use super::convert::{
    ADK_PARTIAL_METADATA_KEY, adk_parts_to_wire, error_message_part, tagged_function_call_part,
};
use crate::a2a::processor::{Awaiting, awaiting};

/// The function a held call's confirmation is asked and answered with, as adk-go names it.
pub const REQUEST_CONFIRMATION_FUNCTION: &str = "adk_request_confirmation";

/// `adk_invocation_id`: the invocation an update came from.
pub const ADK_INVOCATION_ID_METADATA_KEY: &str = "adk_invocation_id";
/// `adk_author`: the agent that wrote the event.
pub const ADK_AUTHOR_METADATA_KEY: &str = "adk_author";
/// `adk_branch`: the event's branch in a multi-agent run.
pub const ADK_BRANCH_METADATA_KEY: &str = "adk_branch";
/// `adk_usage_metadata`: token counts, in the Gemini API's camelCase shape.
pub const ADK_USAGE_METADATA_KEY: &str = "adk_usage_metadata";
/// `adk_citation_metadata`: citations, in the Gemini API's shape.
pub const ADK_CITATION_METADATA_KEY: &str = "adk_citation_metadata";
/// `adk_custom_metadata`: the event's own metadata, without ADK's internal markers.
pub const ADK_CUSTOM_METADATA_KEY: &str = "adk_custom_metadata";
/// `adk_error_code`: the model error code of the event.
pub const ADK_ERROR_CODE_METADATA_KEY: &str = "adk_error_code";
/// `adk_escalate`: an event of the run asked to escalate.
pub const ADK_ESCALATE_METADATA_KEY: &str = "adk_escalate";
/// `adk_transfer_to_agent`: the agent the run last transferred to.
pub const ADK_TRANSFER_TO_AGENT_METADATA_KEY: &str = "adk_transfer_to_agent";

/// How a run's output is laid out in artifacts, as adk-go's `OutputMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// One artifact holds the run's answer: each non-partial event appends to it, with
    /// `lastChunk` set as adk-python reads it. Streamed partial events replace a separate
    /// artifact marked `adk_partial`, which is emptied (`{}`, `lastChunk`) when the run
    /// ends. adk-go's default (`artifact-per-run`).
    #[default]
    ArtifactPerRun,
    /// Each event starts a new artifact; an author's partial events build one artifact up
    /// until its non-partial event closes it with `lastChunk` (`artifact-per-event`).
    ArtifactPerEvent,
}

/// One artifact update a run produces, before it is stored and sent.
#[derive(Debug, Clone)]
pub struct ArtifactUpdate {
    /// The artifact, or the chunk of it, the update carries.
    pub artifact: Artifact,
    /// Whether the parts go onto the artifact with the same ID instead of replacing it.
    pub append: bool,
    /// Whether this is the artifact's last chunk.
    pub last_chunk: bool,
    /// The update's metadata (`adk_*` keys).
    pub metadata: Option<Map<String, Value>>,
}

/// The status a run ends on, before it is stored and sent.
#[derive(Debug, Clone)]
pub struct FinalStatus {
    /// The state the task ends in.
    pub state: TaskState,
    /// The status message's parts; none for no message.
    pub parts: Vec<WirePart>,
    /// The update's metadata (`adk_*` keys).
    pub metadata: Map<String, Value>,
}

/// Follows one agent run's events (see the module documentation).
pub(super) struct EventProcessor {
    base_meta: Map<String, Value>,
    maker: ArtifactMaker,
    escalate: bool,
    transfer_to_agent: Option<String>,
    failed: Option<FinalStatus>,
    pending: Vec<WirePart>,
    pending_call_ids: HashSet<String>,
    awaiting_auth: bool,
    /// Each author's text streamed in partial events since its last non-partial one.
    streamed: HashMap<String, String>,
    /// Converts parts instead of [`adk_parts_to_wire`], when set.
    to_wire_part: Option<super::hooks::ToWirePart>,
}

impl EventProcessor {
    /// A processor whose updates carry `base_meta` (`adk_app_name`, `adk_user_id`,
    /// `adk_session_id`).
    pub fn new(mode: OutputMode, base_meta: Map<String, Value>) -> Self {
        Self {
            base_meta,
            maker: ArtifactMaker::new(mode),
            escalate: false,
            transfer_to_agent: None,
            failed: None,
            pending: Vec::new(),
            pending_call_ids: HashSet::new(),
            awaiting_auth: false,
            streamed: HashMap::new(),
            to_wire_part: None,
        }
    }

    /// Converts each part with `converter` instead of the default conversion.
    pub fn with_to_wire_part(mut self, converter: Option<super::hooks::ToWirePart>) -> Self {
        self.to_wire_part = converter;
        self
    }

    /// `parts` of `event` as A2A parts, through the converter when one is set. A converter
    /// error leaves the part out and is logged.
    fn to_wire(&self, event: &Event, parts: &[Part], long_running_ids: &[String]) -> Vec<WirePart> {
        let Some(convert) = &self.to_wire_part else {
            return adk_parts_to_wire(parts, long_running_ids);
        };
        parts
            .iter()
            .filter_map(|part| match convert(event, part) {
                Ok(converted) => converted,
                Err(error) => {
                    tracing::warn!(%error, "A2A part converter failed; part left out");
                    None
                }
            })
            .collect()
    }

    /// The artifact update `event` produces, if it has content to show.
    pub fn process(&mut self, event: &Event) -> Option<ArtifactUpdate> {
        self.escalate |= event.actions.escalate;
        if let Some(agent) = &event.actions.transfer_to_agent
            && !agent.is_empty()
        {
            self.transfer_to_agent = Some(agent.clone());
        }
        self.awaiting_auth |= awaiting(event) == Some(Awaiting::Auth);

        let meta = event_meta(&self.base_meta, event);
        let mut remaining = self.take_pending(event);
        self.join_streamed_text(event, &mut remaining);

        let error_code = event.llm_response.error_code.as_deref().filter(|code| !code.is_empty());
        let error_message =
            event.llm_response.error_message.as_deref().filter(|message| !message.is_empty());
        let parts = self.to_wire(event, &remaining, &event.long_running_tool_ids);
        if (error_code.is_some() || error_message.is_some()) && self.failed.is_none() {
            let message = error_message.unwrap_or_default();
            let text = match error_code {
                Some(code) => format!("llm error response (code {code}): {message:?}"),
                None => format!("llm error response: {message:?}"),
            };
            let mut status_parts = vec![error_message_part(text)];
            status_parts.extend(parts.iter().cloned());
            self.failed = Some(FinalStatus {
                state: TaskState::Failed,
                parts: status_parts,
                metadata: meta.clone(),
            });
        }
        if parts.is_empty() {
            return None;
        }
        Some(self.maker.transform(event, parts, meta))
    }

    /// The update that empties the temporary partial artifact, sent before the final
    /// status when partial events were streamed ([`OutputMode::ArtifactPerRun`]).
    pub fn final_artifact_update(&mut self) -> Option<ArtifactUpdate> {
        self.maker.final_update()
    }

    /// The status the run ends on: the first model error, else the calls waiting on the
    /// caller, else `COMPLETED`. It carries `adk_escalate` and `adk_transfer_to_agent`.
    pub fn final_status(&mut self) -> FinalStatus {
        let mut status = if let Some(failed) = self.failed.take() {
            failed
        } else if !self.pending.is_empty() {
            let state =
                if self.awaiting_auth { TaskState::AuthRequired } else { TaskState::InputRequired };
            FinalStatus { state, parts: std::mem::take(&mut self.pending), metadata: Map::new() }
        } else {
            FinalStatus {
                state: TaskState::Completed,
                parts: Vec::new(),
                metadata: self.base_meta.clone(),
            }
        };
        if self.escalate {
            status.metadata.insert(ADK_ESCALATE_METADATA_KEY.to_string(), Value::Bool(true));
        }
        if let Some(agent) = &self.transfer_to_agent {
            status.metadata.insert(
                ADK_TRANSFER_TO_AGENT_METADATA_KEY.to_string(),
                Value::String(agent.clone()),
            );
        }
        status
    }

    /// Makes a non-partial event that ends streamed deltas carry the whole answer, as an
    /// `LlmAgent` final event does (`content_complete`): an author's text streamed in
    /// partial events goes in front of the final event's text, unless that text already
    /// starts with it.
    fn join_streamed_text(&mut self, event: &Event, parts: &mut Vec<Part>) {
        let text: String = parts.iter().filter_map(|part| part.text()).collect();
        if event.llm_response.partial {
            self.streamed.entry(event.author.clone()).or_default().push_str(&text);
            return;
        }
        let Some(streamed) = self.streamed.remove(&event.author) else { return };
        let complete = event
            .llm_response
            .provider_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("content_complete"))
            .and_then(Value::as_bool)
            == Some(true);
        if complete || streamed.is_empty() || text.starts_with(&streamed) {
            return;
        }
        match parts.iter_mut().find_map(|part| match part {
            Part::Text { text } => Some(text),
            _ => None,
        }) {
            Some(first) => first.insert_str(0, &streamed),
            None => parts.insert(0, Part::Text { text: streamed }),
        }
    }

    /// Moves `event`'s long-running calls, and the interim responses to them, into the
    /// pending status message; returns the parts that stay in the artifact. A tool
    /// confirmation request is pending too.
    fn take_pending(&mut self, event: &Event) -> Vec<Part> {
        // A held call asks its caller to confirm it, as adk-go asks: a long-running
        // `adk_request_confirmation` call that names the original call. It takes the held
        // call's ID, so the answer decides that call.
        if let Some(request) = &event.actions.tool_confirmation {
            let id = request
                .function_call_id
                .clone()
                .unwrap_or_else(|| format!("adk-confirm-{}", uuid::Uuid::new_v4()));
            if self.pending_call_ids.insert(id.clone()) {
                let args = json!({
                    "originalFunctionCall": {
                        "id": request.function_call_id,
                        "name": request.tool_name,
                        "args": request.args,
                    },
                    "toolConfirmation": { "hint": "", "confirmed": false },
                });
                self.pending.push(tagged_function_call_part(
                    REQUEST_CONFIRMATION_FUNCTION,
                    &args,
                    Some(&id),
                    true,
                ));
            }
        }
        let Some(content) = event.content() else { return Vec::new() };
        // A call is long-running when the event lists its id; older events list the tool
        // name instead.
        let listed = |name: &str, id: &str| {
            event.long_running_tool_ids.iter().any(|listed| listed == id || listed == name)
        };
        let mut remaining = Vec::with_capacity(content.parts.len());
        for part in &content.parts {
            match part {
                Part::FunctionCall { id: Some(id), name, .. } if listed(name, id) => {
                    if self.pending_call_ids.insert(id.clone()) {
                        let wire = self.to_wire(
                            event,
                            std::slice::from_ref(part),
                            std::slice::from_ref(id),
                        );
                        self.pending.extend(wire);
                    }
                }
                Part::FunctionResponse { function_response, id: Some(id), .. }
                    if listed(&function_response.name, id)
                        || self.pending_call_ids.contains(id) =>
                {
                    let response_key = format!("response:{id}");
                    if self.pending_call_ids.insert(response_key) {
                        let wire = self.to_wire(event, std::slice::from_ref(part), &[]);
                        self.pending.extend(wire);
                    }
                }
                other => remaining.push(other.clone()),
            }
        }
        remaining
    }
}

/// The metadata of `event`'s update: `base` plus the event's invocation, author and
/// branch, its citations and token usage, its own metadata, and its model error code.
fn event_meta(base: &Map<String, Value>, event: &Event) -> Map<String, Value> {
    let mut meta = base.clone();
    for (key, value) in [
        (ADK_INVOCATION_ID_METADATA_KEY, &event.invocation_id),
        (ADK_AUTHOR_METADATA_KEY, &event.author),
        (ADK_BRANCH_METADATA_KEY, &event.branch),
    ] {
        if !value.is_empty() {
            meta.insert(key.to_string(), Value::String(value.clone()));
        }
    }
    if let Some(citations) = &event.llm_response.citation_metadata {
        let citations: Vec<Value> = citations
            .citation_sources
            .iter()
            .map(|source| {
                let mut citation = Map::new();
                let fields = [
                    ("uri", source.uri.clone().map(Value::String)),
                    ("title", source.title.clone().map(Value::String)),
                    ("startIndex", source.start_index.map(Value::from)),
                    ("endIndex", source.end_index.map(Value::from)),
                    ("license", source.license.clone().map(Value::String)),
                    ("publicationDate", source.publication_date.clone().map(Value::String)),
                ];
                for (key, value) in fields {
                    if let Some(value) = value {
                        citation.insert(key.to_string(), value);
                    }
                }
                Value::Object(citation)
            })
            .collect();
        meta.insert(ADK_CITATION_METADATA_KEY.to_string(), json!({ "citations": citations }));
    }
    if let Some(usage) = &event.llm_response.usage_metadata {
        let mut counts = Map::new();
        counts.insert("promptTokenCount".to_string(), usage.prompt_token_count.into());
        counts.insert("candidatesTokenCount".to_string(), usage.candidates_token_count.into());
        counts.insert("totalTokenCount".to_string(), usage.total_token_count.into());
        if let Some(cached) = usage.cache_read_input_token_count {
            counts.insert("cachedContentTokenCount".to_string(), cached.into());
        }
        if let Some(thoughts) = usage.thinking_token_count {
            counts.insert("thoughtsTokenCount".to_string(), thoughts.into());
        }
        meta.insert(ADK_USAGE_METADATA_KEY.to_string(), Value::Object(counts));
    }
    let custom: Map<String, Value> = event
        .provider_metadata
        .iter()
        .filter(|(key, _)| !key.starts_with("adk.") && !key.starts_with("adk_"))
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    if !custom.is_empty() {
        meta.insert(ADK_CUSTOM_METADATA_KEY.to_string(), Value::Object(custom));
    }
    if let Some(code) = event.llm_response.error_code.as_deref().filter(|code| !code.is_empty()) {
        meta.insert(ADK_ERROR_CODE_METADATA_KEY.to_string(), Value::String(code.to_string()));
    }
    meta
}

/// Lays updates out in artifacts according to the [`OutputMode`].
enum ArtifactMaker {
    PerRun {
        /// The artifact of the run's answer, once one update was sent.
        response_id: Option<String>,
        /// The temporary artifact of streamed partial events.
        partial_id: Option<String>,
    },
    PerEvent {
        /// Each author's artifact still receiving partial events.
        open: HashMap<String, String>,
    },
}

impl ArtifactMaker {
    fn new(mode: OutputMode) -> Self {
        match mode {
            OutputMode::ArtifactPerRun => Self::PerRun { response_id: None, partial_id: None },
            OutputMode::ArtifactPerEvent => Self::PerEvent { open: HashMap::new() },
        }
    }

    fn transform(
        &mut self,
        event: &Event,
        mut parts: Vec<WirePart>,
        meta: Map<String, Value>,
    ) -> ArtifactUpdate {
        match self {
            Self::PerRun { response_id, partial_id } => {
                let partial = event.llm_response.partial;
                let slot = if partial { partial_id } else { response_id };
                let existing = slot.clone();
                let id = slot.get_or_insert_with(new_artifact_id).clone();
                let mut metadata = meta;
                metadata.insert(ADK_PARTIAL_METADATA_KEY.to_string(), Value::Bool(partial));
                if partial {
                    for part in &mut parts {
                        mark_partial(part);
                    }
                }
                let mut artifact = Artifact::new(ArtifactId::new(id), parts);
                if partial {
                    artifact.metadata = Some(json!({ ADK_PARTIAL_METADATA_KEY: true }));
                }
                ArtifactUpdate {
                    artifact,
                    // A partial update replaces the temporary artifact; the answer grows.
                    append: !partial && existing.is_some(),
                    // A finished event's chunk is complete: adk-python reads `lastChunk`
                    // to tell answers from streamed chunks (adk-go reads `adk_partial`).
                    last_chunk: !partial,
                    metadata: Some(metadata),
                }
            }
            Self::PerEvent { open } => {
                let partial = event.llm_response.partial;
                let continuing = open.get(&event.author).cloned();
                let id = continuing.clone().unwrap_or_else(new_artifact_id);
                if partial {
                    open.insert(event.author.clone(), id.clone());
                } else {
                    open.remove(&event.author);
                }
                let mut artifact = Artifact::new(ArtifactId::new(id), parts);
                if !meta.is_empty() {
                    artifact.metadata = Some(Value::Object(meta));
                }
                ArtifactUpdate {
                    artifact,
                    append: continuing.is_some() && partial,
                    last_chunk: !partial,
                    metadata: None,
                }
            }
        }
    }

    fn final_update(&mut self) -> Option<ArtifactUpdate> {
        let Self::PerRun { partial_id, .. } = self else { return None };
        let id = partial_id.take()?;
        let mut reset = WirePart::data(json!({}));
        mark_partial(&mut reset);
        let mut artifact = Artifact::new(ArtifactId::new(id), vec![reset]);
        artifact.metadata = Some(json!({ ADK_PARTIAL_METADATA_KEY: true }));
        let mut metadata = Map::new();
        metadata.insert(ADK_PARTIAL_METADATA_KEY.to_string(), Value::Bool(true));
        Some(ArtifactUpdate { artifact, append: false, last_chunk: true, metadata: Some(metadata) })
    }
}

fn new_artifact_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Marks `part` as part of a streamed partial event.
fn mark_partial(part: &mut WirePart) {
    let mut metadata = match part.metadata.take() {
        Some(Value::Object(metadata)) => metadata,
        _ => Map::new(),
    };
    metadata.insert(ADK_PARTIAL_METADATA_KEY.to_string(), Value::Bool(true));
    part.metadata = Some(Value::Object(metadata));
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, EventActions, FunctionResponseData, UsageMetadata};

    fn event(author: &str, parts: Vec<Part>, partial: bool) -> Event {
        let mut event = Event::new("inv-1");
        event.author = author.to_string();
        event.set_content(Content { role: "model".to_string(), parts });
        event.llm_response.partial = partial;
        event
    }

    fn text(value: &str) -> Part {
        Part::Text { text: value.to_string() }
    }

    fn base() -> Map<String, Value> {
        let mut base = Map::new();
        base.insert("adk_app_name".to_string(), json!("app"));
        base
    }

    fn data(part: &WirePart) -> Option<&Value> {
        match &part.content {
            a2a_protocol_types::PartContent::Data(data) => Some(data),
            _ => None,
        }
    }

    fn texts(update: &ArtifactUpdate) -> Vec<String> {
        update
            .artifact
            .parts
            .iter()
            .filter_map(|part| part.text_content().map(str::to_owned))
            .collect()
    }

    #[test]
    fn one_artifact_per_run_holds_the_answer_and_partials_go_to_a_temporary_one() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerRun, base());
        let first = processor.process(&event("a", vec![text("Hel")], true)).unwrap();
        let second = processor.process(&event("a", vec![text("lo")], true)).unwrap();
        let answer = processor.process(&event("a", vec![text("Hello")], false)).unwrap();
        let more = processor.process(&event("a", vec![text("Bye")], false)).unwrap();
        let reset = processor.final_artifact_update().unwrap();

        assert_eq!(first.artifact.id, second.artifact.id);
        assert!(
            !first.append && !second.append,
            "a partial update replaces the temporary artifact"
        );
        assert_eq!(first.metadata.as_ref().unwrap()[ADK_PARTIAL_METADATA_KEY], true);
        assert_eq!(
            first.artifact.parts[0].metadata.as_ref().unwrap()[ADK_PARTIAL_METADATA_KEY],
            true
        );
        assert_ne!(answer.artifact.id, first.artifact.id);
        assert!(!answer.append && more.append, "the answer grows from its first update");
        assert_eq!(answer.artifact.id, more.artifact.id);
        assert!(answer.last_chunk && more.last_chunk, "a finished event's chunk is complete");
        assert!(!first.last_chunk && !second.last_chunk);
        assert_eq!(answer.metadata.as_ref().unwrap()[ADK_PARTIAL_METADATA_KEY], false);
        assert_eq!(texts(&answer), ["Hello"]);
        assert_eq!(reset.artifact.id, first.artifact.id);
        assert!(reset.last_chunk && !reset.append);
        assert_eq!(data(&reset.artifact.parts[0]), Some(&json!({})));
        assert!(processor.final_artifact_update().is_none());
    }

    #[test]
    fn one_artifact_per_event_closes_an_authors_partials() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerEvent, base());
        let partial = processor.process(&event("a", vec![text("Hel")], true)).unwrap();
        let other = processor.process(&event("b", vec![text("Hi")], false)).unwrap();
        let more = processor.process(&event("a", vec![text("lo")], true)).unwrap();
        let done = processor.process(&event("a", vec![text("Hello")], false)).unwrap();
        let next = processor.process(&event("a", vec![text("Again")], false)).unwrap();

        assert!(!partial.append && !partial.last_chunk);
        assert_ne!(other.artifact.id, partial.artifact.id);
        assert!(other.last_chunk);
        assert_eq!(more.artifact.id, partial.artifact.id);
        assert!(more.append);
        assert_eq!(done.artifact.id, partial.artifact.id);
        assert!(!done.append && done.last_chunk, "a non-partial event replaces the chunks");
        assert_ne!(next.artifact.id, done.artifact.id);
        assert!(done.metadata.is_none());
        assert_eq!(done.artifact.metadata.as_ref().unwrap()[ADK_AUTHOR_METADATA_KEY], "a");
        assert!(processor.final_artifact_update().is_none());
    }

    #[test]
    fn every_part_type_is_emitted_and_tagged() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerRun, base());
        let update = processor
            .process(&event(
                "a",
                vec![
                    Part::Thinking { thinking: "hmm".to_string(), signature: None },
                    text("answer"),
                    Part::FunctionCall {
                        name: "add".to_string(),
                        args: json!({"a": 1}),
                        id: Some("c1".to_string()),
                        thought_signature: None,
                    },
                    Part::FunctionResponse {
                        function_response: FunctionResponseData::new("add", json!({"result": 1})),
                        id: Some("c1".to_string()),
                        annotations: None,
                    },
                ],
                false,
            ))
            .unwrap();
        let parts = &update.artifact.parts;
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].text_content(), Some("hmm"));
        assert_eq!(parts[0].metadata.as_ref().unwrap()["adk_thought"], true);
        assert_eq!(parts[1].text_content(), Some("answer"));
        assert_eq!(parts[2].metadata.as_ref().unwrap()["adk_type"], "function_call");
        assert_eq!(parts[2].metadata.as_ref().unwrap()["adk_is_long_running"], false);
        assert_eq!(data(&parts[2]).unwrap()["id"], "c1");
        assert_eq!(parts[3].metadata.as_ref().unwrap()["adk_type"], "function_response");
    }

    #[test]
    fn each_update_carries_the_events_metadata() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerRun, base());
        let mut answer = event("writer", vec![text("x")], false);
        answer.branch = "root.writer".to_string();
        answer.llm_response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 3,
            candidates_token_count: 2,
            total_token_count: 5,
            ..Default::default()
        });
        answer.provider_metadata.insert("adk.budget.recorded".to_string(), "true".to_string());
        answer.provider_metadata.insert("trace".to_string(), "t-1".to_string());
        let update = processor.process(&answer).unwrap();
        let meta = update.metadata.unwrap();

        assert_eq!(meta["adk_app_name"], "app");
        assert_eq!(meta[ADK_INVOCATION_ID_METADATA_KEY], "inv-1");
        assert_eq!(meta[ADK_AUTHOR_METADATA_KEY], "writer");
        assert_eq!(meta[ADK_BRANCH_METADATA_KEY], "root.writer");
        assert_eq!(
            meta[ADK_USAGE_METADATA_KEY],
            json!({"promptTokenCount": 3, "candidatesTokenCount": 2, "totalTokenCount": 5})
        );
        assert_eq!(meta[ADK_CUSTOM_METADATA_KEY], json!({"trace": "t-1"}));
    }

    #[test]
    fn long_running_calls_wait_in_the_status_message_not_the_artifact() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerRun, base());
        let mut call = event(
            "a",
            vec![
                text("I need approval."),
                Part::FunctionCall {
                    name: "approve".to_string(),
                    args: json!({}),
                    id: Some("c1".to_string()),
                    thought_signature: None,
                },
            ],
            false,
        );
        call.long_running_tool_ids = vec!["c1".to_string()];
        let interim = event(
            "a",
            vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "approve",
                    json!({"status": "pending"}),
                ),
                id: Some("c1".to_string()),
                annotations: None,
            }],
            false,
        );
        let update = processor.process(&call).unwrap();
        assert!(processor.process(&interim).is_none(), "the interim response waits too");
        let status = processor.final_status();

        assert_eq!(texts(&update), ["I need approval."]);
        assert_eq!(update.artifact.parts.len(), 1);
        assert_eq!(status.state, TaskState::InputRequired);
        assert_eq!(status.parts.len(), 2);
        assert_eq!(status.parts[0].metadata.as_ref().unwrap()["adk_is_long_running"], true);
        assert!(status.metadata.is_empty(), "INPUT_REQUIRED carries only the run's actions");
    }

    #[test]
    fn the_first_model_error_fails_the_run() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerRun, base());
        let mut failed = event("a", vec![text("partial answer")], false);
        failed.llm_response.error_code = Some("SAFETY".to_string());
        failed.llm_response.error_message = Some("blocked".to_string());
        let mut later = event("a", vec![], false);
        later.llm_response.error_message = Some("again".to_string());
        assert!(processor.process(&failed).is_some(), "its parts still reach the artifact");
        processor.process(&later);
        let status = processor.final_status();

        assert_eq!(status.state, TaskState::Failed);
        assert_eq!(
            status.parts[0].text_content(),
            Some(r#"llm error response (code SAFETY): "blocked""#)
        );
        assert_eq!(status.parts[0].metadata.as_ref().unwrap()["adk_is_error_message"], true);
        assert_eq!(status.parts[1].text_content(), Some("partial answer"));
        assert_eq!(status.metadata[ADK_ERROR_CODE_METADATA_KEY], "SAFETY");
    }

    #[test]
    fn the_final_status_carries_escalation_and_the_last_transfer() {
        let mut processor = EventProcessor::new(OutputMode::ArtifactPerRun, base());
        let mut first = event("a", vec![text("x")], false);
        first.actions = EventActions {
            escalate: true,
            transfer_to_agent: Some("b".to_string()),
            ..Default::default()
        };
        let mut second = event("b", vec![text("y")], false);
        second.actions.transfer_to_agent = Some("c".to_string());
        processor.process(&first);
        processor.process(&second);
        let status = processor.final_status();

        assert_eq!(status.state, TaskState::Completed);
        assert!(status.parts.is_empty());
        assert_eq!(status.metadata["adk_app_name"], "app");
        assert_eq!(status.metadata[ADK_ESCALATE_METADATA_KEY], true);
        assert_eq!(status.metadata[ADK_TRANSFER_TO_AGENT_METADATA_KEY], "c");
    }
}
