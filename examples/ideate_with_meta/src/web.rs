//! Functional web UI for Ideate with Meta (`--web`).
//!
//! A dependency-light HTTP server on tokio:
//! - `GET /` serves one self-contained page (ADK-Rust harness theme tokens).
//! - `POST /api/ideate/stream` runs the pipeline and streams stage events as
//!   server-sent events, then one `result` event with cards and render.
//! - `POST /api/ideate` is the same run as a single JSON response.
//! - `POST /api/expand` lets the model polish a one-line spark into a brief.
//!
//! Files travel as base64 JSON fields, so no multipart parsing is needed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use adk_model::muse::{
    ImageOptions, MuseAsrClient, MuseClient, MuseConfig, MuseImagesClient, TranscribeOptions,
};
use base64::Engine as _;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::{IdeaCard, image_bytes, run_pipeline};

/// Incoming ideation request.
#[derive(Debug, Deserialize)]
struct IdeateRequest {
    brief: String,
    #[serde(default)]
    sketch_base64: Option<String>,
    #[serde(default)]
    sketch_mime: Option<String>,
    #[serde(default)]
    audio_base64: Option<String>,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    dry_run: bool,
}

/// Incoming decompose request (brief → what/who/better fields).
#[derive(Debug, Deserialize)]
struct DecomposeRequest {
    brief: String,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    dry_run: bool,
}

/// Decomposed brief fields for the step-by-step composer.
#[derive(Debug, Serialize, Deserialize)]
struct Decomposed {
    what: String,
    who: String,
    better: String,
}

/// Incoming demo request: one AI invention for the auto-demo wall.
#[derive(Debug, Deserialize)]
struct DemoRequest {
    spark: String,
    #[serde(default)]
    what: String,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    dry_run: bool,
}

/// One demo invention: the spark, its brief, and one idea card.
#[derive(Debug, Serialize)]
struct DemoIdea {
    spark: String,
    brief: String,
    card: IdeaCardOut,
}

/// Live demo payload (card fields flattened alongside spark and brief).
#[derive(Debug, Deserialize)]
struct DemoPayload {
    spark: String,
    brief: String,
    #[serde(flatten)]
    card: IdeaCard,
}

/// Incoming expand request.
#[derive(Debug, Deserialize)]
struct ExpandRequest {
    fragment: String,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    dry_run: bool,
}

fn default_model() -> String {
    "muse-spark-1.3".to_string()
}

/// Outgoing ideation result.
#[derive(Debug, Serialize)]
struct IdeateResponse {
    cards: Vec<IdeaCardOut>,
    cost_calls: usize,
    transcript: Option<String>,
    render: Option<RenderOut>,
    stages: Vec<String>,
    model: String,
}

/// Card as JSON for the page.
#[derive(Debug, Clone, Serialize)]
struct IdeaCardOut {
    title: String,
    pitch: String,
    mechanism: String,
    novelty: f64,
    feasibility: f64,
    unit_cost_usd: f64,
    risks: Vec<String>,
    next_step: String,
    /// Small per-concept render (512px), distinct from the hero image.
    image: Option<RenderOut>,
}

/// Normalize a model score to the 0.0–1.0 range the page renders as a
/// percent. Models occasionally answer on a 0–5 or 0–10 scale despite the
/// schema description, so anything above 1 is rescaled or clamped here
/// rather than rendered as e.g. 400%.
fn score01(value: f64) -> f64 {
    if value <= 1.0 {
        value.max(0.0)
    } else if value <= 5.0 {
        value / 5.0
    } else if value <= 10.0 {
        value / 10.0
    } else {
        1.0
    }
}

impl From<&IdeaCard> for IdeaCardOut {
    fn from(card: &IdeaCard) -> Self {
        Self {
            title: card.title.clone(),
            pitch: card.pitch.clone(),
            mechanism: card.mechanism.clone(),
            novelty: score01(card.novelty),
            feasibility: score01(card.feasibility),
            unit_cost_usd: card.unit_cost_usd,
            risks: card.risks.clone(),
            next_step: card.next_step.clone(),
            image: None,
        }
    }
}

/// Rendered concept image for the page.
#[derive(Debug, Clone, Serialize)]
struct RenderOut {
    mime: String,
    base64: String,
}

