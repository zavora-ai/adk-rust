//! RealtimeAgent - an Agent implementation for real-time voice interactions.
//!
//! This module provides `RealtimeAgent`, which implements the `adk_core::Agent` trait
//! and provides the same callback/tool/instruction features as `LlmAgent`, but uses
//! real-time bidirectional audio streaming instead of text-based LLM calls.
//!
//! # Architecture
//!
//! ```text
//!                     ┌─────────────────────────────────────────┐
//!                     │              Agent Trait                │
//!                     │  (name, description, run, sub_agents)   │
//!                     └────────────────┬────────────────────────┘
//!                                      │
//!              ┌───────────────────────┼───────────────────────┐
//!              │                       │                       │
//!     ┌────────▼────────┐    ┌─────────▼─────────┐   ┌─────────▼─────────┐
//!     │    LlmAgent     │    │  RealtimeAgent    │   │  SequentialAgent  │
//!     │  (text-based)   │    │  (voice-based)    │   │   (workflow)      │
//!     └─────────────────┘    └───────────────────┘   └───────────────────┘
//! ```
//!
//! # Shared Features with LlmAgent
//!
//! - **Tools**: Function tools that can be called during conversation
//! - **Callbacks**: before_agent, after_agent, before_tool, after_tool
//! - **Instructions**: Static or dynamic instruction providers
//! - **Sub-agents**: Agent handoff/transfer support
//! - **Context**: Full access to InvocationContext (session, memory, artifacts)
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_realtime::RealtimeAgent;
//! use adk_realtime::openai::OpenAIRealtimeModel;
//!
//! let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17");
//!
//! let agent = RealtimeAgent::builder("voice_assistant")
//!     .model(std::sync::Arc::new(model))
//!     .instruction("You are a helpful voice assistant.")
//!     .voice("alloy")
//!     .tool(Arc::new(weather_tool))
//!     .before_agent_callback(|ctx| async move {
//!         println!("Starting voice session for user: {}", ctx.user_id());
//!         Ok(None)
//!     })
//!     .build()?;
//!
//! // Run through standard ADK runner
//! let runner = Runner::new(agent);
//! runner.run(session, user_content).await?;
//! ```

use crate::config::{RealtimeConfig, ToolDefinition, VadConfig, VadMode};
use crate::events::{ServerEvent, ToolResponse};
use adk_core::{
    AdkError, AfterAgentCallback, AfterToolCallback, Agent, BeforeAgentCallback,
    BeforeToolCallback, CallbackContext, Content, Event, EventActions, EventStream,
    GlobalInstructionProvider, InstructionProvider, InvocationContext, MemoryEntry, Part,
    ReadonlyContext, Result, Tool, ToolContext,
};
use async_stream::stream;
use async_trait::async_trait;

use std::sync::{Arc, Mutex};

/// Shared realtime model type.
pub use crate::model::BoxedModel as BoxedRealtimeModel;

/// A real-time voice agent that implements the ADK Agent trait.
///
/// `RealtimeAgent` provides bidirectional audio streaming while maintaining
/// compatibility with the standard ADK agent ecosystem. It supports the same
/// callbacks, tools, and instruction patterns as `LlmAgent`.
pub struct RealtimeAgent {
    name: String,
    description: String,
    model: BoxedRealtimeModel,

    // Instructions (same as LlmAgent)
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,

    // Voice-specific settings
    voice: Option<String>,
    vad_config: Option<VadConfig>,
    modalities: Vec<String>,

    // Tools (same as LlmAgent)
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,

    // Callbacks (same as LlmAgent)
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
    before_tool_callbacks: Arc<Vec<BeforeToolCallback>>,
    after_tool_callbacks: Arc<Vec<AfterToolCallback>>,

    // Realtime-specific callbacks
    on_audio: Option<AudioCallback>,
    on_transcript: Option<TranscriptCallback>,
    on_speech_started: Option<SpeechCallback>,
    on_speech_stopped: Option<SpeechCallback>,
}

/// Callback for audio output events.
pub type AudioCallback = Arc<
    dyn Fn(&[u8], &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Callback for transcript events.
pub type TranscriptCallback = Arc<
    dyn Fn(&str, &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Callback for speech detection events.
pub type SpeechCallback = Arc<
    dyn Fn(u64) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

impl std::fmt::Debug for RealtimeAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeAgent")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("model", &self.model.model_id())
            .field("voice", &self.voice)
            .field("tools_count", &self.tools.len())
            .field("sub_agents_count", &self.sub_agents.len())
            .finish()
    }
}

