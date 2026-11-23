//! S17 (video analyst): an hour of generated warehouse video with ten labelled events
//! (`adk_vision::analyst::fixtures::WarehouseClip`) is cut into 30-second chunks; each chunk's
//! six frames go through the egress gate to a captioner; each caption is indexed in an
//! `adk-rag` store, kept in `adk-memory` with its keyframe's artifact, and the clip in graph
//! memory. Ten questions are answered from the captions, each citing chunks with their time
//! ranges, and the captioning's cost per hour of video is reported.
//!
//! Claude Haiku 5.5 captions and answers from adk-vision's live recording
//! (`adk-vision/tests/fixtures/video/recorded/`, written by `analyst_live_tests`), served to
//! the real Anthropic client by a local server that answers each request with the recorded
//! answer to the same chunk or question.

use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, InMemorySpendLedger, Llm, ModelCallScope, RunBudget, SpendKey, SpendLedger,
    SpendLimits, SpendPeriod,
};
use adk_memory::text::artifact_references;
use adk_memory::{GraphMemoryService, InMemoryMemoryService, MemoryService};
use adk_rag::{InMemoryVectorStore, Scope, VectorStore};
use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseSpec};
use adk_vision::analyst::{AnalystQuery, Chunking, ClipSource, VideoAnalyst, WordHashEmbedding};
use adk_vision::stream::{FramePrivacy, NoRedaction};
use adk_vision::{
    Digest, EgressAction, EgressConfig, EgressGate, FramesAnalyzer, InMemoryEgressRecorder,
    LlmDescriber, MediaScope, Sampling, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::Checks;
use crate::fixtures::{crate_fixtures, read_json};
use crate::replay::{images_in, requests};

const RECORDING: &str = "video/recorded/analyst-warehouse-hour.claude-haiku-5-5.json";
const ANTHROPIC_HOST: &str = "api.anthropic.com";
const SOURCE: &str = "warehouse-cam-3";
const COLLECTION: &str = "warehouse-chunks";
const ORG: &str = "site-7";

/// Token counts as the client reported them, cache reads and writes inside the prompt count.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    prompt_token_count: i64,
    candidates_token_count: i64,
    cache_read_input_token_count: Option<i64>,
    cache_creation_input_token_count: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Caption {
    from_ms: u64,
    to_ms: u64,
    text: String,
    usage: Usage,
}

#[derive(Clone, Debug, Deserialize)]
struct Answer {
    question: String,
    text: String,
    usage: Usage,
}

/// One question's result: the chunk the answer rests on, and whether it holds the event.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Scored {
    event: String,
    at_ms: u64,
    cited: Option<(u64, u64)>,
    hit: bool,
    supported: Option<bool>,
}

/// The live run: what the model was asked and answered, and how it scored.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recorded {
    model: String,
    recorded_at: String,
    duration_ms: u64,
    width: u32,
    height: u32,
    sample_every_ms: u64,
    chunk_ms: u64,
    frames_per_chunk: usize,
    top_k: usize,
    captions: Vec<Caption>,
    expansions: Vec<Answer>,
    answers: Vec<Answer>,
    scored: Vec<Scored>,
    cost_per_hour_micro_usd: Option<u64>,
}

/// `m:ss.s` or `h:mm:ss.s` as milliseconds.
fn stamp(text: &str) -> Option<u64> {
    let mut total = 0.0f64;
    for part in text.trim().split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((total * 1_000.0).round() as u64)
}

/// Answers each Messages request with the recorded answer to it: an expansion and an answer
/// by their question, a caption by the recorded chunk its time range overlaps most.
struct Replay(Recorded);

