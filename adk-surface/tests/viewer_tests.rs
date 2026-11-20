//! The annotated viewer through the engine and the web API (spec 3.7 §8.2, X12): the answer
//! records the vision tools leave in a session project into the boards' states, only the
//! session's owner opens them, images are served only with their digest, an answer exports
//! as its `RegionSet`, and a kept or corrected region becomes an `adk-eval` case that
//! `cargo adk eval` loads.
//!
//! The fixture records are `tests/fixtures/viewer/*.answer.json` (`generate.py`). The views
//! they project are the web fixture's (`web/src/fixture/viewer/*.view.json`), so the
//! screenshots beside the signed-off boards are of the engine's own output;
//! `ADK_SURFACE_WRITE_VIEWER=1` rewrites them.

#![cfg(feature = "web")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::{Part, RequestContext};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::viewer::{ExampleScope, LabelledExample, RegionCorrection, ViewerView};
use adk_surface::{Caller, LocalSource, Origin, Place, SurfaceEngine, SurfaceError, SurfaceKind};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::FixedOffset;
use serde_json::{Value, json};
use sha2::Digest as _;
use tower::ServiceExt;

const APP: &str = "desk";
const OWNER: &str = "ana";
const PAGE: &str = "sha256:556b495de3f3b8c95e33b8ee5511552ada624e85f74b602af3ee2baea8a22654";
const FRAME: &str = "sha256:594a57e5212677f5d8ed71e730129b7ca43fbf545c5189b27f4c769695c971ae";
const FRAME_OK: &str = "sha256:45e055ba4f630e4bfa506f38fb8506265e6774f6b7767bbfd482ec0272440903";
const VFRAME: &str = "sha256:4484960c0d621670c96be084aa892ae1a277890c601b454f880377d8b3fd55ed";

fn record(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/viewer/{name}.answer.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

struct Fixture {
    engine: SurfaceEngine,
    artifacts: Arc<InMemoryArtifactService>,
}

async fn fixture(eval_root: Option<&Path>) -> Fixture {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    for session in ["s-desk", "s-compare", "s-line", "s-research", "s-new"] {
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await
            .unwrap();
    }
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let mut source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>);
    if let Some(root) = eval_root {
        source = source.eval_fixtures(root);
    }
    let engine = SurfaceEngine::builder(Arc::new(source.build().unwrap()))
        .places([Place::web(), Place::phone()])
        .agent_name("desk", "Document desk")
        .agent_name("inspector", "Inspector")
        .agent_name("research", "Research")
        .utc_offset(FixedOffset::east_opt(3 * 3600).unwrap())
        .build();
    Fixture { engine, artifacts }
}

async fn save(artifacts: &InMemoryArtifactService, session: &str, name: &str, part: Part) {
    artifacts
        .save(SaveRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: session.into(),
            file_name: name.into(),
            part,
            version: None,
        })
        .await
        .unwrap();
}

async fn put_record(fixture: &Fixture, session: &str, record: &Value) {
    let call = record["callId"].as_str().unwrap();
    let part = Part::InlineData {
        mime_type: "application/json".into(),
        data: serde_json::to_vec(record).unwrap(),
        uri: None,
        annotations: None,
    };
    save(&fixture.artifacts, session, &format!("vision-answer-{call}.json"), part).await;
}

async fn put_image(fixture: &Fixture, session: &str, name: &str, bytes: &[u8]) {
    let part = Part::InlineData { mime_type: "image/png".into(), data: bytes.to_vec(), uri: None, annotations: None };
    save(&fixture.artifacts, session, name, part).await;
}

fn owner() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn png(width: u32, height: u32, color: [u8; 4]) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels: Vec<u8> = (0..width * height).flat_map(|_| color).collect();
        encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
    }
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    (bytes, digest)
}

