//! S5, one region in four frames: the twelve targets of a 3024×1964 screenshot of a display
//! at device pixel ratio 2, answered by Claude (pixels of the image it processed), Gemini
//! (`box_2d`, 0–1000), `sam-3.1` (source pixels) and RF-DETR (model-input pixels with a
//! letterbox), all mapped to source pixels and then to screen points.
//!
//! Claude's and `sam-3.1`'s answers are their recorded live responses, served to the real
//! clients. Gemini answered no request when the fixtures were recorded, so its answer is
//! written in its documented format; RF-DETR finds no widgets in a screenshot, so its
//! model-input boxes come from `adk-vision/tests/fixtures/s5/generate.py`.

use std::sync::Arc;

use adk_core::Llm;
use adk_core::egress::EgressPolicy;
use adk_vision::{
    BBox, BackendId, CoordFrame, DescribeSpec, EgressAction, EgressConfig, EgressGate,
    InMemoryEgressRecorder, Letterbox, LlmDescriber, RawRegion, Region, RegionNormalizer,
    SamConfig, SamSegmenter, ScreenSpace, SegmentPrompt, SourceImage, Transform, TransformChain,
    VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::Checks;
use crate::fixtures::{crate_fixtures, edge_error, load_image, read_json};
use crate::replay::{Recording, serve_always};

fn quad(value: &Value) -> Option<BBox> {
    let v: Vec<f32> =
        value.as_array()?.iter().filter_map(|n| n.as_f64().map(|n| n as f32)).collect();
    (v.len() == 4).then(|| BBox::new(v[0], v[1], v[2], v[3]))
}

/// A runtime that releases to `backend` with no rule in the way: S5 is about frames.
fn open_gate() -> EgressGate {
    EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    )
}

/// Each target's region, by label, in source pixels.
fn by_label(regions: &[Region], targets: &[(String, BBox)]) -> Vec<Option<BBox>> {
    targets
        .iter()
        .map(|(name, _)| {
            regions
                .iter()
                .find(|r| {
                    r.label.as_deref().is_some_and(|l| {
                        let l = l.to_ascii_lowercase();
                        l.contains(name.as_str()) || name.contains(&l)
                    })
                })
                .and_then(|r| r.shape.bbox())
        })
        .collect()
}

/// Each target's region by best overlap, for a backend that labels by concept.
fn by_overlap(regions: &[Region], targets: &[(String, BBox)]) -> Vec<Option<BBox>> {
    targets
        .iter()
        .map(|(_, want)| {
            regions
                .iter()
                .filter_map(|r| r.shape.bbox())
                .max_by(|a, b| a.iou(want).total_cmp(&b.iou(want)))
        })
        .collect()
}

async fn claude(image: &SourceImage, prompt: &str) -> Result<Vec<Region>> {
    let server = MockServer::start().await;
    let recording = crate_fixtures().join("cloud/recorded/claude-sonnet-5-5.s5-screen.json");
    serve_always(&server, &Recording::load(&recording)?).await;
    let config = adk_model::anthropic::AnthropicConfig::new("recorded", "claude-sonnet-5-5")
        .allow_insecure_http()
        .with_base_url(server.uri());
    let client: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config)?);
    let describer = LlmDescriber::remote(client, ["api.anthropic.com"])?
        .with_effort(adk_core::Effort::Low)
        .with_max_output_tokens(4_000);
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(open_gate())
        .build()?;
    let spec = DescribeSpec::new(prompt).with_regions();
    Ok(runtime.describe(&VisionCx::new(), &[image], &spec).await?.value.regions)
}

async fn gemini(image: &SourceImage, prompt: &str, answers: &Value) -> Result<Vec<Region>> {
    let regions: Vec<Value> = answers
        .as_array()
        .context("s5.json has Gemini answers")?
        .iter()
        .map(|a| json!({ "label": a["name"], "box_2d": a["box_2d"] }))
        .collect();
    let text = json!({ "text": "twelve widgets", "regions": regions }).to_string();
    let body = json!({
        "candidates": [{ "content": { "role": "model", "parts": [{ "text": text }] }, "finishReason": "STOP" }],
        "usageMetadata": { "promptTokenCount": 1600, "candidatesTokenCount": 300, "totalTokenCount": 1900 }
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(".*:generateContent$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let client: Arc<dyn Llm> = Arc::new(adk_model::GeminiModel::new_with_base_url(
        "recorded",
        "gemini-3.7-flash",
        format!("{}/v1beta/", server.uri()),
    )?);
    let describer = LlmDescriber::remote(client, ["generativelanguage.googleapis.com"])?;
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(open_gate())
        .build()?;
    let spec = DescribeSpec::new(prompt).with_regions();
    Ok(runtime.describe(&VisionCx::new(), &[image], &spec).await?.value.regions)
}

async fn sam(image: &SourceImage) -> Result<Vec<Region>> {
    let server = MockServer::start().await;
    let recording = crate_fixtures().join("cloud/recorded/sam-3-1.s5-screen-rectangle.json");
    serve_always(&server, &Recording::load(&recording)?).await;
    let sam = SamSegmenter::new(
        SamConfig::new("recorded").with_base_url(format!("{}/v1", server.uri())),
    )?
    .with_hosts(["api.meta.ai"]);
    let runtime = VisionRuntime::builder()
        .remote_segmenter(Arc::new(sam))
        .egress_gate(open_gate())
        .build()?;
    let prompt = [SegmentPrompt::Concept { text: "rectangle".into() }];
    Ok(runtime.segment(&VisionCx::new(), image, &prompt).await?.value)
}

fn rf_detr(
    image: &SourceImage,
    fixture: &Value,
    targets: &[(String, BBox)],
) -> Result<Vec<Option<BBox>>> {
    let (w, h) = image.size();
    let frame = CoordFrame::ModelInput { width: 640, height: 640, letterbox: Letterbox::Center };
    let normalizer = RegionNormalizer::new(
        image.digest(),
        TransformChain::new(w, h),
        frame,
        BackendId::new("rf-detr-n"),
    );
    let answers = fixture["rfdetr"]["answers"].as_array().context("s5.json has RF-DETR answers")?;
    targets
        .iter()
        .map(|(name, _)| {
            let answer = answers.iter().find(|a| a["name"] == name.as_str());
            let Some(b) = answer.and_then(|a| quad(&a["box"])) else { return Ok(None) };
            let raw = RawRegion::boxed(frame.box_from_raw([b.x0, b.y0, b.x1, b.y1]), name, 0.9);
            Ok(normalizer.region(raw)?.and_then(|r| r.shape.bbox()))
        })
        .collect()
}

/// A small deterministic generator, so the round trip runs the same chains every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }
}

