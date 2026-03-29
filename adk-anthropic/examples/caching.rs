//! Prompt caching with the Anthropic Messages API.
//!
//! Demonstrates the multi-turn automatic caching pattern from the docs:
//!
//! | Request | Content                                          | Cache behavior                          |
//! |---------|--------------------------------------------------|-----------------------------------------|
//! | 1       | System + User(1) ◀ cache                         | Everything written to cache             |
//! | 2       | System + User(1) + Asst(1) + User(2) ◀ cache     | System+User(1) read; Asst(1)+User(2) written |
//! | 3       | System + ... + Asst(2) + User(3) ◀ cache          | Through User(2) read; Asst(2)+User(3) written |
//!
//! Watch `cache_read_input_tokens` grow on requests 2 and 3.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run`

use adk_anthropic::{
    Anthropic, CacheControlEphemeral, KnownModel, MessageCreateParams, MessageParam, MessageRole,
    pricing::{ModelPricing, estimate_cost},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // Large system prompt (must exceed 2048 tokens for Sonnet 4.6 caching).
    let system_text = build_large_system_prompt();

    // We'll build the conversation incrementally, just like the docs show.
    let mut messages: Vec<MessageParam> = Vec::new();

    // ── Request 1: System + User(1) ──────────────────────────────
    println!("=== Request 1: System + User(1) ===\n");

    messages.push(MessageParam::new_with_string(
        "Based on the style guide, how should I name a Rust function that converts Fahrenheit to Celsius?".to_string(),
        MessageRole::User,
    ));

    let r1 = send_cached(&client, &system_text, &messages).await?;
    print_usage("Request 1", &r1.usage);
    print_answer(&r1);

    // ── Request 2: System + User(1) + Asst(1) + User(2) ─────────
    // Append the assistant response and a new user message.
    println!("=== Request 2: + Asst(1) + User(2) ===\n");

    messages.push(MessageParam::new_with_blocks(r1.content.clone(), MessageRole::Assistant));
    messages.push(MessageParam::new_with_string(
        "Now document that function following the style guide's documentation rules.".to_string(),
        MessageRole::User,
    ));

    let r2 = send_cached(&client, &system_text, &messages).await?;
    print_usage("Request 2", &r2.usage);
    print_answer(&r2);

    // ── Request 3: System + ... + Asst(2) + User(3) ─────────────
    println!("=== Request 3: + Asst(2) + User(3) ===\n");

    messages.push(MessageParam::new_with_blocks(r2.content.clone(), MessageRole::Assistant));
    messages.push(MessageParam::new_with_string(
        "Add error handling to the function so it rejects NaN inputs, following the error handling rules.".to_string(),
        MessageRole::User,
    ));

    let r3 = send_cached(&client, &system_text, &messages).await?;
    print_usage("Request 3", &r3.usage);
    print_answer(&r3);

    // ── Cost summary ─────────────────────────────────────────────
    let c1 = estimate_cost(ModelPricing::SONNET_46, &r1.usage);
    let c2 = estimate_cost(ModelPricing::SONNET_46, &r2.usage);
    let c3 = estimate_cost(ModelPricing::SONNET_46, &r3.usage);
    let total = c1.total() + c2.total() + c3.total();

    // What it would have cost without caching (all tokens at base input price).
    let no_cache_input = (r1.usage.input_tokens
        + r1.usage.cache_creation_input_tokens.unwrap_or(0)
        + r1.usage.cache_read_input_tokens.unwrap_or(0)
        + r2.usage.input_tokens
        + r2.usage.cache_creation_input_tokens.unwrap_or(0)
        + r2.usage.cache_read_input_tokens.unwrap_or(0)
        + r3.usage.input_tokens
        + r3.usage.cache_creation_input_tokens.unwrap_or(0)
        + r3.usage.cache_read_input_tokens.unwrap_or(0)) as f64;
    let no_cache_output =
        (r1.usage.output_tokens + r2.usage.output_tokens + r3.usage.output_tokens) as f64;
    let no_cache_cost = no_cache_input / 1_000_000.0 * ModelPricing::SONNET_46.input
        + no_cache_output / 1_000_000.0 * ModelPricing::SONNET_46.output;

    println!("=== Cost Summary (Sonnet 4.6 pricing) ===\n");
    println!("  With caching:    ${total:.6}");
    println!("  Without caching: ${no_cache_cost:.6}");
    if no_cache_cost > 0.0 {
        println!("  Savings:         {:.1}%", (1.0 - total / no_cache_cost) * 100.0);
    }

    Ok(())
}

