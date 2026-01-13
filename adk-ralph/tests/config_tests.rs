//! Tests for Ralph configuration.

use adk_ralph::{
    AgentModelConfig, ModelConfig, RalphConfig, TelemetryConfig, ValidationError,
    MAX_ITERATIONS_LIMIT, MAX_RETRIES_LIMIT, MAX_TOKENS_LIMIT, SUPPORTED_PROVIDERS,
};

#[test]
fn test_model_config_defaults() {
    let config = ModelConfig::default();
    assert_eq!(config.provider, "gemini");
    assert!(!config.thinking_enabled);
    assert_eq!(config.max_tokens, 4096);
}

#[test]
fn test_model_config_validation() {
    // Valid config
    let valid = ModelConfig::new("anthropic", "claude-sonnet-4-20250514");
    assert!(valid.validate().is_ok());

    // Invalid provider
    let invalid_provider = ModelConfig::new("invalid_provider", "model");
    let err = invalid_provider.validate().unwrap_err();
    assert_eq!(err.field, "provider");
    assert!(err.suggestion.is_some());

    // Empty model name
    let empty_model = ModelConfig::new("anthropic", "");
    let err = empty_model.validate().unwrap_err();
    assert_eq!(err.field, "model_name");

    // Invalid temperature
    let mut invalid_temp = ModelConfig::default();
    invalid_temp.temperature = 3.0;
    let err = invalid_temp.validate().unwrap_err();
    assert_eq!(err.field, "temperature");
}

#[test]
fn test_model_config_builder_pattern() {
    let config = ModelConfig::new("openai", "gpt-4o")
        .with_thinking()
        .with_max_tokens(8192)
        .with_temperature(0.5);

    assert_eq!(config.provider, "openai");
    assert!(config.thinking_enabled);
    assert_eq!(config.max_tokens, 8192);
    assert!((config.temperature - 0.5).abs() < 0.01);
}

#[test]
fn test_agent_model_config_defaults() {
    let config = AgentModelConfig::default();

    // All agents should use Gemini by default
    assert_eq!(config.prd_model.provider, "gemini");
    assert_eq!(config.architect_model.provider, "gemini");
    assert_eq!(config.ralph_model.provider, "gemini");
}

#[test]
fn test_agent_model_config_validation() {
    let config = AgentModelConfig::default();
    assert!(config.validate().is_ok());

    // Invalid PRD model
    let mut invalid = AgentModelConfig::default();
    invalid.prd_model.provider = "invalid".to_string();
    let err = invalid.validate().unwrap_err();
    assert!(err.field.contains("prd_model"));
}

#[test]
fn test_telemetry_config_defaults() {
    let config = TelemetryConfig::default();
    assert!(config.enabled);
    assert!(config.enable_tracing);
    assert!(config.enable_metrics);
    assert_eq!(config.log_level, "info");
}

#[test]
fn test_telemetry_config_validation() {
    let valid = TelemetryConfig::default();
    assert!(valid.validate().is_ok());

    let mut invalid = TelemetryConfig::default();
    invalid.log_level = "invalid_level".to_string();
    let err = invalid.validate().unwrap_err();
    assert_eq!(err.field, "log_level");
}

#[test]
fn test_telemetry_otlp_endpoint_validation() {
    // Valid endpoint
    let mut config = TelemetryConfig::default();
    config.otlp_endpoint = Some("http://localhost:4317".to_string());
    assert!(config.validate().is_ok());

    // Invalid endpoint (no protocol)
    config.otlp_endpoint = Some("localhost:4317".to_string());
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "otlp_endpoint");

    // Empty endpoint
    config.otlp_endpoint = Some("".to_string());
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "otlp_endpoint");
}

#[test]
fn test_ralph_config_defaults() {
    let config = RalphConfig::default();

    assert_eq!(config.max_iterations, 50);
    assert_eq!(config.prd_path, "prd.md");
    assert_eq!(config.design_path, "design.md");
    assert_eq!(config.tasks_path, "tasks.json");
    assert_eq!(config.progress_path, "progress.json");
    assert_eq!(config.max_task_retries, 3);

    assert!(config.validate().is_ok());
}

