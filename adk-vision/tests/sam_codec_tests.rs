//! The licensed mask decoder (feature `sam-codec`): Meta's parser as the oracle, masks scored
//! against the drawn shapes, the licence gate, and the segmenter conformance suite with masks.
//!
//! `tests/fixtures/cloud/oracle/sam-3-1.oracle.json` holds the SHA-256 and set-pixel count
//! of every raster Meta's `@meta-sam/parser` 0.1.0 decoded from the recorded streams
//! (`sam-oracle.mjs` beside it records them; Node never runs here).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_vision::conformance::{DetectorTarget, SegmenterTarget};
use adk_vision::sam::{LicensedMaskDecoder, SamMask, SamMaskDecoder, SamStreamParser};
use adk_vision::{
    AcceptanceStore, DecodeLimits, EgressAction, EgressConfig, EgressGate, FileAcceptanceStore,
    InMemoryAcceptanceStore, InMemoryEgressRecorder, LicenceAcceptance, ManifestSet, Mask,
    MaskDecoderChoice, SAM_MASK_CODEC_MANIFEST, SamConfig, SamSegmenter, SegmentPrompt, Shape,
    SourceImage, VisionCx, VisionError, VisionRuntime,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_json(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(path)).unwrap()).unwrap()
}

/// A store holding a person's acceptance of the codec's licence.
fn accepted() -> Arc<InMemoryAcceptanceStore> {
    let store = Arc::new(InMemoryAcceptanceStore::new());
    let manifests = ManifestSet::builtin().unwrap();
    let codec = manifests.get(SAM_MASK_CODEC_MANIFEST).unwrap();
    store.record(LicenceAcceptance::of(codec, "tester@example.com")).unwrap();
    store
}

/// Every mask of a recorded stream, through the crate's own stream parser.
fn recorded_masks(name: &str) -> Vec<SamMask> {
    let recording = read_json(&format!("cloud/recorded/{name}"));
    let mut parser = SamStreamParser::new();
    let mut records = Vec::new();
    for line in recording["response"].as_str().unwrap().lines() {
        let Some(data) = line.strip_prefix("data: {") else { continue };
        let event: Value = serde_json::from_str(&format!("{{{data}")).unwrap();
        if event["type"] == "response.output_text.delta" {
            records.extend(parser.push(event["delta"].as_str().unwrap()).unwrap());
        }
    }
    records.extend(parser.finish().unwrap());
    records.into_iter().map(|r| r.mask).collect()
}

