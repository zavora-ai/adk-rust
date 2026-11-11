//! # acp_full_protocol — end-to-end ACP v1 server-direction reference
//!
//! This crate wires a **real** ADK-Rust [`Runner`](adk_runner::Runner)-backed
//! `AcpServer` to a small, deterministic agent so the Phase 2 ACP
//! server-direction features can be exercised end to end **without any API
//! key**:
//!
//! - **Embedded-resource prompts** round-trip into the agent as
//!   [`Part::EmbeddedResource`](adk_core::Part::EmbeddedResource).
//! - **Multimodal prompts** (image / audio) are accepted and reach the agent as
//!   [`Part::InlineData`](adk_core::Part::InlineData).
//! - **The server-side permission bridge** fires for a confirmation-gated tool:
//!   an approval executes the tool, a denial skips it, and the outer prompt
//!   completes in both cases.
//! - **`session/load` replay** re-sends the stored conversation in chronological
//!   order.
//! - **`UsageUpdate`** and enriched **`ToolCallUpdate`** notifications surface
//!   token usage and tool results.
//!
//! It also covers the ACP v5 surface `adk-acp` implements: protocol version
//! negotiation, client system prompts, end-of-turn token usage, session titles,
//! status, archiving, options, fork and resume, session modes and config
//! options, and authentication with logout and `auth/status`. Two unstable
//! surfaces have probes of their own:
//!
//! - **MCP over ACP**: prompting `client tools?` lists the tools the client
//!   serves over the ACP connection, and `call <tool>` runs one of them;
//! - **Configurable providers**: prompting `model?` names the model behind the
//!   agent's [`SwitchableModel`], which a client switches with `providers/set`
//!   ([`reference_providers`]).
//!
//! The [`ScriptedAgent`] is intentionally deterministic (it does not call an
//! LLM) so the behavior above is reproducible in tests. It models the same
//! pause/resume tool-confirmation flow an [`LlmAgent`](adk_agent) produces with
//! `require_tool_confirmation`, emitting an
//! [`event.actions.tool_confirmation`](adk_core::ToolConfirmationRequest) the
//! server's permission bridge maps to `session/request_permission`.
//!
//! The [`main`](../acp_full_protocol/index.html) binary serves this agent over
//! the stable ACP v1 stdio transport, and the crate's integration test drives
//! it through the SDK's in-process duplex channel.

use std::sync::{Arc, Mutex};

use adk_acp::agent_client_protocol::v1::schema::{
    AuthAccount, AuthMethod, AuthMethodAgent, AuthenticateRequest, SessionConfigOption,
    SessionMode, SessionModeState,
};
use adk_acp::server::{
    AcpAuthenticator, AcpServerError, LlmProtocol, ModelProvider, ModelProviders, ProviderSettings,
    SessionControls, SwitchableModel,
};
use adk_core::{
    AdkError, Agent, BeforeModelResult, CallbackContext, Content, EmbeddedResource, Event,
    EventStream, FunctionResponseData, InvocationContext, Llm, LlmRequest, LlmResponseStream, Part,
    ReadonlyContext, Result as AdkResult, ToolConfirmationDecision, ToolConfirmationRequest,
    ToolContext, UsageMetadata,
};
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::{FunctionTool, SimpleToolContext};
use async_trait::async_trait;
use serde_json::json;

/// The name the ACP server advertises for this agent.
pub const AGENT_NAME: &str = "acp-full-protocol-agent";

/// The confirmation-gated tool whose execution is bridged to
/// `session/request_permission`.
pub const CONFIRM_TOOL: &str = "delete_file";

/// The path the gated `delete_file` tool operates on.
pub const DELETE_TARGET: &str = "/workspace/report.csv";

/// Stable function-call id used for the gated tool call, so the permission
/// request correlates to the paused tool call.
pub const CONFIRM_CALL_ID: &str = "call-1";

/// The scripted agent's own system instruction, which a client system prompt
/// appends to or overrides.
pub const AGENT_INSTRUCTION: &str = "You are the scripted ACP reference agent.";

