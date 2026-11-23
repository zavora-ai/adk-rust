//! What the S17 tests share: the stores, the scoring, a recording of a live run and the
//! model that replays it, and an oracle captioner that reads the warehouse clip's truth.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, Content, DataLocality, InMemorySpendLedger, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, ModelCallScope, RunBudget, UsageMetadata,
};
use adk_memory::{GraphMemoryService, InMemoryMemoryService};
use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseEvent};
use adk_vision::analyst::{AnalystAnswer, VideoChunk};
use adk_vision::{EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, VisionCx};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "warehouse-chunks";

/// The stores, records and context of one run.
pub struct Setup {
    pub artifacts: Arc<InMemoryArtifactService>,
    pub captions: Arc<InMemoryMemoryService>,
    pub graph: Arc<GraphMemoryService>,
    pub audit: InMemoryAuditRecorder,
    pub egress: Arc<InMemoryEgressRecorder>,
    pub budget: Arc<BudgetTracker>,
    pub ledger: Arc<InMemorySpendLedger>,
    pub cx: VisionCx,
}

impl Setup {
    pub async fn new() -> Self {
        let graph = GraphMemoryService::new("sqlite::memory:").await.unwrap();
        graph.migrate().await.unwrap();
        let audit = InMemoryAuditRecorder::new();
        let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
        let ledger = Arc::new(InMemorySpendLedger::default());
        let cx = VisionCx::new()
            .with_scope(
                ModelCallScope::new()
                    .with_budget(budget.clone())
                    .with_spend_ledger(ledger.clone(), "acme"),
            )
            .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());
        Self {
            artifacts: Arc::new(InMemoryArtifactService::new()),
            captions: Arc::new(InMemoryMemoryService::new()),
            graph: Arc::new(graph),
            audit,
            egress: Arc::new(InMemoryEgressRecorder::new()),
            budget,
            ledger,
            cx,
        }
    }

    /// An egress gate that lets frames go to Anthropic's API, recording each release.
    pub fn gate(&self, policy: EgressPolicy) -> EgressGate {
        EgressGate::new(
            policy,
            EgressConfig::default().with_rule("api.anthropic.com", &["image"], EgressAction::Allow),
            self.egress.clone(),
        )
    }

    pub fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|r| r.action == action).collect()
    }
}

/// One question's result: the event, the chunk the answer rests on, and whether that chunk
/// holds the labelled time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scored {
    pub event: String,
    pub at_ms: u64,
    pub cited: Option<(u64, u64)>,
    pub hit: bool,
    pub supported: Option<bool>,
    pub answer: String,
}

pub fn score(event: &WarehouseEvent, answer: &AnalystAnswer) -> Scored {
    let primary: Option<&VideoChunk> = answer.primary();
    Scored {
        event: event.id.clone(),
        at_ms: event.at_ms,
        cited: primary.map(|c| (c.start_ms, c.end_ms)),
        hit: primary.is_some_and(|c| c.contains(event.at_ms)),
        supported: answer.supported,
        answer: answer.answer.clone(),
    }
}

/// `m:ss.s` or `h:mm:ss.s` as milliseconds.
pub fn parse_stamp(text: &str) -> Option<u64> {
    let mut total = 0.0f64;
    for part in text.trim().split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((total * 1_000.0).round() as u64)
}

/// The time range a caption prompt names: "from <start> to <end>: image 1 at …".
pub fn caption_range(prompt: &str) -> Option<(u64, u64)> {
    let rest = prompt.split_once(", from ")?.1;
    let (start, rest) = rest.split_once(" to ")?;
    let end = rest.split_once(": image")?.0;
    Some((parse_stamp(start)?, parse_stamp(end)?))
}

/// The question an expansion prompt asks for a caption of.
pub fn expansion_of(prompt: &str) -> Option<String> {
    let rest = prompt.strip_prefix("A question about a video: ")?;
    Some(rest.split_once("\n\n")?.0.trim().to_string())
}

/// The question an answer prompt asks.
pub fn question_of(prompt: &str) -> Option<String> {
    let rest = prompt.split_once("\n\nQuestion: ")?.1;
    Some(rest.split_once("\n\n")?.0.trim().to_string())
}

pub fn prompt_of(req: &LlmRequest) -> String {
    req.contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(adk_core::Part::text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn respond(text: &str, usage: UsageMetadata) -> LlmResponseStream {
    let mut response = LlmResponse::new(Content::new("model").with_text(text));
    response.usage_metadata = Some(usage);
    response.turn_complete = true;
    Box::pin(futures::stream::iter([Ok(response)]))
}

/// Usage of `input` prompt and `output` candidate tokens.
fn tokens(input: i32, output: i32) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: input,
        candidates_token_count: output,
        total_token_count: input + output,
        ..Default::default()
    }
}

/// A recorded model call.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedCaption {
    pub from_ms: u64,
    pub to_ms: u64,
    pub text: String,
    /// The usage the provider reported, thinking and cache tokens included.
    pub usage: UsageMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedAnswer {
    pub question: String,
    pub text: String,
    /// The usage the provider reported, thinking and cache tokens included.
    pub usage: UsageMetadata,
}

/// A live S17 run: what was asked of the model and what it answered, and the result.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub model: String,
    pub recorded_at: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub sample_every_ms: u64,
    pub chunk_ms: u64,
    pub frames_per_chunk: usize,
    pub top_k: usize,
    pub captions: Vec<RecordedCaption>,
    #[serde(default)]
    pub expansions: Vec<RecordedAnswer>,
    pub answers: Vec<RecordedAnswer>,
    pub scored: Vec<Scored>,
    pub cost_per_hour_micro_usd: Option<u64>,
}

pub fn recording_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video/recorded").join(name)
}