/// Builder for RealtimeAgent.
pub struct RealtimeAgentBuilder {
    name: String,
    description: Option<String>,
    model: Option<BoxedRealtimeModel>,
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    voice: Option<String>,
    vad_config: Option<VadConfig>,
    modalities: Vec<String>,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
    before_tool_callbacks: Vec<BeforeToolCallback>,
    after_tool_callbacks: Vec<AfterToolCallback>,
    on_audio: Option<AudioCallback>,
    on_transcript: Option<TranscriptCallback>,
    on_speech_started: Option<SpeechCallback>,
    on_speech_stopped: Option<SpeechCallback>,
}

impl RealtimeAgentBuilder {
    /// Create a new builder with the given agent name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            model: None,
            instruction: None,
            instruction_provider: None,
            global_instruction: None,
            global_instruction_provider: None,
            voice: None,
            vad_config: None,
            modalities: vec!["text".to_string(), "audio".to_string()],
            tools: Vec::new(),
            sub_agents: Vec::new(),
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
            before_tool_callbacks: Vec::new(),
            after_tool_callbacks: Vec::new(),
            on_audio: None,
            on_transcript: None,
            on_speech_started: None,
            on_speech_stopped: None,
        }
    }

    /// Set the agent description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the realtime model.
    pub fn model(mut self, model: BoxedRealtimeModel) -> Self {
        self.model = Some(model);
        self
    }

    /// Set a static instruction.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Set a dynamic instruction provider.
    pub fn instruction_provider(mut self, provider: InstructionProvider) -> Self {
        self.instruction_provider = Some(Arc::new(provider));
        self
    }

    /// Set a static global instruction.
    pub fn global_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.global_instruction = Some(instruction.into());
        self
    }

    /// Set a dynamic global instruction provider.
    pub fn global_instruction_provider(mut self, provider: GlobalInstructionProvider) -> Self {
        self.global_instruction_provider = Some(Arc::new(provider));
        self
    }

    /// Set the voice for audio output.
    pub fn voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    /// Set voice activity detection configuration.
    pub fn vad(mut self, config: VadConfig) -> Self {
        self.vad_config = Some(config);
        self
    }

    /// Enable server-side VAD with default settings.
    pub fn server_vad(mut self) -> Self {
        self.vad_config = Some(VadConfig {
            mode: VadMode::ServerVad,
            threshold: Some(0.5),
            prefix_padding_ms: Some(300),
            silence_duration_ms: Some(500),
            interrupt_response: Some(true),
            eagerness: None,
        });
        self
    }

    /// Set output modalities (e.g., ["text", "audio"]).
    pub fn modalities(mut self, modalities: Vec<String>) -> Self {
        self.modalities = modalities;
        self
    }

    /// Add a tool.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add a sub-agent for handoffs.
    pub fn sub_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.sub_agents.push(agent);
        self
    }

    /// Add a before-agent callback.
    pub fn before_agent_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    /// Add an after-agent callback.
    pub fn after_agent_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
        self
    }

    /// Add a before-tool callback.
    pub fn before_tool_callback(mut self, callback: BeforeToolCallback) -> Self {
        self.before_tool_callbacks.push(callback);
        self
    }

    /// Add an after-tool callback.
    pub fn after_tool_callback(mut self, callback: AfterToolCallback) -> Self {
        self.after_tool_callbacks.push(callback);
        self
    }

    /// Set callback for audio output events.
    pub fn on_audio(mut self, callback: AudioCallback) -> Self {
        self.on_audio = Some(callback);
        self
    }

    /// Set callback for transcript events.
    pub fn on_transcript(mut self, callback: TranscriptCallback) -> Self {
        self.on_transcript = Some(callback);
        self
    }

    /// Set callback for speech started events.
    pub fn on_speech_started(mut self, callback: SpeechCallback) -> Self {
        self.on_speech_started = Some(callback);
        self
    }

    /// Set callback for speech stopped events.
    pub fn on_speech_stopped(mut self, callback: SpeechCallback) -> Self {
        self.on_speech_stopped = Some(callback);
        self
    }

    /// Build the RealtimeAgent.
    pub fn build(self) -> Result<RealtimeAgent> {
        let model =
            self.model.ok_or_else(|| AdkError::Agent("RealtimeModel is required".to_string()))?;

        Ok(RealtimeAgent {
            name: self.name,
            description: self.description.unwrap_or_default(),
            model,
            instruction: self.instruction,
            instruction_provider: self.instruction_provider,
            global_instruction: self.global_instruction,
            global_instruction_provider: self.global_instruction_provider,
            voice: self.voice,
            vad_config: self.vad_config,
            modalities: self.modalities,
            tools: self.tools,
            sub_agents: self.sub_agents,
            before_callbacks: Arc::new(self.before_callbacks),
            after_callbacks: Arc::new(self.after_callbacks),
            before_tool_callbacks: Arc::new(self.before_tool_callbacks),
            after_tool_callbacks: Arc::new(self.after_tool_callbacks),
            on_audio: self.on_audio,
            on_transcript: self.on_transcript,
            on_speech_started: self.on_speech_started,
            on_speech_stopped: self.on_speech_stopped,
        })
    }
}

