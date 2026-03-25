//! OpenRouter — live integration example.
//!
//! Exercises the OpenRouter integration through the full ADK agent stack
//! (Runner → LlmAgent → OpenRouterClient) and also validates the native
//! discovery APIs:
//!
//!   1. Basic text generation (chat mode)
//!   2. Streaming text generation
//!   3. Function calling round-trip
//!   4. Conversation continuation through sessions
//!   5. Responses mode with reasoning via `OpenRouterRequestOptions`
//!   6. Provider routing and model fallback options
//!   7. Built-in web search via Responses server tools (optional)
//!   8. Multimodal image input and PDF parsing plugin path
//!   9. Native discovery endpoints (`/models`, `/providers`, `/credits`)
//!
//! # Usage
//!
//! ```bash
//! export OPENROUTER_API_KEY=sk-or-...
//! cargo run --manifest-path examples/openrouter/Cargo.toml
//! ```
//!
//! Optional environment variables:
//! - `OPENROUTER_MODEL`
//! - `OPENROUTER_BASE_URL`
//! - `OPENROUTER_SITE_URL`
//! - `OPENROUTER_APP_NAME`
//! - `OPENROUTER_REASONING_MODEL`
//! - `OPENROUTER_FALLBACK_MODEL`
//! - `OPENROUTER_ENABLE_WEB_SEARCH=1`
//! - `OPENROUTER_WEB_SEARCH_TOOL=web_search_preview`
//! - `OPENROUTER_IMAGE_URL`
//! - `OPENROUTER_PDF_URL`
//! - `OPENROUTER_ENABLE_PDF=1`

use adk_core::{GenerateContentConfig, SessionId, UserId};
use adk_model::openrouter::{
    OpenRouterApiMode, OpenRouterClient, OpenRouterConfig, OpenRouterPlugin,
    OpenRouterProviderPreferences, OpenRouterReasoningConfig, OpenRouterRequestOptions,
    OpenRouterResponseTool,
};
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const APP_NAME: &str = "openrouter-example";
const DEFAULT_MODEL: &str = "openai/gpt-4.1-mini";
const DEFAULT_REASONING_MODEL: &str = "openai/gpt-4.1-mini";
const DEFAULT_SITE_URL: &str = "https://github.com/zavora-ai/adk-rust";
const DEFAULT_APP_NAME: &str = "ADK-Rust OpenRouter Example";
const DEFAULT_WEB_SEARCH_TOOL: &str = "web_search_preview";
const DEFAULT_IMAGE_URL: &str =
    "https://upload.wikimedia.org/wikipedia/commons/3/3f/JPEG_example_flower.jpg";
const DEFAULT_PDF_URL: &str =
    "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf";
const MAX_OUTPUT_TOKENS: i32 = 256;
type ScenarioFuture = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
type ScenarioFn = fn() -> ScenarioFuture;

fn load_dotenv() {
    if let Some(dotenv_path) = find_dotenv_path() {
        let _ = dotenvy::from_path(dotenv_path);
    }
}

fn find_dotenv_path() -> Option<std::path::PathBuf> {
    let Ok(mut dir) = std::env::current_dir() else {
        return None;
    };

    loop {
        let dotenv_path = dir.join(".env");
        if dotenv_path.is_file() {
            return Some(dotenv_path);
        }

        if !dir.pop() {
            return None;
        }
    }
}

