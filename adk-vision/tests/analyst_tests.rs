//! Scenario S17, the video analyst (spec 3.7 §7.13, §7.15): an hour of generated warehouse
//! video with ten labelled events is cut into chunks, each captioned, indexed in an `adk-rag`
//! store with its caption in `adk-memory`, the clip in graph memory and the keyframes in a
//! visual memory; ten questions are answered citing chunks; the cost per hour of video is
//! reported; erasing the clip removes everything it left.
//!
//! The captioner is either an oracle that reads the clip's truth — testing the analyst's own
//! steps, exactly — or the replay of a live `claude-haiku-5-5` run through the egress gate
//! (`tests/fixtures/video/recorded/analyst-warehouse-hour.claude-haiku-5-5.json`, recorded by
//! `analyst_live_tests`), which must cite as the live run did and cost what it cost, or the
//! replay of Qwen3-VL-4B captioning and answering on the device
//! (`analyst-warehouse-60min.qwen3-vl-4b-instruct.json`), which must cite as it did with
//! nothing through the gate and nothing spent.

mod analyst_suite;

use std::sync::{Arc, Mutex};

use adk_core::egress::EgressPolicy;
use adk_core::{BudgetTracker, DataLocality, ModelCallScope, RunBudget, UsageUnit};
use adk_memory::text::artifact_references;
use adk_memory::{MemoryService, SearchRequest};
use adk_rag::{InMemoryVectorStore, Scope, VectorStore};
use adk_vision::analyst::fixtures::{WarehouseClip, WarehouseSpec};
use adk_vision::analyst::{
    AnalystQuery, CHUNK_KIND, Chunking, ClipSource, VISION_ANALYST_ASKED, VISION_ANALYST_ERASED,
    VISION_ANALYST_INDEXED, VideoAnalyst, WordHashEmbedding,
};
use adk_vision::conformance::{VideoAnalyzerTarget, VideoFixture};
use adk_vision::memory::VisualMemory;
use adk_vision::{
    BackendDescriptor, CoordFrame, Embedder, EmbeddingSpace, FrameMeta, FramesAnalyzer,
    LlmDescriber, Locality, MediaScope, Metric, Sampling, SegmentPrompt, SentImage, SourceImage,
    VideoRequest, VisionCx, VisionError, VisionRuntime, artifact_uri,
};
use analyst_suite::{COLLECTION, Oracle, Replay, Setup, score};
use futures::StreamExt;

const SOURCE: &str = "warehouse-cam-3";
const RECORDING: &str = "analyst-warehouse-hour.claude-haiku-5-5.json";
const ON_DEVICE: &str = "analyst-warehouse-60min.qwen3-vl-4b-instruct.json";

/// The clip drawn small: the captioners here read the truth or a recording, not pixels.
fn small_clip(duration_ms: u64) -> WarehouseClip {
    WarehouseClip::new(
        WarehouseSpec { width: 160, height: 90, ..WarehouseSpec::default() }
            .with_duration(duration_ms),
    )
}

fn clip_source(clip: &WarehouseClip) -> ClipSource {
    ClipSource::new(SOURCE, MediaScope::new("site-7", "ops", "s1"))
        .starting_at(clip.spec().started_at)
        .with_run("run-17")
}

fn scope() -> Scope {
    Scope::new("site-7").unwrap()
}

fn oracle(clip: &WarehouseClip, locality: DataLocality) -> Arc<Oracle> {
    Arc::new(Oracle { clip: clip.clone(), locality, calls: Mutex::new(0) })
}

/// An image's vector is its mean colour; enough for keyframes to have a space.
struct MeanColour(BackendDescriptor, EmbeddingSpace);

impl MeanColour {
    fn new() -> Self {
        Self(
            BackendDescriptor::new("mean-colour", Locality::InProcess, CoordFrame::SentPixels),
            EmbeddingSpace {
                model: "mean-colour".into(),
                revision: "1".into(),
                dimensions: 3,
                metric: Metric::Cosine,
            },
        )
    }
}