fn render_out(data: &adk_model::muse::ImageData) -> anyhow::Result<RenderOut> {
    let bytes = image_bytes(data)?;
    let mime = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg"
    } else {
        "image/webp"
    };
    Ok(RenderOut {
        mime: mime.to_string(),
        base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Draw one small concept image; failures leave the card imageless.
async fn concept_image(
    images: &MuseImagesClient,
    title: &str,
    pitch: &str,
    size: Option<String>,
) -> Option<RenderOut> {
    let prompt = format!(
        "Clean small product concept render, studio lighting: {title} — {pitch}. Photorealistic, single product on neutral background."
    );
    let rendered = images
        .generate(prompt, ImageOptions { n: None, size, response_format: None })
        .await
        .ok()?;
    rendered.data.first().and_then(|data| render_out(data).ok())
}

/// Shared server state.
struct AppState {
    key: Option<String>,
}

/// Serve the UI until the process stops.
pub async fn serve(addr: &str, key: Option<String>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let state = Arc::new(AppState { key });
    println!("Ideate with Meta UI: http://{local}");
    println!("Open that URL in a browser. Ctrl-C stops the server.");
    loop {
        let (socket, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(socket, state).await {
                eprintln!("request failed: {error}");
            }
        });
    }
}

async fn handle(mut socket: tokio::net::TcpStream, state: Arc<AppState>) -> anyhow::Result<()> {
    let (method, path, body) = read_request(&mut socket).await?;
    match (method.as_str(), path.as_str()) {
        ("GET", "/") => {
            respond(&mut socket, 200, "text/html; charset=utf-8", INDEX_HTML.as_bytes()).await
        }
        ("GET", "/health") => {
            respond(&mut socket, 200, "application/json", br#"{"ok":true}"#).await
        }
        ("POST", "/api/ideate") => match ideate(&state, &body, |_| {}).await {
            Ok(response) => {
                respond(
                    &mut socket,
                    200,
                    "application/json",
                    &serde_json::to_vec(&response).unwrap_or_default(),
                )
                .await
            }
            Err(error) => respond_json_error(&mut socket, &error).await,
        },
        ("POST", "/api/ideate/stream") => stream_ideate(&mut socket, &state, &body).await,
        ("POST", "/api/decompose") => match decompose(&state, &body).await {
            Ok(fields) => {
                respond(
                    &mut socket,
                    200,
                    "application/json",
                    &serde_json::to_vec(&fields).unwrap_or_default(),
                )
                .await
            }
            Err(error) => respond_json_error(&mut socket, &error).await,
        },
        ("POST", "/api/demo-idea") => match demo_idea(&state, &body).await {
            Ok(idea) => {
                respond(
                    &mut socket,
                    200,
                    "application/json",
                    &serde_json::to_vec(&idea).unwrap_or_default(),
                )
                .await
            }
            Err(error) => respond_json_error(&mut socket, &error).await,
        },
        ("POST", "/api/expand") => match expand(&state, &body).await {
            Ok(brief) => {
                respond(
                    &mut socket,
                    200,
                    "application/json",
                    &serde_json::to_vec(&json!({"brief": brief})).unwrap_or_default(),
                )
                .await
            }
            Err(error) => respond_json_error(&mut socket, &error).await,
        },
        _ => respond(&mut socket, 404, "application/json", br#"{"error":"not found"}"#).await,
    }
}

async fn respond(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(payload).await?;
    Ok(())
}

async fn respond_json_error(
    socket: &mut tokio::net::TcpStream,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    respond(
        socket,
        400,
        "application/json",
        &serde_json::to_vec(&json!({"error": error.to_string()})).unwrap_or_default(),
    )
    .await
}

/// Stream one ideation as server-sent events.
async fn stream_ideate(
    socket: &mut tokio::net::TcpStream,
    state: &AppState,
    body: &[u8],
) -> anyhow::Result<()> {
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
        )
        .await?;
    let (tx, mut rx) = mpsc::channel::<String>(128);
    let state = AppState { key: state.key.clone() };
    let body = body.to_vec();
    let worker = tokio::spawn(async move {
        let tx_lines = tx.clone();
        let result = ideate(&state, &body, move |line| {
            let _ = tx_lines.try_send(frame("stage", &json!({"line": line})));
        })
        .await;
        match result {
            Ok(response) => {
                let _ = tx
                    .send(frame("result", &serde_json::to_value(&response).unwrap_or_default()))
                    .await;
            }
            Err(error) => {
                let _ = tx.send(frame("error", &json!({"error": error.to_string()}))).await;
            }
        }
    });
    while let Some(frame) = rx.recv().await {
        if socket.write_all(frame.as_bytes()).await.is_err() {
            break;
        }
        let _ = socket.flush().await;
    }
    let _ = worker.await;
    Ok(())
}

fn frame(event: &str, data: &serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// The model trio one request drives (chat, images, ASR).
type ModelTrio = (Arc<dyn Llm>, Option<MuseImagesClient>, Option<MuseAsrClient>);

/// Build the model trio for one request.
fn models(state: &AppState, model_name: &str, dry_run: bool) -> anyhow::Result<ModelTrio> {
    if dry_run {
        return Ok((crate::scripted_model(), None, None));
    }
    let key = state
        .key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("server started without META_API_KEY; use dry run"))?;
    Ok((
        Arc::new(MuseClient::new(MuseConfig::new(key.clone(), model_name.to_string()))?),
        Some(MuseImagesClient::new(MuseConfig::new(key.clone(), "muse-image-1.0"))?),
        Some(MuseAsrClient::new(MuseConfig::new(key, "muse-voice-transcribe-1.0"))?),
    ))
}

/// Run one ideation, reporting stage lines through `on_stage`.
async fn ideate(
    state: &AppState,
    body: &[u8],
    mut on_stage: impl FnMut(String),
) -> anyhow::Result<IdeateResponse> {
    let request: IdeateRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("invalid request JSON: {e}"))?;
    anyhow::ensure!(!request.brief.trim().is_empty(), "brief must not be empty");
    let (model, images, asr) = models(state, &request.model, request.dry_run)?;

    let mut brief_text = request.brief.clone();
    let mut transcript = None;
    if let (Some(audio_b64), Some(asr)) = (&request.audio_base64, &asr) {
        on_stage("[voice] transcribing memo…".to_string());
        let wav = decode(audio_b64)?;
        let heard =
            asr.transcribe(wav, "memo.wav", "audio/wav", TranscribeOptions::default()).await?;
        brief_text = format!("Voice memo: {}\n\nBrief: {}", heard.text, brief_text);
        transcript = Some(heard.text);
    }

    let mut parts = vec![Part::Text { text: brief_text }];
    if let Some(sketch_b64) = &request.sketch_base64 {
        let bytes = decode(sketch_b64)?;
        let mime = request.sketch_mime.as_deref().unwrap_or("image/png");
        parts.push(Part::inline_data(mime, bytes));
        on_stage("[sketch] attached for the intake agent".to_string());
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let mut stages: Vec<String> = Vec::new();
    let (cards, _) = run_pipeline(model, parts, counter.clone(), |line| {
        stages.push(line.clone());
        on_stage(line);
    })
    .await?;
    let cost_calls = counter.load(Ordering::SeqCst);

    let mut out_cards: Vec<IdeaCardOut> = cards.iter().map(IdeaCardOut::from).collect();
    let mut render = None;
    if let Some(images) = &images {
        // Hero render for the winner, then a small distinct render per card.
        if let Some(top) = out_cards.first_mut() {
            on_stage("[render] drawing the top concept…".to_string());
            render = concept_image(images, &top.title.clone(), &top.pitch.clone(), None).await;
            top.image = render.clone();
        }
        for card in out_cards.iter_mut().skip(1) {
            let (title, pitch) = (card.title.clone(), card.pitch.clone());
            on_stage(format!("[render] drawing {title}…"));
            card.image = concept_image(images, &title, &pitch, Some("512x512".to_string())).await;
        }
    }

    Ok(IdeateResponse {
        cards: out_cards,
        cost_calls,
        transcript,
        render,
        stages,
        model: request.model,
    })
}

/// Split a brief into the composer's three fields (what / who / better).
async fn decompose(state: &AppState, body: &[u8]) -> anyhow::Result<Decomposed> {
    let request: DecomposeRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("invalid request JSON: {e}"))?;
    let brief = request.brief.trim().to_string();
    anyhow::ensure!(!brief.is_empty(), "brief must not be empty");
    if request.dry_run {
        // Offline split: "X for Y that Z" covers most sparks.
        let (what, rest) = match brief.split_once(" for ") {
            Some((what, rest)) => (what.trim().to_string(), rest),
            None => (brief.clone(), ""),
        };
        let (who, better) = match rest.split_once(" that ") {
            Some((who, better)) => (who.trim().to_string(), better.trim().to_string()),
            None => (rest.trim().to_string(), String::new()),
        };
        return Ok(Decomposed { what, who, better });
    }
    let key = state
        .key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("server started without META_API_KEY; use dry run"))?;
    let client = MuseClient::new(MuseConfig::new(key, request.model.clone()))?;
    let mut llm_request = LlmRequest::new(
        request.model.clone(),
        vec![Content::new("user").with_text(format!(
            "Split this invention brief into three short phrases: what the product is, who it is for, and what it does better. Brief: {brief}"
        ))],
    );
    llm_request.config = Some(GenerateContentConfig {
        effort: Some(adk_core::Effort::Low),
        max_output_tokens: Some(1024),
        response_schema: Some(json!({
            "type": "object",
            "properties": {
                "what": {"type": "string"},
                "who": {"type": "string"},
                "better": {"type": "string"}
            },
            "required": ["what", "who", "better"]
        })),
        ..Default::default()
    });
    let mut stream = client.generate_content(llm_request, false).await?;
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        let response = item?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text: chunk } = part {
                    text.push_str(&chunk);
                }
            }
        }
    }
    let fields: Decomposed = serde_json::from_str(text.trim())
        .map_err(|e| anyhow::anyhow!("model did not return decomposed fields: {e}"))?;
    Ok(fields)
}

/// Invent one demo idea for a seed spark (auto-demo wall; no panel, no render).
async fn demo_idea(state: &AppState, body: &[u8]) -> anyhow::Result<DemoIdea> {
    let request: DemoRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("invalid request JSON: {e}"))?;
    let spark = request.spark.trim().to_string();
    anyhow::ensure!(!spark.is_empty(), "spark must not be empty");
    if request.dry_run {
        // Varied offline card derived from the spark, so the demo works keyless.
        let seed = spark.len() as f64;
        let name =
            request.what.trim().trim_start_matches("a ").trim_start_matches("an ").to_string();
        let title = if name.is_empty() {
            "Workshop Prototype".to_string()
        } else {
            name.split_whitespace()
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let card = IdeaCard {
            title,
            pitch: format!("{spark} — rethought end to end."),
            mechanism: "Modular first prototype from catalogue parts.".to_string(),
            novelty: 0.55 + (seed % 30.0) / 100.0,
            feasibility: 0.9 - (seed % 25.0) / 100.0,
            unit_cost_usd: 24.0 + (seed % 45.0),
            risks: vec!["Needs one honest prototype test.".to_string()],
            next_step: "Build one and test it this week.".to_string(),
        };
        return Ok(DemoIdea {
            brief: format!("I want to invent {spark}."),
            spark,
            card: IdeaCardOut::from(&card),
        });
    }
    let key = state
        .key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("server started without META_API_KEY; use dry run"))?;
    let client = MuseClient::new(MuseConfig::new(key.clone(), request.model.clone()))?;
    let images = MuseImagesClient::new(MuseConfig::new(key, "muse-image-1.0"))?;
    let mut llm_request = LlmRequest::new(
        request.model.clone(),
        vec![Content::new("user").with_text(format!(
            "Invent ONE original, delightful product idea from this spark. Keep the spark as the starting point but make the product specific and ownable. Return spark (short 'a …' phrase), brief (one sentence), and one idea card.\n\nSpark: {spark}"
        ))],
    );
    llm_request.config = Some(GenerateContentConfig {
        effort: Some(adk_core::Effort::Low),
        max_output_tokens: Some(1024),
        temperature: Some(0.9),
        response_schema: Some(json!({
            "type": "object",
            "properties": {
                "spark": {"type": "string"},
                "brief": {"type": "string"},
                "title": {"type": "string"},
                "pitch": {"type": "string"},
                "mechanism": {"type": "string"},
                "novelty": {"type": "number", "description": "Score from 0.0 to 1.0"},
                "feasibility": {"type": "number", "description": "Score from 0.0 to 1.0"},
                "unit_cost_usd": {"type": "number"},
                "risks": {"type": "array", "items": {"type": "string"}},
                "next_step": {"type": "string"}
            },
            "required": ["spark", "brief", "title", "pitch", "mechanism", "novelty", "feasibility", "unit_cost_usd", "risks", "next_step"]
        })),
        ..Default::default()
    });
    let mut stream = client.generate_content(llm_request, false).await?;
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        let response = item?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text: chunk } = part {
                    text.push_str(&chunk);
                }
            }
        }
    }
    let payload: DemoPayload = serde_json::from_str(text.trim())
        .map_err(|e| anyhow::anyhow!("model did not return a demo idea: {e}"))?;
    let mut card = IdeaCardOut::from(&payload.card);
    card.image = concept_image(
        &images,
        &card.title.clone(),
        &card.pitch.clone(),
        Some("512x512".to_string()),
    )
    .await;
    Ok(DemoIdea { spark: payload.spark, brief: payload.brief, card })
}

