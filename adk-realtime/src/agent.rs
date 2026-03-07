use crate::config::{RealtimeConfig, ToolDefinition, VadConfig, VadMode};
use crate::events::{ServerEvent, ToolResponse};
use adk_core::{
    AdkError, AfterAgentCallback, AfterToolCallback, Agent, BeforeAgentCallback,
    BeforeToolCallback, CallbackContext, Content, Event, EventActions, EventStream,
    GlobalInstructionProvider, InstructionProvider, InvocationContext, MemoryEntry, Part,
    ReadonlyContext, Result, Tool, ToolContext, types::AdkIdentity,
};
use async_stream::stream;
use async_trait::async_trait;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Shared realtime model type (thread-safe for async usage).
pub type BoxedRealtimeModel = Arc<dyn crate::model::RealtimeModel>;

/// A real-time voice agent that implements the ADK Agent trait.
pub struct RealtimeAgent {
    name: String,
    description: String,
    model: BoxedRealtimeModel,

    // Instructions
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,

    // Voice-specific settings
    voice: Option<String>,
    vad_config: Option<VadConfig>,
    modalities: Vec<String>,

    // Tools
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<Arc<dyn Agent>>,

    // Callbacks
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

    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn model(mut self, model: BoxedRealtimeModel) -> Self {
        self.model = Some(model);
        self
    }

    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    pub fn instruction_provider(mut self, provider: InstructionProvider) -> Self {
        self.instruction_provider = Some(Arc::new(provider));
        self
    }

