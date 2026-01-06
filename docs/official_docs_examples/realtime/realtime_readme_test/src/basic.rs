//! Validates adk-realtime README examples compile correctly

use adk_realtime::{
    RealtimeAgent, RealtimeConfig, VadConfig, VadMode,
    openai::OpenAIRealtimeModel,
};
use std::sync::Arc;

// Validate: RealtimeAgent builder pattern
fn _realtime_agent_builder() {
    let model = Arc::new(OpenAIRealtimeModel::new("api_key", "gpt-4o-realtime-preview-2024-12-17"));
    
    let _agent = RealtimeAgent::builder("voice_assistant")
        .model(model)
        .instruction("You are a helpful voice assistant.")
        .voice("alloy")
        .server_vad()
        .build();
}

// Validate: Custom VAD config
fn _vad_config_example() {
    let model = Arc::new(OpenAIRealtimeModel::new("api_key", "gpt-4o-realtime-preview-2024-12-17"));
    
    let _agent = RealtimeAgent::builder("assistant")
        .model(model)
        .vad(VadConfig {
            mode: VadMode::ServerVad,
            threshold: Some(0.5),
            prefix_padding_ms: Some(300),
            silence_duration_ms: Some(500),
            interrupt_response: Some(true),
            eagerness: None,
        })
        .build();
}

// Validate: RealtimeConfig
fn _realtime_config_example() {
    let _config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant.")
        .with_voice("alloy");
}

// Validate: OpenAIRealtimeModel
fn _openai_model_example() {
    let _model = OpenAIRealtimeModel::new("api_key", "gpt-4o-realtime-preview-2024-12-17");
}

// Validate: Builder methods
fn _builder_methods() {
    let model = Arc::new(OpenAIRealtimeModel::new("api_key", "gpt-4o-realtime-preview-2024-12-17"));
    
    let _agent = RealtimeAgent::builder("test")
        .model(model)
        .description("A test agent")
        .instruction("Be helpful")
        .voice("coral")
        .modalities(vec!["text".to_string(), "audio".to_string()])
        .build();
}

fn main() {
    println!("✓ RealtimeAgent::builder() compiles");
    println!("✓ .model() compiles");
    println!("✓ .instruction() compiles");
    println!("✓ .voice() compiles");
    println!("✓ .server_vad() compiles");
    println!("✓ .vad() compiles");
    println!("✓ VadConfig struct compiles");
    println!("✓ VadMode::ServerVad compiles");
    println!("✓ RealtimeConfig::default() compiles");
    println!("✓ .with_instruction() compiles");
    println!("✓ .with_voice() compiles");
    println!("✓ OpenAIRealtimeModel::new() compiles");
    println!("✓ .description() compiles");
    println!("✓ .modalities() compiles");
    println!("\nadk-realtime README validation passed!");
}
