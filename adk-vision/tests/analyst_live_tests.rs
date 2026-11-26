//! Scenario S17 live: an hour of generated warehouse video captioned by `claude-haiku-5-5`
//! through the egress gate, 30-second chunks of six frames chosen from frames sampled every
//! 2.5 s, indexed with
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
//!
//! With `mistralrs` too, `s17_captioned_on_the_device_by_qwen3_vl` captions and answers on
//! this machine with Qwen3-VL (4B by default, `ADK_VISION_QWEN` for another) on the device
//! `ADK_VISION_DEVICE` names (`cpu`, `metal`, or automatic): no frame reaches the egress gate
//! and the cost is zero. `ADK_VISION_ANALYST_CHUNKS` bounds it to the hour's first chunks,
//! and only the questions whose events start in them are asked. Its recording is named
//! `analyst-warehouse-<minutes>min.<model>.json`; `analyst_tests` replays the hour recorded
//! on an Apple M4 with `mistralrs-metal` (8.8 s a chunk, 8 of 10 citations, nothing sent or
//! spent).
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models ADK_VISION_DEVICE=cpu ADK_VISION_ANALYST_CHUNKS=20 \
//!     ADK_VISION_RECORD_DIR=tests/fixtures/video/recorded \
//!     cargo test -p adk-vision --features analyst,mistralrs --test analyst_live_tests \
//!     s17_captioned_on_the_device_by_qwen3_vl -- --ignored --nocapture
//! ```

mod analyst_suite;

use std::sync::{Arc, Mutex};

use adk_core::Llm;
use adk_core::egress::EgressPolicy;
use adk_memory::MemoryService;
use adk_rag::{InMemoryVectorStore, Scope, VectorStore};
use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseSpec};
use adk_vision::analyst::{AnalystQuery, Chunking, ClipSource, VideoAnalyst, WordHashEmbedding};
use adk_vision::{FramesAnalyzer, LlmDescriber, MediaScope, Sampling};
use analyst_suite::{COLLECTION, Recorder, Recording, Setup, recording_path, score};

const EVERY_MS: u64 = 2_500;
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
    let haiku = LlmDescriber::remote_with_hosts(recorder.clone(), ["api.anthropic.com"]).unwrap();
    let setup = Setup::on_disk().await;
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
    .keyframes(setup.artifacts.clone())
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
            "{} {:<20} at {:>8} ms cited {:?} supported {:?}: {} [searched with: {}]",
            if s.hit { "HIT " } else { "MISS" },
            s.event,
            s.at_ms,
            s.cited,
            s.supported,
            s.answer,
            answer.expanded.as_deref().unwrap_or_default()
        );
        scored.push(s);
    }
    let hits = scored.iter().filter(|s| s.hit).count();
    let supported = scored.iter().filter(|s| s.hit && s.supported == Some(true)).count();
    println!(
        "{hits}/10 answers cite the chunk of their event ({supported} found in the captions); \
         answers cost {answer_cost} µUSD"
    );

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

