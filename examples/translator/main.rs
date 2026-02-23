//! # Translator Example
//!
//! Multi-agent translation pipeline demonstrating adk-rust best practices:
//! - `instruction_provider` for dynamic language-based instructions
//! - `output_key` for state propagation between agents
//! - `global_instruction` for consistent identity across agents
//! - Callbacks for observability
//! - Proper error handling
//!
//! ## Architecture
//!
//! ```text
//! SequentialAgent (Translation Pipeline)
//! ├── LoopAgent (Content Loop)
//! │   ├── Translator Agent (output_key: "current_translation")
//! │   └── Reviewer Agent (calls exit_loop when approved)
//! └── Formatter Agent (output_key: "final_translation")
//! ```
//!
//! ## Usage
//!
//! Interactive mode:
//! ```bash
//! cargo run --example translator
//! ```
//!
//! Batch mode (file processing):
//! ```bash
//! cargo run --example translator -- --input content.ts --languages es,ja,zh-CN
//! ```

use adk_agent::{LlmAgentBuilder, LoopAgent, SequentialAgent};
use adk_cli::console::run_console;
use adk_core::{Content, Part, ReadonlyContext};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::ExitLoopTool;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Supported target languages for translation
const TARGET_LANGUAGES: &[(&str, &str)] = &[
    ("zh-CN", "Simplified Chinese"),
    ("es", "Spanish"),
    ("ja", "Japanese"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("de", "German"),
    ("fr", "French"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ko", "Korean"),
];

/// Global instruction shared across all agents in the pipeline
const GLOBAL_INSTRUCTION: &str = "\
You are part of a professional technical translation pipeline. \
Maintain consistent terminology and high-quality output. \
CRITICAL RULES that apply to ALL agents:
1. NEVER translate code, variable names, import statements, or technical terms like 'LlmAgent', 'SequentialAgent', 'LoopAgent'
2. Preserve exact formatting and structure
3. Be precise and professional";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();

    // Initialize logging
    tracing_subscriber::fmt::init();

    // Parse arguments manually (examples don't have clap in deps)
    let args: Vec<String> = std::env::args().collect();
    let input_file = args.iter().position(|a| a == "--input").map(|i| &args[i + 1]);
    let languages_arg = args.iter().position(|a| a == "--languages").map(|i| &args[i + 1]);
    let output_dir = args.iter().position(|a| a == "--output").map(|i| PathBuf::from(&args[i + 1]));

    // Use environment variable for API key
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Use environment variable for model name with sensible default
    let model_name =
        std::env::var("TRANSLATOR_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());

    let model = Arc::new(GeminiModel::new(&api_key, &model_name)?);

    if let Some(input_path) = input_file {
        // BATCH MODE: Process file with multiple languages
        run_batch_mode(
            model,
            PathBuf::from(input_path),
            output_dir,
            languages_arg.map(|s| s.as_str()),
        )
        .await?;
    } else {
        // INTERACTIVE MODE: Run console with a single pipeline
        println!("=== ADK-Rust Translation Pipeline ===");
        println!("This agent translates technical content with quality assurance.");
        println!();
        println!("Example input:");
        println!("  Translate to Spanish: Hello, this is a LlmAgent example.");
        println!();
        println!("For batch mode, use:");
        println!("  cargo run --example translator -- --input file.ts --languages es,ja");
        println!();

        // Build pipeline without language pre-set (user specifies in message)
        let pipeline = build_translation_pipeline(model, None)?;

        run_console(Arc::new(pipeline), "translator_app".to_string(), "user".to_string()).await?;
    }

    Ok(())
}

/// Build the translation pipeline with optional target language
///
/// Uses adk-rust best practices:
/// - `instruction_provider` for dynamic instructions based on state
/// - `output_key` for state propagation between agents
/// - `global_instruction` for consistent identity
fn build_translation_pipeline(
    model: Arc<GeminiModel>,
    target_language: Option<String>,
) -> anyhow::Result<SequentialAgent> {
    // Clone for use in closures
    let lang_for_translator = target_language.clone();
    let lang_for_reviewer = target_language.clone();

    // 1. Translator Agent with dynamic instruction and output_key
    let translator = LlmAgentBuilder::new("translator")
        .description("Expert technical translator")
        .global_instruction(GLOBAL_INSTRUCTION)
        .instruction_provider(Box::new(move |_ctx: Arc<dyn ReadonlyContext>| {
            let lang = lang_for_translator.clone();
            Box::pin(async move {
                // Try to get language from state, fallback to provided language or extract from user message
                let target = lang.unwrap_or_else(|| "the target language specified".to_string());

                Ok(format!(
                    "You are an expert technical translator specializing in {target}. \
                     Translate the input content into {target}. \
                     \
                     RULES: \
                     1. Preserve ALL code blocks, variable names, and technical terms \
                        (e.g., 'LlmAgent', 'SequentialAgent', 'LoopAgent', 'ExitLoopTool'). \
                     2. Do NOT translate import/export statements or code. \
                     3. Maintain the exact structure and formatting. \
                     4. If you receive feedback from the reviewer, fix the specific issues mentioned. \
                     \
                     Output ONLY the translated content."
                ))
            })
        }))
        .output_key("current_translation") // Saves output to state["current_translation"]
        .model(model.clone())
        .build()?;

    // 2. Content Reviewer Agent
    let reviewer = LlmAgentBuilder::new("reviewer")
        .description("Technical translation reviewer")
        .global_instruction(GLOBAL_INSTRUCTION)
        .instruction_provider(Box::new(move |_ctx: Arc<dyn ReadonlyContext>| {
            let lang = lang_for_reviewer.clone();
            Box::pin(async move {
                let target = lang.unwrap_or_else(|| "the target language".to_string());

                Ok(format!(
                    "You are a strict technical editor. Review the {target} translation for accuracy. \
                     \
                     CHECKLIST: \
                     1. Are technical terms preserved in English? (LlmAgent, SequentialAgent, etc.) \
                     2. Is the tone professional and natural for {target}? \
                     3. Are there any obvious mistranslations? \
                     4. Is the structure maintained? \
                     \
                     If the translation is APPROVED: Call the 'exit_loop' tool. \
                     If NOT approved: Output specific feedback about what needs to be fixed."
                ))
            })
        }))
        .model(model.clone())
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    // 3. Formatter Agent with final output_key
    let formatter = LlmAgentBuilder::new("formatter")
        .description("Output formatter and cleaner")
        .instruction(
            "You are a code formatter. Clean the content for final output. \
             \
             TASKS: \
             1. Remove any markdown code block markers (```) if present. \
             2. Ensure the output is clean and properly formatted. \
             3. Do NOT modify the actual translation content. \
             \
             Output ONLY the cleaned content.",
        )
        .output_key("final_translation") // Final result accessible via state["final_translation"]
        .model(model.clone())
        .build()?;

    // Content Loop: Translator -> Reviewer (max 3 iterations)
    let content_loop =
        LoopAgent::new("content_loop", vec![Arc::new(translator), Arc::new(reviewer)])
            .with_max_iterations(3)
            .with_description("Translation and review cycle");

    // Main Pipeline: Content Loop -> Formatter
    let pipeline = SequentialAgent::new(
        "translation_pipeline",
        vec![Arc::new(content_loop), Arc::new(formatter)],
    )
    .with_description("Complete translation pipeline with review and formatting");

    Ok(pipeline)
}

/// Run batch mode to translate a file into multiple languages
async fn run_batch_mode(
    model: Arc<GeminiModel>,
    input_path: PathBuf,
    output_dir: Option<PathBuf>,
    languages: Option<&str>,
) -> anyhow::Result<()> {
    // Read input file
    let content = tokio::fs::read_to_string(&input_path).await?;
    println!("Read {} bytes from {:?}", content.len(), input_path);

    // Determine output directory
    let output_dir = output_dir.unwrap_or_else(|| input_path.parent().unwrap().join("content"));
    tokio::fs::create_dir_all(&output_dir).await?;
    println!("Output directory: {:?}", output_dir);

    // Save original
    let en_path = output_dir.join("en.ts");
    save_file(&en_path, &content).await?;
    println!("Saved original to {:?}", en_path);

    // Determine which languages to process
    let langs: Vec<(&str, &str)> = if let Some(lang_str) = languages {
        lang_str
            .split(',')
            .filter_map(|code| TARGET_LANGUAGES.iter().find(|(c, _)| *c == code.trim()).copied())
            .collect()
    } else {
        TARGET_LANGUAGES.to_vec()
    };

    // Session service for runner
    let session_service = Arc::new(InMemorySessionService::new());

    // Track results
    let mut successes = 0;
    let mut failures = 0;

    // Process each language
    for (code, name) in langs {
        println!("\n=== Translating to {} ({}) ===", name, code);

        // Build pipeline with specific target language
        let pipeline = build_translation_pipeline(model.clone(), Some(name.to_string()))?;

        // Create runner
        let config = RunnerConfig {
            app_name: "translator".to_string(),
            agent: Arc::new(pipeline),
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
        };

        let runner = Runner::new(config)?;

        // Create session with initial state
        let mut initial_state = HashMap::new();
        initial_state
            .insert("target_language".to_string(), serde_json::Value::String(name.to_string()));

        let session = session_service
            .create(CreateRequest {
                app_name: "translator".to_string(),
                user_id: "batch_user".to_string(),
                session_id: None,
                state: initial_state,
            })
            .await?;

        // Build prompt
        let prompt = format!("Translate the following content to {}:\n\n{}", name, content);

        let user_content =
            Content { role: "user".to_string(), parts: vec![Part::Text { text: prompt }] };

        // Run pipeline
        let session_id = session.id().to_string();
        let result = run_translation(&runner, &session_service, &session_id, user_content).await;

        match result {
            Ok(translation) => {
                let output_path = output_dir.join(format!("{}.ts", code));
                save_file(&output_path, &translation).await?;
                println!("✓ Saved translation to {:?}", output_path);
                successes += 1;
            }
            Err(e) => {
                eprintln!("✗ Failed to translate to {}: {}", name, e);
                failures += 1;
            }
        }
    }

    println!("\n=== Translation complete ===");
    println!("Results: {} succeeded, {} failed", successes, failures);

    Ok(())
}

/// Run translation and extract final result from session state
async fn run_translation(
    runner: &Runner,
    session_service: &Arc<InMemorySessionService>,
    session_id: &str,
    user_content: Content,
) -> anyhow::Result<String> {
    let mut stream =
        runner.run("batch_user".to_string(), session_id.to_string(), user_content).await?;

    // Process stream and collect any errors
    let mut last_text = String::new();

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                // Track text output for fallback
                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part
                            && !text.is_empty()
                        {
                            last_text = text.clone();
                        }
                    }
                }
                // Progress indicator
                print!(".");
            }
            Err(e) => {
                println!();
                return Err(anyhow::anyhow!("Stream error: {}", e));
            }
        }
    }
    println!(); // Newline after progress dots

    // Try to get final translation from session state (preferred method)
    let updated_session = session_service
        .get(GetRequest {
            app_name: "translator".to_string(),
            user_id: "batch_user".to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    if let Some(serde_json::Value::String(translation)) =
        updated_session.state().get("final_translation")
        && !translation.is_empty()
    {
        return Ok(clean_output(&translation));
    }

    // Fallback to current_translation if final not available
    if let Some(serde_json::Value::String(translation)) =
        updated_session.state().get("current_translation")
        && !translation.is_empty()
    {
        return Ok(clean_output(&translation));
    }

    // Last resort: use the last text we saw
    if !last_text.is_empty() {
        return Ok(clean_output(&last_text));
    }

    Err(anyhow::anyhow!("No translation output generated"))
}

/// Clean output by removing markdown artifacts
fn clean_output(text: &str) -> String {
    text.trim()
        .trim_start_matches("```typescript")
        .trim_start_matches("```ts")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

/// Save content to file, wrapping in export if needed
async fn save_file(path: &PathBuf, content: &str) -> anyhow::Result<()> {
    let final_content = if !content.trim().starts_with("export") {
        format!("export const content = {};", content)
    } else {
        content.to_string()
    };
    tokio::fs::write(path, final_content).await?;
    Ok(())
}