    pub fn global_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.global_instruction = Some(instruction.into());
        self
    }

    pub fn global_instruction_provider(mut self, provider: GlobalInstructionProvider) -> Self {
        self.global_instruction_provider = Some(Arc::new(provider));
        self
    }

    pub fn voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    pub fn vad(mut self, config: VadConfig) -> Self {
        self.vad_config = Some(config);
        self
    }

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

    pub fn modalities(mut self, modalities: Vec<String>) -> Self {
        self.modalities = modalities;
        self
    }

    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn sub_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.sub_agents.push(agent);
        self
    }

    pub fn before_agent_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    pub fn after_agent_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
        self
    }

    pub fn before_tool_callback(mut self, callback: BeforeToolCallback) -> Self {
        self.before_tool_callbacks.push(callback);
        self
    }

    pub fn after_tool_callback(mut self, callback: AfterToolCallback) -> Self {
        self.after_tool_callbacks.push(callback);
        self
    }

    pub fn on_audio(mut self, callback: AudioCallback) -> Self {
        self.on_audio = Some(callback);
        self
    }

    pub fn on_transcript(mut self, callback: TranscriptCallback) -> Self {
        self.on_transcript = Some(callback);
        self
    }

    pub fn on_speech_started(mut self, callback: SpeechCallback) -> Self {
        self.on_speech_started = Some(callback);
        self
    }

    pub fn on_speech_stopped(mut self, callback: SpeechCallback) -> Self {
        self.on_speech_stopped = Some(callback);
        self
    }

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
    pub fn builder(name: impl Into<String>) -> RealtimeAgentBuilder {
        RealtimeAgentBuilder::new(name)
    }

    pub fn instruction(&self) -> Option<&str> {
        self.instruction.as_deref()
    }

    async fn build_config(&self, ctx: &Arc<dyn InvocationContext>) -> Result<RealtimeConfig> {
        let mut config = RealtimeConfig::default();

        if let Some(provider) = &self.global_instruction_provider {
            let global_inst = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            if !global_inst.is_empty() {
                config.instruction = Some(global_inst);
            }
        } else if let Some(ref template) = self.global_instruction {
            let processed = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            config.instruction = Some(processed);
        }

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

        config.voice = self.voice.clone();
        config.turn_detection = self.vad_config.clone();
        config.modalities = Some(self.modalities.clone());

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

        if !self.sub_agents.is_empty() {
            let mut tools = config.tools.unwrap_or_default();
            tools.push(ToolDefinition {
                name: "transfer_to_agent".to_string(),
                description: Some("Transfer execution to another agent.".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_name": { "type": "string", "description": "The name of the agent to transfer to." }
                    },
                    "required": ["agent_name"]
                })),
            });
            config.tools = Some(tools);
        }

        Ok(config)
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
        let invocation_id = ctx.invocation_id().clone();
        let model = self.model.clone();

        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let before_tool_callbacks = self.before_tool_callbacks.clone();
        let after_tool_callbacks = self.after_tool_callbacks.clone();
        let tools = self.tools.clone();

        let on_audio = self.on_audio.clone();
        let on_transcript = self.on_transcript.clone();
        let on_speech_started = self.on_speech_started.clone();
        let on_speech_stopped = self.on_speech_stopped.clone();

        let config = self.build_config(&ctx).await?;

        let s = stream! {
            for callback in before_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut early_event = Event::new(invocation_id.clone());
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);
                        return;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }

            let session = match model.connect(config).await {
                Ok(s) => s,
                Err(e) => { yield Err(AdkError::Model(format!("Failed to connect: {}", e))); return; }
            };

            let mut start_event = Event::new(invocation_id.clone());
            start_event.author = agent_name.clone();
            start_event.llm_response.content = Some(Content {
                role: adk_core::types::Role::System,
                parts: vec![Part::text(format!("Realtime session started: {}", session.session_id()))],
            });
            yield Ok(start_event);

            for part in &ctx.user_content().parts {
                if let Some(text) = part.as_text() {
                    if let Err(e) = session.send_text(text).await {
                        yield Err(AdkError::Model(format!("Failed to send text: {}", e))); return;
                    }
                    if let Err(e) = session.create_response().await {
                        yield Err(AdkError::Model(format!("Failed to create response: {}", e))); return;
                    }
                }
            }

            loop {
                let event = session.next_event().await;
                match event {
                    Some(Ok(server_event)) => {
                        match server_event {
                            ServerEvent::AudioDelta { delta, item_id, .. } => {
                                if let Some(ref cb) = on_audio { cb(&delta, &item_id).await; }
                                let mut audio_event = Event::new(invocation_id.clone());
                                audio_event.author = agent_name.clone();
                                audio_event.llm_response.content = Some(Content {
                                    role: adk_core::types::Role::Model,
                                    parts: vec![Part::InlineData { mime_type: "audio/pcm".parse().unwrap(), data: delta.into() }],
                                });
                                yield Ok(audio_event);
                            }
                            ServerEvent::TextDelta { delta, .. } => {
                                let mut text_event = Event::new(invocation_id.clone());
                                text_event.author = agent_name.clone();
                                text_event.llm_response.content = Some(Content {
                                    role: adk_core::types::Role::Model,
                                    parts: vec![Part::text(delta.clone())],
                                });
                                yield Ok(text_event);
                            }
                            ServerEvent::TranscriptDelta { delta, item_id, .. } => {
                                if let Some(ref cb) = on_transcript { cb(&delta, &item_id).await; }
                            }
                            ServerEvent::SpeechStarted { audio_start_ms, .. } => {
                                if let Some(ref cb) = on_speech_started { cb(audio_start_ms).await; }
                            }
                            ServerEvent::SpeechStopped { audio_end_ms, .. } => {
                                if let Some(ref cb) = on_speech_stopped { cb(audio_end_ms).await; }
                            }
                            ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
                                if name == "transfer_to_agent" {
                                    let args: serde_json::Value = serde_json::from_str(&arguments).unwrap_or(serde_json::json!({}));
                                    let target = args.get("agent_name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                                    let mut transfer_event = Event::new(invocation_id.clone());
                                    transfer_event.author = agent_name.clone();
                                    transfer_event.actions.transfer_to_agent = Some(target);
                                    yield Ok(transfer_event);
                                    let _ = session.close().await;
                                    return;
                                }

                                let tool = tools.iter().find(|t| t.name() == name);
                                let (result, actions) = if let Some(tool) = tool {
                                    let args: serde_json::Value = serde_json::from_str(&arguments).unwrap_or(serde_json::json!({}));
                                    let tool_ctx: Arc<dyn ToolContext> = Arc::new(RealtimeToolContext::new(ctx.clone(), call_id.clone()));
                                    for callback in before_tool_callbacks.as_ref() {
                                        if let Err(e) = callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                                            tracing::warn!("Before tool callback failed: {}", e);
                                        }
                                    }
                                    let result = match tool.execute(tool_ctx.clone(), args).await {
                                        Ok(r) => r,
                                        Err(e) => serde_json::json!({ "error": e.to_string() }),
                                    };
                                    let actions = tool_ctx.actions();
                                    for callback in after_tool_callbacks.as_ref() {
                                        let _ = callback(ctx.clone() as Arc<dyn CallbackContext>).await;
                                    }
                                    (result, actions)
                                } else {
                                    (serde_json::json!({ "error": format!("Tool {} not found", name) }), EventActions::default())
                                };

                                let mut tool_event = Event::new(invocation_id.clone());
                                tool_event.author = agent_name.clone();
                                tool_event.actions = actions.clone();
                                tool_event.llm_response.content = Some(Content {
                                    role: adk_core::types::Role::Custom("function".to_string()),
                                    parts: vec![Part::FunctionResponse {
                                        name: name.clone(),
                                        response: result.clone(),
                                        id: Some(call_id.clone()),
                                    }],
                                });
                                yield Ok(tool_event);

                                if actions.escalate || actions.skip_summarization {
                                    let _ = session.close().await;
                                    return;
                                }

                                let response = ToolResponse { call_id: call_id.clone(), output: result.clone() };
                                if let Err(e) = session.send_tool_response(response).await {
                                    yield Err(AdkError::Model(format!("Failed to send tool response: {}", e)));
                                    let _ = session.close().await;
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Err(e)) => { yield Err(AdkError::Model(format!("Session error: {}", e))); break; }
                    None => break,
                }
            }

            for callback in after_callbacks.as_ref() {
                match callback(ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(invocation_id.clone());
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }
        };
        Ok(Box::pin(s))
    }
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
    fn identity(&self) -> &AdkIdentity {
        self.parent_ctx.identity()
    }
    fn user_content(&self) -> &Content {
        self.parent_ctx.user_content()
    }
    fn metadata(&self) -> &HashMap<String, String> {
        self.parent_ctx.metadata()
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
        if let Some(m) = self.parent_ctx.memory() { m.search(query).await } else { Ok(vec![]) }
    }
}
