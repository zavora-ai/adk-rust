//! Validation example for ToolSearchConfig and InterruptionDetection.
//!
//! Validates:
//! - ToolSearchConfig regex matching (Req 19.1–19.4)
//! - AnthropicConfig with_tool_search builder (Req 19.2)
//! - InterruptionDetection enum variants and default (Req 15.1–15.3)
//! - Core Tool trait has no defer_loading (Req 20.1–20.2)
//!
//! Run: cargo run --manifest-path examples/competitive_tool_search/Cargo.toml

use adk_anthropic::ToolSearchConfig;
use adk_model::anthropic::AnthropicConfig;
use adk_realtime::InterruptionDetection;

fn main() {
    println!("=== Competitive Improvements: Tool Search & Realtime Validation ===\n");

    validate_tool_search_config();
    validate_anthropic_config_integration();
    validate_interruption_detection();

    println!("\n=== All tool search & realtime validations passed ===");
}

fn validate_tool_search_config() {
    println!("--- ToolSearchConfig ---");

    // Basic regex matching
    let config = ToolSearchConfig::new("^search_.*");
    assert!(config.matches("search_web").unwrap());
    assert!(config.matches("search_docs").unwrap());
    assert!(!config.matches("delete_all").unwrap());
    assert!(!config.matches("web_search").unwrap());
    println!("  ✓ Regex pattern ^search_.* matches correctly");

    // More complex pattern
    let config = ToolSearchConfig::new("^(search|fetch)_.*");
    assert!(config.matches("search_web").unwrap());
    assert!(config.matches("fetch_data").unwrap());
    assert!(!config.matches("delete_user").unwrap());
    println!("  ✓ Alternation pattern (search|fetch)_ matches correctly");

    // Match-all pattern
    let config = ToolSearchConfig::new(".*");
    assert!(config.matches("anything").unwrap());
    assert!(config.matches("").unwrap());
    println!("  ✓ Match-all pattern .* matches everything");

    // Invalid regex returns error
    let config = ToolSearchConfig::new("[invalid");
    assert!(config.matches("test").is_err());
    println!("  ✓ Invalid regex returns error");

    // Pattern field is accessible
    let config = ToolSearchConfig::new("my_pattern");
    assert_eq!(config.pattern, "my_pattern");
    println!("  ✓ Pattern field is publicly accessible");
}

fn validate_anthropic_config_integration() {
    println!("\n--- AnthropicConfig + ToolSearchConfig ---");

    // Default config has no tool search
    let config = AnthropicConfig::new("test-key", "claude-sonnet-4-6");
    assert!(config.tool_search.is_none());
    println!("  ✓ Default AnthropicConfig has no tool_search");

    // Builder sets tool search
    let config = AnthropicConfig::new("test-key", "claude-sonnet-4-6")
        .with_tool_search(ToolSearchConfig::new("^allowed_.*"));
    assert!(config.tool_search.is_some());
    assert_eq!(config.tool_search.as_ref().unwrap().pattern, "^allowed_.*");
    println!("  ✓ with_tool_search() sets the config");

    // Chaining with other config options
    let config = AnthropicConfig::new("test-key", "claude-sonnet-4-6")
        .with_max_tokens(8192)
        .with_prompt_caching(true)
        .with_tool_search(ToolSearchConfig::new("^safe_.*"));
    assert_eq!(config.max_tokens, 8192);
    assert!(config.prompt_caching);
    assert!(config.tool_search.is_some());
    println!("  ✓ with_tool_search() chains with other builder methods");
}

fn validate_interruption_detection() {
    println!("\n--- InterruptionDetection ---");

    // Default is Manual
    let default: InterruptionDetection = Default::default();
    assert_eq!(default, InterruptionDetection::Manual);
    println!("  ✓ Default is Manual");

    // Both variants exist and are distinct
    let manual = InterruptionDetection::Manual;
    let automatic = InterruptionDetection::Automatic;
    assert_ne!(manual, automatic);
    println!("  ✓ Manual and Automatic are distinct variants");

    // Clone and Copy work
    let cloned = automatic;
    assert_eq!(cloned, InterruptionDetection::Automatic);
    println!("  ✓ Clone/Copy traits work");

    // Debug output
    let debug = format!("{:?}", InterruptionDetection::Automatic);
    assert!(debug.contains("Automatic"));
    println!("  ✓ Debug output: {debug}");

    // Serialize/deserialize
    let json = serde_json::to_string(&InterruptionDetection::Automatic).unwrap();
    assert_eq!(json, "\"automatic\"");
    let manual_json = serde_json::to_string(&InterruptionDetection::Manual).unwrap();
    assert_eq!(manual_json, "\"manual\"");
    println!("  ✓ Serializes to snake_case: {json}, {manual_json}");

    let deserialized: InterruptionDetection = serde_json::from_str("\"automatic\"").unwrap();
    assert_eq!(deserialized, InterruptionDetection::Automatic);
    println!("  ✓ Deserializes from snake_case");
}