#[test]
fn test_ralph_config_builder() {
    let config = RalphConfig::builder()
        .max_iterations(100)
        .prd_path("custom_prd.md")
        .design_path("custom_design.md")
        .tasks_path("custom_tasks.json")
        .progress_path("custom_progress.json")
        .completion_promise("Done!")
        .max_task_retries(5)
        .build()
        .unwrap();

    assert_eq!(config.max_iterations, 100);
    assert_eq!(config.prd_path, "custom_prd.md");
    assert_eq!(config.design_path, "custom_design.md");
    assert_eq!(config.tasks_path, "custom_tasks.json");
    assert_eq!(config.progress_path, "custom_progress.json");
    assert_eq!(config.completion_promise, "Done!");
    assert_eq!(config.max_task_retries, 5);
}

#[test]
fn test_ralph_config_validation_failures() {
    // Zero max iterations
    let result = RalphConfig::builder().max_iterations(0).build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "max_iterations");

    // Empty PRD path
    let result = RalphConfig::builder().prd_path("").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "prd_path");

    // Empty design path
    let result = RalphConfig::builder().design_path("").build();
    assert!(result.is_err());

    // Empty tasks path
    let result = RalphConfig::builder().tasks_path("").build();
    assert!(result.is_err());

    // Empty progress path
    let result = RalphConfig::builder().progress_path("").build();
    assert!(result.is_err());

    // Zero max retries
    let result = RalphConfig::builder().max_task_retries(0).build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "max_task_retries");
}

#[test]
fn test_ralph_config_max_iterations_limit() {
    // Exceeds limit
    let result = RalphConfig::builder()
        .max_iterations(MAX_ITERATIONS_LIMIT + 1)
        .build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "max_iterations");
    assert!(err.message.contains("exceeds"));
}

#[test]
fn test_ralph_config_max_retries_limit() {
    // Exceeds limit
    let result = RalphConfig::builder()
        .max_task_retries(MAX_RETRIES_LIMIT + 1)
        .build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "max_task_retries");
}

#[test]
fn test_ralph_config_path_with_null_byte() {
    let result = RalphConfig::builder().prd_path("test\x00.md").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "prd_path");
    assert!(err.message.contains("null"));
}

#[test]
fn test_ralph_config_path_with_control_chars() {
    let result = RalphConfig::builder().prd_path("test\x07.md").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.field, "prd_path");
    assert!(err.message.contains("control"));
}

#[test]
fn test_supported_providers() {
    assert!(SUPPORTED_PROVIDERS.contains(&"openai"));
    assert!(SUPPORTED_PROVIDERS.contains(&"anthropic"));
    assert!(SUPPORTED_PROVIDERS.contains(&"gemini"));
    assert!(SUPPORTED_PROVIDERS.contains(&"ollama"));
}

#[test]
fn test_validation_error_display() {
    let err = ValidationError::new("field", "message");
    assert_eq!(err.to_string(), "field: message");

    let err = ValidationError::new("field", "message").with_suggestion("try this");
    assert_eq!(err.to_string(), "field: message. try this");
}

#[test]
fn test_model_config_max_tokens_limit() {
    let mut config = ModelConfig::default();
    config.max_tokens = MAX_TOKENS_LIMIT + 1;
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "max_tokens");
    assert!(err.message.contains("exceeds"));
}

#[test]
fn test_model_name_too_long() {
    let long_name = "a".repeat(257);
    let config = ModelConfig::new("anthropic", &long_name);
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "model_name");
    assert!(err.message.contains("too long"));
}

#[test]
fn test_model_name_with_control_chars() {
    let config = ModelConfig::new("anthropic", "model\x00name");
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "model_name");
    assert!(err.message.contains("control"));
}

#[test]
fn test_temperature_nan_and_infinity() {
    let mut config = ModelConfig::default();
    
    config.temperature = f32::NAN;
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "temperature");
    
    config.temperature = f32::INFINITY;
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "temperature");
    
    config.temperature = f32::NEG_INFINITY;
    let err = config.validate().unwrap_err();
    assert_eq!(err.field, "temperature");
}