impl RealtimeAgent {
    /// Create a new builder.
    pub fn builder(name: impl Into<String>) -> RealtimeAgentBuilder {
        RealtimeAgentBuilder::new(name)
    }

    /// Get the static instruction, if set.
    pub fn instruction(&self) -> Option<&String> {
        self.instruction.as_ref()
    }

    /// Get the voice setting, if set.
    pub fn voice(&self) -> Option<&String> {
        self.voice.as_ref()
    }

    /// Get the VAD configuration, if set.
    pub fn vad_config(&self) -> Option<&VadConfig> {
        self.vad_config.as_ref()
    }

    /// Get the list of tools.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Build the realtime configuration from agent settings.
    async fn build_config(&self, ctx: &Arc<dyn InvocationContext>) -> Result<RealtimeConfig> {
        let mut config = RealtimeConfig::default();

        // Build instruction from providers or static value
        if let Some(provider) = &self.global_instruction_provider {
            let global_inst = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !global_inst.is_empty() {
                config.instruction = Some(global_inst);
            }
        } else if let Some(ref template) = self.global_instruction {
            let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            config.instruction = Some(processed);
        }

        // Add agent-specific instruction
        if let Some(provider) = &self.instruction_provider {
            let inst = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !inst.is_empty() {
                if let Some(existing) = &mut config.instruction {
                    existing.push_str("\n\n");
                    existing.push_str(&inst);
                } else {
                    config.instruction = Some(inst);
                }
            }
        } else if let Some(ref template) = self.instruction {
            let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            if let Some(existing) = &mut config.instruction {
                existing.push_str("\n\n");
                existing.push_str(&processed);
            } else {
                config.instruction = Some(processed);
            }
        }

        // Voice settings
        config.voice = self.voice.clone();
        config.turn_detection = self.vad_config.clone();
        config.modalities = Some(self.modalities.clone());

        // Convert ADK tools to realtime tool definitions
        let tool_defs: Vec<ToolDefinition> = self
            .tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: Some(t.enhanced_description().to_string()),
                parameters: t.parameters_schema(),
            })
            .collect();

        if !tool_defs.is_empty() {
            config.tools = Some(tool_defs);
        }

        // Add transfer_to_agent tool if sub-agents exist
        if !self.sub_agents.is_empty() {
            let mut tools = config.tools.unwrap_or_default();
            tools.push(ToolDefinition {
                name: "transfer_to_agent".to_string(),
                description: Some("Transfer execution to another agent.".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "The name of the agent to transfer to."
                        }
                    },
                    "required": ["agent_name"]
                })),
            });
            config.tools = Some(tools);
        }

        Ok(config)
    }

    /// Execute a tool call.
    #[allow(dead_code)]
    async fn execute_tool(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> (serde_json::Value, EventActions) {
        // Find the tool
        let tool = self.tools.iter().find(|t| t.name() == name);

        if let Some(tool) = tool {
            let args: serde_json::Value =
                serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));

            // Create tool context
            let tool_ctx: Arc<dyn ToolContext> =
                Arc::new(RealtimeToolContext::new(ctx.clone(), call_id.to_string()));

            // Execute before_tool callbacks
            for callback in self.before_tool_callbacks.as_ref() {
                if let Err(e) = callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    return (
                        serde_json::json!({ "error": e.to_string() }),
                        EventActions::default(),
                    );
                }
            }

            // Execute the tool
            let result = match tool.execute(tool_ctx.clone(), args).await {
                Ok(result) => result,
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };

            let actions = tool_ctx.actions();

            // Execute after_tool callbacks
            for callback in self.after_tool_callbacks.as_ref() {
                if let Err(e) = callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    return (serde_json::json!({ "error": e.to_string() }), actions);
                }
            }

            (result, actions)
        } else {
            (
                serde_json::json!({ "error": format!("Tool {} not found", name) }),
                EventActions::default(),
            )
        }
    }
}

