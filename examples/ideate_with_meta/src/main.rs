//! Ideate with Meta — an inventor co-ideation studio on Muse Spark.
//!
//! One pipeline run: intake normalizes the brief (plus an optional sketch the
//! model sees natively and an optional voice memo transcribed by
//! `muse-voice-transcribe-1.0`), a three-voice panel ideates in parallel, and a
//! high-effort converger ranks structured IdeaCards after costing them with a
//! tool. The top card is then rendered with `muse-image-1.0`.
//!
//! ```bash
//! META_API_KEY=... cargo run --manifest-path examples/ideate_with_meta/Cargo.toml -- --brief "solar backpack" [--sketch sketch.png] [--audio memo.wav]
//! cargo run --manifest-path examples/ideate_with_meta/Cargo.toml -- --dry-run --brief "solar backpack"
//! ```
//!
//! `--dry-run` drives the same graph with a scripted model (no key, no spend).
//! `--web` serves a functional UI on `--addr` (default `127.0.0.1:8470`).

mod web;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::{LlmAgentBuilder, ParallelAgent, SequentialAgent};
use adk_core::{
    Agent, Content, Effort, GenerateContentConfig, Part, SessionId, Tool, ToolContext, UserId,
};
use adk_model::muse::{
    ImageOptions, MuseAsrClient, MuseClient, MuseConfig, MuseImagesClient, TranscribeOptions,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

const APP: &str = "ideate-with-meta";
const USER: &str = "inventor";
const SESSION: &str = "notebook-1";

/// One ranked invention card (mirrors the convergence output schema).
#[derive(Debug, Deserialize)]
struct IdeaCard {
    title: String,
    pitch: String,
    #[serde(default)]
    mechanism: String,
    #[serde(default)]
    novelty: f64,
    #[serde(default)]
    feasibility: f64,
    unit_cost_usd: f64,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    next_step: String,
}

fn card_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "pitch": {"type": "string"},
                "mechanism": {"type": "string"},
                "novelty": {"type": "number", "description": "Score from 0.0 (derivative) to 1.0 (groundbreaking)"},
                "feasibility": {"type": "number", "description": "Score from 0.0 (impractical) to 1.0 (buildable today)"},
                "unit_cost_usd": {"type": "number"},
                "risks": {"type": "array", "items": {"type": "string"}},
                "next_step": {"type": "string"}
            },
            // Strict JSON-schema mode requires every property to be required.
            "required": ["title", "pitch", "mechanism", "novelty", "feasibility", "unit_cost_usd", "risks", "next_step"]
        }
    })
}

fn cost_tool(calls: Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "estimate_cost",
        "Back-of-envelope unit economics. Args: {\"idea\": string, \"units\": number}. Returns unit_cost_usd.",
        move |_ctx: Arc<dyn ToolContext>, args: Value| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let idea = args.get("idea").and_then(Value::as_str).unwrap_or("idea");
                let units = args.get("units").and_then(Value::as_f64).unwrap_or(1000.0).max(1.0);
                // Deterministic pseudo-BOM: pricier for longer names, cheaper at scale.
                let base = 25.0 + (idea.len() % 40) as f64;
                let unit = base * (1000.0 / units).powf(0.15);
                Ok(
                    json!({"idea": idea, "units": units, "unit_cost_usd": (unit * 100.0).round() / 100.0}),
                )
            }
        },
    ))
}

fn config(effort: Effort) -> GenerateContentConfig {
    GenerateContentConfig { effort: Some(effort), ..Default::default() }
}

fn pipeline(
    model: Arc<dyn adk_core::Llm>,
    cost_calls: Arc<AtomicUsize>,
) -> anyhow::Result<Arc<dyn Agent>> {
    let intake = LlmAgentBuilder::new("intake")
        .description("Normalizes an inventor brief")
        .instruction("Rewrite the user's brief into 3-5 crisp bullet constraints (problem, user, must-haves). If a sketch image is attached, fold its visible geometry and annotations into the bullets. Output only the bullets.")
        .model(model.clone())
        .generate_content_config(config(Effort::Low))
        .build()?;

    let panel = |name: &str, voice: &str| -> anyhow::Result<Arc<dyn Agent>> {
        let agent = LlmAgentBuilder::new(name)
            .description(voice)
            .instruction(format!("You are the {voice} voice on an invention panel. Given the brief bullets, propose 2-3 distinct product ideas in 2-3 sentences each. Stay in character; do not rank."))
            .model(model.clone())
            .generate_content_config(config(Effort::Medium))
            .temperature(0.9)
            .build()?;
        Ok(Arc::new(agent) as Arc<dyn Agent>)
    };

    let diverge = ParallelAgent::new(
        "divergence_panel",
        vec![
            panel("wild_ideator", "wild inventor (impossible-first, playful)")?,
            panel("engineer", "pragmatic engineer (buildable, cost-aware)")?,
            panel("market_scout", "market scout (buyers, channels, timing)")?,
        ],
    );

    let converge = LlmAgentBuilder::new("converger")
        .description("Ranks panel ideas into structured cards")
        .instruction("Deduplicate the panel's ideas into at least 3 distinct inventions. For your top 3, call estimate_cost once each (units=1000) before finalizing. Then output ONLY a JSON array of idea cards, no prose, no fences.")
        .model(model)
        .generate_content_config(config(Effort::High))
        .output_schema(card_schema())
        .tool(cost_tool(cost_calls))
        .build()?;

    Ok(Arc::new(SequentialAgent::new(
        "ideate_pipeline",
        vec![Arc::new(intake) as Arc<dyn Agent>, Arc::new(diverge), Arc::new(converge)],
    )))
}