/// The authentication method [`WorkspaceToken`] advertises.
pub const TOKEN_METHOD: &str = "workspace-token";

/// Build the confirmation-gated `delete_file` tool.
///
/// It is a real [`FunctionTool`] (not read-only) that reports the `path` it
/// affected, so the resulting `ToolCallUpdate` carries a file location. It is
/// gated behind the ACP permission bridge, so it only runs after the client
/// approves the corresponding `session/request_permission` request.
pub fn build_delete_tool() -> Arc<dyn adk_core::Tool> {
    Arc::new(FunctionTool::new(
        CONFIRM_TOOL,
        "Delete a file at the given path (requires confirmation).",
        |_ctx: Arc<dyn ToolContext>, args: serde_json::Value| async move {
            let path =
                args.get("path").and_then(|value| value.as_str()).unwrap_or("unknown").to_string();
            Ok(json!({ "path": path, "deleted": true }))
        },
    ))
}

/// A deterministic agent that exercises the ACP server-direction features.
///
/// Behavior depends only on the prompt content and the resume decision, so the
/// end-to-end protocol behavior is fully reproducible:
///
/// - A prompt whose text contains `"delete"` pauses for tool confirmation. On
///   resume with an approval the gated tool executes and its result is emitted;
///   on resume with a denial the tool is skipped.
/// - Any other prompt echoes each embedded-resource and inline-data part it
///   received (proving multimodal and embedded content reached the agent),
///   emits a `read_file` tool call and completed result, and reports token
///   usage.
pub struct ScriptedAgent {
    delete_tool: Arc<dyn adk_core::Tool>,
    executed: Arc<Mutex<bool>>,
    applied_decision: Arc<Mutex<Option<ToolConfirmationDecision>>>,
    model: Option<SwitchableModel>,
}

impl ScriptedAgent {
    /// Create a new scripted agent wrapping the given confirmation-gated tool.
    pub fn new(delete_tool: Arc<dyn adk_core::Tool>) -> Self {
        Self {
            delete_tool,
            executed: Arc::new(Mutex::new(false)),
            applied_decision: Arc::new(Mutex::new(None)),
            model: None,
        }
    }

    /// Report `model`'s current backing model when prompted `model?`.
    pub fn with_model(mut self, model: SwitchableModel) -> Self {
        self.model = Some(model);
        self
    }

    /// Shared flag recording whether the gated tool actually executed.
    ///
    /// Tests observe this to confirm approve executes and deny/cancel skip.
    pub fn executed_flag(&self) -> Arc<Mutex<bool>> {
        self.executed.clone()
    }

    /// Shared slot recording the last tool-confirmation decision applied.
    pub fn applied_decision(&self) -> Arc<Mutex<Option<ToolConfirmationDecision>>> {
        self.applied_decision.clone()
    }
}

#[async_trait]
impl Agent for ScriptedAgent {
    fn name(&self) -> &str {
        AGENT_NAME
    }