fn dotenv_value(key: &str) -> Option<String> {
    let dotenv_path = find_dotenv_path()?;
    let contents = std::fs::read_to_string(dotenv_path).ok()?;

    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find_map(|line| {
            let (line_key, line_value) = line.split_once('=')?;
            (line_key.trim() == key).then(|| line_value.trim().trim_matches('"').to_string())
        })
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .or_else(|| dotenv_value(key))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn separator(title: &str) {
    println!("\n{}", "=".repeat(72));
    println!("  {title}");
    println!("{}\n", "=".repeat(72));
}

fn env_or(key: &str, default: &str) -> String {
    env_value(key)
        .unwrap_or_else(|| default.to_string())
}

fn env_flag(key: &str) -> bool {
    env_value(key).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn make_model(model_name: &str, api_mode: OpenRouterApiMode) -> Arc<OpenRouterClient> {
    let api_key = env_value("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY must be set");
    let config = OpenRouterConfig::new(api_key, model_name)
        .with_base_url(env_or("OPENROUTER_BASE_URL", "https://openrouter.ai/api/v1"))
        .with_http_referer(env_or("OPENROUTER_SITE_URL", DEFAULT_SITE_URL))
        .with_title(env_or("OPENROUTER_APP_NAME", DEFAULT_APP_NAME))
        .with_default_api_mode(api_mode);

    Arc::new(OpenRouterClient::new(config).expect("client creation should succeed"))
}

fn base_generation_config() -> GenerateContentConfig {
    GenerateContentConfig { max_output_tokens: Some(MAX_OUTPUT_TOKENS), ..Default::default() }
}

fn config_with_options(options: OpenRouterRequestOptions) -> GenerateContentConfig {
    let mut config = base_generation_config();
    options
        .insert_into_config(&mut config)
        .expect("OpenRouter options should serialize");
    config
}

fn fallback_models(primary: &str) -> Vec<String> {
    let mut models = vec![primary.to_string()];
    let fallback = env_or("OPENROUTER_FALLBACK_MODEL", DEFAULT_MODEL);
    if fallback != primary {
        models.push(fallback);
    }

    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    models
}

async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;

    Ok(Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?)
}

async fn collect_text(
    runner: &Runner,
    session_id: &str,
    content: Content,
) -> anyhow::Result<String> {
    let mut stream = runner.run(UserId::new("user")?, SessionId::new(session_id)?, content).await?;
    let mut full_text = String::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    print!("{text}");
                    full_text.push_str(text);
                }
            }
        }
    }
    println!();
    Ok(full_text)
}

fn provider_metadata_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
}

async fn test_basic_chat() -> anyhow::Result<()> {
    separator("1. Basic text generation (chat mode)");

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::ChatCompletions);
    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-basic")
            .instruction("You are a concise and friendly assistant.")
            .model(model)
            .generate_content_config(base_generation_config())
            .build()?,
    );

    let runner = make_runner(agent, "basic").await?;
    let text = collect_text(
        &runner,
        "basic",
        Content::new("user").with_text(
            "What is OpenRouter, and why would an AI engineer use it? Keep it to two sentences.",
        ),
    )
    .await?;

    assert!(!text.trim().is_empty(), "basic chat should return text");
    println!("✓ Basic text generation passed");
    Ok(())
}

async fn test_streaming_chat() -> anyhow::Result<()> {
    separator("2. Streaming text generation");

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::ChatCompletions);
    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-streaming")
            .instruction("You are a concise assistant. Output exactly what the user asks for.")
            .model(model)
            .generate_content_config(base_generation_config())
            .build()?,
    );

    let runner = make_runner(agent, "streaming").await?;
    let content = Content::new("user")
        .with_text("Output exactly five lines containing only 1, 2, 3, 4, and 5 in that order.");
    let mut stream =
        runner.run(UserId::new("user")?, SessionId::new("streaming")?, content).await?;

    let mut partial_count = 0usize;
    let mut full_text = String::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if event.llm_response.partial {
            partial_count += 1;
        }
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                    full_text.push_str(text);
                }
            }
        }
    }

    println!();
    println!("Received {partial_count} streaming partial events");
    assert!(partial_count > 0, "streaming chat should yield partial events");
    assert!(
        full_text.contains('1') && full_text.contains('5'),
        "streaming output should include the requested sequence"
    );
    println!("✓ Streaming text generation passed");
    Ok(())
}