fn scripted_model() -> Arc<dyn adk_core::Llm> {
    let cards = json!([
        {"title": "Helios Trek", "pitch": "Backpack with fold-out solar wing and power bank.", "mechanism": "hinged panels", "novelty": 0.8, "feasibility": 0.7, "unit_cost_usd": 61.0, "risks": ["panel durability"], "next_step": "hinge prototype"},
        {"title": "Shade Sail Pack", "pitch": "Solar canopy that shades the hiker.", "mechanism": "telescopic frame", "novelty": 0.7, "feasibility": 0.6, "unit_cost_usd": 58.0, "risks": ["wind"], "next_step": "frame test"},
        {"title": "Trail Bank Mini", "pitch": "Clip-on solar brick for any pack.", "mechanism": "modular brick", "novelty": 0.5, "feasibility": 0.9, "unit_cost_usd": 41.0, "risks": ["crowded market"], "next_step": "mold quote"},
    ]);
    Arc::new(ScriptedLlm::new(
        "muse-spark-1.3-scripted",
        [
            ScriptedResponse::text(
                "- problem: power on trails\n- user: hikers\n- must: light, cheap",
            ),
            ScriptedResponse::text("Idea A: solar wing pack. Idea B: canopy pack."),
            ScriptedResponse::text("Idea C: solar sunshade. Idea D: reflective charger."),
            ScriptedResponse::text("Idea E: clip-on solar brick. Idea F: panel hat."),
            ScriptedResponse::function_call(
                "estimate_cost",
                json!({"idea": "Helios Trek", "units": 1000}),
            ),
            ScriptedResponse::text(cards.to_string()),
        ],
    ))
}

struct Args {
    brief: String,
    sketch: Option<String>,
    audio: Option<String>,
    dry_run: bool,
    web: bool,
    addr: String,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut brief = None;
    let mut sketch = None;
    let mut audio = None;
    let mut dry_run = false;
    let mut web = false;
    let mut addr = "127.0.0.1:8470".to_string();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--brief" => brief = iter.next(),
            "--sketch" => sketch = iter.next(),
            "--audio" => audio = iter.next(),
            "--dry-run" => dry_run = true,
            "--web" => web = true,
            "--addr" => {
                addr = iter.next().ok_or_else(|| anyhow::anyhow!("--addr needs a value"))?
            }
            other => anyhow::bail!(
                "unknown argument {other}; use --brief/--sketch/--audio/--dry-run/--web/--addr"
            ),
        }
    }
    Ok(Args {
        brief: if web {
            brief.unwrap_or_default()
        } else {
            brief.ok_or_else(|| anyhow::anyhow!("--brief is required (or pass --web)"))?
        },
        sketch,
        audio,
        dry_run,
        web,
        addr,
    })
}

/// Run the ideation pipeline once, reporting stage lines as they arrive.
///
/// Returns the parsed cards and the number of costing-tool calls.
async fn run_pipeline(
    model: Arc<dyn adk_core::Llm>,
    parts: Vec<Part>,
    cost_calls: Arc<AtomicUsize>,
    mut on_line: impl FnMut(String),
) -> anyhow::Result<(Vec<IdeaCard>, usize)> {
    let agent = pipeline(model, cost_calls.clone())?;
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder().app_name(APP).agent(agent).session_service(sessions).build()?;
    let mut events = runner
        .run(
            UserId::new(USER)?,
            SessionId::new(session.id().to_string())?,
            Content { role: "user".to_string(), parts },
        )
        .await?;

    let mut final_text = String::new();
    while let Some(event) = events.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Text { text } if !text.is_empty() => {
                        on_line(format!("[{}] {text}", event.author));
                        final_text = text.clone();
                    }
                    Part::FunctionCall { name, args, .. } => {
                        on_line(format!("[{}] calls {name}({args})", event.author));
                    }
                    _ => {}
                }
            }
        }
    }

    let json_text = final_text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let cards: Vec<IdeaCard> = serde_json::from_str(json_text)
        .map_err(|e| anyhow::anyhow!("convergence output is not an IdeaCard array: {e}"))?;
    anyhow::ensure!(cards.len() >= 3, "expected at least 3 idea cards");
    let calls = cost_calls.load(Ordering::SeqCst);
    anyhow::ensure!(calls >= 1, "expected estimate_cost to be called");
    Ok((cards, calls))
}