/// Polish a one-line spark into a full brief (used by the guided input).
async fn expand(state: &AppState, body: &[u8]) -> anyhow::Result<String> {
    let request: ExpandRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("invalid request JSON: {e}"))?;
    let fragment = request.fragment.trim().to_string();
    anyhow::ensure!(!fragment.is_empty(), "write a spark first");
    if request.dry_run {
        return Ok(format!(
            "I want to invent {fragment} — noticeably better than what exists today: lighter, cheaper to make, and a pleasure to use every day."
        ));
    }
    let key = state
        .key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("server started without META_API_KEY; use dry run"))?;
    let client = MuseClient::new(MuseConfig::new(key, request.model.clone()))?;
    let mut llm_request = LlmRequest::new(
        request.model.clone(),
        vec![Content::new("user").with_text(format!(
            "Expand this invention spark into ONE polished brief sentence (max 40 words) naming the product, its user, and what it does better. Reply with only the sentence.\n\nSpark: {fragment}"
        ))],
    );
    llm_request.config = Some(GenerateContentConfig {
        effort: Some(adk_core::Effort::Low),
        max_output_tokens: Some(1024),
        temperature: Some(0.7),
        ..Default::default()
    });
    let mut stream = client.generate_content(llm_request, false).await?;
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        let response = item?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text: chunk } = part {
                    text.push_str(&chunk);
                }
            }
        }
    }
    let brief = text.trim().trim_matches('"').to_string();
    anyhow::ensure!(!brief.is_empty(), "the model returned an empty brief");
    Ok(brief)
}

/// Read one HTTP request (headers + content-length body).
async fn read_request(
    socket: &mut tokio::net::TcpStream,
) -> anyhow::Result<(String, String, Vec<u8>)> {
    let mut buffer = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = find(&buffer, b"\r\n\r\n") {
            break pos + 4;
        }
        anyhow::ensure!(buffer.len() < 64 * 1024, "headers too large");
        let read = socket.read(&mut chunk).await?;
        anyhow::ensure!(read > 0, "connection closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let content_length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    anyhow::ensure!(content_length <= 32 * 1024 * 1024, "body too large");
    let mut body = buffer.split_off(header_end);
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await?;
        anyhow::ensure!(read > 0, "connection closed mid-body");
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok((method, path, body))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn decode(value: &str) -> anyhow::Result<Vec<u8>> {
    let cleaned: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    let body = cleaned.split(',').nth(1).unwrap_or(&cleaned);
    Ok(base64::engine::general_purpose::STANDARD.decode(body)?)
}

/// The single-page UI (ADK-Rust harness theme: burnt orange on light, navy dark).
const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Ideate with Meta — powered by ADK-Rust</title>
<style>
  :root {
    color-scheme: light;
    --bg: #ffffff; --card: #ffffff; --raised: #f6f7f9; --border: #e6e8ec;
    --text: #0f172a; --text-2: #475569; --label: #64748b;
    --brand: #f74c00; --accent: #c03a00; --button: #c03a00; --on-button: #ffffff;
    --working: #15803d; --waiting: #c03a00; --done: #1d4ed8; --failed: #b91c1c;
    --pill-waiting-bg: #fdeadf; --pill-waiting: #b3360a;
    --pill-done-bg: #dbeafe; --pill-done: #1d4ed8;
    --pill-decided-bg: #dcfce7; --pill-decided: #15803d;
    --pill-neutral-bg: #f1f5f9; --pill-neutral: #334155;
    --font: "Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    --radius: 10px; --radius-lg: 16px;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      color-scheme: dark;
      --bg: #0a1628; --card: #15233b; --raised: #1c2b45; --border: #2a3d5c;
      --text: #f1f5f9; --text-2: #d5dee8; --label: #a7b4c6;
      --accent: #ff9c78; --button: #f74c00; --on-button: #0a1628;
      --pill-waiting-bg: #3b2a2c; --pill-waiting: #ff9c78;
      --pill-done-bg: #1c3557; --pill-done: #93c5fd;
      --pill-decided-bg: #133626; --pill-decided: #4ade80;
      --pill-neutral-bg: #1c2b45; --pill-neutral: #d5dee8;
    }
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text); font: 15px/1.55 var(--font); }
  header { max-width: 1120px; margin: 0 auto; padding: 28px 20px 4px; }
  .brandline { display: flex; align-items: flex-start; gap: 16px; flex-wrap: wrap; }
  .brandmark { margin-left: auto; display: inline-flex; align-items: center; gap: 8px; font-weight: 700; font-size: 17px; letter-spacing: -0.01em; color: var(--text); text-decoration: none; white-space: nowrap; padding-top: 4px; }
  .brandmark .brand-dot { width: 11px; height: 11px; border-radius: 50%; background: var(--brand); flex: none; }
  .credits { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 10px; }
  .credit { font-size: 12px; font-weight: 600; border: 1px solid var(--border); background: var(--raised); color: var(--text-2); border-radius: 999px; padding: 4px 11px; }
  .credit b { color: var(--accent); font-weight: 700; }
  .showcase { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--card); padding: 16px 18px; margin-bottom: 18px; overflow: hidden; }
  .showcase h2 { font-size: 19px; margin: 0 0 2px; letter-spacing: -0.01em; }
  .showcase .lede { color: var(--text-2); font-size: 13.5px; margin: 0 0 12px; }
  .demo-flow { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin-bottom: 12px; }
  .demo-node { font-size: 12px; font-weight: 700; border: 1px solid var(--border); background: var(--raised); color: var(--label); border-radius: 999px; padding: 5px 11px; transition: all .3s ease; }
  .demo-node.on { border-color: var(--brand); color: var(--accent); background: var(--pill-waiting-bg); animation: pulse 1s ease-in-out infinite; }
  .demo-node.done { border-color: var(--done); color: var(--done); background: var(--pill-done-bg); animation: none; }
  .demo-arrow { color: var(--label); font-size: 12px; }
  @keyframes pulse { 0%, 100% { transform: scale(1); } 50% { transform: scale(1.05); } }
  .demo-status { font-size: 13.5px; color: var(--text-2); min-height: 21px; margin-bottom: 10px; }
  .demo-status b { color: var(--text); }
  .demo-cards { margin-bottom: 12px; }
  .demo-cards .card { animation: rise .45s ease; }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: none; } }
  .demo-spark { font-size: 12px; color: var(--label); margin-bottom: 6px; }
  .thumb { width: 100%; height: 150px; object-fit: cover; border-radius: 8px; display: block; margin-bottom: 10px; background: var(--raised); }
  .thumb-fallback { height: 84px; display: flex; align-items: center; justify-content: center; font-size: 30px; font-weight: 800; color: var(--accent); background: var(--pill-waiting-bg); }
  .pdfrow { margin-top: 10px; }
  @media (prefers-reduced-motion: reduce) { .demo-node.on, .demo-cards .card { animation: none; } }
  h1 { font-size: 27px; margin: 0; letter-spacing: -0.02em; }
  h1 .accent { color: var(--brand); }
  .powered { font-size: 12px; font-weight: 600; background: var(--pill-neutral-bg); color: var(--pill-neutral); border-radius: 999px; padding: 3px 10px; }
  .sub { color: var(--text-2); margin: 6px 0 0; max-width: 72ch; }
  main { max-width: 1120px; margin: 0 auto; padding: 14px 20px 40px; display: grid; grid-template-columns: 400px 1fr; gap: 20px; align-items: start; }
  @media (max-width: 920px) { main { grid-template-columns: 1fr; } }
  .panel { background: var(--card); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 18px; box-shadow: 0 1px 2px rgba(15,23,42,.04), 0 6px 20px rgba(15,23,42,.05); }
  @media (prefers-color-scheme: dark) { .panel { box-shadow: none; } }
  .panel h2 { font-size: 15px; margin: 0 0 4px; }
  .panel .lede { color: var(--text-2); font-size: 13.5px; margin: 0 0 8px; }
  label { display: block; font-size: 12px; font-weight: 600; letter-spacing: .04em; text-transform: uppercase; color: var(--label); margin: 14px 0 6px; }
  textarea, input[type="text"], select { width: 100%; background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: var(--radius); padding: 10px; font: inherit; }
  textarea { min-height: 76px; resize: vertical; }
  input[type="file"] { width: 100%; color: var(--text-2); font-size: 13px; }
  .row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; margin-top: 10px; }
  button { background: var(--button); color: var(--on-button); border: 0; border-radius: var(--radius); padding: 10px 16px; font: 600 15px/1 var(--font); cursor: pointer; min-height: 42px; }
  button:disabled { opacity: .55; cursor: wait; }
  button.ghost { background: var(--card); border: 1px solid var(--border); color: var(--text); font-weight: 500; }
  button.small { padding: 8px 12px; font-size: 13.5px; min-height: 38px; }
  .grow { flex: 1; }
  .hint { font-size: 12.5px; color: var(--label); margin-top: 6px; }
  .composed { background: var(--raised); border: 1px solid var(--border); border-radius: var(--radius); padding: 10px; font-size: 14px; min-height: 44px; }
  #sketchPreview { max-width: 100%; border-radius: var(--radius); margin-top: 8px; display: none; border: 1px solid var(--border); }
  .recwrap { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .recbtn.recording { background: var(--failed); color: #fff; }
  .statusline { display: flex; gap: 10px; align-items: center; margin-bottom: 14px; flex-wrap: wrap; }
  .pill { font-size: 12px; font-weight: 700; border-radius: 999px; padding: 4px 11px; }
  .pill.waiting { background: var(--pill-waiting-bg); color: var(--pill-waiting); }
  .pill.done { background: var(--pill-done-bg); color: var(--pill-done); }
  .pill.decided { background: var(--pill-decided-bg); color: var(--pill-decided); }
  .status-text { color: var(--text-2); font-size: 14px; }
  .progress { height: 4px; background: var(--border); border-radius: 2px; overflow: hidden; margin-bottom: 16px; display: none; }
  .progress i { display: block; height: 100%; width: 30%; background: var(--brand); border-radius: 2px; transition: width .5s ease; }
  .hero { border: 1px solid var(--border); border-radius: var(--radius-lg); overflow: hidden; background: var(--card); display: none; margin-bottom: 18px; }
  .hero img { width: 100%; max-height: 420px; object-fit: cover; display: block; background: var(--raised); }
  .hero .body { padding: 18px; }
  .hero .kicker { font-size: 12px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--accent); }
  .hero h2 { font-size: 24px; margin: 4px 0 6px; letter-spacing: -0.01em; }
  .hero p { margin: 0; color: var(--text-2); font-size: 16px; }
  .hero .stats { display: flex; gap: 22px; flex-wrap: wrap; margin-top: 14px; }
  .hero .stat b { display: block; font-size: 19px; }
  .hero .stat span { font-size: 12px; color: var(--label); text-transform: uppercase; letter-spacing: .04em; }
  .cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 14px; }
  .card { background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 15px; }
  .card.top { outline: 2px solid var(--brand); outline-offset: -1px; }
  .card h3 { margin: 0 0 5px; font-size: 16px; }
  .card p { margin: 0 0 8px; color: var(--text-2); font-size: 14px; }
  .meta { font-size: 12.5px; color: var(--label); margin: 7px 0 2px; }
  .bar { height: 5px; background: var(--border); border-radius: 3px; overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--done); }
  .cost { font-size: 19px; font-weight: 700; margin-top: 8px; }
  .card ul { margin: 6px 0 0; padding-left: 17px; color: var(--text-2); font-size: 13px; }
  .stages { margin-top: 16px; font: 12.5px/1.6 ui-monospace, Menlo, monospace; color: var(--text-2); white-space: pre-wrap; max-height: 260px; overflow: auto; background: var(--raised); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px; display: none; }
  footer { max-width: 1120px; margin: 0 auto; padding: 0 20px 30px; color: var(--label); font-size: 13px; }
  @media (max-width: 560px) {
    h1 { font-size: 23px; }
    button { width: 100%; }
    button.small, button.ghost { width: auto; }
    .hero .body { padding: 14px; }
  }
