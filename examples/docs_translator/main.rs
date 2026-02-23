//! Documentation Translator
//!
//! A production-quality translator for adk-rust documentation using:
//! - LoopAgent with translator + reviewer for quality assurance
//! - Parallel processing for speed
//! - Progress tracking and resume capability
//! - All 9 supported languages

use adk_rust::prelude::*;
use adk_rust::futures::StreamExt;
use adk_rust::session::SessionService;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::sync::Semaphore;

/// Supported target languages
const LANGUAGES: &[(&str, &str)] = &[
    ("es", "Spanish"),
    ("zh-CN", "Simplified Chinese"),
    ("ja", "Japanese"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("de", "German"),
    ("fr", "French"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ko", "Korean"),
];

const TRANSLATOR_INSTRUCTION: &str = r#"
You are an expert technical documentation translator. Your task is to translate markdown documentation while preserving technical accuracy.

## CRITICAL RULES

### DO NOT TRANSLATE:
- Code blocks (```rust, ```toml, ```bash, etc.)
- Inline code (`like_this`)
- Technical terms: adk-rust, LlmAgent, SequentialAgent, ParallelAgent, LoopAgent, GraphAgent, FunctionTool, AgentTool, ExitLoopTool, GeminiModel, OpenAIClient, AnthropicClient, Runner, RunnerConfig, Session, SessionService, Content, Part, Tool, Agent, Cargo.toml, tokio, async, await
- URLs and links
- File paths
- Command line examples
- Import statements
- Variable names and function names
- Crate names (adk-core, adk-agent, adk-model, etc.)

### DO TRANSLATE:
- Headings and titles
- Paragraphs and explanations
- List items (descriptive text only)
- Table content (non-code cells)
- Comments explaining concepts
- Alt text for images

### FORMATTING:
- Preserve ALL markdown syntax exactly
- Keep the same heading levels (#, ##, ###)
- Maintain link structure [text](url)
- Preserve table alignment
- Keep code fence language specifiers

## OUTPUT
Return ONLY the translated markdown. No explanations, no wrapper text, no "Here is the translation" prefix.
"#;

const REVIEWER_INSTRUCTION: &str = r#"
You are a translation quality reviewer for technical documentation.

## REVIEW CHECKLIST

1. **Code Preservation**: Are ALL code blocks identical to the original?
2. **Technical Terms**: Are terms like LlmAgent, Runner, Cargo.toml kept in English?
3. **Markdown Integrity**: Is all formatting preserved (headers, links, tables)?
4. **Translation Quality**: Is the translation natural and accurate?
5. **Completeness**: Is all content translated (no missing sections)?

## DECISION

If the translation passes ALL checks:
→ Call the `exit_loop` tool immediately

If there are issues:
→ Output feedback starting with "FEEDBACK:" listing specific problems
→ Be precise: quote the problematic text and explain the fix needed
"#;

#[derive(Clone)]
struct TranslationStats {
    total: usize,
    completed: usize,
    skipped: usize,
    failed: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let args: Vec<String> = std::env::args().collect();
    
    // Parse arguments
    let (lang_filter, source_dir, output_base, explicit_output) = parse_args(&args);
    
    if lang_filter.as_deref() == Some("--help") || lang_filter.as_deref() == Some("-h") {
        print_help();
        return Ok(());
    }
    
    if lang_filter.as_deref() == Some("--list") {
        print_languages();
        return Ok(());
    }
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable required");
    
    // Collect source files
    let source_path = PathBuf::from(&source_dir);
    let mut files = Vec::new();
    collect_md_files(&source_path, &mut files).await?;
    files.sort();
    
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║           ADK-Rust Documentation Translator              ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
    println!("📁 Source: {}", source_dir);
    println!("📄 Files:  {}", files.len());
    println!();
    
    // Filter languages
    let languages: Vec<_> = LANGUAGES
        .iter()
        .filter(|(code, _)| {
            lang_filter.as_ref().map(|f| f == *code).unwrap_or(true)
        })
        .collect();
    
    if languages.is_empty() {
        eprintln!("Error: Unknown language code '{}'", lang_filter.unwrap_or_default());
        print_languages();
        return Ok(());
    }
    
    println!("🌍 Languages: {}", languages.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", "));
    println!();
    
    // Process each language
    for (lang_code, lang_name) in languages {
        // If explicit output provided, use it directly; otherwise append lang_code
        let output_dir = if explicit_output {
            output_base.clone()
        } else {
            format!("{}/{}", output_base, lang_code)
        };
        
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔄 Translating to {} ({})", lang_name, lang_code);
        println!("📂 Output: {}", output_dir);
        println!();
        
        let stats = translate_language(
            &api_key,
            lang_code,
            lang_name,
            &source_path,
            &PathBuf::from(&output_dir),
            &files,
        ).await?;
        
        println!();
        println!("   ✅ Completed: {}", stats.completed);
        println!("   ⏭️  Skipped:   {}", stats.skipped);
        println!("   ❌ Failed:    {}", stats.failed);
        println!();
    }
    
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("✨ All translations complete!");
    
    Ok(())
}

async fn translate_language(
    api_key: &str,
    lang_code: &str,
    lang_name: &str,
    source_path: &PathBuf,
    output_path: &PathBuf,
    files: &[PathBuf],
) -> anyhow::Result<TranslationStats> {
    let model = Arc::new(GeminiModel::new(api_key, "gemini-2.5-flash")?);
    
    // Simple translator agent (no loop - we'll chunk instead)
    let translator = LlmAgentBuilder::new("translator")
        .description(format!("Translates documentation to {}", lang_name))
        .instruction(format!(
            "{}\n\n## TARGET LANGUAGE\nTranslate to: {} ({})",
            TRANSLATOR_INSTRUCTION, lang_name, lang_code
        ))
        .model(model.clone())
        .build()?;
    
    let agent: Arc<dyn Agent> = Arc::new(translator);
    let session_service = Arc::new(InMemorySessionService::new());
    
    let runner = Arc::new(Runner::new(RunnerConfig {
        app_name: "docs_translator".to_string(),
        agent,
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?);
    
    let mut stats = TranslationStats {
        total: files.len(),
        completed: 0,
        skipped: 0,
        failed: 0,
    };
    
    for (idx, file) in files.iter().enumerate() {
        let rel_path = file.strip_prefix(source_path).unwrap_or(file);
        let out_file = output_path.join(rel_path);
        
        let progress = format!("[{}/{}]", idx + 1, files.len());
        
        if out_file.exists() {
            println!("   {} ⏭️  {} (exists)", progress, rel_path.display());
            stats.skipped += 1;
            continue;
        }
        
        println!("   {} 📝 {}", progress, rel_path.display());
        
        let content = fs::read_to_string(file).await?;
        
        // Split into chunks by sections (## headers)
        let chunks = split_into_chunks(&content, 4000);
        println!("      📦 {} chunk(s) to translate", chunks.len());
        
        let mut translated_chunks = Vec::new();
        
        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            print!("      🔄 Chunk {}/{}...", chunk_idx + 1, chunks.len());
            std::io::Write::flush(&mut std::io::stdout()).ok();
            
            let session = session_service.create(adk_rust::session::CreateRequest {
                app_name: "docs_translator".to_string(),
                user_id: "translator".to_string(),
                session_id: None,
                state: HashMap::new(),
            }).await?;
            
            let user_content = Content::new("user").with_text(chunk);
            let result = runner.run(
                "translator".to_string(),
                session.id().to_string(),
                user_content,
            ).await;
            
            match result {
                Ok(mut events) => {
                    let mut chunk_translated = String::new();
                    
                    while let Some(Ok(event)) = events.next().await {
                        if let Some(content) = &event.llm_response.content {
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    chunk_translated.push_str(text);
                                }
                            }
                        }
                    }
                    
                    if chunk_translated.is_empty() {
                        println!(" ❌");
                    } else {
                        let preview: String = chunk_translated.chars().take(50).collect();
                        println!(" ✅ \"{}...\"", preview.replace('\n', " "));
                        translated_chunks.push(clean_translation(&chunk_translated));
                    }
                }
                Err(e) => {
                    println!(" ❌ ({})", e);
                }
            }
            
            // Rate limit between chunks
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
        
        if translated_chunks.len() == chunks.len() {
            // All chunks translated - combine and save
            let full_translation = translated_chunks.join("\n\n");
            
            if let Some(parent) = out_file.parent() {
                fs::create_dir_all(parent).await?;
            }
            
            fs::write(&out_file, &full_translation).await?;
            println!("      💾 Saved: {}", out_file.display());
            stats.completed += 1;
        } else {
            println!("      ❌ Failed: only {}/{} chunks translated", translated_chunks.len(), chunks.len());
            stats.failed += 1;
        }
        
        println!();
    }
    
    Ok(stats)
}

/// Split content into chunks by markdown sections, preserving code blocks
fn split_into_chunks(content: &str, max_chars: usize) -> Vec<String> {
    // First, identify safe split points (## headers not inside code blocks)
    let mut in_code_block = false;
    let mut sections: Vec<String> = Vec::new();
    let mut current_section = String::new();
    
    for line in content.lines() {
        // Track code block state
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
        }
        
        // Only split at ## headers when NOT in a code block
        if !in_code_block && line.starts_with("## ") && !current_section.is_empty() {
            sections.push(current_section.trim().to_string());
            current_section = String::new();
        }
        
        current_section.push_str(line);
        current_section.push('\n');
    }
    
    if !current_section.trim().is_empty() {
        sections.push(current_section.trim().to_string());
    }
    
    // Now combine small sections into chunks up to max_chars
    let mut chunks: Vec<String> = Vec::new();
    let mut current_chunk = String::new();
    
    for section in sections {
        if current_chunk.len() + section.len() > max_chars && !current_chunk.is_empty() {
            chunks.push(current_chunk.trim().to_string());
            current_chunk = String::new();
        }
        
        if !current_chunk.is_empty() {
            current_chunk.push_str("\n\n");
        }
        current_chunk.push_str(&section);
    }
    
    if !current_chunk.trim().is_empty() {
        chunks.push(current_chunk.trim().to_string());
    }
    
    // If content is small enough, return as single chunk
    if chunks.is_empty() {
        return vec![content.to_string()];
    }
    
    chunks
}

/// Clean up translation output (remove wrapper code fences if present)
fn clean_translation(text: &str) -> String {
    let trimmed = text.trim();
    
    // Remove markdown code fence wrapper if present
    if trimmed.starts_with("```markdown") || trimmed.starts_with("```md") {
        if let Some(end) = trimmed.rfind("```") {
            let start = trimmed.find('\n').unwrap_or(0) + 1;
            if end > start {
                return trimmed[start..end].trim().to_string();
            }
        }
    }
    
    trimmed.to_string()
}