fn api_key() -> anyhow::Result<String> {
    std::env::var("META_API_KEY")
        .or_else(|_| std::env::var("MODEL_API_KEY"))
        .map_err(|_| anyhow::anyhow!("Set META_API_KEY (or MODEL_API_KEY), or pass --dry-run"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenvy::dotenv().ok();
    let args = parse_args()?;

    if args.web {
        let key = api_key().ok();
        if key.is_none() {
            println!("No META_API_KEY found: the UI still serves, but only dry runs will work.");
        }
        return web::serve(&args.addr, key).await;
    }

    println!("=== Ideate with Meta (Muse Spark) ===");
    let (model, images, asr): (
        Arc<dyn adk_core::Llm>,
        Option<MuseImagesClient>,
        Option<MuseAsrClient>,
    ) = if args.dry_run {
        println!("Dry run: scripted model, no API calls, no render.\n");
        (scripted_model(), None, None)
    } else {
        let key = api_key()?;
        (
            Arc::new(MuseClient::new(MuseConfig::spark_1_3(key.clone()))?),
            Some(MuseImagesClient::new(MuseConfig::new(key.clone(), "muse-image-1.0"))?),
            Some(MuseAsrClient::new(MuseConfig::new(key, "muse-voice-transcribe-1.0"))?),
        )
    };

    // Voice memo → transcript prepends the brief (live mode only).
    let mut brief_text = args.brief.clone();
    if let (Some(audio_path), Some(asr)) = (&args.audio, &asr) {
        let wav = std::fs::read(audio_path)?;
        let transcript =
            asr.transcribe(wav, "memo.wav", "audio/wav", TranscribeOptions::default()).await?;
        println!("[voice memo transcript] {}", transcript.text);
        brief_text = format!("Voice memo: {}\n\nBrief: {}", transcript.text, brief_text);
    }

    let mut parts = vec![Part::Text { text: brief_text }];
    if let Some(sketch_path) = &args.sketch {
        let bytes = std::fs::read(sketch_path)?;
        let mime = if sketch_path.ends_with(".jpg") || sketch_path.ends_with(".jpeg") {
            "image/jpeg"
        } else {
            "image/png"
        };
        println!("[sketch attached] {sketch_path} ({} bytes, {mime})", bytes.len());
        parts.push(Part::inline_data(mime, bytes));
    }

    let cost_calls = Arc::new(AtomicUsize::new(0));
    println!("Brief: {}\n", args.brief);
    let (cards, _) =
        run_pipeline(model, parts, cost_calls.clone(), |line| println!("{line}")).await?;
    println!("\n--- Idea cards ({}) ---", cards.len());
    for card in &cards {
        println!(
            "• {} — {} (novelty {:.1}, feasibility {:.1}, ${:.2}) next: {}",
            card.title,
            card.pitch,
            card.novelty,
            card.feasibility,
            card.unit_cost_usd,
            card.next_step
        );
        if !card.mechanism.is_empty() {
            println!("  mechanism: {}", card.mechanism);
        }
        if !card.risks.is_empty() {
            println!("  risks: {}", card.risks.join("; "));
        }
    }

    anyhow::ensure!(cards.len() >= 3, "expected at least 3 idea cards");
    anyhow::ensure!(cost_calls.load(Ordering::SeqCst) >= 1, "expected estimate_cost to be called");

    if let (Some(images), Some(top)) = (&images, cards.first()) {
        let prompt = format!(
            "Clean product concept render, studio lighting: {} — {}. Photorealistic, single product on neutral background.",
            top.title, top.pitch
        );
        let rendered = images.generate(prompt, ImageOptions::default()).await?;
        let bytes = rendered.data.first().map(image_bytes).transpose()?;
        if let Some(bytes) = bytes {
            let ext = if bytes.starts_with(b"\x89PNG") {
                "png"
            } else if bytes.starts_with(b"\xFF\xD8\xFF") {
                "jpg"
            } else {
                "webp"
            };
            std::fs::create_dir_all("out")?;
            let path = format!("out/concept.{ext}");
            std::fs::write(&path, &bytes)?;
            println!("\n[render] top card '{}' → {path} ({} bytes)", top.title, bytes.len());
            anyhow::ensure!(bytes.len() > 10_000, "rendered image suspiciously small");
        }
    } else {
        println!("\n[render] skipped (dry run)");
    }

    println!(
        "\nPASS: Ideate with Meta completed ({} cards, {} cost calls).",
        cards.len(),
        cost_calls.load(Ordering::SeqCst)
    );
    Ok(())
}

fn image_bytes(data: &adk_model::muse::ImageData) -> anyhow::Result<Vec<u8>> {
    Ok(data.bytes()?)
}