async fn test_function_calling() -> anyhow::Result<()> {
    separator("3. Function calling round-trip");

    #[derive(schemars::JsonSchema, serde::Serialize)]
    struct WeatherArgs {
        city: String,
    }

    async fn get_weather(
        _ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({
            "city": city,
            "temperature_c": 24,
            "condition": "Sunny",
            "source": "stub-weather-service"
        }))
    }

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::ChatCompletions);
    let tool = Arc::new(FunctionTool::new(
        "get_weather",
        "Get the current weather for a city.",
        get_weather,
    )
    .with_parameters_schema::<WeatherArgs>());
    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-tools")
            .instruction("Use tools when they are relevant, then summarize the result for the user.")
            .model(model)
            .tool(tool)
            .generate_content_config(base_generation_config())
            .build()?,
    );

    let runner = make_runner(agent, "tools").await?;
    let content = Content::new("user").with_text("What's the weather in Nairobi right now?");
    let mut stream = runner.run(UserId::new("user")?, SessionId::new("tools")?, content).await?;

    let mut saw_function_call = false;
    let mut saw_tool_response = false;
    let mut saw_final_text = false;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, .. } => {
                        println!("  → tool call: {name}({args})");
                        saw_function_call = true;
                    }
                    Part::Text { text } => {
                        if !text.trim().is_empty() && !event.llm_response.partial {
                            print!("{text}");
                            saw_final_text = true;
                        }
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← tool result: {}", function_response.response);
                        saw_tool_response = true;
                    }
                    _ => {}
                }
            }
        }
    }
    println!();

    assert!(saw_function_call, "expected a function call event");
    assert!(
        saw_tool_response || saw_final_text,
        "expected a tool response or final text answer"
    );
    if !saw_final_text {
        println!("  [note] model stopped after the tool response without a final summary");
    }
    println!("✓ Function calling passed");
    Ok(())
}

async fn test_session_continuation() -> anyhow::Result<()> {
    separator("4. Conversation continuation through sessions");

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::ChatCompletions);
    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-memory")
            .instruction("Remember user facts across turns within the same session.")
            .model(model)
            .generate_content_config(base_generation_config())
            .build()?,
    );

    let runner = make_runner(agent, "memory").await?;
    println!("Turn 1:");
    let _ = collect_text(
        &runner,
        "memory",
        Content::new("user").with_text("My favorite programming language is Rust. Remember that."),
    )
    .await?;

    println!("Turn 2:");
    let text = collect_text(
        &runner,
        "memory",
        Content::new("user").with_text("What is my favorite programming language?"),
    )
    .await?;

    assert!(
        text.to_ascii_lowercase().contains("rust"),
        "session continuation should remember the earlier turn"
    );
    println!("✓ Session continuation passed");
    Ok(())
}

async fn test_responses_mode_reasoning() -> anyhow::Result<()> {
    separator("5. Responses mode with reasoning");

    let model_name = env_or("OPENROUTER_REASONING_MODEL", DEFAULT_REASONING_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::Responses);
    let config = config_with_options(
        OpenRouterRequestOptions::default()
            .with_api_mode(OpenRouterApiMode::Responses)
            .with_reasoning(OpenRouterReasoningConfig {
                effort: Some("low".to_string()),
                ..Default::default()
            }),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-responses")
            .instruction("Think briefly, then answer clearly.")
            .model(model)
            .generate_content_config(config)
            .build()?,
    );

    let runner = make_runner(agent, "responses").await?;
    let content = Content::new("user")
        .with_text("What is 127 * 83? Think briefly, then answer in one sentence.");
    let mut stream =
        runner.run(UserId::new("user")?, SessionId::new("responses")?, content).await?;

    let mut saw_text = false;
    let mut saw_thinking = false;
    let mut final_provider_metadata = None;

    while let Some(event) = stream.next().await {
        let event = event?;
        if !event.llm_response.partial {
            final_provider_metadata = event.llm_response.provider_metadata.clone();
        }
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                        if !saw_thinking {
                            println!("  [thinking]");
                        }
                        println!("  {thinking}");
                        saw_thinking = true;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        if saw_thinking && !saw_text {
                            println!("  [answer]");
                        }
                        print!("{text}");
                        saw_text = true;
                    }
                    _ => {}
                }
            }
        }
    }
    println!();

    if let Some(metadata) = final_provider_metadata.as_ref() {
        println!("provider metadata keys: {:?}", provider_metadata_keys(metadata));
    }
    if !saw_thinking {
        println!("  [note] no Part::Thinking was emitted for this model/account setup");
    }
    assert!(saw_text, "responses mode should return a text answer");
    println!("✓ Responses mode reasoning passed");
    Ok(())
}