/// Send a request with automatic caching enabled (top-level `cache_control`).
async fn send_cached(
    client: &Anthropic,
    system_text: &str,
    messages: &[MessageParam],
) -> Result<adk_anthropic::Message, adk_anthropic::Error> {
    let mut params =
        MessageCreateParams::new(256, messages.to_vec(), KnownModel::ClaudeSonnet46.into())
            .with_system(system_text);
    // Top-level cache_control: server caches everything up to the last cacheable block.
    params.cache_control = Some(CacheControlEphemeral::new());
    client.send(params).await
}

fn print_usage(label: &str, usage: &adk_anthropic::Usage) {
    let cost = estimate_cost(ModelPricing::SONNET_46, usage);
    println!("{label}:");
    println!("  input_tokens (uncached):     {}", usage.input_tokens);
    println!("  cache_creation_input_tokens: {}", usage.cache_creation_input_tokens.unwrap_or(0));
    println!("  cache_read_input_tokens:     {}", usage.cache_read_input_tokens.unwrap_or(0));
    println!("  output_tokens:               {}", usage.output_tokens);
    println!("  estimated cost:              {cost}");
    println!();
}

fn print_answer(msg: &adk_anthropic::Message) {
    for block in &msg.content {
        if let Some(text) = block.as_text() {
            let preview: String = text.text.chars().take(200).collect();
            println!("{preview}");
            if text.text.len() > 200 {
                println!("...");
            }
        }
    }
    println!();
}