</style>
</head>
<body>
<header>
  <div class="brandline">
    <div>
      <h1>Ideate with <span class="accent">Meta</span></h1>
      <div class="credits">
        <span class="credit">Created by <b>Meta Muse Wonder</b></span>
      </div>
    </div>
    <a class="brandmark" href="https://github.com/zavora-ai/adk-rust" aria-label="ADK-Rust"><i class="brand-dot" aria-hidden="true"></i>ADK-Rust</a>
  </div>
  <p class="sub">An inventor co-ideation studio on Meta Muse models, built with ADK-Rust. Start from a spark — the AI helps you grow it into a brief — then a three-voice agent panel argues, ranks costed idea cards, and renders the winner.</p>
</header>
<main>
  <section class="panel" aria-label="Input studio">
    <h2>Start from a spark</h2>
    <p class="lede">One line is enough. Shuffle for a random spark, or expand yours with AI.</p>
    <label for="spark">I want to invent a…</label>
    <textarea id="spark" placeholder="quieter kettle that never wakes the baby"></textarea>
    <div class="row">
      <button type="button" class="ghost small" id="shuffle">Shuffle a spark</button>
      <button type="button" class="ghost small" id="expand">Expand with AI</button>
    </div>

    <h2 style="margin-top:22px">…or build it step by step</h2>
    <p class="lede">Answer three prompts; the brief composes itself. Shuffle and Expand fill these in for you.</p>
    <label for="qWhat">What is it?</label>
    <input type="text" id="qWhat" placeholder="a solar backpack">
    <label for="qWho">Who is it for?</label>
    <input type="text" id="qWho" placeholder="long-distance hikers">
    <label for="qBetter">What should it do better?</label>
    <input type="text" id="qBetter" placeholder="charge gear without adding weight">
    <label>Composed brief</label>
    <div class="composed" id="composed">Answer the prompts and your brief appears here.</div>
    <div class="row"><button type="button" class="ghost small" id="useComposed">Use this brief</button></div>

    <label for="sketch">Sketch (optional, PNG/JPEG)</label>
    <input type="file" id="sketch" accept="image/png,image/jpeg">
    <img id="sketchPreview" alt="Sketch preview">

    <label>Voice memo (optional)</label>
    <div class="recwrap">
      <button type="button" class="ghost small recbtn" id="record">Record memo</button>
      <span class="hint" id="recState">or attach a WAV:</span>
      <input type="file" id="audio" accept="audio/wav,audio/x-wav" style="width:auto">
    </div>
    <audio id="playback" controls style="display:none; width:100%; margin-top:8px"></audio>

    <label for="model">Model</label>
    <select id="model">
      <option value="muse-spark-1.3" selected>muse-spark-1.3 (recommended)</option>
      <option value="muse-spark-1.2">muse-spark-1.2</option>
      <option value="muse-spark-1.1">muse-spark-1.1</option>
      <option value="muse-wonder-1.0">muse-wonder-1.0 (flagship)</option>
    </select>
    <div class="row">
      <input type="checkbox" id="dry" style="width:auto">
      <label for="dry" style="margin:0; text-transform:none; letter-spacing:0; font-size:14px; font-weight:500; color:var(--text-2)">Dry run (scripted, no key)</label>
    </div>
    <div class="row"><button id="go" class="grow">Ideate</button></div>
    <p class="hint">Live runs need META_API_KEY on the server and take ~1-2 min (panel, then a render per card). Watch them think on the right.</p>
  </section>

  <section aria-label="Results">
    <div class="showcase" id="demo" aria-label="Anyone can become an inventor">
      <h2>Anyone can become an Inventor…</h2>
      <p class="lede">No workshop required. Watch Muse invent live: it dreams a spark, a simulated panel argues it, and an idea card lands — eight inventions, then it rests. Saved in your browser, so they are still here when you come back.</p>
      <div class="demo-flow" aria-hidden="true">
        <span class="demo-node" id="dnSpark">AI dreams a spark</span><span class="demo-arrow">→</span>
        <span class="demo-node" id="dnPanel">Panel argues</span><span class="demo-arrow">→</span>
        <span class="demo-node" id="dnCost">Costing</span><span class="demo-arrow">→</span>
        <span class="demo-node" id="dnCard">Idea created</span>
      </div>
      <div class="demo-status" id="demoStatus">Warming up the workshop…</div>
      <div class="cards demo-cards" id="demoCards"></div>
      <div class="row">
        <button type="button" class="ghost small" id="demoReplay">Replay 8 inventions</button>
        <button type="button" class="ghost small" id="demoClear">Clear saved</button>
        <span class="hint" id="demoCount"></span>
      </div>
    </div>
    <div id="realResults" hidden>
    <div class="statusline">
      <span class="pill waiting" id="pill">Waiting</span>
      <span class="status-text" id="status">Describe an invention and hit Ideate.</span>
        <button type="button" class="ghost small" id="showDemo" hidden>Show inventor examples</button>
      </div>
    <div class="progress" id="progress"><i></i></div>
    <article class="hero" id="hero"></article>
    <div class="cards" id="cards"></div>
    <div class="stages" id="stages"></div>
    </div>
  </section>