async fn test_routing_and_fallback() -> anyhow::Result<()> {
    separator("6. Provider routing and model fallback");

    let primary_model = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&primary_model, OpenRouterApiMode::ChatCompletions);
    let options = OpenRouterRequestOptions::default()
        .with_models(fallback_models(&primary_model))
        .with_route("fallback")
        .with_provider_preferences(OpenRouterProviderPreferences {
            allow_fallbacks: Some(true),
            zdr: Some(true),
            ..Default::default()
        });

    let mut config = config_with_options(options);
    config.temperature = Some(0.1);

    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-routing")
            .instruction("Answer in one short sentence.")
            .model(model)
            .generate_content_config(config)
            .build()?,
    );

    let runner = make_runner(agent, "routing").await?;
    let text = collect_text(
        &runner,
        "routing",
        Content::new("user").with_text("Give one reason why routing fallback matters."),
    )
    .await?;

    println!("configured fallback models: {:?}", fallback_models(&primary_model));
    assert!(!text.trim().is_empty(), "routing scenario should return text");
    println!("✓ Provider routing and fallback passed");
    Ok(())
}

async fn test_web_search() -> anyhow::Result<()> {
    separator("7. Built-in web search via Responses tools");

    if !env_flag("OPENROUTER_ENABLE_WEB_SEARCH") {
        println!("skipped; set OPENROUTER_ENABLE_WEB_SEARCH=1 to run this scenario");
        return Ok(());
    }

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::Responses);
    let config = config_with_options(
        OpenRouterRequestOptions::default()
            .with_api_mode(OpenRouterApiMode::Responses)
            .with_response_tool(OpenRouterResponseTool {
                kind: env_or("OPENROUTER_WEB_SEARCH_TOOL", DEFAULT_WEB_SEARCH_TOOL),
                ..Default::default()
            }),
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-web-search")
            .instruction("Use available search tools when they help. Cite the result if possible.")
            .model(model)
            .generate_content_config(config)
            .build()?,
    );

    let runner = make_runner(agent, "web-search").await?;
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new("web-search")?,
            Content::new("user").with_text(
                "Find one current fact about the Rust programming language and cite the source in one sentence.",
            ),
        )
        .await?;

    let mut full_text = String::new();
    let mut citation_count = 0usize;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                    full_text.push_str(text);
                }
            }
        }
        citation_count += event
            .llm_response
            .citation_metadata
            .as_ref()
            .map(|meta| meta.citation_sources.len())
            .unwrap_or(0);
    }
    println!();
    println!("citation sources observed: {citation_count}");

    assert!(!full_text.trim().is_empty(), "web search scenario should return text");
    println!("✓ Built-in web search passed");
    Ok(())
}