/// Builds a system prompt large enough to be cacheable (≥2048 tokens for Sonnet 4.6).
fn build_large_system_prompt() -> String {
    let mut prompt = String::from(
        "You are an expert Rust developer and code reviewer. You must follow this comprehensive \
         style guide in all your responses. Every recommendation you make must cite the specific \
         section of this guide that applies.\n\n",
    );

    let sections = [
        (
            "Naming Conventions",
            "Use snake_case for functions and variables. Use CamelCase for types and traits. \
          Use SCREAMING_SNAKE_CASE for constants. Prefix private helper functions with an underscore \
          only when disambiguation is needed. Avoid abbreviations unless universally understood \
          (e.g., `io`, `fmt`). Module names should be singular nouns. Trait names should be adjectives \
          or describe capability (e.g., `Readable`, `Serialize`). Generic type parameters use single \
          uppercase letters: T for general, E for error, K/V for key-value. Enum variants should be \
          CamelCase and descriptive. Lifetime parameters should be short but meaningful. Builder \
          methods should return Self for chaining. Constructor methods should be named new or with \
          a descriptive suffix like from_str or with_capacity.",
        ),
        (
            "Error Handling",
            "Use `thiserror` for library error types. Every error variant must include an actionable \
          message. Use `anyhow` only in binaries and examples, never in libraries. Propagate errors \
          with `?` operator. Never use `.unwrap()` in library code. Use `.expect()` only when the \
          invariant is documented. Implement `From` conversions for error types at crate boundaries. \
          Include context in error messages: what operation failed, what input caused it, and what \
          the caller should do about it. Error types should implement Send + Sync for async. \
          Use error chains to preserve the original cause. Log errors at the point of handling. \
          Distinguish between recoverable and unrecoverable errors clearly.",
        ),
        (
            "Documentation",
            "Every public item needs a rustdoc comment. Start with a one-line summary. Include \
          `# Examples` sections with compilable code. Document error conditions under `# Errors`. \
          Document panics under `# Panics`. Use `# Safety` for unsafe functions. Link related items \
          with backtick syntax. Keep examples minimal but complete. Module-level docs should \
          explain the module's purpose and provide a quick-start example. Use `# Arguments` to \
          document function parameters when they are not self-explanatory. Include `# Returns` for \
          complex return types. Add `# Performance` notes for hot-path functions.",
        ),
        (
            "Testing",
            "Write unit tests in `#[cfg(test)]` modules. Write integration tests in `tests/`. Use \
          `proptest` for property-based testing with 100+ iterations. Name tests `test_<function>_<scenario>`. \
          Name property tests `prop_<property>`. Test error paths, not just happy paths. Use \
          `assert_eq!` over `assert!` for better error messages. Mock external dependencies. \
          Keep tests independent and deterministic. Use test fixtures for complex setup. Prefer \
          table-driven tests for multiple input/output pairs. Test boundary conditions explicitly.",
        ),
        (
            "Async Patterns",
            "Use `async-trait` for async trait methods. Use `tokio::sync::RwLock` for async-safe \
          interior mutability. Prefer `Arc<T>` for shared ownership across async boundaries. \
          Use `async-stream` for creating streams. Never block the async runtime with synchronous I/O. \
          Use `tokio::spawn` for CPU-bound work. Prefer structured concurrency with `tokio::select!` \
          and `JoinSet`. Cancel safety must be documented for all async functions. Use `tokio::time` \
          for timeouts and intervals. Prefer channels over shared state for task communication.",
        ),
        (
            "Performance",
            "Use `&str` over `String` in function parameters. Use `impl Into<String>` for flexible \
          string inputs. Prefer iterators over collecting into Vec. Use `Cow<str>` when ownership \
          is conditional. Profile before optimizing. Use `#[inline]` sparingly and only with \
          benchmarks. Prefer stack allocation for small fixed-size data. Use `SmallVec` for \
          typically-small collections. Avoid unnecessary cloning — use references where possible. \
          Use `capacity` hints for Vec and HashMap when the size is known. Prefer `extend` over \
          repeated `push`. Use `entry` API for HashMap to avoid double lookups.",
        ),
        (
            "Project Structure",
            "One concern per module. Keep `lib.rs` thin — just re-exports and module declarations. \
          Separate types into their own files when they exceed 200 lines. Group related types in \
          subdirectories with `mod.rs`. Use feature flags for optional dependencies. Keep the \
          dependency tree shallow. Pin dependency versions in applications, use ranges in libraries. \
          Use workspace inheritance for shared configuration. Keep build times in mind.",
        ),
        (
            "Code Review Checklist",
            "Check for proper error handling. Verify all public items are documented. Ensure tests \
          cover edge cases. Look for unnecessary allocations. Verify async safety. Check for \
          potential deadlocks in concurrent code. Ensure backward compatibility for public APIs. \
          Verify serde attributes are correct for wire types. Check that feature flags are properly \
          gated. Ensure no secrets or PII in code or comments. Verify exhaustive match arms.",
        ),
        (
            "Logging and Observability",
            "Use `tracing` for structured logging. Never use `println!` or `eprintln!` in library code. \
          Use lowercase messages. Use dot-notation fields. Log errors with `error = %err`. \
          Use `#[instrument]` for automatic span creation. Use `debug!` for details, `info!` for \
          status, `warn!` for issues, `error!` for failures. Include request IDs in spans for \
          distributed tracing. Keep log messages concise but informative. Avoid logging sensitive data.",
        ),
        (
            "Security Practices",
            "Never hardcode secrets or API keys. Use environment variables for configuration. \
          Validate all external input at system boundaries. Use constant-time comparison for \
          secrets. Sanitize error messages before exposing to users. Use TLS for all network \
          communication. Implement rate limiting for public APIs. Keep dependencies updated for \
          security patches. Follow the principle of least privilege. Audit unsafe code blocks.",
        ),
    ];

    for (title, body) in sections {
        prompt.push_str(&format!("## {title}\n\n{body}\n\nRemember: violations of {title} rules must be flagged during code review and fixed before merging. These rules are non-negotiable and apply to all code in the repository without exception.\n\n"));
    }

    // Pad to ensure we exceed the 2048-token minimum for Sonnet 4.6 caching.
    prompt.push_str("## Summary\n\nAll of the above rules are mandatory. When reviewing code, check every rule in every section. When writing code, follow every rule in every section. No exceptions. No shortcuts. Quality is non-negotiable.\n\n");
    prompt.push_str(
        "## Appendix: Common Mistakes\n\n\
        1. Using String instead of &str in function parameters wastes allocations.\n\
        2. Forgetting to document error conditions leads to confusion for callers.\n\
        3. Using unwrap() in library code causes panics in production.\n\
        4. Not testing error paths means bugs hide until production.\n\
        5. Blocking the async runtime with synchronous I/O causes deadlocks.\n\
        6. Hardcoding secrets in source code is a security vulnerability.\n\
        7. Not using thiserror for library errors makes error handling inconsistent.\n\
        8. Skipping property-based tests misses edge cases that unit tests cannot cover.\n\
        9. Not using tracing for logging makes debugging in production impossible.\n\
        10. Ignoring clippy warnings leads to code quality degradation over time.\n\
        11. Not pinning dependency versions in applications causes reproducibility issues.\n\
        12. Using println! in library code pollutes stdout for consumers.\n\
        13. Not implementing Send + Sync on error types breaks async compatibility.\n\
        14. Forgetting cache_control on repeated prompts wastes API credits.\n\
        15. Not validating external input at system boundaries opens security holes.\n\n",
    );

    prompt
}
