//! Live describers on real providers: each locates the twelve S5 widgets of a 3024×1964
//! screenshot and the five shapes of a 960×540 image, and every answer is scored against
//! the drawn truth in source pixels.
//!
//! `#[ignore]`d; each test needs its provider's key and skips without it:
//!
//! ```text
//! ANTHROPIC_API_KEY=... cargo test -p adk-vision --features cloud,nim \
//!     --test cloud_live_tests -- --ignored --nocapture
//! ```
//!
//! `ADK_VISION_<PROVIDER>_BASE` points a provider at a recording proxy; the recorded
//! exchanges are the fixtures `cloud_tests` replays.

use std::path::Path;
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, Effort, InMemorySpendLedger, Llm, ModelCallScope, RunBudget, SpendKey,
    SpendLedger, SpendPeriod, UsageUnit,
};
use adk_vision::cloud::nim::NimConfig;
use adk_vision::{
    BBox, DecodeLimits, DescribeSpec, EgressAction, EgressConfig, EgressGate,
    InMemoryEgressRecorder, LlmDescriber, SourceImage, Transform, VisionCx, VisionRuntime,
};
use serde_json::Value;

struct Case {
    name: &'static str,
    image: SourceImage,
    truth: Vec<(String, BBox)>,
    prompt: String,
}

fn fixtures() -> Vec<Case> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let s5: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("s5/s5.json")).unwrap()).unwrap();
    let s5_truth: Vec<(String, BBox)> = s5["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let b: Vec<f32> =
                t["box"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
            (t["name"].as_str().unwrap().to_string(), BBox::new(b[0], b[1], b[2], b[3]))
        })
        .collect();
    let names = s5_truth.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
    let shapes: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("cloud/shapes-960x540.groundtruth.json")).unwrap(),
    )
    .unwrap();
    let shapes_truth: Vec<(String, BBox)> = shapes["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let b: Vec<f32> =
                o["bbox"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
            (o["label"].as_str().unwrap().to_string(), BBox::new(b[0], b[1], b[2], b[3]))
        })
        .collect();
    let shape_names =
        shapes_truth.iter().map(|(n, _)| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
    let load = |path: &str| {
        SourceImage::decode(&std::fs::read(dir.join(path)).unwrap(), &DecodeLimits::default())
            .unwrap()
    };
    vec![
        Case {
            name: "s5-screen",
            image: load("s5/s5-screen.png"),
            truth: s5_truth,
            prompt: format!(
                "This is a screenshot of a form. Locate each of these labelled widgets: {names}. \
                 Label each region with the widget's name exactly as given (for example \
                 `name-field`)."
            ),
        },
        Case {
            name: "shapes-960x540",
            image: load("cloud/shapes-960x540.png"),
            truth: shapes_truth,
            prompt: format!(
                "Locate these objects and label each region exactly as given: {shape_names}."
            ),
        },
    ]
}

async fn run(provider: &str, describer: LlmDescriber) {
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime =
        VisionRuntime::builder().remote_describer(Arc::new(describer)).egress_gate(gate).build();
    let runtime = runtime.unwrap();
    for case in fixtures() {
        let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
        let ledger = Arc::new(InMemorySpendLedger::default());
        let cx = VisionCx::new().with_scope(
            ModelCallScope::new()
                .with_budget(budget.clone())
                .with_spend_ledger(ledger.clone(), "live"),
        );
        let spec = DescribeSpec::new(case.prompt.clone()).with_regions();
        let started = std::time::Instant::now();
        let out = runtime.describe(&cx, &[&case.image], &spec).await;
        let elapsed = started.elapsed().as_millis();
        let out = match out {
            Ok(out) => out,
            Err(err) => {
                println!("LIVE {provider} {}: error {err}", case.name);
                continue;
            }
        };
        let sent = out.calls[0].egress.len();
        let resized = out.value.regions.first().map(|r| {
            r.origin.chain.steps.iter().find_map(|s| match s {
                Transform::Resize { width, height } => Some((*width, *height)),
                _ => None,
            })
        });
        let mut errors = Vec::new();
        let mut ious = Vec::new();
        for (name, truth) in &case.truth {
            let found = out.value.regions.iter().find(|r| {
                r.label.as_deref().is_some_and(|l| {
                    let (l, n) = (l.to_ascii_lowercase(), name.to_ascii_lowercase());
                    l.contains(n.as_str()) || n.contains(l.as_str())
                })
            });
            match found.and_then(|r| r.shape.bbox()) {
                Some(b) => {
                    let e = [
                        (b.x0 - truth.x0).abs(),
                        (b.y0 - truth.y0).abs(),
                        (b.x1 - truth.x1).abs(),
                        (b.y1 - truth.y1).abs(),
                    ]
                    .into_iter()
                    .fold(0f32, f32::max);
                    errors.push((name.clone(), e));
                    ious.push(b.iou(truth));
                }
                None => errors.push((name.clone(), f32::NAN)),
            }
        }
        let found = errors.iter().filter(|(_, e)| e.is_finite()).count();
        let max =
            errors.iter().filter(|(_, e)| e.is_finite()).map(|(_, e)| *e).fold(0f32, f32::max);
        let mean = errors.iter().filter(|(_, e)| e.is_finite()).map(|(_, e)| *e).sum::<f32>()
            / found.max(1) as f32;
        let miou = ious.iter().sum::<f32>() / ious.len().max(1) as f32;
        let cost = ledger.spent(&SpendKey::org("live"), SpendPeriod::Day).await.unwrap();
        let usage = budget.usage();
        println!(
            "LIVE {provider} {}: found {found}/{} max_err_px {max:.1} mean_err_px {mean:.1} \
             mean_iou {miou:.3} cost_micro_usd {cost} tokens {} images {} released {sent} \
             resized_to {resized:?} ms {elapsed} errors {errors:?}",
            case.name,
            case.truth.len(),
            usage.total_tokens,
            budget.units().get(UsageUnit::Images),
        );
    }
}