#[async_trait::async_trait]
impl Embedder for MeanColour {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    fn space(&self) -> &EmbeddingSpace {
        &self.1
    }

    async fn embed_images(
        &self,
        _: &VisionCx,
        images: &[SentImage],
    ) -> adk_vision::Result<Vec<Vec<f32>>> {
        Ok(images
            .iter()
            .map(|i| {
                let p = i.pixels();
                let (w, h) = p.size();
                let mut sum = [0.0f32; 3];
                for y in (0..h).step_by(4) {
                    for x in (0..w).step_by(4) {
                        let px = p.pixel(x, y);
                        for c in 0..3 {
                            sum[c] += f32::from(px[c]) + 1.0;
                        }
                    }
                }
                sum.to_vec()
            })
            .collect())
    }

    async fn embed_texts(&self, _: &VisionCx, texts: &[&str]) -> adk_vision::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 1.0, 1.0]).collect())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_hour_of_warehouse_video_is_chunked_captioned_indexed_answered_and_erased() {
    let clip = small_clip(3_600_000);
    let setup = Setup::new().await;
    let qwen = oracle(&clip, DataLocality::InProcess);
    let describer = LlmDescriber::local(qwen.clone()).unwrap();
    let runtime = Arc::new(
        VisionRuntime::builder()
            .video_analyzer(Arc::new(FramesAnalyzer::local(&describer)))
            .embedder(Arc::new(MeanColour::new()))
            .egress_gate(setup.gate(EgressPolicy::public_only()))
            .build()
            .unwrap(),
    );
    let keyframe_store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let visual = Arc::new(
        VisualMemory::builder(runtime.clone(), keyframe_store.clone(), "warehouse-keyframes")
            .open()
            .await
            .unwrap(),
    );
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let analyst = VideoAnalyst::builder(
        runtime,
        store.clone(),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(Chunking::fixed(30_000))
    .frames_per_chunk(3)
    .captions(setup.captions.clone())
    .graph(setup.graph.clone())
    .keyframes(setup.artifacts.clone())
    .visual_memory(visual)
    .answerer(&describer)
    .open()
    .await
    .unwrap();

    // Index: an hour sampled every 10 s, in 30-second chunks of three frames each.
    let source = clip_source(&clip);
    let out = analyst
        .index(&setup.cx, &scope(), &source, clip.frames(10_000), Sampling::at(0.1))
        .await
        .unwrap();
    let indexed = out.value;
    assert_eq!(indexed.chunks.len(), 120);
    assert_eq!((indexed.duration_ms, indexed.frames_read), (3_600_000, 360));
    for (i, chunk) in indexed.chunks.iter().enumerate() {
        let start = i as u64 * 30_000;
        assert_eq!((chunk.index, chunk.start_ms, chunk.end_ms), (i as u64, start, start + 30_000));
        assert_eq!(chunk.describer.as_deref(), Some("qwen3-vl-4b-instruct"));
        assert!(chunk.keyframe.is_some(), "chunk {i} kept no keyframe");
    }
    assert_eq!((indexed.usage.frames, indexed.usage.requests), (360, 120));
    assert_eq!(
        indexed.cost_per_hour_micro_usd(),
        Some(0),
        "captioning on the device costs nothing"
    );
    assert_eq!(setup.budget.units().get(UsageUnit::Frames), 360, "every caption frame is metered");
    assert_eq!(
        setup.budget.units().get(UsageUnit::Images),
        120,
        "one keyframe embedding per chunk"
    );
    assert!(setup.egress.is_empty(), "an on-device captioner sends nothing off the machine");
    assert_eq!(out.calls.len(), 240, "one caption call and one keyframe embedding per chunk");
    assert_eq!(setup.records(VISION_ANALYST_INDEXED).len(), 1);

    // Ten questions, each answered citing the chunk the event starts in.
    let mut hits = 0;
    for event in clip.events() {
        let answer =
            analyst.ask(&setup.cx, &scope(), &event.question, &AnalystQuery::top(8)).await.unwrap();
        let scored = score(event, &answer);
        assert_eq!(
            answer.citations.len(),
            1,
            "the oracle's citation of a line it was not shown is dropped"
        );
        assert_eq!(answer.answered_by.as_deref(), Some("qwen3-vl-4b-instruct"));
        assert_eq!(answer.supported, Some(true));
        assert_eq!(
            answer.expanded.as_deref(),
            Some(analyst_suite::capitalised(&event.description).as_str())
        );
        assert!(answer.retrieved.len() <= 8);
        hits += usize::from(scored.hit);
    }
    assert_eq!(hits, 10);
    assert_eq!(setup.records(VISION_ANALYST_ASKED).len(), 10);

    // Captions in adk-memory carry their time range and keyframe reference.
    let found = setup
        .captions
        .search(SearchRequest {
            query: "forklift aisle".into(),
            user_id: "ops".into(),
            app_name: "site-7".into(),
            limit: Some(5),
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap()
        .memories;
    let forklift = found
        .iter()
        .find(|m| adk_memory::text::extract_text(&m.content).starts_with("[3:00.0–3:30.0]"))
        .expect("the forklift chunk's caption");
    let chunk = indexed.chunks.iter().find(|c| c.start_ms == 180_000).unwrap();
    let refs = artifact_references(&forklift.content);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].uri, artifact_uri(&chunk.keyframe.as_ref().unwrap().artifact));

    // The clip is a graph entity: one observation per chunk, related to what it names.
    let (entities, relations) = setup.graph.read_graph("site-7", "ops").await.unwrap();
    let entity = entities.iter().find(|e| e.name == SOURCE).unwrap();
    assert_eq!((entity.entity_type.as_str(), entity.observations.len()), ("vision.clip", 120));
    assert!(relations.iter().any(|r| r.source == SOURCE && r.target == "forklift-aisle-2"));

    // Erasure removes the chunks, captions, graph entity, keyframes and their vectors.
    let erasure = analyst.erase_source(&setup.cx, &scope(), SOURCE).await.unwrap();
    assert_eq!((erasure.chunks, erasure.captions, erasure.entities), (120, 120, 1));
    // Keyframes are stored by content: chunks whose middle frames are the same picture
    // share one artifact and one visual-memory vector.
    let distinct: std::collections::BTreeSet<String> = indexed
        .chunks
        .iter()
        .map(|c| artifact_uri(&c.keyframe.as_ref().unwrap().artifact))
        .collect();
    assert!(distinct.len() > 100, "{} distinct keyframes", distinct.len());
    assert_eq!(
        (erasure.keyframes.len(), erasure.visual_chunks),
        (distinct.len(), distinct.len() as u64)
    );
    assert!(analyst.chunks(&scope(), SOURCE).await.unwrap().is_empty());
    assert_eq!(store.count(COLLECTION, &scope(), None).await.unwrap(), 0);
    assert_eq!(keyframe_store.count("warehouse-keyframes", &scope(), None).await.unwrap(), 0);
    let (entities, _) = setup.graph.read_graph("site-7", "ops").await.unwrap();
    assert!(entities.iter().all(|e| e.name != SOURCE));
    let left = setup.captions.export_user("site-7", "ops").await.unwrap();
    assert!(left.is_empty(), "captions remain: {left:?}");
    assert!(
        adk_artifact::ArtifactService::list(
            &*setup.artifacts,
            adk_artifact::ListRequest {
                app_name: "site-7".into(),
                user_id: "ops".into(),
                session_id: "s1".into()
            }
        )
        .await
        .unwrap()
        .file_names
        .is_empty()
    );
    let erased = setup.records(VISION_ANALYST_ERASED);
    assert_eq!(erased.len(), 1);
    let answer = analyst
        .ask(&setup.cx, &scope(), &clip.events()[0].question, &AnalystQuery::top(8))
        .await
        .unwrap();
    assert!(answer.retrieved.is_empty() && answer.citations.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_recorded_live_run_cites_as_it_did_and_costs_what_it_cost_through_the_gate() {
    let replay = Arc::new(Replay::load(RECORDING));
    let recording = replay.recording.clone();
    let clip = small_clip(recording.duration_ms);
    let setup = Setup::new().await;
    let haiku = LlmDescriber::remote_with_hosts(replay, ["api.anthropic.com"]).unwrap();
    let runtime = Arc::new(
        VisionRuntime::builder()
            .remote_video_analyzer(Arc::new(FramesAnalyzer::remote(&haiku)))
            .egress_gate(setup.gate(EgressPolicy::public_only()))
            .build()
            .unwrap(),
    );
    let analyst = VideoAnalyst::builder(
        runtime,
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(Chunking::fixed(recording.chunk_ms))
    .frames_per_chunk(recording.frames_per_chunk)
    .captions(setup.captions.clone())
    .graph(setup.graph.clone())
    .answerer(&haiku)
    .open()
    .await
    .unwrap();
    let every = recording.sample_every_ms;
    let indexed = analyst
        .index(
            &setup.cx,
            &scope(),
            &clip_source(&clip),
            clip.frames(every),
            Sampling::at(1_000.0 / every as f32),
        )
        .await
        .unwrap()
        .value;
    assert_eq!(indexed.chunks.len() as u64, recording.duration_ms / recording.chunk_ms);

    // Every frame sent went through the gate to Anthropic, and only those.
    let released = setup.egress.released();
    assert_eq!(released.len() as u64, indexed.usage.frames);
    assert!(
        released
            .iter()
            .all(|r| r.hosts == ["api.anthropic.com"] && r.backend.as_str() == recording.model)
    );

    let mut scored = Vec::new();
    for event in clip.events() {
        let answer = analyst
            .ask(&setup.cx, &scope(), &event.question, &AnalystQuery::top(recording.top_k))
            .await
            .unwrap();
        scored.push(score(event, &answer));
    }
    let hits = scored.iter().filter(|s| s.hit).count();
    assert!(hits >= 8, "{hits}/10 answers cite the chunk of their event: {scored:#?}");
    assert_eq!(scored, recording.scored, "the replay cites as the live run did");
    assert_eq!(indexed.cost_per_hour_micro_usd(), recording.cost_per_hour_micro_usd);
    assert!(
        indexed.cost_per_hour_micro_usd().is_some_and(|c| c > 0),
        "a priced remote captioner reports its cost"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_on_device_run_cites_as_it_did_with_nothing_sent_or_spent() {
    use adk_core::{SpendKey, SpendLedger, SpendPeriod};

    let replay = Arc::new(Replay::on_device(ON_DEVICE));
    let recording = replay.recording.clone();
    let clip = small_clip(recording.duration_ms);
    let setup = Setup::new().await;
    let qwen = LlmDescriber::local(replay).unwrap();
    let runtime = Arc::new(
        VisionRuntime::builder()
            .video_analyzer(Arc::new(FramesAnalyzer::local(&qwen)))
            .egress_gate(setup.gate(EgressPolicy::public_only()))
            .build()
            .unwrap(),
    );
    let analyst = VideoAnalyst::builder(
        runtime,
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(Chunking::fixed(recording.chunk_ms))
    .frames_per_chunk(recording.frames_per_chunk)
    .answerer(&qwen)
    .open()
    .await
    .unwrap();
    let every = recording.sample_every_ms;
    let indexed = analyst
        .index(
            &setup.cx,
            &scope(),
            &clip_source(&clip),
            clip.frames(every),
            Sampling::at(1_000.0 / every as f32),
        )
        .await
        .unwrap()
        .value;
    assert_eq!(indexed.chunks.len() as u64, recording.duration_ms / recording.chunk_ms);
    assert_eq!(indexed.cost_per_hour_micro_usd(), Some(0));

    let mut scored = Vec::new();
    for event in clip.events() {
        let answer = analyst
            .ask(&setup.cx, &scope(), &event.question, &AnalystQuery::top(recording.top_k))
            .await
            .unwrap();
        // Every event's chunk is among those retrieved; a miss is the answerer's.
        assert!(
            answer.retrieved.iter().any(|h| h.chunk.contains(event.at_ms)),
            "{} was not retrieved",
            event.id
        );
        scored.push(score(event, &answer));
    }
    assert_eq!(scored, recording.scored, "the replay cites as the live run did");
    assert_eq!(scored.iter().filter(|s| s.hit).count(), 8);
    assert!(setup.egress.released().is_empty(), "no frame passed the gate");
    let spent = setup.ledger.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap();
    assert_eq!(spent, 0, "the device's calls cost nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scene_cuts_follow_the_picture_and_stay_within_their_bounds() {
    let clip = small_clip(600_000);
    let setup = Setup::new().await;
    let qwen = oracle(&clip, DataLocality::InProcess);
    let runtime = Arc::new(
        VisionRuntime::builder()
            .video_analyzer(Arc::new(FramesAnalyzer::local(&LlmDescriber::local(qwen).unwrap())))
            .build()
            .unwrap(),
    );
    let chunking = Chunking::scene_change().with_threshold(0.2).with_bounds(5_000, 60_000);
    let analyst = VideoAnalyst::builder(
        runtime,
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(chunking)
    .open()
    .await
    .unwrap();
    let indexed = analyst
        .index(&setup.cx, &scope(), &clip_source(&clip), clip.frames(5_000), Sampling::at(0.2))
        .await
        .unwrap()
        .value;
    let chunks = &indexed.chunks;
    assert!(chunks.windows(2).all(|w| w[0].end_ms == w[1].start_ms), "scenes tile the clip");
    assert!(chunks.iter().all(|c| c.end_ms - c.start_ms <= 60_000));
    // The lights going out is a cut, and so is their coming back.
    let lights = clip.event("lights-out").unwrap();
    let starts: Vec<u64> = chunks.iter().map(|c| c.start_ms).collect();
    assert!(starts.iter().any(|&s| s >= lights.at_ms && s < lights.at_ms + 5_000), "{starts:?}");
    assert!(
        starts.iter().any(|&s| s >= lights.until_ms && s < lights.until_ms + 5_000),
        "{starts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_cap_stops_the_captioning_and_keeps_what_was_indexed() {
    let clip = small_clip(600_000);
    let setup = Setup::new().await;
    let qwen = oracle(&clip, DataLocality::InProcess);
    let runtime = Arc::new(
        VisionRuntime::builder()
            .video_analyzer(Arc::new(FramesAnalyzer::local(
                &LlmDescriber::local(qwen.clone()).unwrap(),
            )))
            .build()
            .unwrap(),
    );
    let analyst = VideoAnalyst::builder(
        runtime,
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .frames_per_chunk(3)
    .open()
    .await
    .unwrap();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new().max_units(UsageUnit::Frames, 30)));
    let cx = VisionCx::new().with_scope(ModelCallScope::new().with_budget(budget.clone()));
    let err = analyst
        .index(&cx, &scope(), &clip_source(&clip), clip.frames(10_000), Sampling::at(0.1))
        .await
        .unwrap_err();
    let adk: adk_core::AdkError = err.into();
    assert!(adk.message.contains("frames"), "the refusal names the unit: {adk}");
    assert_eq!(*qwen.calls.lock().unwrap(), 10, "ten chunks of three frames fit under the cap");
    assert_eq!(analyst.chunks(&scope(), SOURCE).await.unwrap().len(), 10);
    assert_eq!(budget.units().get(UsageUnit::Frames), 30);

    // A cancelled call stops before the next chunk; indexing again replaces the clip.
    let cancelled = VisionCx::new();
    cancelled.cancellation().cancel();
    let err = analyst
        .index(&cancelled, &scope(), &clip_source(&clip), clip.frames(10_000), Sampling::at(0.1))
        .await
        .unwrap_err();
    assert!(matches!(err, VisionError::Cancelled), "{err}");
    let again = analyst
        .index(&setup.cx, &scope(), &clip_source(&clip), clip.frames(10_000), Sampling::at(0.1))
        .await
        .unwrap()
        .value;
    assert_eq!(again.chunks.len(), 20);
    assert_eq!(analyst.chunks(&scope(), SOURCE).await.unwrap().len(), 20);
    assert_eq!(
        setup.records(VISION_ANALYST_ERASED).len(),
        1,
        "the earlier chunks were erased first"
    );
}

#[tokio::test]
async fn an_analyst_needs_a_video_analyzer_and_keyframes_for_a_visual_memory() {
    let runtime =
        Arc::new(VisionRuntime::builder().embedder(Arc::new(MeanColour::new())).build().unwrap());
    let err = VideoAnalyst::builder(
        runtime.clone(),
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .open()
    .await
    .unwrap_err();
    assert!(matches!(err, VisionError::BackendNotFound(_)), "{err}");
    let clip = small_clip(600_000);
    let with_video = Arc::new(
        VisionRuntime::builder()
            .video_analyzer(Arc::new(FramesAnalyzer::local(
                &LlmDescriber::local(oracle(&clip, DataLocality::InProcess)).unwrap(),
            )))
            .embedder(Arc::new(MeanColour::new()))
            .build()
            .unwrap(),
    );
    let visual = Arc::new(
        VisualMemory::builder(with_video.clone(), Arc::new(InMemoryVectorStore::new()), "k")
            .open()
            .await
            .unwrap(),
    );
    let err = VideoAnalyst::builder(
        with_video.clone(),
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .visual_memory(visual)
    .open()
    .await
    .unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
    let err = VideoAnalyst::builder(
        with_video,
        Arc::new(InMemoryVectorStore::new()),
        Arc::new(WordHashEmbedding::default()),
        COLLECTION,
    )
    .chunking(Chunking::fixed(0))
    .open()
    .await
    .unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
    assert_eq!(CHUNK_KIND, "videoChunk");
}

/// Ten minutes of the clip sampled every 10 s, as a conformance fixture asking for captions.
async fn caption_fixture() -> VideoFixture {
    let clip = small_clip(600_000);
    let frames: Vec<(FrameMeta, SourceImage)> = clip
        .frames(10_000)
        .map(|f| {
            let f = f.unwrap();
            let adk_vision::FramePixels::Ephemeral(pixels) = &f.pixels else { unreachable!() };
            (f.meta(), SourceImage::from_pixels((**pixels).clone()).unwrap())
        })
        .collect()
        .await;
    let request = VideoRequest::Caption { chunk_ms: 30_000 };
    VideoFixture::frames(frames, Sampling::at(0.1), request).with_caption(30_000).with_refused(
        VideoRequest::Track { prompts: vec![SegmentPrompt::Concept { text: "forklift".into() }] },
    )
}

mod oracle_local_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Local(Arc::new(FramesAnalyzer::local(
            &LlmDescriber::local(oracle(&small_clip(600_000), DataLocality::InProcess)).unwrap(),
        ).with_frames_per_call(3))),
        fixture = caption_fixture().await,
    }
}

mod oracle_remote_conformance {
    use super::*;

    adk_vision::video_analyzer_conformance_tests! {
        backend = VideoAnalyzerTarget::Remote(Arc::new(FramesAnalyzer::remote(
            &LlmDescriber::remote_with_hosts(oracle(&small_clip(600_000), DataLocality::Remote), ["api.anthropic.com"]).unwrap(),
        ))),
        fixture = caption_fixture().await,
    }
}
