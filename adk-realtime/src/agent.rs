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
//! let model = OpenAIRealtimeModel::new(api_key, "gpt-realtime");
//!
//! let agent = RealtimeAgent::builder("voice_assistant")
//!     .model(Box::new(model))
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
    AdkError, AfterAgentCallback, AfterToolCallback, Agent, AgentInteractionMode,
    BeforeAgentCallback, BeforeToolCallback, CallbackContext, Content, Event, EventActions,
    EventStream, GlobalInstructionProvider, InstructionProvider, InvocationContext, MemoryEntry,
    Part, ReadonlyContext, Result, Tool, ToolCallbackContext, ToolContext, Toolset,
};
use async_stream::stream;
use async_trait::async_trait;

use std::sync::{Arc, Mutex};

const MAX_BUFFERED_PLAYBACK_AUDIO_BYTES: usize = 16 * 1024 * 1024;

/// Shared realtime model type (thread-safe for async usage).
pub type BoxedRealtimeModel = Arc<dyn crate::model::RealtimeModel>;

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
    toolsets: Vec<Arc<dyn Toolset>>,
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

    // Video avatar configuration
    #[cfg(feature = "video-avatar")]
    avatar_config: Option<crate::avatar::AvatarConfig>,

    // Video avatar provider instance
    #[cfg(feature = "video-avatar")]
    avatar_provider: Option<std::sync::Arc<dyn crate::avatar::AvatarProvider>>,
}