/// Random chains of crops, resizes, pads and rotations: a box mapped to the target and back
/// lands within 0.5 px of where it started.
fn round_trips(cases: usize) -> Result<(usize, f32)> {
    let mut rng = Rng(0x0005_eed0_f5c7_u64);
    let mut worst = 0f32;
    for _ in 0..cases {
        let (w, h) = (16 + rng.next() % 4000, 16 + rng.next() % 4000);
        let mut chain = TransformChain::new(w, h);
        for _ in 0..rng.next() % 7 {
            let (cw, ch) = chain.output_size();
            let step = match rng.next() % 4 {
                0 if cw > 2 && ch > 2 => {
                    let width = 1 + rng.next() % (cw - 1);
                    let height = 1 + rng.next() % (ch - 1);
                    let x = rng.next() % (cw - width + 1);
                    let y = rng.next() % (ch - height + 1);
                    Transform::Crop { x, y, width, height }
                }
                1 => Transform::Resize {
                    width: 8 + rng.next() % 4000,
                    height: 8 + rng.next() % 4000,
                },
                2 => Transform::Pad {
                    left: rng.next() % 64,
                    top: rng.next() % 64,
                    right: rng.next() % 64,
                    bottom: rng.next() % 64,
                },
                _ => Transform::Rotate { quarter_turns: (rng.next() % 4) as u8 },
            };
            chain.push(step)?;
        }
        let x0 = (rng.next() % w) as f32;
        let y0 = (rng.next() % h) as f32;
        let b = BBox::new(
            x0,
            y0,
            x0 + 1.0 + (rng.next() % 200) as f32,
            y0 + 1.0 + (rng.next() % 200) as f32,
        );
        let back = chain.bbox_to_source(&chain.bbox_to_target(&b));
        worst = worst.max(edge_error(&back, &b));
    }
    Ok((cases, worst))
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note("Claude Sonnet 5.5 and sam-3.1 on recorded live responses; Gemini and RF-DETR answers synthetic, in their documented frames");
    let fixture = read_json(&crate_fixtures().join("s5/s5.json"))?;
    let image = load_image(&crate_fixtures().join("s5/s5-screen.png"))?;
    let targets: Vec<(String, BBox)> = fixture["targets"]
        .as_array()
        .context("s5.json has targets")?
        .iter()
        .filter_map(|t| Some((t["name"].as_str()?.to_string(), quad(&t["box"])?)))
        .collect();
    let screen = ScreenSpace::new(
        fixture["image"]["display"].as_str().unwrap_or("built-in"),
        [0.0, 0.0],
        fixture["image"]["devicePixelRatio"].as_f64().unwrap_or(2.0) as f32,
    );
    let names = targets.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
    let prompt = format!(
        "This is a screenshot of a form. Locate each of these labelled widgets: {names}. \
         Label each region with the widget's name exactly as given (for example `name-field`)."
    );

    let answers = [
        (
            "Claude (pixels of the image it processed)",
            by_label(&claude(&image, &prompt).await?, &targets),
        ),
        (
            "Gemini (box_2d on 0–1000)",
            by_label(&gemini(&image, &prompt, &fixture["gemini"]).await?, &targets),
        ),
        ("sam-3.1 (source pixels)", by_overlap(&sam(&image).await?, &targets)),
        ("RF-DETR (640×640 model input, letterboxed)", rf_detr(&image, &fixture, &targets)?),
    ];
    let mut landed = 0;
    let mut clicks = 0;
    for (backend, found) in &answers {
        let mut worst = 0f32;
        let mut within = 0;
        for ((_, want), got) in targets.iter().zip(found) {
            let Some(got) = got else { continue };
            let error = edge_error(got, want);
            worst = worst.max(error);
            if error <= 2.0 && got.is_within(image.size().0, image.size().1) {
                within += 1;
            }
            let click = screen.to_points(got.center());
            if screen.bbox_to_points(want).contains(click) {
                clicks += 1;
            }
        }
        landed += within;
        checks.check(
            &format!("{backend}: {within}/12 within 2 px (worst {worst:.2} px)"),
            within == 12,
        );
    }
    checks.check("all 48 regions land within 2 px of the truth in source pixels", landed == 48);
    checks.check(
        "mapped to screen points at device pixel ratio 2, every click point falls inside its target",
        clicks == 48,
    );
    let (cases, worst) = round_trips(1_000)?;
    checks.check(
        &format!("{cases} random transform chains round-trip within 0.5 px (worst {worst:.4} px)"),
        worst <= 0.5,
    );
    Ok(checks)
}