fn key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|k| !k.is_empty())
}

fn base(provider: &str, default: &str) -> String {
    std::env::var(format!("ADK_VISION_{provider}_BASE")).unwrap_or_else(|_| default.to_string())
}

async fn claude(model: &str) {
    let Some(key) = key("ANTHROPIC_API_KEY") else { return };
    let mut config = adk_model::anthropic::AnthropicConfig::new(key, model);
    let url = base("ANTHROPIC", "https://api.anthropic.com");
    if url.starts_with("http://") {
        config = config.allow_insecure_http();
    }
    config = config.with_base_url(url);
    let client: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config).unwrap());
    let describer = LlmDescriber::remote(client, ["api.anthropic.com"])
        .unwrap()
        .with_effort(Effort::Low)
        .with_max_output_tokens(4_000);
    run(model, describer).await;
}

#[tokio::test]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn claude_sonnet_5_5_locates_the_drawn_shapes() {
    claude("claude-sonnet-5-5").await;
}

#[tokio::test]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn claude_haiku_5_5_locates_the_drawn_shapes() {
    claude("claude-haiku-5-5").await;
}

async fn nim(model: &str) {
    let Some(key) = key("NVIDIA_API_KEY") else { return };
    let config = NimConfig::new(base("NIM", "https://integrate.api.nvidia.com/v1"), key, model);
    let client: Arc<dyn Llm> = Arc::new(config.client().unwrap());
    let describer = LlmDescriber::remote(client, ["integrate.api.nvidia.com"])
        .unwrap()
        .with_max_output_tokens(1_500);
    run(model, describer).await;
}

#[tokio::test]
#[ignore = "live: needs NVIDIA_API_KEY"]
async fn nvidia_super_vl_locates_the_drawn_shapes() {
    nim("nvidia/nemotron-3.5-super-vl-120b-a12b").await;
}

#[tokio::test]
#[ignore = "live: needs NVIDIA_API_KEY"]
async fn nvidia_gemma_4_locates_the_drawn_shapes() {
    nim("google/gemma-4-31b-it").await;
}

#[tokio::test]
#[ignore = "live: needs GEMINI_API_KEY; quota-limited"]
async fn gemini_locates_the_drawn_shapes() {
    let Some(key) = key("GEMINI_API_KEY") else { return };
    let model = "gemini-3.8-flash";
    let url = base("GEMINI", "https://generativelanguage.googleapis.com/v1beta/");
    let client: Arc<dyn Llm> =
        Arc::new(adk_model::GeminiModel::new_with_base_url(key, model, url).unwrap());
    let describer = LlmDescriber::remote(client, ["generativelanguage.googleapis.com"])
        .unwrap()
        .with_effort(Effort::Low)
        .with_max_output_tokens(4_000);
    run(model, describer).await;
}

#[tokio::test]
#[ignore = "live: needs META_API_KEY"]
async fn muse_locates_the_drawn_shapes() {
    let Some(key) = key("META_API_KEY") else { return };
    let model = "muse-spark-1.3";
    let config = adk_model::MuseConfig::new(key, model)
        .with_base_url(base("MUSE", "https://api.meta.ai/v1"))
        .with_max_tokens(4_000);
    let client: Arc<dyn Llm> = Arc::new(adk_model::MuseClient::new(config).unwrap());
    let describer = LlmDescriber::remote(client, ["api.meta.ai"]).unwrap();
    run(model, describer).await;
}