/// The board states, from the fixture records.
async fn board_states() -> Vec<(&'static str, ViewerView)> {
    let fixture = fixture(Some(Path::new("/nonexistent-eval-root"))).await;
    put_record(&fixture, "s-desk", &record("document-a")).await;
    put_record(&fixture, "s-desk", &record("compare-b")).await;
    put_record(&fixture, "s-compare", &record("compare-a")).await;
    put_record(&fixture, "s-compare", &record("compare-b")).await;
    put_record(&fixture, "s-line", &record("photo")).await;
    put_record(&fixture, "s-line", &record("empty")).await;
    put_record(&fixture, "s-research", &record("video")).await;
    let engine = &fixture.engine;
    let view = |session: &'static str, image: &'static str, answer: Option<&'static str>, compare: Option<&'static str>| async move {
        engine.viewer(&owner(), session, image, answer, compare).await.unwrap()
    };
    vec![
        ("document", view("s-desk", PAGE, Some("c-31"), None).await),
        ("compare", view("s-compare", PAGE, Some("c-31"), Some("c-34")).await),
        ("photo", view("s-line", FRAME, None, None).await),
        ("empty", view("s-line", FRAME_OK, None, None).await),
        ("video", view("s-research", VFRAME, None, None).await),
    ]
}

fn web_fixture(state: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("web/src/fixture/viewer/{state}.view.json"))
}

#[tokio::test]
async fn the_boards_states_are_the_engines_own_projection() {
    for (state, view) in board_states().await {
        let mut value = serde_json::to_value(&view).unwrap();
        // The fixture's eval files are the boards' (board choice V13), not this machine's.
        value["examples"]["canSave"] = json!(true);
        value["examples"]["files"] = json!(["evals/vision/invoices.test.json", "evals/vision/line-2.test.json"]);
        value["examples"]["file"] = json!(if state == "document" || state == "compare" {
            "evals/vision/invoices.test.json"
        } else {
            "evals/vision/line-2.test.json"
        });
        let path = web_fixture(state);
        if std::env::var_os("ADK_SURFACE_WRITE_VIEWER").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap() + "\n").unwrap();
            continue;
        }
        let drawn: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(drawn, value, "{state}: run with ADK_SURFACE_WRITE_VIEWER=1 to rewrite {}", path.display());
    }
}

#[tokio::test]
async fn a_document_page_reads_as_the_board() {
    let states = board_states().await;
    let view = &states.iter().find(|(state, _)| *state == "document").unwrap().1;
    assert_eq!(view.title, "INV-2291 · page 1");
    assert_eq!(view.meta, "1654 × 2339 · INV-2291.pdf, page 1 at 200 dpi");
    assert_eq!(view.image.digest_short, "sha256:556b49…a22654");
    assert_eq!(view.answers.len(), 2, "two answers for this page");
    let answer = view.answer.as_ref().unwrap();
    assert_eq!((answer.name.as_str(), answer.place.as_str()), ("PP-OCRv6 small", "on this computer"));
    let orders: Vec<u32> = answer.regions.iter().filter_map(|r| r.order).collect();
    assert_eq!(orders, (1..=11).collect::<Vec<_>>(), "listed in reading order");
    let chips: Vec<(String, u32)> = answer.labels.iter().map(|l| (l.label.clone(), l.count)).collect();
    assert_eq!(
        chips,
        [("title", 1), ("text", 5), ("table", 1), ("seal", 1), ("handwriting", 2), ("checkbox", 1)]
            .map(|(l, n)| (l.to_string(), n))
    );
    let five = answer.regions.iter().find(|r| r.order == Some(5)).unwrap();
    assert_eq!(answer.selected, Some(five.index), "the escalated value is selected");
    assert_eq!(five.heading, "5 · handwriting");
    assert_eq!(five.detail, "PO-7781S");
    assert_eq!(five.tag, "5 · handwriting · 0.58");
    let text = |label: &str| {
        five.facts
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.parts.iter().map(|p| p.text.as_str()).collect::<String>())
            .unwrap()
    };
    assert_eq!(text("Read as"), "PO-7781S · 0.58");
    assert_eq!(text("Now"), "PO-77815 · Gemini read a covered crop of it, sha256:d4781e…272822");
    assert_eq!(text("Box"), "1250, 466 → 1532, 540 source pixels");
    assert_eq!(text("Model"), "pp-ocrv6-small@3f2a9c1 · Apache-2.0");
    assert_eq!(text("Answered in"), "Model-input pixels, 960 × 960, letterboxed");
    assert_eq!(
        five.chain,
        [
            "Page 1 of INV-2291.pdf at 200 dpi · 1654 × 2339",
            "Resized to 679 × 960",
            "Padded 140 left, 141 right · 960 × 960",
            "Answer mapped back to source pixels",
        ]
    );
    let call: String = answer.run.call.iter().map(|p| p.text.as_str()).collect();
    assert_eq!((answer.run.text.as_str(), call.as_str()), ("Document desk · Northwind batch · 10:31", "vision_read · c-31"));
}