async fn test_multimodal_and_plugins() -> anyhow::Result<()> {
    separator("8. Multimodal inputs and plugin path");

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let model = make_model(&model_name, OpenRouterApiMode::ChatCompletions);
    let agent = Arc::new(
        LlmAgentBuilder::new("openrouter-multimodal")
            .instruction("Describe uploaded content briefly and accurately.")
            .model(model)
            .generate_content_config(base_generation_config())
            .build()?,
    );

    let runner = make_runner(agent, "multimodal").await?;
    let image_url = env_or("OPENROUTER_IMAGE_URL", DEFAULT_IMAGE_URL);
    let image_text = collect_text(
        &runner,
        "multimodal",
        Content::new("user")
            .with_text("Describe the main subject of this image in one sentence.")
            .with_file_uri("image/jpeg", image_url),
    )
    .await?;
    assert!(!image_text.trim().is_empty(), "image input should return text");

    if env_flag("OPENROUTER_ENABLE_PDF") {
        println!("\n  [pdf]");
        let pdf_model = make_model(&model_name, OpenRouterApiMode::ChatCompletions);
        let pdf_config = config_with_options(
            OpenRouterRequestOptions::default().with_plugin(OpenRouterPlugin {
                id: "file-parser".to_string(),
                enabled: Some(true),
                ..Default::default()
            }),
        );
        let pdf_agent = Arc::new(
            LlmAgentBuilder::new("openrouter-pdf")
                .instruction("Summarize the provided PDF briefly.")
                .model(pdf_model)
                .generate_content_config(pdf_config)
                .build()?,
        );
        let pdf_runner = make_runner(pdf_agent, "pdf").await?;
        let pdf_text = collect_text(
            &pdf_runner,
            "pdf",
            Content::new("user")
                .with_text("Summarize this PDF in one sentence.")
                .with_file_uri("application/pdf", env_or("OPENROUTER_PDF_URL", DEFAULT_PDF_URL)),
        )
        .await?;
        assert!(!pdf_text.trim().is_empty(), "pdf input should return text");
    } else {
        println!("PDF step skipped; set OPENROUTER_ENABLE_PDF=1 to run the file-parser path");
    }

    println!("✓ Multimodal inputs passed");
    Ok(())
}

async fn test_discovery() -> anyhow::Result<()> {
    separator("9. Native discovery APIs");

    let model_name = env_or("OPENROUTER_MODEL", DEFAULT_MODEL);
    let client = make_model(&model_name, OpenRouterApiMode::ChatCompletions);

    let models = client.list_models().await?;
    println!("models discovered: {}", models.len());
    if let Some(model) = models.iter().find(|model| model.id == model_name) {
        println!(
            "configured model found: {} (context_length={:?}, supported_parameters={})",
            model.id,
            model.context_length,
            model.supported_parameters.len()
        );
    }

    if let Some((author, slug)) = model_name.split_once('/') {
        match client.get_model_endpoints(author, slug).await {
            Ok(endpoints) => {
                println!("model endpoints: {}", endpoints.endpoints.len());
            }
            Err(err) => println!("model endpoint lookup failed: {err}"),
        }
    }

    let providers = client.list_providers().await?;
    println!("providers discovered: {}", providers.len());

    match client.get_credits().await {
        Ok(credits) => {
            println!(
                "credits: total_credits={} total_usage={}",
                credits.total_credits, credits.total_usage
            );
        }
        Err(err) => println!("credits lookup failed for this key: {err}"),
    }

    assert!(!models.is_empty(), "discovery should return at least one model");
    assert!(!providers.is_empty(), "discovery should return at least one provider");
    println!("✓ Native discovery APIs passed");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("OpenRouter — Live Integration Example");
    println!("=====================================");
    println!("Using the full ADK stack: Runner → LlmAgent → OpenRouterClient\n");

    let scenarios: Vec<(&str, ScenarioFn)> = vec![
        ("Basic text generation", || Box::pin(test_basic_chat())),
        ("Streaming text generation", || Box::pin(test_streaming_chat())),
        ("Function calling", || Box::pin(test_function_calling())),
        ("Conversation continuation", || Box::pin(test_session_continuation())),
        ("Responses mode reasoning", || Box::pin(test_responses_mode_reasoning())),
        ("Routing and fallback", || Box::pin(test_routing_and_fallback())),
        ("Built-in web search", || Box::pin(test_web_search())),
        ("Multimodal and plugins", || Box::pin(test_multimodal_and_plugins())),
        ("Native discovery", || Box::pin(test_discovery())),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;
    let total = scenarios.len();

    for (name, run) in scenarios {
        match run().await {
            Ok(()) => passed += 1,
            Err(err) => {
                eprintln!("\n✗ {name} FAILED: {err:#}");
                failed += 1;
            }
        }
    }

    separator("Summary");
    println!("  {passed}/{total} passed, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