</main>
<footer>Ideate with Meta · Created by Meta Muse Wonder · ADK-Rust example · Muse Spark (panel + converger) · Muse Voice Transcribe (memos) · Muse Image (renders) · Pipeline: intake → parallel panel → converger → render.</footer>
<script>
const $ = (id) => document.getElementById(id);
const SPARKS = [
  { text: "a better solar backpack for long-distance hikers", what: "a solar backpack", who: "long-distance hikers", better: "charge gear without adding weight" },
  { text: "a near-silent desk fan for small apartments that still moves real air", what: "a desk fan", who: "small-apartment dwellers", better: "move real air almost silently" },
  { text: "a kettle that knows when the baby is asleep", what: "a smart kettle", who: "new parents", better: "heat water without waking the baby" },
  { text: "a bike lock that cannot be forgotten at home", what: "a bike lock", who: "forgetful cyclists", better: "never get left at home" },
  { text: "a lunchbox that keeps soup hot and salad cold in one box", what: "a dual-zone lunchbox", who: "commuters", better: "keep hot and cold foods apart in one box" },
  { text: "a houseplant pot that asks for water before the plant suffers", what: "a self-reporting plant pot", who: "busy plant owners", better: "ask for water before the plant suffers" },
  { text: "a beach umbrella that follows the sun by itself", what: "a sun-tracking beach umbrella", who: "beachgoers", better: "follow the sun without manual tilting" },
  { text: "a grocery bag that weighs itself and writes the list", what: "a self-weighing grocery bag", who: "weekly shoppers", better: "weigh itself and write the shopping list" },
  { text: "a doorbell that recognizes the dog walker", what: "a recognizing doorbell", who: "dog owners", better: "recognize familiar visitors like the dog walker" },
  { text: "a camping stove that charges a phone from leftover heat", what: "a heat-recovery camping stove", who: "campers", better: "charge a phone from leftover heat" },
  { text: "an umbrella that dries itself before it drips indoors", what: "a self-drying umbrella", who: "commuters", better: "dry itself before dripping indoors" },
  { text: "a notebook that turns handwriting into calendar entries", what: "a smart paper notebook", who: "planners", better: "turn handwriting into calendar entries" }
];
// --- Auto-demo: AI invents 8 ideas on the right, saved in the browser. ---
const DEMO_KEY = "ideate.demo.v2";
const DEMO_MAX = 8;
let demoIdeas = [];
let demoDismissed = false;
let demoRunning = false;
let demoStop = false;
try {
  const saved = JSON.parse(localStorage.getItem(DEMO_KEY) || "null");
  if (saved) { demoIdeas = saved.ideas || []; demoDismissed = !!saved.dismissed; }
} catch (e) { /* fresh browser */ }
function saveDemo() {
  try { localStorage.setItem(DEMO_KEY, JSON.stringify({ ideas: demoIdeas, dismissed: demoDismissed })); }
  catch (e) {
    try {
      const slim = demoIdeas.map((d) => ({ ...d, card: { ...d.card, image: null } }));
      localStorage.setItem(DEMO_KEY, JSON.stringify({ ideas: slim, dismissed: demoDismissed }));
    } catch (e2) { /* storage blocked */ }
  }
}
function thumbHtml(card) {
  if (card.image && card.image.base64) return "<img class='thumb' alt='Concept render for " + escapeHtml(card.title) + "' src='data:" + card.image.mime + ";base64," + card.image.base64 + "'>";
  return "<div class='thumb thumb-fallback' aria-hidden='true'>" + escapeHtml((card.title || "I").slice(0, 1)) + "</div>";
}
function demoCardHtml(idea, i) {
  const card = idea.card;
  return `<article class="card${i === 0 ? " top" : ""}">
      <div class="demo-spark">Spark: ${escapeHtml(idea.spark || "")}</div>
      ${thumbHtml(card)}
      <h3>${escapeHtml(card.title)}</h3>
      <p>${escapeHtml(card.pitch)}</p>
      <div class="meta">Novelty ${(Math.min(1, Math.max(0, card.novelty)) * 100).toFixed(0)}%</div><div class="bar"><i style="width:${Math.min(100, Math.max(0, card.novelty * 100))}%"></i></div>
      <div class="meta">Feasibility ${(Math.min(1, Math.max(0, card.feasibility)) * 100).toFixed(0)}%</div><div class="bar"><i style="width:${Math.min(100, Math.max(0, card.feasibility * 100))}%"></i></div>
      <div class="cost">$${Number(card.unit_cost_usd).toFixed(2)}<span class="meta"> / unit</span></div>
      <div class="meta">Mechanism: ${escapeHtml(card.mechanism || "—")}</div>
      <div class="meta">Next step: ${escapeHtml(card.next_step || "—")}</div>
      <div class="row pdfrow"><button type="button" class="small" data-pdf-demo="${i}">Download invention brief (PDF)</button><button type="button" class="ghost small" data-use="${i}">Use this spark</button></div>
    </article>`;
}
function renderDemo() {
  $("demoCards").innerHTML = demoIdeas.map(demoCardHtml).join("");
  $("demoCount").textContent = demoIdeas.length ? demoIdeas.length + " / " + DEMO_MAX + " inventions saved in your browser" : "";
  document.querySelectorAll("[data-pdf-demo]").forEach((b) => b.addEventListener("click", () => {
    const idea = demoIdeas[Number(b.getAttribute("data-pdf-demo"))];
    if (idea) downloadBrief(idea.card, idea.brief || idea.spark);
  }));
  document.querySelectorAll("[data-use]").forEach((b) => b.addEventListener("click", () => {
    const idea = demoIdeas[Number(b.getAttribute("data-use"))];
    if (!idea) return;
    $("spark").value = idea.brief || idea.spark;
    setStatus("decided", "Demo spark loaded on the left — tweak it, then Ideate for the full panel and render.");
  }));
}
function setDemoNode(id, mode) {
  const el = $(id); if (!el) return;
  el.classList.toggle("on", mode === "on");
  el.classList.toggle("done", mode === "done");
}
function resetDemoNodes() { ["dnSpark", "dnPanel", "dnCost", "dnCard"].forEach((id) => setDemoNode(id, "")); }
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
async function runDemo() {
  if (demoRunning || demoDismissed) return;
  if (demoIdeas.length >= DEMO_MAX) {
    resetDemoNodes(); ["dnSpark", "dnPanel", "dnCost", "dnCard"].forEach((id) => setDemoNode(id, "done"));
    $("demoStatus").innerHTML = "<b>Eight inventions, done.</b> Yours could be next — describe it on the left and hit Ideate.";
    renderDemo(); return;
  }
  demoRunning = true; demoStop = false;
  const order = [...SPARKS].sort(() => Math.random() - 0.5);
  while (demoIdeas.length < DEMO_MAX && !demoStop) {
    const seed = order.find((s) => !demoIdeas.some((d) => d.spark === s.text)) || order[demoIdeas.length % order.length];
    resetDemoNodes(); setDemoNode("dnSpark", "on");
    $("demoStatus").innerHTML = "AI is dreaming a spark… <b>" + escapeHtml(seed.text) + "</b>";
    // Simulated panel: stages light up while the real AI call creates the card.
    const phases = [["dnPanel", "Wild inventor, engineer and market scout are arguing…"], ["dnCost", "Converger is costing the favourite…"]];
    let phase = 0;
    const ticker = setInterval(() => {
      if (phase < phases.length) { setDemoNode("dnSpark", "done"); setDemoNode(phases[phase][0], "on"); $("demoStatus").innerHTML = phases[phase][1] + " <b>" + escapeHtml(seed.text) + "</b>"; phase++; }
    }, 2000);
    try {
      let data = null;
      const ask = async (dry) => {
        const r = await fetch("/api/demo-idea", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ spark: seed.text, what: seed.what, model: $("model").value, dry_run: dry }) });
        const j = await r.json(); if (!r.ok) throw new Error(j.error || ("HTTP " + r.status)); return j;
      };
      try { data = await ask($("dry").checked); } catch (e) { data = await ask(true); }
      clearInterval(ticker);
      if (demoStop) break;
      setDemoNode("dnPanel", "done"); setDemoNode("dnCost", "done"); setDemoNode("dnCard", "done");
      demoIdeas.push(data); saveDemo(); renderDemo();
      $("demoStatus").innerHTML = "Idea " + demoIdeas.length + " of " + DEMO_MAX + " created: <b>" + escapeHtml(data.card.title) + "</b>";
    } catch (error) {
      clearInterval(ticker);
      $("demoStatus").textContent = "Demo paused: " + String(error.message || error);
      break;
    }
    if (demoIdeas.length < DEMO_MAX && !demoStop) await wait(4200);
  }
  demoRunning = false;
  if (!demoStop && demoIdeas.length >= DEMO_MAX) $("demoStatus").innerHTML = "<b>Eight inventions, done.</b> Yours could be next — describe it on the left and hit Ideate.";
}
function hideDemoForReal() {
  demoStop = true; demoDismissed = true; saveDemo();
  $("demo").hidden = true; $("realResults").hidden = false; $("showDemo").hidden = false;
}
$("showDemo").onclick = () => {
  demoDismissed = false; saveDemo();
  $("demo").hidden = false; $("realResults").hidden = true;
  renderDemo(); runDemo();
};
$("demoReplay").onclick = () => { demoIdeas = []; demoDismissed = false; saveDemo(); renderDemo(); runDemo(); };
$("demoClear").onclick = () => { demoStop = true; demoIdeas = []; saveDemo(); renderDemo(); resetDemoNodes(); $("demoStatus").textContent = "Saved inventions cleared. Replay to watch Muse invent again."; };
renderDemo();
if (demoDismissed) { $("demo").hidden = true; $("realResults").hidden = false; $("showDemo").hidden = false; }
else { setTimeout(runDemo, 1600); }