#[tokio::test]
async fn a_photo_carries_masks_and_its_measurement() {
    let states = board_states().await;
    let view = &states.iter().find(|(state, _)| *state == "photo").unwrap().1;
    let answer = view.answer.as_ref().unwrap();
    assert_eq!(answer.name, "RF-DETR S + SAM 2.1 Tiny");
    let rows: Vec<(String, String)> =
        answer.regions.iter().map(|r| (r.label.clone(), r.detail.clone())).collect();
    assert_eq!(
        rows,
        [
            ("seal tear", "mask · 14.3 mm"),
            ("seal", "mask"),
            ("part", "box"),
            ("part", "box"),
            ("part", "box"),
            ("part", "box · at the edge"),
            ("fiducial", "box · calibration"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    let tear = &answer.regions[0];
    assert_eq!(tear.heading, "Seal tear");
    assert_eq!(tear.tag, "seal tear · 0.91 · 14.3 mm");
    let facts: Vec<String> = tear
        .facts
        .iter()
        .map(|f| format!("{}: {}", f.label, f.parts.iter().map(|p| p.text.as_str()).collect::<String>()))
        .collect();
    assert_eq!(facts[1], "Found by: inspector-rfdetr-s-v3 · your fine-tune, your manifest");
    assert_eq!(facts[2], "Outlined by: sam2.1-tiny@8c1e2f0 · from the box");
    assert_eq!(facts[0], "Shape: Mask, 2,775 px · box 922, 490 → 1024, 532");
    assert_eq!(facts[3], "Answered in: Model-input pixels, 1024 × 1024, letterboxed");
    assert_eq!(
        tear.chain,
        ["Frame 1920 × 1080", "Resized to 1024 × 576", "Padded 224 top and bottom · 1024 × 1024", "Mask mapped back to source pixels"]
    );
    let seal = &answer.regions[1];
    let area = seal.mask.as_ref().unwrap().area as f64;
    assert!((area - std::f64::consts::PI * 84.0 * 84.0).abs() < 400.0, "the seal is a disc of radius 84: {area}");
    let measurements = answer.measurements.as_ref().unwrap();
    assert_eq!(
        measurements.calibration,
        "In the belt's plane, from the fiducial: 40.0 mm is 245 px, so 6.12 px per mm. Valid only in that plane."
    );
    let call: String = answer.run.call.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(call, "vision_segment c-2207 · vision_measure c-2208");
}

#[tokio::test]
async fn two_backends_compare_region_by_region() {
    let states = board_states().await;
    let view = &states.iter().find(|(state, _)| *state == "compare").unwrap().1;
    let compare = view.compare.as_ref().unwrap();
    assert_eq!((compare.agree, compare.differ, compare.only_one), (9, 2, 1));
    assert_eq!(compare.a.answer.stats, ["11 regions", "1.1 s", "$0"]);
    assert_eq!(compare.b.answer.stats, ["12 regions", "14.8 s", "$0"]);
    let differences: Vec<(u32, String, String)> =
        compare.differences.iter().map(|d| (d.number, d.a.clone(), d.b.clone())).collect();
    assert_eq!(
        differences,
        [(3, "06.11.2028 · 0.71", "06.11.2026 · 0.95"), (5, "PO-7781S · 0.58", "PO-77815 · 0.83"), (12, "—", "Page 1 of 1 · 0.86")]
            .map(|(n, a, b)| (n, a.to_string(), b.to_string()))
    );
    assert!(compare.overlap_text.contains("mean IoU 0.9"), "{}", compare.overlap_text);
}

#[tokio::test]
async fn an_empty_answer_is_an_answer_with_its_call_and_cost() {
    let states = board_states().await;
    let view = &states.iter().find(|(state, _)| *state == "empty").unwrap().1;
    let answer = view.answer.as_ref().unwrap();
    assert!(answer.regions.is_empty());
    assert_eq!((answer.name.as_str(), answer.place.as_str()), ("sam-3.1", "Meta, in the cloud"));
    let empty = answer.empty.as_ref().unwrap();
    let sentence: String = empty.text.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(
        sentence,
        "sam-3.1 answered with an empty result: nothing in this frame matched “torn seal”. That is an answer, not an error."
    );
    let facts: Vec<String> = empty
        .facts
        .iter()
        .map(|f| format!("{}: {}", f.label, f.parts.iter().map(|p| p.text.as_str()).collect::<String>()))
        .collect();
    assert_eq!(
        facts,
        [
            "Asked: vision_segment · concept “torn seal” · c-2311",
            "Sent: A released copy to api.meta.ai · nothing needed covering",
            "Cost: $0.0025 · billed though nothing matched",
            "Run: Inspector · line 2 · 11:02:44",
        ]
    );
}

#[tokio::test]
async fn a_video_frame_carries_its_tracks() {
    let states = board_states().await;
    let view = &states.iter().find(|(state, _)| *state == "video").unwrap().1;
    let answer = view.answer.as_ref().unwrap();
    assert_eq!(answer.name, "RF-DETR M + ByteTrack");
    let tags: Vec<&str> = answer.regions.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["forklift · T4 · 0.94", "pallet · T9 · 0.91", "pallet · T11 · 0.83", "person · blurred · T2 · 0.90"]);
    let forklift = &answer.regions[0];
    let facts: Vec<String> = forklift
        .facts
        .iter()
        .map(|f| format!("{}: {}", f.label, f.parts.iter().map(|p| p.text.as_str()).collect::<String>()))
        .collect();
    assert_eq!(
        facts,
        [
            "Seen: 10:12.2 → 10:40.0 · 139 frames",
            "Box now: 570, 556 → 1000, 920",
            "Found by: warehouse-rfdetr-m · your manifest",
            "Tracked by: ByteTrack · trackforge 0.5, on this computer",
            "Answered in: Model-input pixels, 640 × 640, letterboxed",
        ]
    );
    let video = answer.video.as_ref().unwrap();
    assert_eq!((video.at_text.as_str(), video.length_text.as_str()), ("10:34.1", "20:00"));
    assert_eq!(video.frame_text, "frame 19,023 · sampled 5 a second");
    let chips: Vec<(String, String)> = video.tracks.iter().map(|t| (t.name.clone(), t.frames_text.clone())).collect();
    assert_eq!(chips, [("T4", "139"), ("T9", "1.2k"), ("T11", "980"), ("T2", "230")].map(|(a, b)| (a.to_string(), b.to_string())));
    assert_eq!(view.min_score, 0.6);
}

#[tokio::test]
async fn only_the_sessions_owner_opens_its_viewer_and_images() {
    let fixture = fixture(None).await;
    put_record(&fixture, "s-desk", &record("document-a")).await;
    let other = Caller::human("bo", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"]);
    let opened = fixture.engine.viewer(&other, "s-desk", PAGE, None, None).await;
    assert!(matches!(opened, Err(SurfaceError::NotFound { .. })), "{opened:?}");
    let image = fixture.engine.viewer_image(&other, "s-desk", PAGE).await;
    assert!(matches!(image, Err(SurfaceError::NotFound { .. })), "{image:?}");
    let export = fixture.engine.viewer_export(&other, "s-desk", "c-31", None).await;
    assert!(matches!(export, Err(SurfaceError::NotFound { .. })), "{export:?}");
}

#[tokio::test]
async fn an_image_is_served_only_with_its_digest() {
    let fixture = fixture(None).await;
    let (bytes, digest) = png(40, 20, [10, 20, 30, 255]);
    let (_, other) = png(40, 20, [90, 20, 30, 255]);
    put_image(&fixture, "s-new", &format!("vision-{}.png", &digest[7..]), &bytes).await;
    put_image(&fixture, "s-new", &format!("vision-{}.png", &other[7..]), &bytes).await;
    let served = fixture.engine.viewer_image(&owner(), "s-new", &digest).await.unwrap();
    assert_eq!(served.bytes, bytes);
    let tampered = fixture.engine.viewer_image(&owner(), "s-new", &other).await;
    assert!(matches!(tampered, Err(SurfaceError::Invalid(_))), "{tampered:?}");
}

#[tokio::test]
async fn an_image_nothing_answered_for_opens_by_its_name() {
    let fixture = fixture(None).await;
    let (bytes, digest) = png(40, 20, [10, 20, 30, 255]);
    put_image(&fixture, "s-new", "id-card.png", &bytes).await;
    let view = fixture.engine.viewer(&owner(), "s-new", "id-card.png", None, None).await.unwrap();
    assert_eq!(view.image.digest, digest);
    assert_eq!((view.image.width, view.image.height), (40, 20));
    assert!(view.answers.is_empty() && view.answer.is_none());
    assert_eq!(view.title, "id-card");
}

/// A `vision_read` record of a real image, two blocks, the second read as `PO-7781S`.
fn page_of(digest: &str, width: u32, height: u32) -> Value {
    let origin = json!({
        "backend": "pp-ocrv6-small",
        "manifest": "pp-ocrv6-small",
        "frame": { "kind": "sentPixels" },
        "chain": { "sourceWidth": width, "sourceHeight": height, "steps": [] },
    });
    json!({
        "kind": "adkVisionAnswer",
        "version": 1,
        "tool": "vision_read",
        "callId": "c-31",
        "agent": "desk",
        "at": "2026-11-20T07:31:00Z",
        "image": { "digest": digest, "width": width, "height": height, "mimeType": "image/png", "name": "invoice.png" },
        "args": { "image": "invoice.png" },
        "regionSet": { "image": digest, "width": width, "height": height, "regions": [
            { "image": digest, "shape": { "kind": "box", "x0": 2.0, "y0": 2.0, "x1": 30.0, "y1": 8.0 }, "label": "title", "score": 0.99, "origin": origin },
            { "image": digest, "shape": { "kind": "box", "x0": 4.0, "y0": 10.0, "x1": 36.0, "y1": 18.0 }, "label": "handwriting", "score": 0.58, "origin": origin },
        ] },
        "page": { "blocks": [ { "kind": "title", "text": "INVOICE" }, { "kind": "handwriting", "text": "PO-7781S" } ], "readingOrder": [0, 1] },
        "calls": [ { "task": "read", "backend": "pp-ocrv6-small", "locality": { "kind": "inProcess" }, "durationMs": 1100, "costMicroUsd": 0 } ],
    })
}

#[tokio::test]
async fn a_corrected_region_is_an_eval_case_that_adk_eval_loads() {
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture(Some(root.path())).await;
    let (bytes, digest) = png(40, 20, [240, 240, 240, 255]);
    put_image(&fixture, "s-new", &format!("vision-{}.png", &digest[7..]), &bytes).await;
    let page = page_of(&digest, 40, 20);
    put_record(&fixture, "s-new", &page).await;

    let file = "evals/vision/invoices.test.json";
    let mut example = LabelledExample {
        answer: "c-31".into(),
        file: file.into(),
        case_name: "inv-po-number".into(),
        scope: ExampleScope::Region,
        region: Some(RegionCorrection {
            index: Some(1),
            label: "handwriting".into(),
            text: Some("PO-77815".into()),
            bbox: [5.0, 10.0, 35.0, 18.0],
        }),
    };
    let saved = fixture.engine.save_example(&owner(), "s-new", &example).await.unwrap();
    assert_eq!(saved.image, format!("images/{}.png", &digest[7..]));
    assert_eq!(saved.text, "Saved to evals/vision/invoices.test.json as inv-po-number. The run's answer stays as it was.");

    example.case_name = "inv-page".into();
    example.scope = ExampleScope::Page;
    fixture.engine.save_example(&owner(), "s-new", &example).await.unwrap();
    // Saving again under a name replaces that case.
    fixture.engine.save_example(&owner(), "s-new", &example).await.unwrap();

    // `cargo adk eval` reads the file through adk-eval, which verifies each image by digest.
    let loaded = adk_eval::TestFile::load(root.path().join(file)).unwrap();
    let names: Vec<&str> = loaded.eval_cases.iter().map(|case| case.eval_id.as_str()).collect();
    assert_eq!(names, ["inv-po-number", "inv-page"]);
    let expected = |i: usize| loaded.eval_cases[i].conversation[0].expected_vision.clone().unwrap();
    assert_eq!(expected(0).text.as_deref(), Some("PO-77815"));
    assert_eq!(expected(1).text.as_deref(), Some("INVOICE\nPO-77815"));
    assert!(loaded.eval_cases[0].unloaded_images().is_empty(), "the image is loaded and verified");

    // The run's own answer is untouched, and the export is still its RegionSet.
    let exported = fixture.engine.viewer_export(&owner(), "s-new", "c-31", None).await.unwrap();
    assert_eq!(exported, page["regionSet"]);
    let view = fixture.engine.viewer(&owner(), "s-new", &digest, None, None).await.unwrap();
    assert_eq!(view.answer.unwrap().regions[1].detail, "PO-7781S");
    assert_eq!(view.examples.files, [file]);

    // A box outside the image and a file outside the root are refused.
    example.region.as_mut().unwrap().bbox = [5.0, 10.0, 45.0, 18.0];
    let outside = fixture.engine.save_example(&owner(), "s-new", &example).await;
    assert!(matches!(outside, Err(SurfaceError::Invalid(_))), "{outside:?}");
    example.region.as_mut().unwrap().bbox = [5.0, 10.0, 35.0, 18.0];
    example.file = "../escape.test.json".into();
    let escape = fixture.engine.save_example(&owner(), "s-new", &example).await;
    assert!(matches!(escape, Err(SurfaceError::Invalid(_))), "{escape:?}");
}

#[tokio::test]
async fn an_example_with_nothing_in_it_expects_no_regions() {
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture(Some(root.path())).await;
    let (bytes, digest) = png(40, 20, [30, 30, 30, 255]);
    put_image(&fixture, "s-new", &format!("vision-{}.png", &digest[7..]), &bytes).await;
    let mut empty = record("empty");
    empty["image"]["digest"] = json!(digest);
    empty["image"]["width"] = json!(40);
    empty["image"]["height"] = json!(20);
    empty["regionSet"] = json!({ "image": digest, "width": 40, "height": 20, "regions": [] });
    put_record(&fixture, "s-new", &empty).await;
    let example = LabelledExample {
        answer: "c-2311".into(),
        file: "evals/vision/line-2.test.json".into(),
        case_name: "line-2-no-torn-seal".into(),
        scope: ExampleScope::Nothing,
        region: None,
    };
    fixture.engine.save_example(&owner(), "s-new", &example).await.unwrap();
    let loaded = adk_eval::TestFile::load(root.path().join("evals/vision/line-2.test.json")).unwrap();
    let turn = &loaded.eval_cases[0].conversation[0];
    assert_eq!(turn.expected_vision.as_ref().unwrap().count, Some(0));
}

fn api(engine: SurfaceEngine, device: bool) -> axum::Router {
    let mut metadata = std::collections::HashMap::new();
    if device {
        metadata.insert(adk_kernel::pairing::DEVICE_ID_METADATA.to_string(), "phone-1".to_string());
        metadata.insert(adk_kernel::pairing::DEVICE_KIND_METADATA.to_string(), "phone".to_string());
    }
    axum::Router::new().nest("/api", adk_surface::web::router(engine)).layer(axum::Extension(Some(
        RequestContext { user_id: OWNER.into(), scopes: vec!["owner".into()], metadata: metadata.into_iter().collect() },
    )))
}

#[tokio::test]
async fn the_web_api_serves_the_viewer_its_image_and_the_regionset() {
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture(Some(root.path())).await;
    let (bytes, digest) = png(40, 20, [240, 240, 240, 255]);
    put_image(&fixture, "s-new", &format!("vision-{}.png", &digest[7..]), &bytes).await;
    put_record(&fixture, "s-new", &page_of(&digest, 40, 20)).await;
    let app = api(fixture.engine.clone(), false);
    let encoded = digest.replace(':', "%3A");

    let get = |uri: String| {
        let app = app.clone();
        async move { app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap() }
    };
    let response = get(format!("/api/surface/vision/s-new/images/{encoded}/view")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let view: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(view["image"]["href"], json!(format!("/api/surface/vision/s-new/images/{encoded}")));

    let response = get(format!("/api/surface/vision/s-new/images/{encoded}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(to_bytes(response.into_body(), usize::MAX).await.unwrap(), bytes);

    let response = get("/api/surface/vision/s-new/answers/c-31/regions".into()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-disposition"], "attachment; filename=\"c-31.regions.json\"");

    let body = json!({
        "answer": "c-31",
        "file": "evals/vision/invoices.test.json",
        "caseName": "inv-title",
        "scope": "region",
        "region": { "index": 0, "label": "title", "text": "INVOICE", "box": [2, 2, 30, 8] },
    });
    let post = |app: axum::Router| {
        let body = body.clone();
        async move {
            app.oneshot(
                Request::post("/api/surface/vision/s-new/examples")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    assert_eq!(post(app.clone()).await.status(), StatusCode::OK);
    // A phone reads the viewer and saves nothing (board choice V15).
    let phone = api(fixture.engine.clone(), true);
    let refused = post(phone.clone()).await;
    assert_ne!(refused.status(), StatusCode::OK);
    let read = phone
        .oneshot(Request::get(format!("/api/surface/vision/s-new/images/{encoded}/view")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
}