#[async_trait]
impl Agent for RealtimeAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let agent_name = self.name.clone();
        let invocation_id = ctx.invocation_id().to_string();
        let model = self.model.clone();
        let _sub_agents = self.sub_agents.clone();

        // Clone callback refs
        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let before_tool_callbacks = self.before_tool_callbacks.clone();
        let after_tool_callbacks = self.after_tool_callbacks.clone();
        let tools = self.tools.clone();

        // Clone realtime callbacks
        let on_audio = self.on_audio.clone();
        let on_transcript = self.on_transcript.clone();
        let on_speech_started = self.on_speech_started.clone();
        let on_speech_stopped = self.on_speech_stopped.clone();

        // Build config
        let config = self.build_config(&ctx).await?;

        let s = stream! {
            // ===== BEFORE AGENT CALLBACKS =====
            for callback in before_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut early_event = Event::new(&invocation_id);
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);
                        return;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            // ===== CONNECT TO REALTIME SESSION =====
            let session = match model.connect(config).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(AdkError::Model(format!("Failed to connect: {}", e)));
                    return;
                }
            };

            // Yield session started event
            let mut start_event = Event::new(&invocation_id);
            start_event.author = agent_name.clone();
            start_event.llm_response.content = Some(Content {
                role: "system".to_string(),
                parts: vec![Part::Text {
                    text: format!("Realtime session started: {}", session.session_id()),
                }],
            });
            yield Ok(start_event);

            // ===== SEND INITIAL USER CONTENT =====
            // If user provided text input, send it to start the conversation
            let user_content = ctx.user_content();
            for part in &user_content.parts {
                if let Part::Text { text } = part {
                    if let Err(e) = session.send_text(text).await {
                        yield Err(AdkError::Model(format!("Failed to send text: {}", e)));
                        return;
                    }
                    // Request a response
                    if let Err(e) = session.create_response().await {
                        yield Err(AdkError::Model(format!("Failed to create response: {}", e)));
                        return;
                    }
                }
            }

            // ===== PROCESS REALTIME EVENTS =====
            loop {
                let event = session.next_event().await;

                match event {
                    Some(Ok(server_event)) => {
                        match server_event {
                            ServerEvent::AudioDelta { delta, item_id, .. } => {
                                // Call audio callback if set
                                if let Some(ref cb) = on_audio {
                                    cb(&delta, &item_id).await;
                                }

                                // Yield audio event
                                // delta is already Vec<u8> (bytes)
                                let mut audio_event = Event::new(&invocation_id);
                                audio_event.author = agent_name.clone();
                                audio_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::InlineData {
                                        mime_type: "audio/pcm".to_string(),
                                        data: delta.to_vec(),
                                    }],
                                });
                                yield Ok(audio_event);
                            }

                            ServerEvent::TextDelta { delta, .. } => {
                                let mut text_event = Event::new(&invocation_id);
                                text_event.author = agent_name.clone();
                                text_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text { text: delta.clone() }],
                                });
                                yield Ok(text_event);
                            }

                            ServerEvent::TranscriptDelta { delta, item_id, .. } => {
                                if let Some(ref cb) = on_transcript {
                                    cb(&delta, &item_id).await;
                                }
                            }

                            ServerEvent::SpeechStarted { audio_start_ms, .. } => {
                                if let Some(ref cb) = on_speech_started {
                                    cb(audio_start_ms).await;
                                }
                            }

                            ServerEvent::SpeechStopped { audio_end_ms, .. } => {
                                if let Some(ref cb) = on_speech_stopped {
                                    cb(audio_end_ms).await;
                                }
                            }

                            ServerEvent::FunctionCallDone {
                                call_id,
                                name,
                                arguments,
                                ..
                            } => {
                                // Handle transfer_to_agent
                                if name == "transfer_to_agent" {
                                    let args: serde_json::Value = serde_json::from_str(&arguments)
                                        .unwrap_or(serde_json::json!({}));
                                    let target = args.get("agent_name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default()
                                        .to_string();

                                    let mut transfer_event = Event::new(&invocation_id);
                                    transfer_event.author = agent_name.clone();
                                    transfer_event.actions.transfer_to_agent = Some(target);
                                    yield Ok(transfer_event);

                                    let _ = session.close().await;
                                    return;
                                }

                                // Execute tool
                                let tool = tools.iter().find(|t| t.name() == name);

                                let (result, actions) = if let Some(tool) = tool {
                                    let args: serde_json::Value = serde_json::from_str(&arguments)
                                        .unwrap_or(serde_json::json!({}));

                                    let tool_ctx: Arc<dyn ToolContext> = Arc::new(
                                        RealtimeToolContext::new(ctx.clone(), call_id.clone())
                                    );

                                    // Execute before_tool callbacks
                                    for callback in before_tool_callbacks.as_ref() {
                                        if let Err(e) = callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                                            let error_result = serde_json::json!({ "error": e.to_string() });
                                            (error_result, EventActions::default())
                                        } else {
                                            continue;
                                        };
                                    }

                                    let result = match tool.execute(tool_ctx.clone(), args).await {
                                        Ok(r) => r,
                                        Err(e) => serde_json::json!({ "error": e.to_string() }),
                                    };

                                    let actions = tool_ctx.actions();

                                    // Execute after_tool callbacks
                                    for callback in after_tool_callbacks.as_ref() {
                                        let _ = callback(ctx.clone() as Arc<dyn CallbackContext>).await;
                                    }

                                    (result, actions)
                                } else {
                                    (
                                        serde_json::json!({ "error": format!("Tool {} not found", name) }),
                                        EventActions::default(),
                                    )
                                };

                                // Yield tool event
                                let mut tool_event = Event::new(&invocation_id);
                                tool_event.author = agent_name.clone();
                                tool_event.actions = actions.clone();
                                tool_event.llm_response.content = Some(Content {
                                    role: "function".to_string(),
                                    parts: vec![Part::FunctionResponse {
                                        function_response: adk_core::FunctionResponseData {
                                            name: name.clone(),
                                            response: result.clone(),
                                        },
                                        id: Some(call_id.clone()),
                                    }],
                                });
                                yield Ok(tool_event);

                                // Check for escalation
                                if actions.escalate || actions.skip_summarization {
                                    let _ = session.close().await;
                                    return;
                                }

                                // Send tool response back to session
                                let response = ToolResponse {
                                    call_id,
                                    output: result,
                                };
                                if let Err(e) = session.send_tool_response(response).await {
                                    yield Err(AdkError::Model(format!("Failed to send tool response: {}", e)));
                                    let _ = session.close().await;
                                    return;
                                }
                            }

                            ServerEvent::ResponseDone { .. } => {
                                // Response complete, continue listening
                            }

                            ServerEvent::Error { error, .. } => {
                                yield Err(AdkError::Model(format!(
                                    "Realtime error: {} - {}",
                                    error.code.unwrap_or_default(),
                                    error.message
                                )));
                            }


                            _ => {
                                // Ignore other events
                            }
                        }
                    }
                    Some(Err(e)) => {
                        yield Err(AdkError::Model(format!("Session error: {}", e)));
                        break;
                    }
                    None => {
                        // Session closed
                        break;
                    }
                }
            }

            // ===== AFTER AGENT CALLBACKS =====
            for callback in after_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(&invocation_id);
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}