function fillSteps(fields) {
  $("qWhat").value = fields.what || "";
  $("qWho").value = fields.who || "";
  $("qBetter").value = fields.better || "";
  compose();
}
$("shuffle").onclick = () => {
  const current = $("spark").value;
  let next = SPARKS[Math.floor(Math.random() * SPARKS.length)];
  if (next.text === current) next = SPARKS[(SPARKS.indexOf(next) + 1) % SPARKS.length];
  $("spark").value = next.text;
  fillSteps(next);
  setStatus("decided", "Spark loaded into the steps below — tweak any field, then Ideate.");
};
const compose = () => {
  const what = $("qWhat").value.trim(), who = $("qWho").value.trim(), better = $("qBetter").value.trim();
  $("composed").textContent = (what || who || better)
    ? "I want to invent " + (what || "something new") + (who ? " for " + who : "") + (better ? " that " + better : "") + "."
    : "Answer the prompts and your brief appears here.";
};
["qWhat", "qWho", "qBetter"].forEach((id) => $(id).addEventListener("input", compose));
$("useComposed").onclick = () => {
  const text = $("composed").textContent;
  if (text.startsWith("I want to invent")) $("spark").value = text;
};
$("expand").onclick = async () => {
  const fragment = $("spark").value.trim();
  if (!fragment) { setStatus("failed", "Write a spark first, then expand it."); return; }
  $("expand").disabled = true;
  try {
    const response = await fetch("/api/expand", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ fragment, model: $("model").value, dry_run: $("dry").checked }) });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || ("HTTP " + response.status));
    $("spark").value = data.brief;
    setStatus("decided", "Brief expanded — splitting it into steps…");
    try {
      const split = await fetch("/api/decompose", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ brief: data.brief, model: $("model").value, dry_run: $("dry").checked }) });
      const fields = await split.json();
      if (split.ok) fillSteps(fields);
    } catch (splitError) { /* steps stay manual */ }
    setStatus("decided", "Brief expanded and split into steps — tweak any field, then Ideate.");
  } catch (error) { setStatus("failed", String(error.message || error)); }
  finally { $("expand").disabled = false; }
};
$("sketch").onchange = () => {
  const file = $("sketch").files[0];
  const preview = $("sketchPreview");
  if (file) { preview.src = URL.createObjectURL(file); preview.style.display = "block"; }
  else { preview.style.display = "none"; }
};

// Voice memo: record mic audio, downsample to 16kHz mono WAV in-page.
let mediaRecorder = null, audioChunks = [], recordedWav = null, recordStarted = 0, recordTimer = null;
$("record").onclick = async () => {
  if (mediaRecorder) { mediaRecorder.stream.getTracks().forEach((t) => t.stop()); return; }
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const ctx = new AudioContext();
    const source = ctx.createMediaStreamSource(stream);
    const processor = ctx.createScriptProcessor(4096, 1, 1);
    const buffers = [];
    processor.onaudioprocess = (event) => buffers.push(new Float32Array(event.inputBuffer.getChannelData(0)));
    source.connect(processor); processor.connect(ctx.destination);
    mediaRecorder = { stream, ctx, processor, buffers };
    recordStarted = Date.now();
    $("record").textContent = "Stop recording"; $("record").classList.add("recording");
    $("recState").textContent = "Recording… 0s";
    recordTimer = setInterval(() => { $("recState").textContent = "Recording… " + Math.floor((Date.now() - recordStarted) / 1000) + "s"; }, 500);
    stream.getAudioTracks()[0].onended = stopRecording;
  } catch (error) { setStatus("failed", "Microphone unavailable: " + error.message); }
};
function stopRecording() {
  if (!mediaRecorder) return;
  clearInterval(recordTimer);
  const { ctx, buffers } = mediaRecorder;
  mediaRecorder = null;
  $("record").textContent = "Record memo"; $("record").classList.remove("recording");
  const total = buffers.reduce((n, b) => n + b.length, 0);
  const merged = new Float32Array(total);
  let offset = 0; for (const b of buffers) { merged.set(b, offset); offset += b.length; }
  recordedWav = encodeWav(downsample(merged, ctx.sampleRate, 16000), 16000);
  const blob = new Blob([recordedWav], { type: "audio/wav" });
  $("playback").src = URL.createObjectURL(blob);
  $("playback").style.display = "block";
  $("recState").textContent = "Memo ready (" + (total / ctx.sampleRate).toFixed(1) + "s) — it will be transcribed on Ideate.";
  ctx.close();
}
function downsample(buffer, from, to) {
  if (from === to) return buffer;
  const ratio = from / to, out = new Float32Array(Math.floor(buffer.length / ratio));
  for (let i = 0; i < out.length; i++) out[i] = buffer[Math.floor(i * ratio)];
  return out;
}
function encodeWav(samples, rate) {
  const buffer = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(buffer);
  const write = (o, s) => { for (let i = 0; i < s.length; i++) view.setUint8(o + i, s.charCodeAt(i)); };
  write(0, "RIFF"); view.setUint32(4, 36 + samples.length * 2, true); write(8, "WAVEfmt ");
  view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, rate, true); view.setUint32(28, rate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
  write(36, "data"); view.setUint32(40, samples.length * 2, true);
  samples.forEach((s, i) => view.setInt16(44 + i * 2, Math.max(-1, Math.min(1, s)) * 0x7fff, true));
  return new Uint8Array(buffer);
}