/// Callback for audio output events (receives raw PCM bytes).
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
            .field("toolsets_count", &self.toolsets.len())
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
    toolsets: Vec<Arc<dyn Toolset>>,
    sub_agents: Vec<Arc<dyn Agent>>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
    before_tool_callbacks: Vec<BeforeToolCallback>,
    after_tool_callbacks: Vec<AfterToolCallback>,
    on_audio: Option<AudioCallback>,
    on_transcript: Option<TranscriptCallback>,
    on_speech_started: Option<SpeechCallback>,
    on_speech_stopped: Option<SpeechCallback>,

    #[cfg(feature = "video-avatar")]
    avatar_config: Option<crate::avatar::AvatarConfig>,

    #[cfg(feature = "video-avatar")]
    avatar_provider: Option<std::sync::Arc<dyn crate::avatar::AvatarProvider>>,
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
            toolsets: Vec::new(),
            sub_agents: Vec::new(),
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
            before_tool_callbacks: Vec::new(),
            after_tool_callbacks: Vec::new(),
            on_audio: None,
            on_transcript: None,
            on_speech_started: None,
            on_speech_stopped: None,
            #[cfg(feature = "video-avatar")]
            avatar_config: None,
            #[cfg(feature = "video-avatar")]
            avatar_provider: None,
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

    /// Register a dynamic toolset for per-invocation tool resolution.
    ///
    /// Toolsets are resolved at the start of each `run()` call using the
    /// invocation's `ReadonlyContext`. This enables context-dependent tools
    /// like per-user browser sessions from a pool.
    pub fn toolset(mut self, toolset: Arc<dyn Toolset>) -> Self {
        self.toolsets.push(toolset);
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

    /// Set the video avatar configuration for this agent.
    ///
    /// When set, the avatar configuration is included in the session setup
    /// payload sent to the realtime provider. If the provider does not support
    /// video avatars, a warning is logged and the session proceeds audio-only.
    ///
    /// Requires the `video-avatar` feature flag.
    #[cfg(feature = "video-avatar")]
    pub fn avatar(mut self, config: crate::avatar::AvatarConfig) -> Self {
        self.avatar_config = Some(config);
        self
    }

    /// Set the video avatar provider for this agent.
    ///
    /// When both an `AvatarConfig` (with a provider kind) and an `AvatarProvider`
    /// instance are set, the runner routes audio through the avatar provider
    /// for lip-sync rendering instead of sending raw audio to the client.
    ///
    /// Requires the `video-avatar` feature flag.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_realtime::avatar::heygen::{HeyGenConfig, HeyGenProvider};
    ///
    /// let provider = Arc::new(HeyGenProvider::new(HeyGenConfig::new("key")));
    /// let agent = RealtimeAgentBuilder::new("assistant")
    ///     .avatar(avatar_config)
    ///     .avatar_provider(provider)
    ///     .build()?;
    /// ```
    #[cfg(feature = "video-avatar")]
    pub fn avatar_provider(
        mut self,
        provider: std::sync::Arc<dyn crate::avatar::AvatarProvider>,
    ) -> Self {
        self.avatar_provider = Some(provider);
        self
    }

    /// Build the RealtimeAgent.
    pub fn build(self) -> Result<RealtimeAgent> {
        let model =
            self.model.ok_or_else(|| AdkError::agent("RealtimeModel is required".to_string()))?;

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
            toolsets: self.toolsets,
            sub_agents: self.sub_agents,
            before_callbacks: Arc::new(self.before_callbacks),
            after_callbacks: Arc::new(self.after_callbacks),
            before_tool_callbacks: Arc::new(self.before_tool_callbacks),
            after_tool_callbacks: Arc::new(self.after_tool_callbacks),
            on_audio: self.on_audio,
            on_transcript: self.on_transcript,
            on_speech_started: self.on_speech_started,
            on_speech_stopped: self.on_speech_stopped,
            #[cfg(feature = "video-avatar")]
            avatar_config: self.avatar_config,
            #[cfg(feature = "video-avatar")]
            avatar_provider: self.avatar_provider,
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

    /// Get the avatar configuration, if set.
    ///
    /// Requires the `video-avatar` feature flag.
    #[cfg(feature = "video-avatar")]
    pub fn avatar_config(&self) -> Option<&crate::avatar::AvatarConfig> {
        self.avatar_config.as_ref()
    }

    /// Get the avatar provider, if set.
    ///
    /// Requires the `video-avatar` feature flag.
    #[cfg(feature = "video-avatar")]
    pub fn avatar_provider(&self) -> Option<&std::sync::Arc<dyn crate::avatar::AvatarProvider>> {
        self.avatar_provider.as_ref()
    }

    /// Build the realtime configuration from agent settings.
    async fn build_config(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        resolved_tools: &[Arc<dyn Tool>],
    ) -> Result<RealtimeConfig> {
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
        let tool_defs: Vec<ToolDefinition> = resolved_tools
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

        // Include avatar configuration in session setup if present.
        // Currently no realtime provider supports video avatars natively,
        // so we log a warning and proceed audio-only. The config is still
        // placed in `extra` so future provider implementations can read it.
        #[cfg(feature = "video-avatar")]
        if let Some(ref avatar) = self.avatar_config {
            tracing::warn!(
                agent = %self.name,
                source_url = %avatar.source_url,
                "video avatar configured but the current realtime provider does not support video avatars; proceeding audio-only"
            );
            let avatar_json = serde_json::to_value(avatar).unwrap_or_else(|e| {
                tracing::warn!("failed to serialize avatar config: {e}");
                serde_json::Value::Null
            });
            let extra = config.extra.get_or_insert_with(|| serde_json::json!({}));
            if let Some(obj) = extra.as_object_mut() {
                obj.insert("avatarConfig".to_string(), avatar_json);
            }
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
            let tool_cb_ctx =
                Arc::new(ToolCallbackContext::new(ctx.clone(), name.to_string(), args.clone()));
            for callback in self.before_tool_callbacks.as_ref() {
                if let Err(e) = callback(tool_cb_ctx.clone() as Arc<dyn CallbackContext>).await {
                    return (
                        serde_json::json!({ "error": e.to_string() }),
                        EventActions::default(),
                    );
                }
            }

            // Execute the tool
            let result = match tool.execute(tool_ctx.clone(), args.clone()).await {
                Ok(result) => result,
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };

            let actions = tool_ctx.actions();

            // Execute after_tool callbacks
            let tool_cb_ctx =
                Arc::new(ToolCallbackContext::new(ctx.clone(), name.to_string(), args.clone()));
            for callback in self.after_tool_callbacks.as_ref() {
                if let Err(e) = callback(tool_cb_ctx.clone() as Arc<dyn CallbackContext>).await {
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

    fn interaction_mode(&self) -> AgentInteractionMode {
        AgentInteractionMode::Realtime
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
        let toolsets = self.toolsets.clone();

        // Clone realtime callbacks
        let on_audio = self.on_audio.clone();
        let on_transcript = self.on_transcript.clone();
        let on_speech_started = self.on_speech_started.clone();
        let on_speech_stopped = self.on_speech_stopped.clone();

        // Clone avatar provider for the stream closure
        #[cfg(feature = "video-avatar")]
        let avatar_provider = self.avatar_provider.clone();
        #[cfg(feature = "video-avatar")]
        let avatar_config_for_session = self.avatar_config.clone();

        // ===== RESOLVE TOOLSETS =====
        let mut resolved_tools: Vec<Arc<dyn Tool>> = tools.clone();
        let static_tool_names: std::collections::HashSet<String> =
            tools.iter().map(|t| t.name().to_string()).collect();
        let mut toolset_source: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for toolset in &toolsets {
            let toolset_tools = toolset.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            for tool in &toolset_tools {
                let name = tool.name().to_string();
                if static_tool_names.contains(&name) {
                    return Err(AdkError::agent(format!(
                        "Duplicate tool name '{}': conflict between static tool and toolset '{}'",
                        name,
                        toolset.name()
                    )));
                }
                if let Some(other_toolset_name) = toolset_source.get(&name) {
                    return Err(AdkError::agent(format!(
                        "Duplicate tool name '{}': conflict between toolset '{}' and toolset '{}'",
                        name,
                        other_toolset_name,
                        toolset.name()
                    )));
                }
                toolset_source.insert(name, toolset.name().to_string());
                resolved_tools.push(tool.clone());
            }
        }

        // Build config with resolved tools
        let config = self.build_config(&ctx, &resolved_tools).await?;

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
                    yield Err(AdkError::model(format!("Failed to connect: {}", e)));
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

            // ===== START AVATAR SESSION (if configured) =====
            #[cfg(feature = "video-avatar")]
            let avatar_session_id: Option<String> = {
                if let (Some(provider), Some(config)) = (&avatar_provider, &avatar_config_for_session) {
                    match provider.start_session(config).await {
                        Ok(session_info) => {
                            tracing::info!(
                                provider = %session_info.provider,
                                session_id = %session_info.session_id,
                                "avatar session started"
                            );
                            // Emit avatar session info as an event for the client
                            let mut avatar_event = Event::new(&invocation_id);
                            avatar_event.author = agent_name.clone();
                            avatar_event.llm_response.content = Some(Content {
                                role: "system".to_string(),
                                parts: vec![Part::Text {
                                    text: serde_json::to_string(&session_info).unwrap_or_default(),
                                }],
                            });
                            yield Ok(avatar_event);
                            Some(session_info.session_id)
                        }
                        Err(e) => {
                            // Graceful degradation: log warning, continue audio-only
                            tracing::warn!(
                                error = %e,
                                "avatar session creation failed, falling back to audio-only"
                            );
                            None
                        }
                    }
                } else {
                    None
                }
            };
            #[cfg(not(feature = "video-avatar"))]
            let _avatar_session_id: Option<String> = None;

            // Spawn keep-alive task for avatar session
            #[cfg(feature = "video-avatar")]
            let _avatar_keep_alive_handle: Option<tokio::task::JoinHandle<()>> = {
                if let (Some(provider), Some(sess_id)) = (&avatar_provider, &avatar_session_id) {
                    Some(crate::avatar::spawn_keep_alive(
                        provider.clone(),
                        sess_id.clone(),
                        std::time::Duration::from_secs(30),
                    ))
                } else {
                    None
                }
            };

            // ===== SEND INITIAL USER CONTENT =====
            // If user provided text input, send it to start the conversation
            let user_content = ctx.user_content();
            for part in &user_content.parts {
                if let Part::Text { text } = part {
                    if let Err(e) = session.send_text(text).await {
                        yield Err(AdkError::model(format!("Failed to send text: {}", e)));
                        return;
                    }
                    // Request a response
                    if let Err(e) = session.create_response().await {
                        yield Err(AdkError::model(format!("Failed to create response: {}", e)));
                        return;
                    }
                }
            }

            // ===== PROCESS REALTIME EVENTS =====
            let mut audio_buffers = std::collections::HashMap::<String, Vec<u8>>::new();
            let mut oversized_audio = std::collections::HashSet::<String>::new();
            loop {
                let event = session.next_event().await;

                match event {
                    Some(Ok(server_event)) => {
                        match server_event {
                            ServerEvent::AudioDelta { delta, item_id, .. } => {
                                // Route audio through avatar provider if active
                                #[cfg(feature = "video-avatar")]
                                if let (Some(provider), Some(sess_id)) = (&avatar_provider, &avatar_session_id) {
                                    if let Err(e) = provider.send_audio(sess_id, &delta).await {
                                        tracing::warn!(error = %e, "avatar send_audio failed");
                                    }
                                    // Don't yield raw audio to client — avatar provides video+audio
                                    // Still call the on_audio callback for monitoring
                                    if let Some(ref cb) = on_audio {
                                        cb(&delta, &item_id).await;
                                    }
                                    continue;
                                }

                                // No avatar provider — send raw audio to client
                                if let Some(ref cb) = on_audio {
                                    cb(&delta, &item_id).await;
                                }

                                if !oversized_audio.contains(&item_id) {
                                    let buffer = audio_buffers.entry(item_id.clone()).or_default();
                                    if buffer.len().saturating_add(delta.len())
                                        <= MAX_BUFFERED_PLAYBACK_AUDIO_BYTES
                                    {
                                        buffer.extend_from_slice(&delta);
                                    } else {
                                        audio_buffers.remove(&item_id);
                                        oversized_audio.insert(item_id.clone());
                                        tracing::warn!(
                                            item.id = item_id,
                                            limit.bytes = MAX_BUFFERED_PLAYBACK_AUDIO_BYTES,
                                            "realtime playback buffer exceeded its limit; raw audio events continue"
                                        );
                                    }
                                }

                                // Yield audio event (delta is already raw bytes)
                                let mut audio_event = Event::new(&invocation_id);
                                audio_event.author = agent_name.clone();
                                audio_event.provider_metadata.insert(
                                    "adk.realtime.audio_stream".to_string(),
                                    "pcm16-24000-mono".to_string(),
                                );
                                audio_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::InlineData {
                                        mime_type: "audio/pcm".to_string(),
                                        data: delta,
                                        uri: None,
                                        annotations: None,
                                    }],
                                });
                                yield Ok(audio_event);
                            }

                            ServerEvent::AudioDone { item_id, .. } => {
                                let oversized = oversized_audio.remove(&item_id);
                                if !oversized
                                    && let Some(pcm) = audio_buffers.remove(&item_id)
                                    && !pcm.is_empty()
                                {
                                    let mut audio_event = Event::new(&invocation_id);
                                    audio_event.author = agent_name.clone();
                                    audio_event.provider_metadata.insert(
                                        "adk.realtime.audio_playback".to_string(),
                                        "wav-24000-mono".to_string(),
                                    );
                                    audio_event.llm_response.content = Some(Content {
                                        role: "model".to_string(),
                                        parts: vec![Part::InlineData {
                                            mime_type: "audio/wav".to_string(),
                                            data: pcm16_mono_wav(&pcm, 24_000),
                                            uri: None,
                                            annotations: None,
                                        }],
                                    });
                                    yield Ok(audio_event);
                                }
                            }

                            ServerEvent::TextDelta { delta, item_id, .. } => {
                                let mut text_event = Event::with_id(
                                    format!("{invocation_id}:realtime-text:{item_id}"),
                                    &invocation_id,
                                );
                                text_event.author = agent_name.clone();
                                text_event.llm_response.partial = true;
                                text_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text { text: delta.clone() }],
                                });
                                yield Ok(text_event);
                            }

                            ServerEvent::TextDone { text, item_id, .. } => {
                                let mut text_event = Event::with_id(
                                    format!("{invocation_id}:realtime-text:{item_id}"),
                                    &invocation_id,
                                );
                                text_event.author = agent_name.clone();
                                text_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text { text }],
                                });
                                yield Ok(text_event);
                            }

                            ServerEvent::TranscriptDelta { delta, item_id, .. } => {
                                if let Some(ref cb) = on_transcript {
                                    cb(&delta, &item_id).await;
                                }
                                let mut transcript_event = Event::with_id(
                                    format!("{invocation_id}:realtime-transcript:{item_id}"),
                                    &invocation_id,
                                );
                                transcript_event.author = agent_name.clone();
                                transcript_event.llm_response.partial = true;
                                transcript_event.provider_metadata.insert(
                                    "adk.realtime.transcript".to_string(),
                                    "output".to_string(),
                                );
                                transcript_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text { text: delta }],
                                });
                                yield Ok(transcript_event);
                            }

                            ServerEvent::TranscriptDone { transcript, item_id, .. } => {
                                let mut transcript_event = Event::with_id(
                                    format!("{invocation_id}:realtime-transcript:{item_id}"),
                                    &invocation_id,
                                );
                                transcript_event.author = agent_name.clone();
                                transcript_event.provider_metadata.insert(
                                    "adk.realtime.transcript".to_string(),
                                    "output".to_string(),
                                );
                                transcript_event.llm_response.content = Some(Content {
                                    role: "model".to_string(),
                                    parts: vec![Part::Text { text: transcript }],
                                });
                                yield Ok(transcript_event);
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
                                let tool = resolved_tools.iter().find(|t| t.name() == name);

                                let (result, actions) = if let Some(tool) = tool {
                                    let args: serde_json::Value = serde_json::from_str(&arguments)
                                        .unwrap_or(serde_json::json!({}));

                                    let tool_ctx: Arc<dyn ToolContext> = Arc::new(
                                        RealtimeToolContext::new(ctx.clone(), call_id.clone())
                                    );

                                    let cb_ctx: Arc<dyn CallbackContext> =
                                        Arc::new(ToolCallbackContext::new(
                                            ctx.clone(),
                                            name.clone(),
                                            args.clone(),
                                        ));

                                    let result = execute_tool_with_callbacks(
                                        tool.as_ref(),
                                        tool_ctx.clone(),
                                        cb_ctx,
                                        args.clone(),
                                        before_tool_callbacks.as_ref(),
                                        after_tool_callbacks.as_ref(),
                                    )
                                    .await;

                                    (result, tool_ctx.actions())
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
                                        function_response: adk_core::FunctionResponseData::new(name.clone(), result.clone()),
                                        id: Some(call_id.clone()),
                                        annotations: None,
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
                                    yield Err(AdkError::model(format!("Failed to send tool response: {}", e)));
                                    let _ = session.close().await;
                                    return;
                                }
                            }

                            ServerEvent::ResponseDone { .. } => {
                                // Response complete, continue listening
                            }

                            ServerEvent::Error { error, .. } => {
                                yield Err(AdkError::model(format!(
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
                        yield Err(AdkError::model(format!("Session error: {}", e)));
                        break;
                    }
                    None => {
                        // Session closed
                        break;
                    }
                }
            }

            // ===== STOP AVATAR SESSION (cleanup) =====
            #[cfg(feature = "video-avatar")]
            {
                // Abort keep-alive task
                if let Some(handle) = _avatar_keep_alive_handle {
                    handle.abort();
                }
                // Stop the avatar session
                if let (Some(provider), Some(sess_id)) = (&avatar_provider, &avatar_session_id) {
                    if let Err(e) = provider.stop_session(sess_id).await {
                        tracing::warn!(error = %e, "avatar session cleanup failed");
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

fn pcm16_mono_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36_u32.saturating_add(data_len)).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&sample_rate.saturating_mul(2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Tool context for realtime agent tool execution.
/// Runs one tool through its before- and after-tool callbacks.
///
/// The callback contract matches the standard agent loop, which the realtime path did not
/// honour: a before-callback returning `Ok(Some(content))` substitutes a result and the tool
/// does **not** run, `Ok(None)` allows it, and an error refuses it and skips the after
/// callbacks. Previously the loop evaluated `(error_result, EventActions::default())` as a
/// discarded expression statement and fell through to `tool.execute`, so a gate could neither
/// deny nor substitute — it reported a decision while the tool ran regardless. After-callback
/// results were dropped by `let _ =`.
async fn execute_tool_with_callbacks(
    tool: &dyn Tool,
    tool_ctx: Arc<dyn ToolContext>,
    cb_ctx: Arc<dyn CallbackContext>,
    args: serde_json::Value,
    before_tool_callbacks: &[BeforeToolCallback],
    after_tool_callbacks: &[AfterToolCallback],
) -> serde_json::Value {
    let mut short_circuit: Option<serde_json::Value> = None;
    let mut run_after_tool_callbacks = true;

    for callback in before_tool_callbacks {
        match callback(cb_ctx.clone()).await {
            Ok(Some(content)) => {
                short_circuit = Some(content_to_tool_result(&content));
                break;
            }
            Ok(None) => continue,
            Err(e) => {
                short_circuit = Some(serde_json::json!({ "error": e.to_string() }));
                run_after_tool_callbacks = false;
                break;
            }
        }
    }

    let mut result = match short_circuit {
        Some(result) => result,
        None => match tool.execute(tool_ctx, args).await {
            Ok(value) => value,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        },
    };

    if run_after_tool_callbacks {
        for callback in after_tool_callbacks {
            match callback(cb_ctx.clone()).await {
                Ok(Some(modified)) => {
                    result = content_to_tool_result(&modified);
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    result = serde_json::json!({ "error": e.to_string() });
                    break;
                }
            }
        }
    }

    result
}

/// Turns a callback's substitute `Content` into the result sent back to the provider.
///
/// A callback returns `Content` because that is what the standard agent loop puts on the
/// event stream. The realtime transport wants a JSON tool result, so a function response is
/// unwrapped to its payload and anything else is carried as its text.
fn content_to_tool_result(content: &Content) -> serde_json::Value {
    for part in &content.parts {
        if let Part::FunctionResponse { function_response, .. } = part {
            return function_response.response.clone();
        }
    }

    let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
    serde_json::json!({ "result": text })
}

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

    /// Shared state from the parent context, so realtime tools coordinate with the rest of
    /// the run rather than seeing `None`.
    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.parent_ctx.shared_state()
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

    /// The caller's scopes, from the parent context.
    ///
    /// Without this the trait default returned an empty list, so a scope-checking tool saw an
    /// unauthenticated caller in realtime and behaved differently than in the standard loop.
    fn user_scopes(&self) -> Vec<String> {
        self.parent_ctx.user_scopes()
    }

    /// Secrets resolved through the parent context.
    ///
    /// The trait default returns `None`, which a tool cannot distinguish from a secret that is
    /// genuinely absent.
    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.parent_ctx.get_secret(name).await
    }
}

#[cfg(test)]
mod tool_safety_tests {
    //! A before-tool callback must be able to stop a tool, and a realtime tool must see the
    //! same capabilities it sees in the standard loop.
    //!
    //! The dispatch loop built `(error_result, EventActions::default())` as a discarded
    //! expression statement and then fell through to `tool.execute`, so a denying callback
    //! reported a decision that had no effect. After-callback results were dropped with
    //! `let _ =`. `RealtimeToolContext` implemented only the required methods, inheriting
    //! `user_scopes() -> vec![]`, `get_secret() -> None`, and `shared_state() -> None`, so a
    //! scope- or secret-checking tool behaved differently in realtime than under a Runner.

    use super::*;
    use adk_core::{RunConfig, SharedState, State};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn completed_pcm_audio_is_wrapped_as_a_playable_wav() {
        let pcm = [0_u8, 1, 2, 3];
        let wav = pcm16_mono_wav(&pcm, 24_000);

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), pcm.len() as u32);
        assert_eq!(&wav[44..], pcm);
    }

    /// Counts how many times it is executed.
    struct CountingTool {
        executions: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            "counting"
        }
        fn description(&self) -> &str {
            "counts executions"
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({ "ran": true }))
        }
    }

    /// The minimum context the callback path needs.
    struct TestToolContext {
        actions: Mutex<EventActions>,
        content: Content,
    }

    impl TestToolContext {
        fn new() -> Self {
            Self { actions: Mutex::new(EventActions::default()), content: Content::new("user") }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestToolContext {
        fn invocation_id(&self) -> &str {
            "inv"
        }
        fn agent_name(&self) -> &str {
            "agent"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for TestToolContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for TestToolContext {
        fn function_call_id(&self) -> &str {
            "call-1"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
    }

    /// Runs the tool through the callback gate with the supplied callbacks.
    async fn dispatch(
        before: Vec<BeforeToolCallback>,
        after: Vec<AfterToolCallback>,
        executions: Arc<AtomicUsize>,
    ) -> serde_json::Value {
        let tool = CountingTool { executions };
        let ctx = Arc::new(TestToolContext::new());
        execute_tool_with_callbacks(
            &tool,
            ctx.clone() as Arc<dyn ToolContext>,
            ctx as Arc<dyn CallbackContext>,
            serde_json::json!({}),
            &before,
            &after,
        )
        .await
    }

    #[tokio::test]
    async fn a_before_callback_error_prevents_execution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let before: Vec<BeforeToolCallback> =
            vec![Box::new(|_ctx| Box::pin(async { Err(AdkError::tool("denied by policy")) }))];

        let result = dispatch(before, vec![], Arc::clone(&executions)).await;

        assert_eq!(executions.load(Ordering::SeqCst), 0, "a refused tool must not run: {result}");
        assert!(
            result["error"].as_str().unwrap_or_default().contains("denied by policy"),
            "the refusal reason must reach the provider: {result}"
        );
    }

    #[tokio::test]
    async fn a_before_callback_substitution_prevents_execution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let before: Vec<BeforeToolCallback> = vec![Box::new(|_ctx| {
            Box::pin(async {
                Ok(Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::FunctionResponse {
                        function_response: adk_core::FunctionResponseData::new(
                            "counting",
                            serde_json::json!({ "cached": true }),
                        ),
                        id: None,
                        annotations: None,
                    }],
                }))
            })
        })];

        let result = dispatch(before, vec![], Arc::clone(&executions)).await;

        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "a substituted result must not run the tool"
        );
        assert_eq!(result, serde_json::json!({ "cached": true }));
    }

    #[tokio::test]
    async fn a_permitting_callback_lets_the_tool_run() {
        let executions = Arc::new(AtomicUsize::new(0));
        let before: Vec<BeforeToolCallback> = vec![Box::new(|_ctx| Box::pin(async { Ok(None) }))];

        let result = dispatch(before, vec![], Arc::clone(&executions)).await;

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(result, serde_json::json!({ "ran": true }));
    }

    #[tokio::test]
    async fn an_after_callback_error_becomes_the_result() {
        let executions = Arc::new(AtomicUsize::new(0));
        let after: Vec<AfterToolCallback> =
            vec![Box::new(|_ctx| Box::pin(async { Err(AdkError::tool("post-check failed")) }))];

        let result = dispatch(vec![], after, Arc::clone(&executions)).await;

        assert_eq!(executions.load(Ordering::SeqCst), 1, "the tool ran, as it should have");
        assert!(
            result["error"].as_str().unwrap_or_default().contains("post-check failed"),
            "an after-callback failure must not be dropped: {result}"
        );
    }

    #[tokio::test]
    async fn after_callbacks_are_skipped_when_a_before_callback_refuses() {
        let executions = Arc::new(AtomicUsize::new(0));
        let after_ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&after_ran);

        let before: Vec<BeforeToolCallback> =
            vec![Box::new(|_ctx| Box::pin(async { Err(AdkError::tool("refused")) }))];
        let after: Vec<AfterToolCallback> = vec![Box::new(move |_ctx| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            })
        })];

        let result = dispatch(before, after, Arc::clone(&executions)).await;

        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert_eq!(after_ran.load(Ordering::SeqCst), 0, "matching the standard loop's ordering");
        assert!(result["error"].as_str().unwrap_or_default().contains("refused"), "{result}");
    }

    // ── Context capabilities ──────────────────────────────────────────

    struct TestState;
    impl State for TestState {
        fn get(&self, _key: &str) -> Option<serde_json::Value> {
            None
        }
        fn set(&mut self, _key: String, _value: serde_json::Value) {}
        fn all(&self) -> HashMap<String, serde_json::Value> {
            HashMap::new()
        }
    }

    struct TestSession;
    impl adk_core::Session for TestSession {
        fn id(&self) -> &str {
            "session"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn state(&self) -> &dyn State {
            &TestState
        }
        fn conversation_history(&self) -> Vec<Content> {
            Vec::new()
        }
    }

    /// A parent context carrying the capabilities a tool should still see in realtime.
    struct CapableParent {
        content: Content,
        config: RunConfig,
        session: TestSession,
        shared: Arc<SharedState>,
    }

    #[async_trait]
    impl ReadonlyContext for CapableParent {
        fn invocation_id(&self) -> &str {
            "inv"
        }
        fn agent_name(&self) -> &str {
            "agent"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for CapableParent {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }

        fn shared_state(&self) -> Option<Arc<SharedState>> {
            Some(Arc::clone(&self.shared))
        }
    }

    #[async_trait]
    impl InvocationContext for CapableParent {
        fn agent(&self) -> Arc<dyn Agent> {
            unreachable!("not used by these tests")
        }
        fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
            None
        }
        fn session(&self) -> &dyn adk_core::Session {
            &self.session
        }
        fn run_config(&self) -> &RunConfig {
            &self.config
        }
        fn end_invocation(&self) {}
        fn ended(&self) -> bool {
            false
        }
        fn user_scopes(&self) -> Vec<String> {
            vec!["repo:write".to_string()]
        }
        async fn get_secret(&self, name: &str) -> Result<Option<String>> {
            Ok(Some(format!("secret-for-{name}")))
        }
    }

    #[tokio::test]
    async fn the_realtime_tool_context_preserves_parent_capabilities() {
        let parent = Arc::new(CapableParent {
            content: Content::new("user"),
            config: RunConfig::default(),
            session: TestSession,
            shared: Arc::new(SharedState::new()),
        }) as Arc<dyn InvocationContext>;

        let ctx = RealtimeToolContext::new(parent, "call-1".to_string());

        assert_eq!(
            ctx.user_scopes(),
            vec!["repo:write".to_string()],
            "an empty scope list makes an authenticated caller look anonymous"
        );
        assert_eq!(ctx.get_secret("api_key").await.unwrap().as_deref(), Some("secret-for-api_key"));
        assert!(ctx.shared_state().is_some(), "shared state must reach realtime tools");
        assert_eq!(ctx.app_name(), "app", "identity still delegates");
    }
}
