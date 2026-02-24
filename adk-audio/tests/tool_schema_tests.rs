//! Property P8: Tool Schema Validity
//!
//! *For any* audio tool, `parameters_schema()` SHALL return valid JSON Schema,
//! and `name()` SHALL be a non-empty string.
//!
//! **Validates: Requirement 9**

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use adk_audio::{
    ApplyFxTool, AudioFrame, AudioResult, FxChain, GenerateMusicTool, MusicProvider, MusicRequest,
    SpeakTool, SttOptions, SttProvider, TranscribeTool, Transcript, TtsProvider, TtsRequest, Voice,
};
use adk_core::Tool;
use async_trait::async_trait;
use futures::Stream;

struct StubTts;
#[async_trait]
impl TtsProvider for StubTts {
    async fn synthesize(&self, _: &TtsRequest) -> AudioResult<AudioFrame> {
        Ok(AudioFrame::silence(16000, 1, 100))
    }
    async fn synthesize_stream(
        &self,
        _: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
    fn voice_catalog(&self) -> &[Voice] {
        &[]
    }
}

struct StubStt;
#[async_trait]
impl SttProvider for StubStt {
    async fn transcribe(&self, _: &AudioFrame, _: &SttOptions) -> AudioResult<Transcript> {
        Ok(Transcript::default())
    }
    async fn transcribe_stream(
        &self,
        _: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct StubMusic;
#[async_trait]
impl MusicProvider for StubMusic {
    async fn generate(&self, _: &MusicRequest) -> AudioResult<AudioFrame> {
        Ok(AudioFrame::silence(16000, 1, 100))
    }
    fn supported_genres(&self) -> &[String] {
        &[]
    }
}

#[test]
fn test_speak_tool_schema() {
    let tool = SpeakTool::new(Arc::new(StubTts), "default");
    assert!(!tool.name().is_empty());
    let schema = tool.parameters_schema().expect("schema should exist");
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["text"].is_object());
    let required = schema["required"].as_array().expect("required array");
    assert!(required.iter().any(|v| v == "text"));
}

#[test]
fn test_transcribe_tool_schema() {
    let tool = TranscribeTool::new(Arc::new(StubStt));
    assert!(!tool.name().is_empty());
    let schema = tool.parameters_schema().expect("schema should exist");
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["audio_data"].is_object());
}

#[test]
fn test_generate_music_tool_schema() {
    let tool = GenerateMusicTool::new(Arc::new(StubMusic));
    assert!(!tool.name().is_empty());
    let schema = tool.parameters_schema().expect("schema should exist");
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["prompt"].is_object());
}

#[test]
fn test_apply_fx_tool_schema() {
    let mut chains = HashMap::new();
    chains.insert("normalize".to_string(), FxChain::new());
    let tool = ApplyFxTool::new(chains);
    assert!(!tool.name().is_empty());
    let schema = tool.parameters_schema().expect("schema should exist");
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["chain"].is_object());
}

/// P8: All tools have non-empty names and valid schemas
#[test]
fn prop_all_tools_valid() {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(SpeakTool::new(Arc::new(StubTts), "default")),
        Box::new(TranscribeTool::new(Arc::new(StubStt))),
        Box::new(GenerateMusicTool::new(Arc::new(StubMusic))),
        Box::new(ApplyFxTool::new(HashMap::new())),
    ];

    for tool in &tools {
        assert!(!tool.name().is_empty(), "tool name should not be empty");
        assert!(!tool.description().is_empty(), "tool description should not be empty");
        let schema = tool.parameters_schema();
        assert!(schema.is_some(), "tool {} should have a schema", tool.name());
        let schema = schema.unwrap();
        assert_eq!(schema["type"], "object", "tool {} schema should be an object", tool.name());
    }
}