/// Tool context for realtime agent tool execution.
struct RealtimeToolContext {
    parent_ctx: Arc<dyn InvocationContext>,
    function_call_id: String,
    actions: Mutex<EventActions>,
}

impl RealtimeToolContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>, function_call_id: String) -> Self {
        Self { parent_ctx, function_call_id, actions: Mutex::new(EventActions::default()) }
    }
}

#[async_trait]
impl ReadonlyContext for RealtimeToolContext {
    fn invocation_id(&self) -> &str {
        self.parent_ctx.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.parent_ctx.agent_name()
    }

    fn user_id(&self) -> &str {
        self.parent_ctx.user_id()
    }

    fn app_name(&self) -> &str {
        self.parent_ctx.app_name()
    }

    fn session_id(&self) -> &str {
        self.parent_ctx.session_id()
    }

    fn branch(&self) -> &str {
        self.parent_ctx.branch()
    }

    fn user_content(&self) -> &Content {
        self.parent_ctx.user_content()
    }
}

#[async_trait]
impl CallbackContext for RealtimeToolContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.parent_ctx.artifacts()
    }
}

#[async_trait]
impl ToolContext for RealtimeToolContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }

    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        if let Some(memory) = self.parent_ctx.memory() {
            memory.search(query).await
        } else {
            Ok(vec![])
        }
    }
}
