//! # Schema Normalization Demo
//!
//! Demonstrates how ADK-Rust's provider-aware schema normalization works.
//!
//! Each LLM provider has different JSON Schema requirements for function-calling.
//! ADK-Rust normalizes schemas at request time using provider-specific adapters,
//! so MCP tools work seamlessly across all providers without manual schema tweaking.
//!
//! ## What this example shows:
//!
//! 1. The same raw MCP tool schema is normalized differently per provider
//! 2. Gemini applies destructive transforms (resolves $ref, collapses combiners)
//! 3. OpenAI Strict preserves structure but adds `additionalProperties: false`
//! 4. Anthropic passes most features through with minimal changes
//! 5. Tool name truncation handles multi-byte UTF-8 safely
//!
//! ## Run
//!
//! ```bash
//! cargo run -p schema-normalization-example
//! ```

use adk_core::{GenericSchemaAdapter, SchemaAdapter, SchemaCache};
use adk_gemini::schema_adapter::GeminiSchemaAdapter;
use adk_model::anthropic::AnthropicSchemaAdapter;
use adk_model::openai::{OpenAiSchemaAdapter, OpenAiStrictSchemaAdapter};
use serde_json::json;

fn main() {
    tracing_subscriber::fmt().with_env_filter("info").with_target(false).init();

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║       ADK-Rust: Provider-Aware Schema Normalization         ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // A complex MCP tool schema with features that different providers handle differently
    let raw_schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "definitions": {
            "Address": {
                "type": "object",
                "properties": {
                    "street": { "type": "string" },
                    "city": { "type": "string" },
                    "zip": { "type": "string", "format": "postal-code" }
                },
                "additionalProperties": false
            }
        },
        "properties": {
            "name": { "type": ["string", "null"], "description": "Customer name" },
            "email": { "type": "string", "format": "email" },
            "age": { "type": "integer", "exclusiveMinimum": 0, "exclusiveMaximum": 150 },
            "address": { "$ref": "#/definitions/Address" },
            "status": { "const": "active" },
            "tags": {
                "type": "array",
                "items": { "type": "string", "format": "hostname" }
            },
            "metadata": {
                "anyOf": [
                    { "type": "null" },
                    { "type": "object", "additionalProperties": true }
                ]
            }
        },
        "required": ["name", "email"],
        "additionalProperties": false,
        "if": { "properties": { "status": { "const": "active" } } },
        "then": { "required": ["address"] }
    });

    println!("━━━ Raw MCP Tool Schema ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("{}\n", serde_json::to_string_pretty(&raw_schema).unwrap());

    // --- Gemini Adapter ---
    demo_adapter(
        "Gemini (Standard)",
        &GeminiSchemaAdapter::new(),
        &raw_schema,
        &[
            "• Resolves $ref → inlines Address definition",
            "• Collapses anyOf → picks first non-null sub-schema",
            "• Collapses type arrays → [\"string\", \"null\"] becomes \"string\"",
            "• Removes: $schema, additionalProperties, exclusiveMin/Max, if/then",
            "• Converts const → single-element enum",
            "• Strips unsupported format (postal-code, hostname)",
            "• Preserves allowed format (email)",
        ],
    );

    // --- Gemini Vertex AI Adapter ---
    demo_adapter(
        "Gemini (Vertex AI)",
        &GeminiSchemaAdapter::vertex_ai(),
        &raw_schema,
        &[
            "• Same as standard Gemini EXCEPT:",
            "• Sets additionalProperties: false (instead of removing it)",
        ],
    );

    // --- OpenAI Strict Adapter ---
    demo_adapter(
        "OpenAI (Strict Mode)",
        &OpenAiStrictSchemaAdapter,
        &raw_schema,
        &[
            "• Preserves $ref and definitions (OpenAI supports them)",
            "• Preserves anyOf (OpenAI supports nullable patterns)",
            "• Preserves type arrays [\"string\", \"null\"]",
            "• Adds additionalProperties: false to ALL object schemas",
            "• Strips: $schema, if/then/else, unsupported formats",
            "• Converts const → enum",
        ],
    );

    // --- OpenAI Non-Strict Adapter ---
    demo_adapter(
        "OpenAI (Non-Strict)",
        &OpenAiSchemaAdapter,
        &raw_schema,
        &[
            "• Minimal safe fixes only",
            "• Preserves $ref, anyOf, additionalProperties, type arrays",
            "• Strips: $schema, if/then/else, unsupported formats",
            "• Converts const → enum",
        ],
    );

    // --- Anthropic Adapter ---
    demo_adapter(
        "Anthropic",
        &AnthropicSchemaAdapter,
        &raw_schema,
        &[
            "• Near pass-through — Anthropic supports most JSON Schema",
            "• Preserves: $ref, definitions, anyOf, additionalProperties, type arrays, const, ALL formats",
            "• Only strips: $schema, if/then/else",
        ],
    );

    // --- Generic Adapter ---
    demo_adapter(
        "Generic (Ollama, etc.)",
        &GenericSchemaAdapter,
        &raw_schema,
        &[
            "• Conservative safe transforms for unknown providers",
            "• Strips: $schema, if/then/else, unsupported formats",
            "• Converts const → enum",
            "• Adds implicit type: object",
            "• Does NOT resolve $ref or collapse combiners",
        ],
    );

    // --- Tool Name Truncation Demo ---
    println!("\n━━━ Tool Name Truncation ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let adapters: Vec<(&str, Box<dyn SchemaAdapter>)> = vec![
        ("Gemini", Box::new(GeminiSchemaAdapter::new())),
        ("OpenAI", Box::new(OpenAiSchemaAdapter)),
        ("Anthropic", Box::new(AnthropicSchemaAdapter)),
    ];

    let long_name =
        "mcp_server_github_com_organization_repository_pull_request_review_comments_list_all";
    let emoji_name = "🔧_tool_with_emoji_名前が長いツール_herramienta_larga";

    for (provider, adapter) in &adapters {
        let truncated = adapter.normalize_tool_name(long_name);
        println!("  {provider:12} │ \"{long_name}\"");
        println!("  {:12} │ → \"{}\" ({} bytes)\n", "", truncated, truncated.len());
    }

    println!("  Multi-byte UTF-8 handling:");
    for (provider, adapter) in &adapters {
        let truncated = adapter.normalize_tool_name(emoji_name);
        println!("  {provider:12} │ \"{emoji_name}\"");
        println!("  {:12} │ → \"{}\" ({} bytes)\n", "", truncated, truncated.len());
    }

    // --- Schema Cache Demo ---
    println!("\n━━━ Schema Cache ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let cache = SchemaCache::new();
    let adapter = GeminiSchemaAdapter::new();

    println!("  Cache empty: {} entries", cache.len());

    let _result1 = cache.get_or_normalize(&raw_schema, &adapter);
    println!("  After first normalize: {} entry (computed)", cache.len());

    let _result2 = cache.get_or_normalize(&raw_schema, &adapter);
    println!("  After second normalize: {} entry (cache hit!)", cache.len());

    let different_schema = json!({"type": "string", "format": "email"});
    let _result3 = cache.get_or_normalize(&different_schema, &adapter);
    println!("  After different schema: {} entries", cache.len());

    cache.clear();
    println!("  After cache.clear(): {} entries", cache.len());

    println!("\n✅ All providers normalize schemas correctly at request time!");
    println!("   MCP tools work seamlessly across Gemini, OpenAI, Anthropic, and more.\n");
}

fn demo_adapter(
    name: &str,
    adapter: &dyn SchemaAdapter,
    schema: &serde_json::Value,
    notes: &[&str],
) {
    println!("━━━ {} ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n", name);

    for note in notes {
        println!("  {note}");
    }
    println!();

    let normalized = adapter.normalize_schema(schema.clone());
    println!("{}\n", serde_json::to_string_pretty(&normalized).unwrap());
}