function setStatus(kind, text) {
  const pill = $("pill");
  pill.className = "pill " + (kind === "done" ? "done" : kind === "decided" ? "decided" : kind === "failed" ? "waiting" : "waiting");
  pill.textContent = kind === "done" ? "Done" : kind === "decided" ? "Ready" : kind === "failed" ? "Failed" : "Working";
  if (kind === "failed") pill.style.background = "#fee2e2", pill.style.color = "#b91c1c";
  else pill.style.background = "", pill.style.color = "";
  $("status").textContent = text;
}
const readAsDataUrl = (file) => new Promise((resolve, reject) => {
  const reader = new FileReader();
  reader.onload = () => resolve(reader.result);
  reader.onerror = reject;
  reader.readAsDataURL(file);
});
const blobToDataUrl = (blob) => readAsDataUrl(blob);

$("go").onclick = async () => {
  const brief = $("spark").value.trim();
  if (!brief) { setStatus("failed", "Write a spark first."); return; }
  hideDemoForReal();
  $("go").disabled = true;
  setStatus("working", "Panel is thinking — watch it work below…");
  $("progress").style.display = "block";
  $("progress").firstElementChild.style.width = "55%";
  $("hero").style.display = "none"; $("hero").innerHTML = "";
  $("cards").innerHTML = "";
  const stages = $("stages"); stages.style.display = "block"; stages.textContent = "";
  try {
    const payload = { brief, model: $("model").value, dry_run: $("dry").checked };
    const sketch = $("sketch").files[0];
    if (sketch) { payload.sketch_base64 = await readAsDataUrl(sketch); payload.sketch_mime = sketch.type || "image/png"; }
    if (recordedWav) { payload.audio_base64 = await blobToDataUrl(new Blob([recordedWav], { type: "audio/wav" })); }
    else {
      const audio = $("audio").files[0];
      if (audio) payload.audio_base64 = await readAsDataUrl(audio);
    }
    const response = await fetch("/api/ideate/stream", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(payload) });
    if (!response.ok || !response.body) throw new Error("HTTP " + response.status);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "", result = null;
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let split;
      while ((split = buffer.indexOf("\n\n")) >= 0) {
        const frame = buffer.slice(0, split); buffer = buffer.slice(split + 2);
        const event = (frame.match(/^event: (.*)$/m) || [])[1];
        const dataLine = (frame.match(/^data: (.*)$/m) || [])[1];
        if (!dataLine) continue;
        const data = JSON.parse(dataLine);
        if (event === "stage") { stages.textContent += data.line + "\n"; stages.scrollTop = stages.scrollHeight; }
        else if (event === "result") result = data;
        else if (event === "error") throw new Error(data.error);
      }
    }
    if (!result) throw new Error("Stream ended without a result.");
    $("progress").firstElementChild.style.width = "100%";
    renderResult(result);
    setStatus("done", result.cards.length + " idea cards · " + result.cost_calls + " costing calls" + (result.transcript ? " · memo transcribed" : ""));
  } catch (error) {
    setStatus("failed", String(error.message || error));
  } finally {
    $("go").disabled = false;
    setTimeout(() => { $("progress").style.display = "none"; $("progress").firstElementChild.style.width = "30%"; }, 600);
  }
};

function renderResult(data) {
  const top = data.cards[0];
  if (top) {
    $("hero").style.display = "block";
    $("hero").innerHTML =
      (data.render ? "<img alt='Top concept render' src='data:" + data.render.mime + ";base64," + data.render.base64 + "'>" : "") +
      "<div class='body'><div class='kicker'>Top idea</div><h2>" + escapeHtml(top.title) + "</h2><p>" + escapeHtml(top.pitch) + "</p>" +
      "<div class='stats'>" +
      "<div class='stat'><b>$" + top.unit_cost_usd.toFixed(2) + "</b><span>per unit</span></div>" +
      "<div class='stat'><b>" + (top.novelty * 100).toFixed(0) + "%</b><span>novelty</span></div>" +
      "<div class='stat'><b>" + (top.feasibility * 100).toFixed(0) + "%</b><span>feasibility</span></div>" +
      "<div class='stat'><b>" + escapeHtml(top.next_step || "—") + "</b><span>next step</span></div>" +
      "</div><div class='row pdfrow'><button type='button' data-pdf-real='0'>Download invention brief (PDF)</button></div></div>";
  }
  window.__lastCards = data.cards;
  $("cards").innerHTML = data.cards.map((card, i) => `
    <article class="card${i === 0 ? " top" : ""}">
      ${thumbHtml(card)}
      <h3>${i + 1}. ${escapeHtml(card.title)}</h3>
      <p>${escapeHtml(card.pitch)}</p>
      <div class="meta">Novelty ${(Math.min(1, Math.max(0, card.novelty)) * 100).toFixed(0)}%</div><div class="bar"><i style="width:${Math.min(100, Math.max(0, card.novelty * 100))}%"></i></div>
      <div class="meta">Feasibility ${(Math.min(1, Math.max(0, card.feasibility)) * 100).toFixed(0)}%</div><div class="bar"><i style="width:${Math.min(100, Math.max(0, card.feasibility * 100))}%"></i></div>
      <div class="cost">$${card.unit_cost_usd.toFixed(2)}<span class="meta"> / unit</span></div>
      <div class="meta">Mechanism: ${escapeHtml(card.mechanism || "—")}</div>
      <div class="meta">Next step: ${escapeHtml(card.next_step || "—")}</div>
      ${card.risks && card.risks.length ? "<ul>" + card.risks.map(r => "<li>" + escapeHtml(r) + "</li>").join("") + "</ul>" : ""}
      <div class="row pdfrow"><button type="button" class="small" data-pdf-real="${i}">Download invention brief (PDF)</button></div>
    </article>`).join("");
  document.querySelectorAll("[data-pdf-real]").forEach((b) => b.addEventListener("click", () => {
    const card = (window.__lastCards || [])[Number(b.getAttribute("data-pdf-real"))];
    if (card) downloadBrief(card, $("spark").value.trim());
  }));
}