#[tokio::test]
async fn the_licensed_decoder_matches_metas_parser_bit_for_bit() {
    let oracle = read_json("cloud/oracle/sam-3-1.oracle.json");
    let oracle = oracle["masks"].as_array().unwrap();
    let decoder = LicensedMaskDecoder::new(accepted().as_ref()).unwrap();
    let mut recordings: Vec<&str> =
        oracle.iter().map(|m| m["recording"].as_str().unwrap()).collect();
    recordings.dedup();
    let mut checked = 0;
    for recording in recordings {
        let masks = recorded_masks(recording);
        let rasters = decoder.decode(&masks).await.unwrap();
        let want: Vec<&Value> = oracle.iter().filter(|m| m["recording"] == recording).collect();
        assert_eq!(rasters.len(), want.len(), "{recording}");
        for ((raster, mask), want) in rasters.iter().zip(&masks).zip(want) {
            assert_eq!(
                (u64::from(mask.width), u64::from(mask.height)),
                (want["width"].as_u64().unwrap(), want["height"].as_u64().unwrap())
            );
            let bytes: Vec<u8> = raster.iter().map(|b| u8::from(*b)).collect();
            assert_eq!(
                hex::encode(Sha256::digest(&bytes)),
                want["sha256"].as_str().unwrap(),
                "{recording} mask {}",
                want["index"]
            );
            assert_eq!(
                bytes.iter().map(|b| u64::from(*b)).sum::<u64>(),
                want["set"].as_u64().unwrap()
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 16);
}

/// The drawn shapes of `shapes-960x540.png` as masks, pixel centres inside each shape.
fn truth_masks() -> Vec<(&'static str, Mask)> {
    let (w, h) = (960u32, 540u32);
    let raster = |inside: &dyn Fn(f32, f32) -> bool| {
        let mut bits = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                bits[(y * w + x) as usize] = inside(x as f32 + 0.5, y as f32 + 0.5);
            }
        }
        Mask::encode(w, h, &bits)
    };
    let tri = [[140.0f32, 480.0], [300.0, 480.0], [220.0, 330.0]];
    let in_tri = |x: f32, y: f32| {
        let sign =
            |a: [f32; 2], b: [f32; 2]| (x - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (y - b[1]);
        let (d1, d2, d3) = (sign(tri[0], tri[1]), sign(tri[1], tri[2]), sign(tri[2], tri[0]));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    vec![
        ("red rectangle", raster(&|x, y| (80.0..320.0).contains(&x) && (60.0..220.0).contains(&y))),
        ("blue circle", raster(&|x, y| (x - 640.0).powi(2) + (y - 150.0).powi(2) <= 90.0 * 90.0)),
        ("green triangle", raster(&in_tri)),
    ]
}

fn iou(a: &Mask, b: &Mask) -> f32 {
    let (a, b) = (a.decode(), b.decode());
    let inter = a.iter().zip(&b).filter(|(x, y)| **x && **y).count();
    let union = a.iter().zip(&b).filter(|(x, y)| **x || **y).count();
    if union == 0 { 0.0 } else { inter as f32 / union as f32 }
}

/// Serves each concept's recording by the phrase in its request.
struct ByConcept(Vec<(String, Value)>);

impl Respond for ByConcept {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        let text = body["input"][0]["content"][0]["text"].as_str().unwrap_or_default();
        let (_, recording) = self.0.iter().find(|(c, _)| c == text).unwrap();
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(recording["response"].as_str().unwrap().to_string())
    }
}

fn runtime(sam: SamSegmenter) -> VisionRuntime {
    let gate = EgressGate::new(
        EgressPolicy::allow_private_networks(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    VisionRuntime::builder().remote_segmenter(Arc::new(sam)).egress_gate(gate).build().unwrap()
}

#[tokio::test]
async fn recorded_masks_cover_the_drawn_shapes() {
    let truth = truth_masks();
    let server = MockServer::start().await;
    let recordings = truth
        .iter()
        .map(|(name, _)| {
            let file = format!("cloud/recorded/sam-3-1.shapes-{}.json", name.replace(' ', "-"));
            (name.to_string(), read_json(&file))
        })
        .collect();
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ByConcept(recordings))
        .mount(&server)
        .await;
    let config = SamConfig::new("test-key")
        .with_base_url(format!("{}/v1", server.uri()))
        .with_mask_decoder(MaskDecoderChoice::Licensed)
        .with_licence_acceptances(accepted());
    let runtime = runtime(SamSegmenter::new(config).unwrap());
    let image = SourceImage::decode(
        &std::fs::read(fixtures().join("cloud/shapes-960x540.png")).unwrap(),
        &DecodeLimits::default(),
    )
    .unwrap();
    let prompts: Vec<SegmentPrompt> = truth
        .iter()
        .map(|(name, _)| SegmentPrompt::Concept { text: (*name).to_string() })
        .collect();
    let out = runtime.segment(&VisionCx::new(), &image, &prompts).await.unwrap();
    for (name, want) in &truth {
        let region = out.value.iter().find(|r| r.label.as_deref() == Some(*name)).unwrap();
        let Shape::Mask(mask) = &region.shape else { panic!("{name} is not a mask") };
        let score = iou(mask, want);
        println!("recorded sam-3.1 {name}: mask IoU {score:.4}");
        // A box covers a circle at pi/4 and a triangle at 1/2; only a decoded mask scores here.
        assert!(score > 0.97, "{name}: mask IoU {score}");
    }
}

#[tokio::test]
async fn the_licensed_decoder_needs_a_recorded_acceptance() {
    let manifests = ManifestSet::builtin().unwrap();
    let codec = manifests.get(SAM_MASK_CODEC_MANIFEST).unwrap();
    assert_eq!(codec.licence.id, "LicenseRef-SAM");
    let licensed = || SamConfig::new("k").with_mask_decoder(MaskDecoderChoice::Licensed);

    // No store, and an empty project store: refused, naming the licence and the command.
    let dir = tempfile::tempdir().unwrap();
    let project = Arc::new(FileAcceptanceStore::in_project(dir.path()));
    for config in [licensed(), licensed().with_licence_acceptances(project.clone())] {
        let err = SamSegmenter::new(config).unwrap_err();
        assert!(matches!(err, VisionError::LicenceAcceptanceRequired { .. }), "{err}");
        let message = err.to_string();
        assert!(message.contains("licence acceptance required"), "{message}");
        assert!(message.contains(&codec.licence.url), "{message}");
        assert!(message.contains("adk-rust vision models accept sam-mask-codec"), "{message}");
    }

    // An acceptance of another licence URL does not cover it.
    let mut stale = codec.clone();
    stale.licence.url = "https://example.org/an-older-sam-license".into();
    project.record(LicenceAcceptance::of(&stale, "alice")).unwrap();
    assert!(SamSegmenter::new(licensed().with_licence_acceptances(project.clone())).is_err());

    project.record(LicenceAcceptance::of(codec, "alice")).unwrap();
    assert!(SamSegmenter::new(licensed().with_licence_acceptances(project)).is_ok());
}

/// Answers the conformance fixture (red `(100, 80)–(260, 200)`, blue `(380, 260)–(560,
/// 420)` on 640×480) in Meta's format with real `one_bit` masks: each a solid raster at half
/// the box's size, stretched back over the box.
struct Colours;

impl Respond for Colours {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        use base64::Engine as _;
        let body: Value = request.body_json().unwrap();
        let text = body["input"][0]["content"][0]["text"].as_str().unwrap_or_default();
        let url = body["input"][0]["content"][1]["image_url"].as_str().unwrap_or_default();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.split_once(',').map_or("", |(_, d)| d))
            .unwrap_or_default();
        let (w, h) =
            image::load_from_memory(&bytes).map(|i| (i.width(), i.height())).unwrap_or((640, 480));
        let s = |v: f32, side: u32, of: f32| (v * side as f32 / of).round() as u32;
        let line = match text {
            "red" => Some((100.0, 80.0, 260.0, 200.0)),
            "blue" => Some((380.0, 260.0, 560.0, 420.0)),
            _ => None,
        }
        .map(|(x0, y0, x1, y1)| {
            let (x1, y1, x2, y2) =
                (s(x0, w, 640.0), s(y0, h, 480.0), s(x1, w, 640.0) - 1, s(y1, h, 480.0) - 1);
            let (rw, rh) = ((x2 - x1 + 2) / 2, (y2 - y1 + 2) / 2);
            let payload =
                adk_vision_sam_codec::encode_one_bit(&vec![1; (rw * rh) as usize], rw, rh).unwrap();
            format!(
                "<0f>0<|box;x1={x1};y1={y1};x2={x2};y2={y2};w={w};h={h}|><|mask;x=0;y=0;data={rh},{rw},{payload}|>\n"
            )
        })
        .unwrap_or_default();
        let event = |kind: &str, data: Value| format!("event: {kind}\ndata: {data}\n\n");
        let mut stream = String::new();
        if !line.is_empty() {
            stream += &event(
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": line}),
            );
        }
        stream += &event(
            "response.output_text.done",
            json!({"type": "response.output_text.done", "text": line}),
        );
        stream +=
            &event("response.completed", json!({"type": "response.completed", "response": {}}));
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(stream)
    }
}