/// Records every call to a model: caption prompts by their time range, answer prompts by
/// their question.
pub struct Recorder {
    pub inner: Arc<dyn Llm>,
    pub recording: Mutex<Recording>,
}

#[async_trait]
impl Llm for Recorder {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        use futures::StreamExt;
        let prompt = prompt_of(&req);
        let mut responses: Vec<adk_core::Result<LlmResponse>> =
            self.inner.generate_content(req, stream).await?.collect().await;
        let mut text = String::new();
        let mut usage = UsageMetadata::default();
        for response in responses.iter().flatten() {
            if !response.partial {
                for part in response.content.iter().flat_map(|c| c.parts.iter()) {
                    if let Some(t) = part.text() {
                        text.push_str(t);
                    }
                }
            }
            if let Some(u) = &response.usage_metadata {
                usage = u.clone();
            }
        }
        let mut recording = self.recording.lock().unwrap();
        if let Some(question) = expansion_of(&prompt) {
            recording.expansions.push(RecordedAnswer { question, text, usage });
        } else if let Some((from_ms, to_ms)) =
            caption_range(&prompt).filter(|_| !prompt.contains("Question:"))
        {
            recording.captions.push(RecordedCaption { from_ms, to_ms, text, usage });
        } else if let Some(question) = question_of(&prompt) {
            recording.answers.push(RecordedAnswer { question, text, usage });
        }
        drop(recording);
        Ok(Box::pin(futures::stream::iter(std::mem::take(&mut responses))))
    }

    fn data_locality(&self) -> DataLocality {
        self.inner.data_locality()
    }
}

/// Replays a recording: a caption prompt gets the recorded caption whose range overlaps its
/// own most, an answer prompt the recorded answer to its question.
pub struct Replay {
    pub recording: Recording,
    pub locality: DataLocality,
}

impl Replay {
    pub fn load(name: &str) -> Self {
        let text = std::fs::read_to_string(recording_path(name)).unwrap();
        Self { recording: serde_json::from_str(&text).unwrap(), locality: DataLocality::Remote }
    }
}

#[async_trait]
impl Llm for Replay {
    fn name(&self) -> &str {
        &self.recording.model
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let prompt = prompt_of(&req);
        if let Some(question) = expansion_of(&prompt) {
            let found = self
                .recording
                .expansions
                .iter()
                .find(|a| a.question == question)
                .unwrap_or_else(|| panic!("no recorded expansion of {question:?}"));
            return Ok(respond(&found.text, found.usage.clone()));
        }
        if let Some(question) = question_of(&prompt) {
            let found = self
                .recording
                .answers
                .iter()
                .find(|a| a.question == question)
                .unwrap_or_else(|| panic!("no recorded answer to {question:?}"));
            return Ok(respond(&found.text, found.usage.clone()));
        }
        let (from, to) =
            caption_range(&prompt).unwrap_or_else(|| panic!("an unexpected prompt: {prompt}"));
        let overlap = |c: &RecordedCaption| c.to_ms.min(to).saturating_sub(c.from_ms.max(from));
        let found = self
            .recording
            .captions
            .iter()
            .max_by_key(|c| overlap(c))
            .filter(|c| overlap(c) > 0)
            .unwrap_or_else(|| panic!("no recorded caption for {from}–{to} ms"));
        Ok(respond(&found.text, found.usage.clone()))
    }

    fn data_locality(&self) -> DataLocality {
        self.locality
    }
}

/// A captioner that knows the clip: a chunk in which an event starts is captioned with that
/// event's description, any other chunk as an empty warehouse. An answer prompt is answered
/// by citing the line whose caption is the asked event's description, and a line it was not
/// shown.
pub struct Oracle {
    pub clip: WarehouseClip,
    pub locality: DataLocality,
    pub calls: Mutex<u64>,
}

#[async_trait]
impl Llm for Oracle {
    fn name(&self) -> &str {
        "qwen3-vl-4b-instruct"
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        *self.calls.lock().unwrap() += 1;
        let prompt = prompt_of(&req);
        if let Some(question) = expansion_of(&prompt) {
            let event = self.clip.events().iter().find(|e| e.question == question).unwrap();
            let caption = serde_json::json!({"caption": capitalised(&event.description)});
            return Ok(respond(&caption.to_string(), tokens(60, 20)));
        }
        if let Some(question) = question_of(&prompt) {
            let event = self.clip.events().iter().find(|e| e.question == question).unwrap();
            let line = prompt
                .lines()
                .find(|l| l.ends_with(&format!(": {}.", capitalised(&event.description))))
                .and_then(|l| l.strip_prefix('[')?.split_once(']')?.0.parse::<u64>().ok());
            let chunks: Vec<u64> = line.into_iter().chain([99]).collect();
            let answer = serde_json::json!({"answer": format!("{}.", event.description), "supported": true, "chunks": chunks});
            return Ok(respond(&answer.to_string(), tokens(400, 30)));
        }
        let (from, to) = caption_range(&prompt).unwrap();
        let starting: Vec<&WarehouseEvent> =
            self.clip.events().iter().filter(|e| (from..to).contains(&e.at_ms)).collect();
        let caption = match starting.as_slice() {
            [] => serde_json::json!({
                "caption": "Racks of cartons stand along the wall of an empty warehouse floor.",
                "objects": ["racks", "cartons"]
            }),
            events => serde_json::json!({
                "caption": events.iter().map(|e| format!("{}.", capitalised(&e.description))).collect::<Vec<_>>().join(" "),
                "objects": events.iter().map(|e| e.id.clone()).collect::<Vec<_>>()
            }),
        };
        Ok(respond(&caption.to_string(), tokens(900, 40)))
    }

    fn data_locality(&self) -> DataLocality {
        self.locality
    }
}

pub fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