// --- Invention brief PDF: transforms one card into a formatted document. ---
function pdfSanitize(s) {
  return String(s == null ? "" : s).replace(/[^\x20-\x7E]/g, "-").replace(/\\/g, "\\\\").replace(/\(/g, "\\(").replace(/\)/g, "\\)");
}
function pdfWrap(s, n) {
  const words = String(s || "").split(/\s+/).filter(Boolean);
  const lines = []; let cur = "";
  for (const w of words) { if ((cur + " " + w).trim().length > n) { if (cur) lines.push(cur); cur = w; } else cur = (cur + " " + w).trim(); }
  if (cur) lines.push(cur);
  return lines.length ? lines : [""];
}
async function cardJpeg(card) {
  if (!card.image || !card.image.base64) return null;
  try {
    const img = new Image();
    await new Promise((res, rej) => { img.onload = res; img.onerror = rej; img.src = "data:" + card.image.mime + ";base64," + card.image.base64; });
    const scale = Math.min(1, 900 / img.naturalWidth);
    const w = Math.max(1, Math.round(img.naturalWidth * scale)), h = Math.max(1, Math.round(img.naturalHeight * scale));
    const canvas = document.createElement("canvas"); canvas.width = w; canvas.height = h;
    canvas.getContext("2d").drawImage(img, 0, 0, w, h);
    const b64 = canvas.toDataURL("image/jpeg", 0.86).split(",")[1];
    const bin = atob(b64); const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return { bytes, w, h };
  } catch (e) { return null; }
}
async function downloadBrief(card, brief) {
  const img = await cardJpeg(card);
  const W = 595, H = 842, M = 48, CW = W - M * 2;
  let pages = [], ops = [], y = 0;
  const op = (s) => ops.push(s);
  const text = (s, x, yy, size, font, r, g, b) => op("BT /" + font + " " + size + " Tf " + r + " " + g + " " + b + " rg 1 0 0 1 " + x + " " + yy + " Tm (" + pdfSanitize(s) + ") Tj ET");
  const rect = (x, yy, w, h, r, g, b) => op(r + " " + g + " " + b + " rg " + x + " " + yy + " " + w + " " + h + " re f");
  function newPage(first) {
    if (ops.length) pages.push(ops.join("\n"));
    ops = [];
    rect(0, 0, W, H, 1, 1, 1);
    if (first) {
      rect(0, H - 62, W, 62, 0.97, 0.30, 0.00);
      text("INVENTION BRIEF", M, H - 38, 19, "F2", 1, 1, 1);
      text("Ideate with Meta - Created by Meta Muse Wonder", M, H - 54, 8.5, "F1", 1, 1, 1);
      y = H - 92;
    } else { y = H - 64; }
  }
  function need(h) { if (y - h < 58) newPage(false); }
  function para(s, size, font, r, g, b, gap) {
    const per = Math.max(18, Math.floor(CW / (size * 0.50)));
    for (const line of pdfWrap(s, per)) { need(size + 6); text(line, M, y, size, font, r, g, b); y -= size + 5; }
    y -= gap || 4;
  }
  function heading(s) {
    need(30); y -= 6;
    rect(M, y - 3, 3, 14, 0.97, 0.30, 0.00);
    text(s.toUpperCase(), M + 9, y, 10.5, "F2", 0.06, 0.09, 0.16);
    y -= 17;
  }
  function bullet(s) {
    const per = Math.max(18, Math.floor((CW - 14) / 5.3));
    pdfWrap(s, per).forEach((line, i) => { need(16); text(i === 0 ? "-  " + line : "   " + line, M + 2, y, 10, "F1", 0.20, 0.25, 0.33); y -= 14; });
  }
  function bar(label, v) {
    const pct = Math.round(Math.min(1, Math.max(0, Number(v) || 0)) * 100);
    need(24); text(label + "  " + pct + "%", M, y, 9.5, "F1", 0.20, 0.25, 0.33);
    rect(M + 130, y - 2, 180, 7, 0.90, 0.91, 0.93); rect(M + 130, y - 2, 180 * pct / 100, 7, 0.97, 0.30, 0.00);
    y -= 20;
  }
  newPage(true);
  para(card.title || "Untitled invention", 23, "F2", 0.06, 0.09, 0.16, 2);
  para(card.pitch || "", 12, "F3", 0.28, 0.33, 0.42, 8);
  if (img) {
    let dw = CW, dh = dw * img.h / img.w;
    if (dh > 225) { dh = 225; dw = Math.round(dh * img.w / img.h); }
    need(dh + 30);
    const ix = M + Math.round((CW - dw) / 2);
    op("q " + dw + " 0 0 " + dh + " " + ix + " " + (y - dh) + " cm /Im1 Do Q");
    y -= dh + 6;
    text("Concept render - muse-image-1.0", M, y, 8, "F1", 0.39, 0.45, 0.55); y -= 18;
  }
  // Score strip
  need(42); rect(M, y - 22, CW, 34, 0.96, 0.97, 0.99);
  text("UNIT COST", M + 12, y + 2, 7.5, "F2", 0.39, 0.45, 0.55); text("$" + Number(card.unit_cost_usd || 0).toFixed(2), M + 12, y - 14, 13, "F2", 0.06, 0.09, 0.16);
  text("NOVELTY", M + 170, y + 2, 7.5, "F2", 0.39, 0.45, 0.55); text(Math.round(Math.min(1, Math.max(0, card.novelty || 0)) * 100) + "%", M + 170, y - 14, 13, "F2", 0.06, 0.09, 0.16);
  text("FEASIBILITY", M + 320, y + 2, 7.5, "F2", 0.39, 0.45, 0.55); text(Math.round(Math.min(1, Math.max(0, card.feasibility || 0)) * 100) + "%", M + 320, y - 14, 13, "F2", 0.06, 0.09, 0.16);
  y -= 50;
  text("Prepared " + new Date().toLocaleDateString() + " - scores and costs are panel estimates, not quotes.", M, y, 8.5, "F1", 0.39, 0.45, 0.55); y -= 14;
  if (brief) { heading("Starting brief"); para(brief, 10.5, "F1", 0.20, 0.25, 0.33, 6); }
  heading("The idea"); para(card.pitch || "-", 10.5, "F1", 0.20, 0.25, 0.33, 6);
  heading("How it works"); para(card.mechanism || "-", 10.5, "F1", 0.20, 0.25, 0.33, 6);
  heading("Panel scores"); bar("Novelty", card.novelty); bar("Feasibility", card.feasibility); y -= 4;
  heading("Unit economics");
  para("Estimated unit cost at first production run: $" + Number(card.unit_cost_usd || 0).toFixed(2) + " per unit. Treat this as a target to beat with supplier quotes, not a quote itself.", 10.5, "F1", 0.20, 0.25, 0.33, 6);
  heading("Risks to retire first");
  (card.risks && card.risks.length ? card.risks : ["No panel risks recorded."]).forEach(bullet); y -= 6;
  heading("Next step"); para(card.next_step || "-", 10.5, "F1", 0.20, 0.25, 0.33, 6);
  heading("Your first 7 days");
  ["Day 1-2: Sketch the mechanism and list every part you can buy off the shelf.", "Day 3-4: Build the roughest working version - ugly is fine, working matters.", "Day 5: Put it in front of one real user and watch without explaining.", "Day 6: Fix the one thing that embarrassed you most.", "Day 7: Decide with evidence: build properly, pivot, or park it. Next step from the panel: " + (card.next_step || "-")].forEach(bullet);
  pages.push(ops.join("\n"));
  // Footers
  pages = pages.map((c, i) => c + "\nBT /F1 7.5 Tf 0.39 0.45 0.55 rg 1 0 0 1 " + M + " 28 Tm (Created by Meta Muse Wonder - Ideate with Meta - Built with ADK-Rust - Page " + (i + 1) + " of " + pages.length + ") Tj ET");
  // Assemble PDF (byte-accurate; the JPEG stream is binary)
  const bodies = {};
  bodies[1] = "<< /Type /Catalog /Pages 2 0 R >>";
  bodies[3] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
  bodies[4] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>";
  bodies[5] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Oblique >>";
  if (img) bodies[6] = { image: img };
  const kids = [];
  let n = img ? 7 : 6;
  pages.forEach((content) => {
    const pageNum = n++, contNum = n++;
    kids.push(pageNum + " 0 R");
    bodies[pageNum] = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 " + W + " " + H + "] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >>" + (img ? " /XObject << /Im1 6 0 R >>" : "") + " >> /Contents " + contNum + " 0 R >>";
    bodies[contNum] = "<< /Length " + content.length + " >>\nstream\n" + content + "\nendstream";
  });
  bodies[2] = "<< /Type /Pages /Kids [" + kids.join(" ") + "] /Count " + pages.length + " >>";
  const enc = new TextEncoder();
  const chunks = [], offsets = {};
  let byteLen = 0;
  const push = (data) => { const b = typeof data === "string" ? enc.encode(data) : data; chunks.push(b); byteLen += b.length; };
  push("%PDF-1.4\n");
  const nums = Object.keys(bodies).map(Number).sort((a, b) => a - b);
  for (const num of nums) {
    offsets[num] = byteLen;
    push(num + " 0 obj\n");
    const body = bodies[num];
    if (body && body.image) {
      push("<< /Type /XObject /Subtype /Image /Width " + body.image.w + " /Height " + body.image.h + " /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length " + body.image.bytes.length + " >>\nstream\n");
      push(body.image.bytes);
      push("\nendstream\nendobj\n");
    } else push(body + "\nendobj\n");
  }
  const xref = byteLen, total = Math.max(...nums) + 1;
  let tail = "xref\n0 " + total + "\n0000000000 65535 f \n";
  for (let i = 1; i < total; i++) tail += String(offsets[i] || 0).padStart(10, "0") + " 00000 n \n";
  tail += "trailer\n<< /Size " + total + " /Root 1 0 R >>\nstartxref\n" + xref + "\n%%EOF";
  push(tail);
  const blob = new Blob(chunks, { type: "application/pdf" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = (card.title || "invention").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") + "-invention-brief.pdf";
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 4000);
}

function escapeHtml(text) {
  return String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
</script>
</body>
</html>
"##;