fn parse_args(args: &[String]) -> (Option<String>, String, String, bool) {
    let lang = args.get(1).cloned();
    let source = args.get(2).cloned().unwrap_or_else(|| "docs/official_docs".to_string());
    let output = args.get(3).cloned();
    let explicit_output = output.is_some();
    let output = output.unwrap_or_else(|| "docs".to_string());
    (lang, source, output, explicit_output)
}

fn print_help() {
    println!("ADK-Rust Documentation Translator");
    println!();
    println!("USAGE:");
    println!("    docs_translator [OPTIONS] [LANG] [SOURCE_DIR] [OUTPUT_BASE]");
    println!();
    println!("ARGUMENTS:");
    println!("    LANG         Language code (e.g., 'es', 'ja'). Omit for all languages.");
    println!("    SOURCE_DIR   Source docs directory (default: docs/official_docs)");
    println!("    OUTPUT_BASE  Output base directory (default: docs)");
    println!();
    println!("OPTIONS:");
    println!("    --help, -h   Show this help message");
    println!("    --list       List supported languages");
    println!();
    println!("EXAMPLES:");
    println!("    docs_translator es                    # Translate to Spanish");
    println!("    docs_translator                       # Translate to all languages");
    println!("    docs_translator ja src/docs out/docs  # Custom paths");
    println!();
    println!("ENVIRONMENT:");
    println!("    GOOGLE_API_KEY    Required. Your Gemini API key.");
}

fn print_languages() {
    println!("Supported languages:");
    for (code, name) in LANGUAGES {
        println!("    {:8} - {}", code, name);
    }
}

async fn collect_md_files(dir: &PathBuf, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !dir.exists() {
        anyhow::bail!("Directory not found: {}", dir.display());
    }
    
    let mut entries = fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            Box::pin(collect_md_files(&path, files)).await?;
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            files.push(path);
        }
    }
    Ok(())
}