    fn description(&self) -> &str {
        "Deterministic ACP full-protocol reference agent"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        // Decisions are keyed by function-call ID so an approval covers only the call it
        // was granted for, never a later call to the same tool.
        let decision = ctx.run_config().tool_confirmation_decisions.get(CONFIRM_CALL_ID).copied();
        let user_content = ctx.user_content().clone();
        let invocation = ctx.invocation_id().to_string();
        let delete_tool = self.delete_tool.clone();
        let executed = self.executed.clone();
        let applied = self.applied_decision.clone();
        let hooks = ctx.run_config().invocation_hooks.clone();
        let callback_ctx = ctx.clone() as Arc<dyn CallbackContext>;
        let readonly_ctx = ctx.clone() as Arc<dyn ReadonlyContext>;
        let toolsets: Vec<_> = ctx
            .run_config()
            .runtime_toolsets
            .iter()
            .map(|toolset| toolset.toolset().clone())
            .collect();
        let model = self.model.clone();
        let prompt_text: String = user_content.parts.iter().filter_map(Part::text).collect();

        let stream = async_stream::stream! {
            // ── Resume path: the client answered the permission request ──
            if let Some(decision) = decision {
                *applied.lock().expect("decision lock") = Some(decision);
                match decision {
                    ToolConfirmationDecision::Approve => {
                        // Actually run the gated tool through a lightweight
                        // ToolContext, then surface its result as a completed
                        // tool call (enriched ToolCallUpdate).
                        let tool_ctx: Arc<dyn ToolContext> = Arc::new(
                            SimpleToolContext::new(AGENT_NAME)
                                .with_function_call_id(CONFIRM_CALL_ID),
                        );
                        let result = delete_tool
                            .execute(tool_ctx, json!({ "path": DELETE_TARGET }))
                            .await
                            .unwrap_or_else(|error| json!({ "error": error.to_string() }));
                        *executed.lock().expect("executed lock") = true;

                        let mut result_event = Event::new(&invocation);
                        result_event.author = AGENT_NAME.to_string();
                        let mut content = Content::new("function");
                        content.parts.push(Part::FunctionResponse {
                            function_response: FunctionResponseData::new(CONFIRM_TOOL, result),
                            id: Some(CONFIRM_CALL_ID.to_string()),
                            annotations: None,
                        });
                        result_event.set_content(content);
                        yield Ok(result_event);

                        let mut done = Event::new(&invocation);
                        done.author = AGENT_NAME.to_string();
                        done.set_content(
                            Content::new("model")
                                .with_text(format!("tool executed: {DELETE_TARGET}")),
                        );
                        yield Ok(done);
                    }
                    ToolConfirmationDecision::Deny => {
                        *executed.lock().expect("executed lock") = false;
                        let mut done = Event::new(&invocation);
                        done.author = AGENT_NAME.to_string();
                        done.set_content(Content::new("model").with_text("tool call skipped"));
                        yield Ok(done);
                    }
                }
                return;
            }

            // ── Instructions probe: show what a model request would carry ──
            //
            // The agent makes no model calls, so it shows the system
            // instructions a model request of this turn would carry: its own
            // instruction after the run's invocation hooks, which is where the
            // ACP server applies a client's system prompt.
            if user_content.parts.iter().any(|part| matches!(part, Part::Text { text } if text == "instructions?")) {
                let mut probe = LlmRequest::new(
                    "probe",
                    vec![Content::new("system").with_text(AGENT_INSTRUCTION), user_content.clone()],
                );
                for hook in &hooks {
                    match hook.before_model(callback_ctx.clone(), probe).await {
                        Ok(BeforeModelResult::Continue(request)) => probe = request,
                        Ok(BeforeModelResult::Skip(_)) => return,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
                let instructions: Vec<String> = probe
                    .contents
                    .iter()
                    .filter(|content| content.role == "system")
                    .map(|content| content.parts.iter().filter_map(Part::text).collect::<Vec<_>>().join(""))
                    .collect();
                let mut event = Event::new(&invocation);
                event.author = AGENT_NAME.to_string();
                event.set_content(
                    Content::new("model").with_text(format!("instructions: {}", instructions.join(" | "))),
                );
                yield Ok(event);
                return;
            }

            // ── MCP over ACP: tools the client serves over the connection ──
            //
            // The ACP server turns each `acp` MCP server the client declared into
            // a runtime toolset of the turn.
            if prompt_text == "client tools?" || prompt_text.starts_with("call ") {
                let mut tools = Vec::new();
                for toolset in &toolsets {
                    match toolset.tools(readonly_ctx.clone()).await {
                        Ok(listed) => tools.extend(listed),
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
                let text = match prompt_text.strip_prefix("call ") {
                    None => {
                        let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
                        format!("client tools: {}", names.join(", "))
                    }
                    Some(name) => match tools.iter().find(|tool| tool.name() == name) {
                        None => format!("no client tool named {name}"),
                        Some(tool) => {
                            let tool_ctx: Arc<dyn ToolContext> =
                                Arc::new(SimpleToolContext::new(AGENT_NAME));
                            match tool.execute(tool_ctx, json!({})).await {
                                Ok(result) => format!("{name} returned {result}"),
                                Err(error) => format!("{name} failed: {error}"),
                            }
                        }
                    },
                };
                let mut event = Event::new(&invocation);
                event.author = AGENT_NAME.to_string();
                event.set_content(Content::new("model").with_text(text));
                yield Ok(event);
                return;
            }

            // ── Configurable providers: which model backs the agent now ──
            if prompt_text == "model?" {
                let text = match model.as_ref().map(SwitchableModel::current) {
                    None => "model: none configured".to_string(),
                    Some(None) => "model: disabled".to_string(),
                    Some(Some(current)) => format!("model: {}", current.name()),
                };
                let mut event = Event::new(&invocation);
                event.author = AGENT_NAME.to_string();
                event.set_content(Content::new("model").with_text(text));
                yield Ok(event);
                return;
            }

            // ── Fresh turn: classify the prompt ──
            let mut wants_delete = false;
            let mut echoes: Vec<String> = Vec::new();
            for part in &user_content.parts {
                match part {
                    Part::Text { text } if text.to_ascii_lowercase().contains("delete") => {
                        wants_delete = true;
                    }
                    Part::EmbeddedResource { resource } => {
                        echoes.push(describe_embedded(resource));
                    }
                    Part::InlineData { mime_type, data, .. } => {
                        echoes.push(format!("inline-data:{mime_type}:{}", data.len()));
                    }
                    _ => {}
                }
            }

            // A "delete" prompt pauses for confirmation, mirroring an LlmAgent
            // configured with `require_tool_confirmation(delete_file)`.
            if wants_delete {
                let mut event = Event::new(&invocation);
                event.author = AGENT_NAME.to_string();
                event.llm_response.interrupted = true;
                event.llm_response.turn_complete = true;
                event.set_content(
                    Content::new("model").with_text("Tool confirmation required"),
                );
                event.actions.tool_confirmation = Some(ToolConfirmationRequest {
                    tool_name: CONFIRM_TOOL.to_string(),
                    function_call_id: Some(CONFIRM_CALL_ID.to_string()),
                    args: json!({ "path": DELETE_TARGET }),
                    acting: None,
                    origin: None,
                });
                yield Ok(event);
                return;
            }

            // ── Rich turn: echo received content, emit a tool call/result,
            //    and report usage ──
            for echo in echoes {
                let mut event = Event::new(&invocation);
                event.author = AGENT_NAME.to_string();
                event.set_content(Content::new("model").with_text(echo));
                yield Ok(event);
            }

            // Tool call → ToolCall (in progress).
            let mut call_event = Event::new(&invocation);
            call_event.author = AGENT_NAME.to_string();
            let mut call_content = Content::new("model");
            call_content.parts.push(Part::FunctionCall {
                name: "read_file".to_string(),
                args: json!({ "path": "src/main.rs" }),
                id: Some("read-1".to_string()),
                thought_signature: None,
            });
            call_event.set_content(call_content);
            yield Ok(call_event);

            // Tool result → enriched ToolCallUpdate (content + location + kind).
            let mut result_event = Event::new(&invocation);
            result_event.author = AGENT_NAME.to_string();
            let mut result_content = Content::new("function");
            result_content.parts.push(Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "read_file",
                    json!({ "path": "src/main.rs", "content": "fn main() {}" }),
                ),
                id: Some("read-1".to_string()),
                annotations: None,
            });
            result_event.set_content(result_content);
            yield Ok(result_event);

            // Final event carries a message and usage metadata → AgentMessageChunk
            // plus a single UsageUpdate.
            let mut usage_event = Event::new(&invocation);
            usage_event.author = AGENT_NAME.to_string();
            usage_event.set_content(Content::new("model").with_text("turn complete"));
            usage_event.llm_response.usage_metadata = Some(UsageMetadata {
                prompt_token_count: 42,
                candidates_token_count: 8,
                total_token_count: 50,
                cost: Some(0.0001),
                ..Default::default()
            });
            yield Ok(usage_event);
        };

        Ok(Box::pin(stream))
    }
}

/// Render an embedded resource as a stable, assertable echo string.
fn describe_embedded(resource: &EmbeddedResource) -> String {
    match resource {
        EmbeddedResource::Text(text) => {
            format!("embedded-resource:{}:{}", text.uri, text.text)
        }
        EmbeddedResource::Blob(blob) => {
            format!("embedded-resource:{}:{}bytes", blob.uri, blob.data.len())
        }
    }
}

/// Build a fresh in-memory session service.
pub fn build_session_service() -> Arc<dyn SessionService> {
    Arc::new(InMemorySessionService::new())
}

/// Session controls for the reference agent: an `ask` and a `code` mode, and a
/// `verbose` toggle.
pub struct ReferenceControls;

impl SessionControls for ReferenceControls {
    fn modes(&self) -> Option<SessionModeState> {
        Some(SessionModeState::new(
            "ask",
            vec![SessionMode::new("ask", "Ask"), SessionMode::new("code", "Code")],
        ))
    }

    fn config_options(&self) -> Vec<SessionConfigOption> {
        vec![SessionConfigOption::boolean("verbose", "Verbose", false)]
    }
}

/// An authenticator that accepts the [`TOKEN_METHOD`] method when it holds a
/// token, supports logout, and reports one account.
///
/// The reference server requires it when `ACP_EXAMPLE_TOKEN` is set.
pub struct WorkspaceToken {
    token: String,
}

impl WorkspaceToken {
    /// Accept authentication while `token` is non-empty.
    pub fn new(token: impl Into<String>) -> Self {
        Self { token: token.into() }
    }
}

#[async_trait]
impl AcpAuthenticator for WorkspaceToken {
    fn methods(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::Agent(
            AuthMethodAgent::new(TOKEN_METHOD, "Workspace token")
                .description("The token in ACP_EXAMPLE_TOKEN".to_string()),
        )]
    }

    async fn authenticate(&self, request: &AuthenticateRequest) -> Result<(), AcpServerError> {
        if &*request.method_id.0 == TOKEN_METHOD && !self.token.is_empty() {
            Ok(())
        } else {
            Err(AcpServerError::MalformedMessage("no workspace token is configured".into()))
        }
    }

    fn supports_logout(&self) -> bool {
        true
    }

    async fn accounts(&self) -> Vec<AuthAccount> {
        vec![AuthAccount::new("Reference workspace")]
    }
}

/// A stand-in for a model client, named by the protocol and base URL it would
/// route to. The scripted agent never sends it requests.
pub struct RoutedModel {
    name: String,
}

impl RoutedModel {
    /// A model routed by `api_type` to `base_url`.
    pub fn new(api_type: &LlmProtocol, base_url: &str) -> Self {
        Self { name: format!("{api_type:?} at {base_url}") }
    }
}

#[async_trait]
impl Llm for RoutedModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        Err(AdkError::model("the reference agent does not call models"))
    }
}

/// The reference server's providers: a required `main` provider over
/// `model`, speaking OpenAI or Anthropic. A client's `providers/set` replaces
/// `model` with a [`RoutedModel`] for the new routing target; a real
/// application would build a model client from the settings, headers
/// included.
pub fn reference_providers(model: &SwitchableModel) -> ModelProviders {
    ModelProviders::new().provider(
        ModelProvider::new("main", model.clone(), |settings: &ProviderSettings| {
            Ok(Arc::new(RoutedModel::new(&settings.api_type, &settings.base_url)) as Arc<dyn Llm>)
        })
        .supports(LlmProtocol::OpenAi)
        .supports(LlmProtocol::Anthropic)
        .required(true)
        .current(LlmProtocol::OpenAi, DEFAULT_PROVIDER_URL),
    )
}

/// The routing target of the reference server's default model.
pub const DEFAULT_PROVIDER_URL: &str = "https://api.openai.com/v1";

/// The reference server's switchable model, starting at
/// [`DEFAULT_PROVIDER_URL`].
pub fn reference_model() -> SwitchableModel {
    SwitchableModel::new(Arc::new(RoutedModel::new(&LlmProtocol::OpenAi, DEFAULT_PROVIDER_URL)))
}