fn colours() -> SamSegmenter {
    let server = futures::executor::block_on(MockServer::start());
    futures::executor::block_on(Mock::given(method("POST")).respond_with(Colours).mount(&server));
    let config = SamConfig::new("test-key")
        .with_base_url(format!("{}/v1", server.uri()))
        .with_mask_decoder(MaskDecoderChoice::Licensed)
        .with_licence_acceptances(accepted());
    let sam = SamSegmenter::new(config).unwrap().with_hosts(["api.meta.ai"]);
    std::mem::forget(server);
    sam
}

#[tokio::test]
async fn conformance_regions_are_decoded_masks() {
    let fixture = adk_vision::conformance::Fixture::shapes();
    let runtime = runtime(colours());
    let out = runtime
        .segment(&VisionCx::new(), &fixture.image, &[SegmentPrompt::Concept { text: "red".into() }])
        .await
        .unwrap();
    let Shape::Mask(mask) = &out.value[0].shape else { panic!("not a mask") };
    assert_eq!(mask.area() as f32, mask.bbox().unwrap().area());
}

mod segmenter_conformance {
    use super::*;

    adk_vision::segmenter_conformance_tests! {
        backend = SegmenterTarget::Remote(Arc::new(colours())),
    }
}

mod detector_conformance {
    use super::*;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Remote(Arc::new(colours())),
    }
}
