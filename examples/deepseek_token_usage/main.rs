//! DeepSeek Token Usage, Caching & Thinking Example
//!
//! Demonstrates three DeepSeek features:
//!
//! 1. **Token usage metadata** — prompt, candidate, and total token counts
//! 2. **Automatic KV caching** — DeepSeek caches repeated prefixes automatically.
//!    `cache_read_input_token_count` shows tokens served from cache (10x cheaper).
//!    No configuration needed — just reuse the same prefix across requests.
//! 3. **Reasoning mode** — `deepseek-reasoner` produces `Part::Thinking` blocks
//!    with chain-of-thought reasoning. `thinking_token_count` tracks the cost.
//!
//! ```bash
//! export DEEPSEEK_API_KEY=...
//! cargo run --example deepseek_token_usage --features deepseek
//! ```

use adk_core::{Content, Llm, LlmRequest, Part, UsageMetadata};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use futures::StreamExt;
use std::collections::HashMap;

fn print_usage(label: &str, usage: &UsageMetadata) {
    println!("--- {label} ---");
    println!("  prompt tokens:       {}", usage.prompt_token_count);
    println!("  candidate tokens:    {}", usage.candidates_token_count);
    println!("  total tokens:        {}", usage.total_token_count);
    if let Some(cached) = usage.cache_read_input_token_count {
        println!("  cache hit tokens:    {cached}  ← 10x cheaper (0.1 yuan/M)");
    }
    if let Some(thinking) = usage.thinking_token_count {
        println!("  reasoning tokens:    {thinking}  ← chain-of-thought cost");
    }
    println!();
}

/// Long document prefix for caching demo.
const DOCUMENT: &str = r#"
# Rust Programming Language Reference

## Ownership and Borrowing
Rust's ownership system ensures memory safety without garbage collection.
Each value has exactly one owner. When the owner goes out of scope, the value
is dropped. Values can be borrowed immutably (&T) or mutably (&mut T).
The borrow checker enforces: either one mutable reference OR any number of
immutable references, but not both simultaneously.

## Lifetimes
Lifetimes annotate how long references are valid. The compiler uses lifetime
elision rules to infer lifetimes in common cases. Explicit lifetime annotations
use 'a syntax. Lifetime bounds constrain generic types: T: 'a means T must
live at least as long as 'a.

## Traits and Generics
Traits define shared behavior. impl Trait for Type implements a trait.
Generic functions use <T> syntax. Trait bounds constrain generics: T: Display.
Associated types provide type aliases within traits. Default implementations
allow traits to provide method bodies.

## Error Handling
Result<T, E> for recoverable errors, panic! for unrecoverable.
The ? operator propagates errors. Custom error types implement std::error::Error.
thiserror crate for derive macros, anyhow for application-level errors.

## Async/Await
async fn returns impl Future. .await suspends until the future completes.
Requires a runtime (tokio, async-std). Pin<Box<dyn Future>> for dynamic dispatch.
Stream trait for async iterators. async-trait crate for async methods in traits.

## Smart Pointers
Box<T> for heap allocation. Rc<T> for shared ownership (single-threaded).
Arc<T> for shared ownership (multi-threaded). RefCell<T> for interior mutability.
Mutex<T> and RwLock<T> for thread-safe interior mutability.

## Macros
Declarative macros (macro_rules!) for pattern-based code generation.
Procedural macros for custom derive, attribute, and function-like macros.
Common derives: Debug, Clone, PartialEq, Serialize, Deserialize.

## Concurrency
std::thread for OS threads. Channels (mpsc) for message passing.
Mutex and RwLock for shared state. Rayon for data parallelism.
Tokio for async concurrency with tasks and channels.
"#;

// ---------------------------------------------------------------------------
// Part 1: Automatic KV caching
// ---------------------------------------------------------------------------

async fn demo_caching(api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Part 1: Automatic KV Caching ===\n");
    println!("  DeepSeek caches repeated prefixes automatically.");
    println!("  Cache hit tokens cost 0.1 yuan/M vs 1 yuan/M for misses.\n");

    let model = DeepSeekClient::new(DeepSeekConfig::chat(api_key.to_string()))?;

    let system = format!(
        "You are a Rust expert. Answer questions using ONLY the reference below.\n\n{DOCUMENT}"
    );

    // Request 1 — cache miss
    println!("  Request 1 (populating cache):");
    let request = LlmRequest {
        model: String::new(),
        contents: vec![
            Content::new("system").with_text(&system),
            Content::new("user").with_text("Explain the ownership rules in Rust."),
        ],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = model.generate_content(request, true).await?;
    let mut text = String::new();
    let mut final_usage = None;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if response.turn_complete {
            final_usage = response.usage_metadata;
        }
    }

    let preview = &text[..text.len().min(150)];
    println!("  Response: {preview}...\n");
    if let Some(usage) = &final_usage {
        print_usage("Request 1", usage);
    }

    // Request 2 — cache hit expected
    println!("  Request 2 (cache hit expected):");
    let request = LlmRequest {
        model: String::new(),
        contents: vec![
            Content::new("system").with_text(&system),
            Content::new("user").with_text("How does async/await work in Rust?"),
        ],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = model.generate_content(request, true).await?;
    text.clear();
    final_usage = None;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if response.turn_complete {
            final_usage = response.usage_metadata;
        }
    }

    let preview = &text[..text.len().min(150)];
    println!("  Response: {preview}...\n");
    if let Some(usage) = &final_usage {
        print_usage("Request 2", usage);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Part 2: Reasoning mode with thinking traces
// ---------------------------------------------------------------------------

async fn demo_reasoning(api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Part 2: Reasoning Mode (deepseek-reasoner) ===\n");
    println!("  The reasoner model produces Part::Thinking blocks with");
    println!("  chain-of-thought reasoning before the final answer.\n");

    let model = DeepSeekClient::new(DeepSeekConfig::reasoner(api_key.to_string()))?;

    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(
            "In a room there are 100 murderers. You kill one of them. \
             How many murderers are left in the room?",
        )],
        config: None,
        tools: HashMap::new(),
    };

    println!("  Question: 100 murderers puzzle\n");

    let mut stream = model.generate_content(request, true).await?;
    let mut thinking_count = 0;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        thinking_count += 1;
                        let preview = &thinking[..thinking.len().min(120)];
                        println!("  💭 Thinking #{thinking_count}: {preview}...");
                    }
                    Part::Text { text } => print!("{text}"),
                    _ => {}
                }
            }
        }
        if response.turn_complete {
            println!();
            if let Some(usage) = &response.usage_metadata {
                print_usage("Reasoning request", usage);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    println!("=== DeepSeek Token Usage, Caching & Thinking Demo ===\n");

    demo_caching(&api_key).await?;
    demo_reasoning(&api_key).await?;

    println!("=== Key Takeaways ===");
    println!("• DeepSeek caches prefixes automatically — no configuration needed");
    println!("• Cache hit tokens cost 10x less (0.1 vs 1 yuan per million)");
    println!("• deepseek-reasoner produces Part::Thinking with chain-of-thought");
    println!("• thinking_token_count tracks reasoning cost separately");
    println!("• Reasoning tokens are billed but provide better accuracy");

    Ok(())
}