#[cfg(feature = "mistralrs")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "loads Qwen3-VL (4B: 8.88 GB) through the fetcher; see the module docs"]
async fn s17_captioned_on_the_device_by_qwen3_vl() {
    use adk_core::SpendLedger;
    use adk_mistralrs::{Device, DeviceConfig, MistralRsConfig};
    use adk_vision::cloud::Normalized1000;
    use adk_vision::mistralrs::{MAX_PIXELS, QWEN3_VL_4B};
    use adk_vision::{
        FetchConfig, InMemoryAcceptanceStore, InputLimits, ManifestSet, ModelFetcher,
    };
    use futures::StreamExt;

    let id = std::env::var("ADK_VISION_QWEN").unwrap_or_else(|_| QWEN3_VL_4B.into());
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let model = fetcher.fetch(ManifestSet::builtin().unwrap().get(&id).unwrap()).await.unwrap();
    let device = std::env::var("ADK_VISION_DEVICE").unwrap_or_else(|_| "auto".into());
    let config = MistralRsConfig {
        device: DeviceConfig {
            device: match device.as_str() {
                "cpu" => Device::Cpu,
                "metal" => Device::Metal,
                _ => Device::Auto,
            },
            ..DeviceConfig::default()
        },
        ..MistralRsConfig::default()
    };
    let loading = std::time::Instant::now();
    let qwen = LlmDescriber::mistralrs(&model, config).await.unwrap();
    let load_ms = loading.elapsed().as_millis() as u64;
    println!("loaded {id} on {device} in {load_ms} ms");

    let hour = WarehouseClip::new(WarehouseSpec::default());
    let chunks = std::env::var("ADK_VISION_ANALYST_CHUNKS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(hour.spec().duration_ms / CHUNK_MS);
    let indexed_ms = (chunks * CHUNK_MS).min(hour.spec().duration_ms);
    let recorder = Arc::new(Recorder {
        inner: Arc::clone(qwen.model()),
        recording: Mutex::new(Recording {
            model: id.clone(),
            recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            duration_ms: hour.spec().duration_ms,
            sample_every_ms: EVERY_MS,
            chunk_ms: CHUNK_MS,
            frames_per_chunk: FRAMES_PER_CHUNK,
            top_k: TOP_K,
            device: Some(device.clone()),
            load_ms: Some(load_ms),
            indexed_ms: Some(indexed_ms),
            ..Recording::default()
        }),
    });
    // The describer `LlmDescriber::mistralrs` loaded, over the recorder.
    let local = LlmDescriber::local(recorder.clone())
        .unwrap()
        .with_id(id.clone())
        .with_adapter(Arc::new(Normalized1000::qwen3_vl(&id)))
        .with_limits(InputLimits::unlimited().with_max_pixels(MAX_PIXELS));
    let setup = Setup::on_disk().await;
    let runtime = Arc::new(
        adk_vision::VisionRuntime::builder()
            .video_analyzer(Arc::new(FramesAnalyzer::local(&local)))
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
    .keyframes(setup.artifacts.clone())
    .answerer(&local)
    .open()
    .await
    .unwrap();

    let scope = Scope::new("site-7").unwrap();
    let source = ClipSource::new("warehouse-cam-3", MediaScope::new("site-7", "ops", "s1"))
        .starting_at(hour.spec().started_at)
        .with_run("s17-local");
    let frames = hour.frames(EVERY_MS).take((indexed_ms / EVERY_MS) as usize).boxed();
    let started = std::time::Instant::now();
    let indexed = analyst
        .index(&setup.cx, &scope, &source, frames, Sampling::at(1_000.0 / EVERY_MS as f32))
        .await
        .unwrap()
        .value;
    let index_wall_ms = started.elapsed().as_millis() as u64;
    println!(
        "indexed {} chunks ({} of {} min) on {device} in {:.0} s, {:.1} s a chunk: {} frames in {} requests, {:?} input and {:?} output tokens, cost {:?} µUSD",
        indexed.chunks.len(),
        indexed.duration_ms / 60_000,
        hour.spec().duration_ms / 60_000,
        index_wall_ms as f64 / 1_000.0,
        index_wall_ms as f64 / 1_000.0 / indexed.chunks.len().max(1) as f64,
        indexed.usage.frames,
        indexed.usage.requests,
        indexed.usage.input_tokens,
        indexed.usage.output_tokens,
        indexed.usage.cost_micro_usd,
    );
    assert!(setup.egress.released().is_empty(), "no frame reached the egress gate");
    assert_eq!(indexed.usage.cost_micro_usd, Some(0));
    let asked: Vec<_> = hour.events().iter().filter(|e| e.at_ms < indexed_ms).collect();
    for event in &asked {
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
    for event in &asked {
        let started = std::time::Instant::now();
        let answer = analyst
            .ask(&setup.cx, &scope, &event.question, &AnalystQuery::top(TOP_K))
            .await
            .unwrap();
        assert_eq!(answer.usage.cost_micro_usd.unwrap_or(0), 0);
        let s = score(event, &answer);
        println!(
            "{} {:<20} at {:>8} ms cited {:?} supported {:?} in {:.0} s: {} [searched with: {}]",
            if s.hit { "HIT " } else { "MISS" },
            s.event,
            s.at_ms,
            s.cited,
            s.supported,
            started.elapsed().as_secs_f32(),
            s.answer,
            answer.expanded.as_deref().unwrap_or_default()
        );
        scored.push(s);
    }
    let hits = scored.iter().filter(|s| s.hit).count();
    println!(
        "{hits}/{} answers cite the chunk of their event; nothing left the machine",
        asked.len()
    );
    assert!(setup.egress.released().is_empty(), "nothing left the machine");
    let spent = setup
        .ledger
        .spent(&adk_core::SpendKey::org("acme"), adk_core::SpendPeriod::Lifetime)
        .await
        .unwrap();
    assert_eq!(spent, 0, "the spend ledger holds nothing for the device's calls");

    let mut recording = recorder.recording.lock().unwrap().clone();
    recording.width = hour.spec().width;
    recording.height = hour.spec().height;
    recording.scored = scored;
    recording.cost_per_hour_micro_usd = indexed.cost_per_hour_micro_usd();
    recording.index_wall_ms = Some(index_wall_ms);
    if let Ok(dir) = std::env::var("ADK_VISION_RECORD_DIR") {
        let name = format!("analyst-warehouse-{}min.{id}.json", indexed_ms / 60_000);
        let path = std::path::Path::new(&dir).join(&name);
        std::fs::write(&path, serde_json::to_string_pretty(&recording).unwrap() + "\n").unwrap();
        println!("recorded to {}", path.display());
    }
    let erasure = analyst.erase_source(&setup.cx, &scope, "warehouse-cam-3").await.unwrap();
    assert_eq!(erasure.chunks, indexed.chunks.len() as u64);
}