impl Replay {
    fn answer(&self, prompt: &str) -> Option<(&str, &Usage)> {
        if let Some(rest) = prompt.strip_prefix("A question about a video: ") {
            let question = rest.split_once("\n\n")?.0.trim();
            let found = self.0.expansions.iter().find(|a| a.question == question)?;
            return Some((&found.text, &found.usage));
        }
        if let Some((_, rest)) = prompt.split_once("\n\nQuestion: ") {
            let question = rest.split_once("\n\n")?.0.trim();
            let found = self.0.answers.iter().find(|a| a.question == question)?;
            return Some((&found.text, &found.usage));
        }
        let rest = prompt.split_once(", from ")?.1;
        let (start, rest) = rest.split_once(" to ")?;
        let (from, to) = (stamp(start)?, stamp(rest.split_once(": image")?.0)?);
        let overlap = |c: &Caption| c.to_ms.min(to).saturating_sub(c.from_ms.max(from));
        let found = self.0.captions.iter().max_by_key(|c| overlap(c)).filter(|c| overlap(c) > 0)?;
        Some((&found.text, &found.usage))
    }
}

impl Respond for Replay {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let prompt = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .filter_map(|block| block["text"].as_str().map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n");
        let Some((text, usage)) = self.answer(&prompt) else {
            return ResponseTemplate::new(400).set_body_json(json!({
                "type": "error",
                "error": { "type": "invalid_request_error", "message": "no recorded answer" }
            }));
        };
        let read = usage.cache_read_input_token_count.unwrap_or(0);
        let created = usage.cache_creation_input_token_count.unwrap_or(0);
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_recorded",
            "type": "message",
            "role": "assistant",
            "model": self.0.model,
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": usage.prompt_token_count - read - created,
                "output_tokens": usage.candidates_token_count,
                "cache_read_input_tokens": read,
                "cache_creation_input_tokens": created,
                "cache_creation": { "ephemeral_5m_input_tokens": created, "ephemeral_1h_input_tokens": 0 }
            }
        }))
    }
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let recorded: Recorded = serde_json::from_value(read_json(&crate_fixtures().join(RECORDING))?)
        .context("reading the S17 recording")?;
    checks.note(format!(
        "{} recorded live on {}, through the real Anthropic client and the egress gate; the \
         warehouse clip drawn at {}×{}",
        recorded.model, recorded.recorded_at, recorded.width, recorded.height
    ));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Replay(recorded.clone()))
        .mount(&server)
        .await;
    let config = adk_model::anthropic::AnthropicConfig::new("recorded", &recorded.model)
        .allow_insecure_http()
        .with_base_url(server.uri());
    let claude: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config)?);
    let haiku = LlmDescriber::remote(claude, [ANTHROPIC_HOST])?;

    let egress = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_rule(ANTHROPIC_HOST, &["image"], EgressAction::Allow),
        egress.clone(),
    );
    let runtime = Arc::new(
        VisionRuntime::builder()
            .remote_video_analyzer(Arc::new(FramesAnalyzer::remote(&haiku)))
            .egress_gate(gate)
            .build()?,
    );
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let audit = InMemoryAuditRecorder::new();
    let cx = VisionCx::new()
        .with_scope(
            ModelCallScope::new()
                .with_budget(Arc::new(BudgetTracker::new(RunBudget::new())))
                .with_spend_ledger(ledger.clone(), ORG),
        )
        .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let captions = Arc::new(InMemoryMemoryService::new());
    let graph = Arc::new(GraphMemoryService::new("sqlite::memory:").await?);
    graph.migrate().await?;
    let analyst = VideoAnalyst::builder(
        runtime,
        store.clone(),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(Chunking::fixed(recorded.chunk_ms))
    .frames_per_chunk(recorded.frames_per_chunk)
    .captions(captions.clone())
    .graph(graph.clone())
    .keyframes(artifacts, FramePrivacy::new(Arc::new(NoRedaction)))
    .answerer(&haiku)
    .open()
    .await?;

    let clip = WarehouseClip::new(
        WarehouseSpec {
            width: recorded.width,
            height: recorded.height,
            ..WarehouseSpec::default()
        }
        .with_duration(recorded.duration_ms),
    );
    let scope = Scope::new(ORG)?;
    let source = ClipSource::new(SOURCE, MediaScope::new(ORG, "ops", "s17"))
        .starting_at(clip.spec().started_at)
        .with_run("s17");
    let every = recorded.sample_every_ms;
    let started = std::time::Instant::now();
    let indexed = analyst
        .index(&cx, &scope, &source, clip.frames(every), Sampling::at(1_000.0 / every as f32))
        .await?
        .value;
    let chunks = indexed.chunks.len() as u64;
    let in_store = store.count(COLLECTION, &scope, None).await?;
    let kept = captions.export_user(ORG, "ops").await?;
    let with_keyframe = kept
        .iter()
        .filter(|m| {
            artifact_references(&m.entry.content).iter().any(|r| r.mime_type.starts_with("image/"))
        })
        .count();
    let in_graph = graph.open_nodes(ORG, "ops", vec![SOURCE.into()]).await?.len();
    checks.check(
        &format!(
            "the hour is cut into {chunks} chunks of {} s, captioned in {:.0} s: {in_store} in the adk-rag store, {with_keyframe} captions in adk-memory naming their keyframe, the clip in graph memory",
            recorded.chunk_ms / 1_000,
            started.elapsed().as_secs_f32()
        ),
        chunks == recorded.duration_ms / recorded.chunk_ms
            && in_store == chunks
            && kept.len() as u64 == chunks
            && with_keyframe as u64 == chunks
            && in_graph == 1,
    );

    // What left the machine: every frame the captioner saw, each released by the gate.
    let released = egress.released();
    let mut sent: Vec<Digest> = requests(&server)
        .await
        .iter()
        .flat_map(images_in)
        .map(|bytes| Digest::of(&bytes))
        .collect();
    let mut gated: Vec<Digest> = released.iter().map(|r| r.egress).collect();
    sent.sort();
    gated.sort();
    checks.check(
        &format!(
            "{} frames went to Claude, {} a chunk, and they are exactly the {} copies the egress gate released to {ANTHROPIC_HOST}",
            sent.len(),
            recorded.frames_per_chunk,
            gated.len()
        ),
        indexed.usage.frames == chunks * recorded.frames_per_chunk as u64
            && sent.len() as u64 == indexed.usage.frames
            && sent == gated
            && released.iter().all(|r| r.hosts == [ANTHROPIC_HOST]),
    );

    let mut scored = Vec::new();
    let mut answer_cost = 0;
    for event in clip.events() {
        let answer =
            analyst.ask(&cx, &scope, &event.question, &AnalystQuery::top(recorded.top_k)).await?;
        answer_cost += answer.usage.cost_micro_usd.unwrap_or(0);
        let primary = answer.primary();
        scored.push(Scored {
            event: event.id.clone(),
            at_ms: event.at_ms,
            cited: primary.map(|c| (c.start_ms, c.end_ms)),
            hit: primary.is_some_and(|c| c.contains(event.at_ms)),
            supported: answer.supported,
        });
    }
    let hits = scored.iter().filter(|s| s.hit).count();
    let missed: Vec<&str> = scored.iter().filter(|s| !s.hit).map(|s| s.event.as_str()).collect();
    checks.check(
        &format!(
            "{hits} of 10 answers cite a chunk whose time range holds the labelled event (missed: {}), as the live run did",
            if missed.is_empty() { "none".to_string() } else { missed.join(", ") }
        ),
        hits >= 8 && scored == recorded.scored,
    );

    let per_hour = indexed.cost_per_hour_micro_usd();
    let caption_cost = indexed.usage.cost_micro_usd.unwrap_or(0);
    let spent = ledger.spent(&SpendKey::org(ORG), SpendPeriod::Lifetime).await?;
    checks.check(
        &format!(
            "captioning costs {} µUSD per hour of video, as live; the ledger holds the {caption_cost} µUSD of captions and the {answer_cost} µUSD of answers ({spent} µUSD)",
            per_hour.map_or_else(|| "unknown".to_string(), |c| c.to_string())
        ),
        per_hour.is_some_and(|c| c > 0)
            && per_hour == recorded.cost_per_hour_micro_usd
            && spent == caption_cost + answer_cost,
    );
    checks.note(format!(
        "{hits}/10 cited; captioning ${:.4} per hour of video ({} input and {} output tokens)",
        per_hour.unwrap_or(0) as f64 / 1e6,
        indexed.usage.input_tokens.unwrap_or(0),
        indexed.usage.output_tokens.unwrap_or(0)
    ));
    Ok(checks)
}
