//! Scenario S17 live: an hour of generated warehouse video captioned by `claude-haiku-5-5`
//! through the egress gate, 30-second chunks of six frames sampled every 5 s, indexed with
//! `WordHashEmbedding`, and ten questions answered by the same model from the captions.
//!
//! `#[ignore]`d; needs `ANTHROPIC_API_KEY`. `ADK_VISION_ANALYST_MODEL` names another model.
//! With `ADK_VISION_RECORD_DIR` set, the run is written there as the recording
//! `analyst_tests` replays.
//!
//! ```text
//! ANTHROPIC_API_KEY=... ADK_VISION_RECORD_DIR=tests/fixtures/video/recorded \
//!     cargo test -p adk-vision --features analyst --test analyst_live_tests -- --ignored --nocapture
//! ```

mod analyst_suite;

use std::sync::{Arc, Mutex};

use adk_core::Llm;
use adk_core::egress::EgressPolicy;
use adk_memory::MemoryService;
use adk_rag::{InMemoryVectorStore, Scope, VectorStore};
use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseSpec};
use adk_vision::analyst::{AnalystQuery, Chunking, ClipSource, VideoAnalyst, WordHashEmbedding};
use adk_vision::stream::{FramePrivacy, NoRedaction};
use adk_vision::{FramesAnalyzer, LlmDescriber, MediaScope, Sampling};
use analyst_suite::{COLLECTION, Recorder, Recording, Setup, recording_path, score};

const EVERY_MS: u64 = 5_000;
const CHUNK_MS: u64 = 30_000;
const FRAMES_PER_CHUNK: usize = 6;
const TOP_K: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn s17_an_hour_of_warehouse_video_answered_from_captions() {
    let Some(key) = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty()) else {
        return;
    };
    let model =
        std::env::var("ADK_VISION_ANALYST_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into());
    let duration_ms = std::env::var("ADK_VISION_ANALYST_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_600_000);
    let config = adk_model::anthropic::AnthropicConfig::new(key, &model);
    let client: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config).unwrap());
    let recorder = Arc::new(Recorder {
        inner: client,
        recording: Mutex::new(Recording {
            model: model.clone(),
            recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            duration_ms,
            sample_every_ms: EVERY_MS,
            chunk_ms: CHUNK_MS,
            frames_per_chunk: FRAMES_PER_CHUNK,
            top_k: TOP_K,
            ..Recording::default()
        }),
    });
    let haiku = LlmDescriber::remote(recorder.clone(), ["api.anthropic.com"]).unwrap();
    let setup = Setup::new().await;
    let runtime = Arc::new(
        adk_vision::VisionRuntime::builder()
            .remote_video_analyzer(Arc::new(FramesAnalyzer::remote(&haiku)))
            .egress_gate(setup.gate(EgressPolicy::public_only()))
            .build()
            .unwrap(),
    );
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let analyst = VideoAnalyst::builder(
        runtime,
        store.clone(),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(Chunking::fixed(CHUNK_MS))
    .frames_per_chunk(FRAMES_PER_CHUNK)
    .captions(setup.captions.clone())
    .graph(setup.graph.clone())
    .keyframes(setup.artifacts.clone(), FramePrivacy::new(Arc::new(NoRedaction)))
    .answerer(&haiku)
    .open()
    .await
    .unwrap();

    let clip = WarehouseClip::new(WarehouseSpec::default().with_duration(duration_ms));
    let scope = Scope::new("site-7").unwrap();
    let source = ClipSource::new("warehouse-cam-3", MediaScope::new("site-7", "ops", "s1"))
        .starting_at(clip.spec().started_at)
        .with_run("s17-live");
    let started = std::time::Instant::now();
    let indexed = analyst
        .index(
            &setup.cx,
            &scope,
            &source,
            clip.frames(EVERY_MS),
            Sampling::at(1_000.0 / EVERY_MS as f32),
        )
        .await
        .unwrap()
        .value;
    println!(
        "indexed {} chunks of {} min of video in {:.0} s: {} frames sent in {} requests, {:?} input and {:?} output tokens, cost {:?} µUSD, {:?} µUSD per hour of video",
        indexed.chunks.len(),
        indexed.duration_ms / 60_000,
        started.elapsed().as_secs_f32(),
        indexed.usage.frames,
        indexed.usage.requests,
        indexed.usage.input_tokens,
        indexed.usage.output_tokens,
        indexed.usage.cost_micro_usd,
        indexed.cost_per_hour_micro_usd()
    );
    assert_eq!(
        setup.egress.released().len() as u64,
        indexed.usage.frames,
        "every frame sent passed the gate"
    );
    for event in clip.events() {
        let chunk = indexed.chunks.iter().find(|c| c.contains(event.at_ms)).unwrap();
        println!(
            "  truth {:>8} {:<20} chunk {}: {}",
            event.at_ms,
            event.id,
            chunk.time_range(),
            chunk.caption
        );
    }

    let mut scored = Vec::new();
    let mut answer_cost = 0;
    for event in clip.events() {
        let answer = analyst
            .ask(&setup.cx, &scope, &event.question, &AnalystQuery::top(TOP_K))
            .await
            .unwrap();
        answer_cost += answer.usage.cost_micro_usd.unwrap_or(0);
        let s = score(event, &answer);
        println!(
            "{} {:<20} at {:>8} ms cited {:?}: {}",
            if s.hit { "HIT " } else { "MISS" },
            s.event,
            s.at_ms,
            s.cited,
            s.answer
        );
        scored.push(s);
    }
    let hits = scored.iter().filter(|s| s.hit).count();
    println!("{hits}/10 answers cite the chunk of their event; answers cost {answer_cost} µUSD");

    let mut recording = recorder.recording.lock().unwrap().clone();
    recording.width = clip.spec().width;
    recording.height = clip.spec().height;
    recording.scored = scored;
    recording.cost_per_hour_micro_usd = indexed.cost_per_hour_micro_usd();
    if let Ok(dir) = std::env::var("ADK_VISION_RECORD_DIR") {
        let name = format!("analyst-warehouse-hour.{model}.json");
        let path = std::path::Path::new(&dir).join(&name);
        std::fs::write(&path, serde_json::to_string_pretty(&recording).unwrap() + "\n").unwrap();
        println!("recorded to {} ({})", path.display(), recording_path(&name).display());
    }

    let erasure = analyst.erase_source(&setup.cx, &scope, "warehouse-cam-3").await.unwrap();
    assert_eq!(erasure.chunks, indexed.chunks.len() as u64);
    assert_eq!(store.count(COLLECTION, &scope, None).await.unwrap(), 0);
    assert!(setup.captions.export_user("site-7", "ops").await.unwrap().is_empty());
    assert!(hits >= 8, "{hits}/10");
}
