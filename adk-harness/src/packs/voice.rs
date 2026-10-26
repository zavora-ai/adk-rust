//! The voice surface (spec 3.1 §7.17, feature `voice`; §5 H5).
//!
//! The same [`HarnessAgent`] is the agent of an `IntegratedRealtimeRunner`: the runner
//! governs calls under the harness's name, its tools are the runner's ADK tools and
//! [`HarnessAgent::run_config`] is the runner's `tool_governance`, so a spoken request runs
//! under the session's mode, the harness's tool classes and the subject's
//! standing rules, through the same governed path, audit trail, action ledger and spend
//! ledger as a typed one. A held call is surfaced as a realtime event
//! (`ServerEvent::ToolConfirmationRequested`), recorded in the session and the approval
//! store, and answered by voice — a spoken decision applied through the approval store —
//! or in the UI, through the approvals inbox that shares that store. The spoken decision
//! and the tool call are in one audit trail: `harness.voice.decided` cites the transcript's
//! digest, and the governed `ToolCall` record and `harness.bash.*` follow it.
//!
//! `store_to_memory` follows the harness memory policy: memory is written only through the
//! memory tool, under approval (spec 3.1 §7.15, I33), so the integration's automatic memory
//! write is off, and recalled context enters only through the harness's own instruction
//! (I40), so its memory injection is off too. Transcripts are persisted as session events.
//!
//! Under the `kernel` feature the surface also answers kernel processes parked on a held call
//! (spec 3.2 §7.11): `VoiceSurface::with_parked_processes` gives it the kernel's process
//! table and approval service, a spoken answer to an offered process is decided through that
//! service — whose run store wakes the parked run — and a process a freeze parked is refused,
//! since only `unfreeze` wakes it (K-I21). The surface's own realtime-session holds are
//! answered first, as before.
//!
//! The run config the surface governs calls with carries no confirmation handler: a handler
//! answers inside the call, which would block the event loop that carries the user's spoken
//! answer, so every call needing a decision is held and resumed instead. A spoken decision
//! is allow-once or deny; a standing rule is still created only at the console or in the
//! inbox (I2).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, ApprovalScope, ApprovalStore, Content, GovernanceControl, ReadonlyContext,
    RequestContext, Result, RunConfig, Tool, ToolApproval, ToolConfirmationRequest,
};
#[cfg(feature = "kernel")]
use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalService, InboxQuery, RunCheckpointRef, RunState,
    SubjectRef,
};
use adk_realtime::config::RealtimeConfig;
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig, ResumedToolCall};
use adk_realtime::metering::RealtimePricing;
use adk_realtime::model::BoxedModel;
use adk_realtime::{RealtimeError, ServerEvent};
use adk_session::SessionService;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent::HarnessAgent;
use crate::audit::HarnessAudit;
use crate::mode::HarnessMode;
use crate::prompt::HARNESS_SYSTEM_PROMPT;
use crate::toolset::mode_banner;

/// Action of the audit record written when a spoken request's call is held (spec 3.1 §9.3).
pub const VOICE_AUDIT_HELD: &str = "harness.voice.held";
/// Action of the audit record written when a held call is decided by voice or the inbox.
pub const VOICE_AUDIT_DECIDED: &str = "harness.voice.decided";
/// Action of the audit record written when a decided call has run.
pub const VOICE_AUDIT_RESUMED: &str = "harness.voice.resumed";
/// Action of the audit record written when a spoken decision on a parked kernel process is
/// refused, as for a process a freeze parked (spec 3.2 §7.11, K-I21).
pub const VOICE_AUDIT_REFUSED: &str = "harness.voice.refused";

/// The error code of a decision on a kernel process a freeze parked: only `unfreeze` wakes it
/// (spec 3.2 §7.11, K-I21).
pub const VOICE_FROZEN_CODE: &str = "harness.voice.frozen";

/// The error code of a decision on a process that is not parked on a held call the voice
/// user may decide (spec 3.2 §7.11).
pub const VOICE_NOT_PARKED_CODE: &str = "harness.voice.not_parked";

/// How often the surface asks the approval store whether the inbox decided a held call.
const INBOX_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// What a spoken answer to a held call means. A spoken standing rule ("approve and
/// remember", spec 3.1 §7.5) would be one more, so a `match` on it needs a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SpokenDecision {
    /// Run the held call once.
    Approve,
    /// Do not run the held call.
    Deny,
}

/// Who decided a held call. Other surfaces, such as the console, the kernel or an ACP
/// client, would be more, so a `match` on it needs a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecisionSource {
    /// A spoken answer in the voice session.
    Voice,
    /// The approvals inbox, or a caller of [`VoiceSurface::decide`].
    Inbox,
}

/// Reads a spoken answer to a held call from its transcript.
///
/// Returns `None` for anything that is not plainly an approval or a denial, so an unrelated
/// utterance leaves the call held: a decision is never inferred from an ambiguous answer.
///
/// # Example
///
/// ```rust
/// use adk_harness::{SpokenDecision, parse_spoken_decision};
///
/// assert_eq!(parse_spoken_decision("Yes."), Some(SpokenDecision::Approve));
/// assert_eq!(parse_spoken_decision("yes, go ahead"), Some(SpokenDecision::Approve));
/// assert_eq!(parse_spoken_decision("No, don't."), Some(SpokenDecision::Deny));
/// assert_eq!(parse_spoken_decision("what would it do?"), None);
/// assert_eq!(parse_spoken_decision("yes and no"), None);
/// ```
pub fn parse_spoken_decision(transcript: &str) -> Option<SpokenDecision> {
    let words: Vec<String> = transcript
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect();
    if words.is_empty() || words.len() > 8 {
        return None;
    }
    const APPROVE: [&str; 12] = [
        "yes",
        "yeah",
        "yep",
        "yup",
        "approve",
        "approved",
        "ok",
        "okay",
        "sure",
        "proceed",
        "confirm",
        "affirmative",
    ];
    const APPROVE_PHRASES: [&str; 4] = ["go ahead", "do it", "run it", "go for it"];
    const DENY: [&str; 10] =
        ["no", "nope", "deny", "denied", "don't", "dont", "stop", "cancel", "never", "negative"];
    let joined = words.join(" ");
    let approves = words.iter().any(|w| APPROVE.contains(&w.as_str()))
        || APPROVE_PHRASES.iter().any(|phrase| joined.contains(phrase));
    let denies = words.iter().any(|w| DENY.contains(&w.as_str()));
    match (approves, denies) {
        (true, false) => Some(SpokenDecision::Approve),
        (false, true) => Some(SpokenDecision::Deny),
        _ => None,
    }
}

/// How a [`VoiceSurface`] is opened.
///
/// # Example
///
/// ```rust
/// use adk_harness::VoiceConfig;
///
/// let config = VoiceConfig::new("adk-rust").with_voice("Kore");
/// assert_eq!(config.app_name, "adk-rust");
/// ```
#[derive(Clone)]
pub struct VoiceConfig {
    /// The app name sessions, modes, rules and spend are kept under.
    pub app_name: String,
    /// The provider voice, when the provider takes one.
    pub voice: Option<String>,
    /// Text appended to the harness's voice instruction.
    pub instruction: Option<String>,
    /// Prices each response's usage for the spend ledger.
    pub pricing: Option<RealtimePricing>,
    /// How long one tool call may run.
    pub tool_timeout: Duration,
    /// The identity the server authenticated for the session, on a served surface.
    pub request_context: Option<RequestContext>,
    /// Gemini Live context window compression, so a session that sends camera or screen
    /// frames is not ended after about two minutes (spec 3.3 §5.6).
    pub context_window_compression: bool,
    /// The kill switches every call checks, beside the harness's: the one **Stop all**
    /// freezes, so a stop reaches the realtime session's calls (spec 3.3 S-I10).
    pub governance: Vec<GovernanceControl>,
    /// Tools the realtime session offers beside the harness toolset:
    /// [`HarnessAgent::voice_tools`], so a spoken request reaches every tool a typed one does
    /// (spec 3.3 §5.6).
    pub tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for VoiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceConfig")
            .field("app_name", &self.app_name)
            .field("voice", &self.voice)
            .field("instruction", &self.instruction.is_some())
            .field("tool_timeout", &self.tool_timeout)
            .field("context_window_compression", &self.context_window_compression)
            .field("governance", &self.governance.len())
            .field("tools", &self.tools.iter().map(|tool| tool.name()).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl VoiceConfig {
    /// A configuration for `app_name` with the defaults.
    pub fn new(app_name: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
            voice: None,
            instruction: None,
            pricing: None,
            tool_timeout: Duration::from_secs(300),
            request_context: None,
            context_window_compression: false,
            governance: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// Sets the provider voice.
    #[must_use]
    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    /// Appends `instruction` to the harness's voice instruction.
    #[must_use]
    pub fn with_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Prices usage for the spend ledger.
    #[must_use]
    pub fn with_pricing(mut self, pricing: RealtimePricing) -> Self {
        self.pricing = Some(pricing);
        self
    }

    /// Runs every call as the identity the server authenticated.
    #[must_use]
    pub fn with_request_context(mut self, request_context: RequestContext) -> Self {
        self.request_context = Some(request_context);
        self
    }

    /// Turns on Gemini Live context window compression with the server's defaults
    /// (`RealtimeConfig::with_context_window_compression`): without it, a session that sends
    /// camera or screen frames ends after about two minutes (spec 3.3 §5.6). Set it for a
    /// Gemini Live model whenever video may be sent, and never for the OpenAI Realtime API,
    /// whose `session.update` would carry the key.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::VoiceConfig;
    ///
    /// let config = VoiceConfig::new("adk-rust").with_context_window_compression();
    /// assert!(config.context_window_compression);
    /// ```
    #[must_use]
    pub fn with_context_window_compression(mut self) -> Self {
        self.context_window_compression = true;
        self
    }

    /// Adds `control` to the kill switches every call of the session checks, as a `Runner`
    /// adds its own to each run: give it the one **Stop all** freezes, so a stop refuses the
    /// realtime session's calls and its resumes too (spec 3.3 S-I10).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::GovernanceControl;
    /// use adk_harness::VoiceConfig;
    ///
    /// let stop = GovernanceControl::new();
    /// let config = VoiceConfig::new("adk-rust").with_governance(stop.clone());
    /// assert!(config.governance[0].same_as(&stop));
    /// ```
    #[must_use]
    pub fn with_governance(mut self, control: GovernanceControl) -> Self {
        if !self.governance.iter().any(|existing| existing.same_as(&control)) {
            self.governance.push(control);
        }
        self
    }

    /// Offers `tools` beside the harness toolset; a tool whose name the harness toolset has
    /// is left out. Pass [`HarnessAgent::voice_tools`] so the session has every tool the
    /// harness's model has (spec 3.3 §5.6).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let tools = harness.voice_tools("adk-rust", "ana", "voice-1").await;
    /// let config = VoiceConfig::new("adk-rust").with_tools(tools);
    /// ```
    #[must_use]
    pub fn with_tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }
}

/// The context a toolset lists its tools in for a realtime session (spec 3.3 §5.6).
struct ListingContext {
    app_name: String,
    user_id: String,
    session_id: String,
    agent_name: String,
    content: Content,
}

#[async_trait::async_trait]
impl ReadonlyContext for ListingContext {
    fn invocation_id(&self) -> &str {
        &self.session_id
    }

    fn agent_name(&self) -> &str {
        &self.agent_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.content
    }
}

impl HarnessAgent {
    /// The tools the harness's model is given beside its toolset — those added with
    /// `.tool()` and `.toolset()`, the skills toolset, partition crossings and MCP servers —
    /// listed now for `user_id`'s session `session_id`, for a realtime session of the
    /// harness: pass them to [`VoiceConfig::with_tools`] (spec 3.3 §5.6). Provider-executed
    /// web tools are left out, since the text model's provider runs them, and so is a
    /// toolset that cannot list its tools, with a warning.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let tools = harness.voice_tools("adk-rust", "ana", "voice-1").await;
    /// let surface = VoiceSurface::open(harness, realtime, sessions, "ana", "voice-1",
    ///     VoiceConfig::new("adk-rust").with_tools(tools))?;
    /// ```
    pub async fn voice_tools(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
    ) -> Vec<Arc<dyn Tool>> {
        let (tools, toolsets) = &self.runtime().agent_tools;
        let mut found = tools.clone();
        let ctx: Arc<dyn ReadonlyContext> = Arc::new(ListingContext {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            agent_name: self.info().name.clone(),
            content: Content::new("user"),
        });
        for toolset in toolsets {
            match toolset.tools(Arc::clone(&ctx)).await {
                Ok(listed) => found.extend(listed),
                Err(error) => tracing::warn!(
                    toolset = toolset.name(),
                    error = %error,
                    "a toolset's tools could not be listed for the voice session"
                ),
            }
        }
        found
    }
}

/// What the voice surface reports. New kinds of event can be added, so a `match` on it
/// needs a wildcard arm.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum VoiceEvent {
    /// A provider event: audio, transcripts, responses.
    Provider(ServerEvent),
    /// A call of the spoken request was held for the user's decision.
    Held {
        /// The provider's call ID.
        call_id: String,
        /// The held request; its fingerprint is the decision's key.
        request: ToolConfirmationRequest,
    },
    /// A held call was decided.
    Decided {
        /// The provider's call ID; for a parked kernel process decided by voice, the
        /// process's pid (spec 3.2 §7.11).
        call_id: String,
        /// The decision.
        decision: SpokenDecision,
        /// Who decided.
        by: DecisionSource,
    },
    /// A decided call ran (or, denied, was answered as not run).
    Resumed(ResumedToolCall),
}

/// A held call the surface is waiting on.
#[derive(Clone, Debug)]
struct HeldCall {
    call_id: String,
    request: ToolConfirmationRequest,
}

/// The harness over a realtime session: voice in and out, the same policy, spoken approvals.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
///
/// use adk_harness::{HarnessAgent, Pack, VoiceConfig, VoiceEvent, VoiceSurface};
///
/// let harness = Arc::new(
///     HarnessAgent::builder("harness")
///         .model(model)
///         .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
///         .pack(Pack::Voice)
///         .build()?,
/// );
/// let surface = VoiceSurface::open(harness, realtime_model, sessions, "ana", "s1", VoiceConfig::new("adk-rust"))?;
/// surface.connect().await?;
/// surface.say("what's failing in the build?").await?;
/// while let Some(event) = surface.next_event().await {
///     match event? {
///         VoiceEvent::Held { request, .. } => println!("say yes or no: {}", request.tool_name),
///         VoiceEvent::Resumed(call) => println!("{} ran: {}", call.name, call.result),
///         _ => {}
///     }
/// }
/// ```
pub struct VoiceSurface {
    harness: Arc<HarnessAgent>,
    runner: IntegratedRealtimeRunner,
    scope: ApprovalScope,
    store: Arc<dyn ApprovalStore>,
    held: Mutex<Vec<HeldCall>>,
    pending: Mutex<VecDeque<VoiceEvent>>,
    inbox_checked: Mutex<Instant>,
    audit: Option<HarnessAudit>,
    /// The groups the voice user decides parked processes in: the request context's scopes.
    #[cfg(feature = "kernel")]
    approver_groups: Vec<String>,
    #[cfg(feature = "kernel")]
    parked: Option<ParkedAnswers>,
    /// Kernel processes offered for the user's spoken answer, oldest first.
    #[cfg(feature = "kernel")]
    offered: Mutex<Vec<String>>,
}

/// What the surface answers parked kernel processes through (spec 3.2 §7.11).
#[cfg(feature = "kernel")]
#[derive(Clone)]
struct ParkedAnswers {
    processes: adk_kernel::ProcessTable,
    approvals: Arc<dyn ApprovalService>,
}

/// A kernel process parked on a held call, with the inbox request that decides it (spec 3.2
/// §7.11).
#[cfg(feature = "kernel")]
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ParkedProcess {
    /// The process: its durable run's id.
    pub pid: String,
    /// The pending request in the approvals inbox; its run id is the process's.
    pub request: ApprovalRequest,
    /// Whether a freeze parked the process: then only `unfreeze` wakes it and no voice
    /// decision is taken (K-I21).
    pub frozen: bool,
}

impl std::fmt::Debug for VoiceSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceSurface")
            .field("harness", &self.harness.info().name)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

fn realtime_error(error: RealtimeError) -> AdkError {
    AdkError::model(format!("voice surface: {error}"))
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

impl VoiceSurface {
    /// Opens the voice surface of `harness` for `user_id`'s session `session_id` over
    /// `model`, with the harness toolset and `config.tools` as the runner's tools and
    /// `harness.run_config()`, with `config.governance`'s kill switches, as its governance
    /// (spec 3.1 §7.17, spec 3.3 §5.6).
    ///
    /// The harness must be marked `Pack::Voice`. Transcripts persist to `sessions`; the
    /// integration's memory write and memory injection are off, as the harness memory policy
    /// requires.
    ///
    /// # Errors
    ///
    /// `harness.voice.unmarked` when the harness was built without `Pack::Voice`; the
    /// runner's configuration error otherwise.
    pub fn open(
        harness: Arc<HarnessAgent>,
        model: BoxedModel,
        sessions: Arc<dyn SessionService>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        config: VoiceConfig,
    ) -> Result<Self> {
        if !harness.info().packs.iter().any(|pack| pack == "voice") {
            return Err(AdkError::new(
                adk_core::ErrorComponent::Agent,
                adk_core::ErrorCategory::InvalidInput,
                "harness.voice.unmarked",
                "the voice surface runs a harness built with .pack(Pack::Voice) (spec 3.1 §7.17)",
            ));
        }
        let user_id = user_id.into();
        let session_id = session_id.into();
        let scope = ApprovalScope::new(&config.app_name, &user_id, &session_id);
        // Calls are governed as the harness's own: its policy classifies tools by the agent
        // they belong to, and the acting agent of every fingerprint and audit record is the
        // harness, as in the standard loop.
        let agent_name = harness.info().name.clone();

        // The harness layer: policy, approval store, ledgers, budget, audit, acting. No
        // confirmation handler: a decision answered inside the call would block the event
        // loop that carries the spoken answer, so calls are held and resumed instead.
        let mut run_config: RunConfig = harness.run_config();
        run_config.tool_confirmation_handler = None;
        for control in &config.governance {
            if !run_config.governance.iter().any(|existing| existing.same_as(control)) {
                run_config.governance.push(control.clone());
            }
        }
        if let Some(request_context) = &config.request_context {
            run_config.request_context = Some(request_context.clone());
        }
        let store = run_config.approval_store.clone().unwrap_or_else(|| {
            Arc::clone(harness.runtime().approval_store()) as Arc<dyn ApprovalStore>
        });
        let audit = run_config.audit.clone().map(|trail| {
            let acting = run_config.acting_context_for(&config.app_name, &agent_name, &user_id);
            HarnessAudit::new(trail, AuditActor::new(&acting, &session_id), session_id.clone())
        });

        let mut realtime = RealtimeConfig::default()
            .with_instruction(voice_instruction(&harness, config.instruction.as_deref()))
            .with_transcription();
        if let Some(voice) = &config.voice {
            realtime = realtime.with_voice(voice.clone());
        }
        if config.context_window_compression {
            realtime = realtime.with_context_window_compression(None);
        }

        #[cfg(feature = "kernel")]
        let approver_groups = config
            .request_context
            .as_ref()
            .map(|context| context.scopes.clone())
            .unwrap_or_default();
        let mut builder = IntegratedRealtimeRunner::builder()
            .model(model)
            .identity(&config.app_name, &user_id, &session_id)
            .agent_name(agent_name)
            .session_service(sessions)
            .config(realtime)
            .tool_timeout(config.tool_timeout)
            .tool_governance(run_config)
            .concurrent_tool_dispatch(true)
            .integration_config(IntegrationConfig {
                persist_transcripts: true,
                store_to_memory: false,
                inject_memory_context: false,
                max_memory_injection: 0,
                max_history_injection: 20,
            });
        let mut names = std::collections::HashSet::new();
        for tool in harness.runtime().toolset().all_tools().into_iter().chain(config.tools) {
            if names.insert(tool.name().to_string()) {
                builder = builder.adk_tool(tool);
            }
        }
        if let Some(pricing) = config.pricing {
            builder = builder.realtime_pricing(pricing);
        }
        if let Some(request_context) = config.request_context {
            builder = builder.request_context(request_context);
        }
        let runner = builder.build().map_err(realtime_error)?;
        Ok(Self {
            harness,
            runner,
            scope,
            store,
            held: Mutex::new(Vec::new()),
            pending: Mutex::new(VecDeque::new()),
            inbox_checked: Mutex::new(Instant::now()),
            audit,
            #[cfg(feature = "kernel")]
            approver_groups,
            #[cfg(feature = "kernel")]
            parked: None,
            #[cfg(feature = "kernel")]
            offered: Mutex::new(Vec::new()),
        })
    }

    /// The harness the surface runs.
    pub fn harness(&self) -> &Arc<HarnessAgent> {
        &self.harness
    }

    /// The approval scope held calls are recorded under.
    pub fn scope(&self) -> &ApprovalScope {
        &self.scope
    }

    /// The runner the surface drives, for callers that need the realtime API directly.
    pub fn runner(&self) -> &IntegratedRealtimeRunner {
        &self.runner
    }

    /// Connects to the provider, then runs any call an earlier session held that has a
    /// decision since.
    ///
    /// # Errors
    ///
    /// The provider's connection error, or the resume path's.
    pub async fn connect(&self) -> Result<()> {
        self.runner.connect().await.map_err(realtime_error)?;
        for resumed in self.runner.resume_held_calls().await.map_err(realtime_error)? {
            self.record_resumed(&resumed).await?;
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(VoiceEvent::Resumed(resumed));
        }
        Ok(())
    }

    /// Sends typed text as a user turn and asks for a response.
    ///
    /// # Errors
    ///
    /// The provider's error.
    pub async fn say(&self, text: &str) -> Result<()> {
        self.runner.send_text(text).await.map_err(realtime_error)?;
        self.runner.create_response().await.map_err(realtime_error)
    }

    /// Sends one chunk of the user's audio.
    ///
    /// # Errors
    ///
    /// The provider's error.
    pub async fn send_audio(&self, audio: &adk_realtime::audio::AudioChunk) -> Result<()> {
        self.runner.send_audio_chunk(audio).await.map_err(realtime_error)
    }

    /// Commits the audio sent so far as one user turn, for a provider without server VAD.
    ///
    /// # Errors
    ///
    /// The provider's error.
    pub async fn commit_audio(&self) -> Result<()> {
        self.runner.commit_audio().await.map_err(realtime_error)
    }

    /// The calls waiting for the user's decision, oldest first.
    pub fn held_calls(&self) -> Vec<(String, ToolConfirmationRequest)> {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|held| (held.call_id.clone(), held.request.clone()))
            .collect()
    }

    /// The next event: a provider event, a hold, a decision or a resumed call.
    ///
    /// A completed input transcript while a call is held is read as the user's answer
    /// ([`parse_spoken_decision`]); a plain approval or denial decides the oldest held call
    /// by voice, an ambiguous utterance leaves it held. While a call is held, the approval
    /// store is polled so a decision made in the inbox resumes the call too.
    ///
    /// Returns `None` when the session has ended.
    ///
    /// # Errors
    ///
    /// The provider's error, a refused spend reservation, a blocked output, or a failure of
    /// the approval store or the audit trail.
    pub async fn next_event(&self) -> Option<Result<VoiceEvent>> {
        if let Some(event) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).pop_front() {
            return Some(Ok(event));
        }
        if let Err(error) = self.poll_inbox().await {
            return Some(Err(error));
        }
        if let Some(event) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).pop_front() {
            return Some(Ok(event));
        }

        loop {
            let event = match self.runner.next_event().await? {
                Ok(event) => event,
                Err(error) => return Some(Err(realtime_error(error))),
            };
            return match event {
                ServerEvent::ToolConfirmationRequested { call_id, request } => {
                    self.held
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(HeldCall { call_id: call_id.clone(), request: request.clone() });
                    match self.sync_held().await {
                        // Answered before its hold was reported: nothing to ask.
                        Ok(false) => continue,
                        Ok(true) => {}
                        Err(error) => return Some(Err(error)),
                    }
                    if let Err(error) = self.record_held(&call_id, &request).await {
                        return Some(Err(error));
                    }
                    Some(Ok(VoiceEvent::Held { call_id, request }))
                }
                ServerEvent::InputTranscriptCompleted { item_id, content_index, transcript } => {
                    let oldest =
                        self.held.lock().unwrap_or_else(|e| e.into_inner()).first().cloned();
                    let decision = parse_spoken_decision(&transcript);
                    match (oldest, decision) {
                        (Some(held), Some(decision)) => {
                            if let Err(error) = self
                                .apply(&held, decision, DecisionSource::Voice, Some(&transcript))
                                .await
                            {
                                return Some(Err(error));
                            }
                        }
                        // The session's own holds come first; an offered kernel process
                        // takes a plain answer only when none waits (spec 3.2 §7.11).
                        #[cfg(feature = "kernel")]
                        (None, Some(decision)) => {
                            if let Err(error) = self.answer_offered(decision, &transcript).await {
                                return Some(Err(error));
                            }
                        }
                        _ => {}
                    }
                    // The transcript itself is reported first; a decision and the resumed
                    // call follow.
                    Some(Ok(VoiceEvent::Provider(ServerEvent::InputTranscriptCompleted {
                        item_id,
                        content_index,
                        transcript,
                    })))
                }
                other => Some(Ok(VoiceEvent::Provider(other))),
            };
        }
    }

    /// Decides the held call with `fingerprint` from outside the voice session — the
    /// inbox's route — and runs it.
    ///
    /// # Errors
    ///
    /// `harness.voice.not_held` when no held call has `fingerprint`; the approval store's
    /// or the resume path's error otherwise.
    pub async fn decide(
        &self,
        fingerprint: &str,
        decision: SpokenDecision,
    ) -> Result<Vec<ResumedToolCall>> {
        let held = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|held| held.request.fingerprint() == fingerprint)
            .cloned()
            .ok_or_else(|| {
                AdkError::new(
                    adk_core::ErrorComponent::Agent,
                    adk_core::ErrorCategory::NotFound,
                    "harness.voice.not_held",
                    "no held call has this fingerprint: it was decided or never held",
                )
            })?;
        self.apply(&held, decision, DecisionSource::Inbox, None).await?;
        let mut resumed = Vec::new();
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|event| match event {
            VoiceEvent::Resumed(call) if call.call_id == held.call_id => {
                resumed.push(call.clone());
                false
            }
            _ => true,
        });
        Ok(resumed)
    }

    /// Closes the provider session.
    pub async fn close(&self) {
        if let Err(error) = self.runner.close().await {
            tracing::debug!(error = %error, "closing the voice session failed");
        }
    }

    /// Records `decision` for `held` in the approval store and resumes what has a decision.
    async fn apply(
        &self,
        held: &HeldCall,
        decision: SpokenDecision,
        by: DecisionSource,
        transcript: Option<&str>,
    ) -> Result<()> {
        let fingerprint = held.request.fingerprint();
        let approval = match decision {
            SpokenDecision::Approve => ToolApproval::approve(),
            SpokenDecision::Deny => ToolApproval::deny().with_message("the user said no"),
        };
        self.store.decide(&self.scope, &fingerprint, approval).await?;
        if let Some(audit) = &self.audit {
            audit
                .record(
                    VOICE_AUDIT_DECIDED,
                    json!({
                        "functionCallId": held.call_id,
                        "tool": held.request.tool_name,
                        "fingerprintDigest": digest(&fingerprint),
                        "decision": decision,
                        "by": by,
                        "transcriptDigest": transcript.map(digest),
                    }),
                )
                .await?;
        }
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).push_back(VoiceEvent::Decided {
            call_id: held.call_id.clone(),
            decision,
            by,
        });
        self.resume().await
    }

    /// Keeps the surface's held calls to those the session still holds, and reports whether
    /// the newest one is among them.
    ///
    /// The session is the record (`adk_core::held_tool_calls`): a realtime model that hears
    /// the user's answer before the surface applies it may repeat the waiting call, and the
    /// repeat's hold supersedes the identical earlier one, so one answer settles both and
    /// the command runs once.
    async fn sync_held(&self) -> Result<bool> {
        let session: Vec<String> = self
            .runner
            .held_calls()
            .await
            .map_err(realtime_error)?
            .into_iter()
            .filter_map(|held| held.request.function_call_id)
            .collect();
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let newest = held.last().map(|call| session.contains(&call.call_id));
        held.retain(|call| session.contains(&call.call_id));
        Ok(newest.unwrap_or(false))
    }

    /// Runs every held call with a decision, reporting each as [`VoiceEvent::Resumed`].
    async fn resume(&self) -> Result<()> {
        let resumed = self.runner.resume_held_calls().await.map_err(realtime_error)?;
        self.sync_held().await?;
        for call in resumed {
            self.record_resumed(&call).await?;
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(VoiceEvent::Resumed(call));
        }
        Ok(())
    }

    /// While a call is held, asks the store whether the inbox decided it.
    async fn poll_inbox(&self) -> Result<()> {
        let held: Vec<HeldCall> = self.held.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if held.is_empty() {
            return Ok(());
        }
        {
            let mut checked = self.inbox_checked.lock().unwrap_or_else(|e| e.into_inner());
            if checked.elapsed() < INBOX_POLL_INTERVAL {
                return Ok(());
            }
            *checked = Instant::now();
        }
        let mut decided = Vec::new();
        for call in held {
            let fingerprint = call.request.fingerprint();
            if self.store.has_decision(&self.scope, &fingerprint).await? {
                decided.push(call);
            }
        }
        if decided.is_empty() {
            return Ok(());
        }
        for call in &decided {
            if let Some(audit) = &self.audit {
                audit
                    .record(
                        VOICE_AUDIT_DECIDED,
                        json!({
                            "functionCallId": call.call_id,
                            "tool": call.request.tool_name,
                            "fingerprintDigest": digest(&call.request.fingerprint()),
                            "by": DecisionSource::Inbox,
                        }),
                    )
                    .await?;
            }
        }
        self.resume().await?;
        // A decision the resume path consumed as a denial still cleared the hold.
        let resumed_ids: Vec<String> = {
            let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending
                .iter()
                .filter_map(|event| match event {
                    VoiceEvent::Resumed(call) => Some(call.call_id.clone()),
                    _ => None,
                })
                .collect()
        };
        for call in decided.into_iter().filter(|call| resumed_ids.contains(&call.call_id)) {
            let decision = if self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|event| matches!(event, VoiceEvent::Resumed(resumed) if resumed.call_id == call.call_id && resumed.result.get("error").is_none()))
            {
                SpokenDecision::Approve
            } else {
                SpokenDecision::Deny
            };
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).push_front(
                VoiceEvent::Decided { call_id: call.call_id, decision, by: DecisionSource::Inbox },
            );
        }
        Ok(())
    }

    /// Runs every held call of the session that has a decision, as the runner's
    /// `resume_held_calls` does, and records `harness.voice.resumed` for each: for a caller
    /// that reads the runner's events itself instead of [`next_event`](Self::next_event), such
    /// as a voice conversation of the agent surfaces (spec 3.3 §5.6).
    ///
    /// # Errors
    ///
    /// The resume path's error, such as a frozen kill switch, or the audit trail's.
    pub async fn resume_held_calls(&self) -> Result<Vec<ResumedToolCall>> {
        let resumed = self.runner.resume_held_calls().await.map_err(realtime_error)?;
        for call in &resumed {
            self.record_resumed(call).await?;
        }
        Ok(resumed)
    }

    /// Records `harness.voice.held` for the call `call_id` the runner held with `request`:
    /// for a caller that reads the runner's events itself instead of
    /// [`next_event`](Self::next_event) (spec 3.3 §5.6).
    ///
    /// # Errors
    ///
    /// The audit trail's error.
    pub async fn record_held(
        &self,
        call_id: &str,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let Some(audit) = &self.audit else {
            return Ok(());
        };
        audit
            .record(
                VOICE_AUDIT_HELD,
                json!({
                    "functionCallId": call_id,
                    "tool": request.tool_name,
                    "fingerprintDigest": digest(&request.fingerprint()),
                    "surface": "voice",
                }),
            )
            .await
    }

    async fn record_resumed(&self, call: &ResumedToolCall) -> Result<()> {
        let Some(audit) = &self.audit else {
            return Ok(());
        };
        audit
            .record(
                VOICE_AUDIT_RESUMED,
                json!({
                    "functionCallId": call.call_id,
                    "tool": call.name,
                    "executed": call.result.get("error").is_none(),
                }),
            )
            .await
    }
}

#[cfg(feature = "kernel")]
impl VoiceSurface {
    /// Answers kernel processes parked on a held call through `approvals`, the approval
    /// service the kernel's processes submit their holds to (spec 3.2 §7.11).
    ///
    /// Build `approvals` with `StoreApprovalService::with_run_store` over the kernel's run
    /// store, so a decision wakes the parked run. The voice user decides as
    /// `SubjectRef::human(user_id)`, a member of the request context's scopes, as the
    /// approvals REST maps a caller. The session's own held calls are answered first.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let surface = VoiceSurface::open(harness, realtime, sessions, "ana", "voice-1", config)?
    ///     .with_parked_processes(kernel.processes().clone(), approvals);
    /// for parked in surface.parked_processes().await? {
    ///     println!("{} waits on {}", parked.pid, parked.request.tool_name);
    /// }
    /// ```
    #[must_use]
    pub fn with_parked_processes(
        mut self,
        processes: adk_kernel::ProcessTable,
        approvals: Arc<dyn ApprovalService>,
    ) -> Self {
        self.parked = Some(ParkedAnswers { processes, approvals });
        self
    }

    /// The kernel processes parked on a held call the voice user may decide, oldest first;
    /// a process a freeze parked is listed with [`ParkedProcess::frozen`] set.
    ///
    /// # Errors
    ///
    /// `harness.voice.no_kernel` without [`with_parked_processes`](Self::with_parked_processes);
    /// the approval service's or the process table's error otherwise.
    pub async fn parked_processes(&self) -> Result<Vec<ParkedProcess>> {
        let parked = self.parked_answers()?;
        let mut query = InboxQuery::for_approver(self.approver(), self.approver_groups.clone());
        let mut found = Vec::new();
        loop {
            let page = parked.approvals.inbox(query.clone()).await?;
            for request in page.items {
                let Some(pid) = request.run_id.clone() else { continue };
                if let Some(process) = parked_state(parked, &pid).await? {
                    found.push(ParkedProcess { pid, request, frozen: process });
                }
            }
            match page.next_cursor {
                Some(cursor) => query.cursor = Some(cursor),
                None => return Ok(found),
            }
        }
    }

    /// Offers parked process `pid` for the user's spoken answer: the next plain yes or no,
    /// while no call of this session is held, decides it (spec 3.2 §7.11). Ask the user
    /// about it — with [`say`](Self::say), or on the screen — after offering it.
    ///
    /// # Errors
    ///
    /// [`VOICE_FROZEN_CODE`] for a process a freeze parked, [`VOICE_NOT_PARKED_CODE`] for one
    /// not parked on a request the voice user may decide.
    pub async fn offer_parked(&self, pid: &str) -> Result<ParkedProcess> {
        let parked = self.parked_process(pid).await?;
        let mut offered = self.offered.lock().unwrap_or_else(|e| e.into_inner());
        if !offered.iter().any(|offered| offered == pid) {
            offered.push(pid.to_string());
        }
        Ok(parked)
    }

    /// The processes offered and not yet decided, oldest first.
    pub fn offered_processes(&self) -> Vec<String> {
        self.offered.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Decides the held call parked process `pid` waits on, through the kernel's approval
    /// service, whose run store wakes the process (spec 3.2 §7.11). Returns the decided
    /// request.
    ///
    /// # Errors
    ///
    /// [`VOICE_FROZEN_CODE`] for a process a freeze parked, which only `unfreeze` wakes
    /// (K-I21); [`VOICE_NOT_PARKED_CODE`] for one not parked on a request the voice user may
    /// decide; the approval service's error, such as `approval.not_an_approver`.
    pub async fn decide_process(
        &self,
        pid: &str,
        decision: SpokenDecision,
    ) -> Result<ApprovalRequest> {
        self.decide_parked(pid, decision, DecisionSource::Inbox, None).await
    }

    /// Applies a spoken `decision` to the oldest offered process, reporting it as
    /// [`VoiceEvent::Decided`] by voice.
    async fn answer_offered(&self, decision: SpokenDecision, transcript: &str) -> Result<()> {
        let Some(pid) = self.offered.lock().unwrap_or_else(|e| e.into_inner()).first().cloned()
        else {
            return Ok(());
        };
        self.decide_parked(&pid, decision, DecisionSource::Voice, Some(transcript)).await?;
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).push_back(VoiceEvent::Decided {
            call_id: pid,
            decision,
            by: DecisionSource::Voice,
        });
        Ok(())
    }

    async fn decide_parked(
        &self,
        pid: &str,
        decision: SpokenDecision,
        by: DecisionSource,
        transcript: Option<&str>,
    ) -> Result<ApprovalRequest> {
        let answers = self.parked_answers()?.clone();
        let parked = match self.parked_process(pid).await {
            Ok(parked) => parked,
            Err(error) => {
                self.withdraw(pid);
                if error.code == VOICE_FROZEN_CODE {
                    self.record_refused(pid, &error, transcript).await?;
                }
                return Err(error);
            }
        };
        let approver = self.approver();
        let approval = match decision {
            SpokenDecision::Approve => ApprovalDecision::approve(approver),
            SpokenDecision::Deny => ApprovalDecision::reject(approver, "the user said no"),
        }
        .with_groups(self.approver_groups.clone());
        let decided =
            answers.approvals.decide(&parked.request.id, parked.request.version, approval).await?;
        self.withdraw(pid);
        if let Some(audit) = &self.audit {
            audit
                .record(
                    VOICE_AUDIT_DECIDED,
                    json!({
                        "pid": pid,
                        "approvalId": decided.id,
                        "tool": decided.tool_name,
                        "fingerprintDigest": digest(&decided.fingerprint),
                        "decision": decision,
                        "by": by,
                        "transcriptDigest": transcript.map(digest),
                    }),
                )
                .await?;
        }
        Ok(decided)
    }

    /// `pid` as a parked process the voice user may decide, or why not.
    async fn parked_process(&self, pid: &str) -> Result<ParkedProcess> {
        let answers = self.parked_answers()?;
        let frozen = match parked_state(answers, pid).await? {
            Some(true) => {
                return Err(AdkError::new(
                    adk_core::ErrorComponent::Agent,
                    adk_core::ErrorCategory::Forbidden,
                    VOICE_FROZEN_CODE,
                    format!(
                        "process {pid} was parked by a freeze: only unfreeze wakes it, so no \
                         voice decision is taken (spec 3.2 K-I21)"
                    ),
                ));
            }
            Some(false) => false,
            None => return Err(not_parked(pid, "it is not waiting on a held call")),
        };
        let mut query = InboxQuery::for_approver(self.approver(), self.approver_groups.clone());
        loop {
            let page = answers.approvals.inbox(query.clone()).await?;
            if let Some(request) =
                page.items.into_iter().find(|request| request.run_id.as_deref() == Some(pid))
            {
                return Ok(ParkedProcess { pid: pid.to_string(), request, frozen });
            }
            match page.next_cursor {
                Some(cursor) => query.cursor = Some(cursor),
                None => {
                    return Err(not_parked(
                        pid,
                        "no pending request names it in the voice user's inbox",
                    ));
                }
            }
        }
    }

    fn parked_answers(&self) -> Result<&ParkedAnswers> {
        self.parked.as_ref().ok_or_else(|| {
            AdkError::new(
                adk_core::ErrorComponent::Agent,
                adk_core::ErrorCategory::InvalidInput,
                "harness.voice.no_kernel",
                "the voice surface answers kernel processes only after \
                 VoiceSurface::with_parked_processes",
            )
        })
    }

    fn approver(&self) -> SubjectRef {
        SubjectRef::human(self.scope.user_id.clone())
    }

    fn withdraw(&self, pid: &str) {
        self.offered.lock().unwrap_or_else(|e| e.into_inner()).retain(|offered| offered != pid);
    }

    async fn record_refused(
        &self,
        pid: &str,
        error: &AdkError,
        transcript: Option<&str>,
    ) -> Result<()> {
        let Some(audit) = &self.audit else {
            return Ok(());
        };
        audit
            .record(
                VOICE_AUDIT_REFUSED,
                json!({
                    "pid": pid,
                    "code": error.code,
                    "transcriptDigest": transcript.map(digest),
                }),
            )
            .await
    }
}

/// Whether process `pid` is parked on a held call: `Some(frozen)` while it waits `Waiting`
/// on a held call or after a freeze, `None` otherwise.
#[cfg(feature = "kernel")]
async fn parked_state(parked: &ParkedAnswers, pid: &str) -> Result<Option<bool>> {
    let Some(process) = parked.processes.get(pid).await.map_err(AdkError::from)? else {
        return Ok(None);
    };
    if process.run.state != RunState::Waiting {
        return Ok(None);
    }
    if process.meta.suspended_by == Some(adk_kernel::Suspension::Freeze) {
        return Ok(Some(true));
    }
    let awaiting =
        matches!(process.run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. }));
    Ok((awaiting && process.meta.suspended_by.is_none()).then_some(false))
}

#[cfg(feature = "kernel")]
fn not_parked(pid: &str, why: &str) -> AdkError {
    AdkError::new(
        adk_core::ErrorComponent::Agent,
        adk_core::ErrorCategory::NotFound,
        VOICE_NOT_PARKED_CODE,
        format!("process {pid} is not parked on a call the voice user may decide: {why}"),
    )
}

/// The realtime instruction: the harness prompt, the environment, the workspace's
/// instruction files as the harness renders them (delimited, and only listed before the
/// workspace is trusted, spec 3.1 §7.7), the session's default mode banner, and how to
/// handle a held call by voice.
fn voice_instruction(harness: &HarnessAgent, extra: Option<&str>) -> String {
    let info = harness.info();
    let mode: HarnessMode = info.default_mode;
    let mut instruction = format!(
        "{HARNESS_SYSTEM_PROMPT}\n\n<environment>\nworkspace root: {}\ndefault mode: {mode}\n\
         isolation: {}\npacks: {}\n</environment>\n\n{}{}\n\nYou are speaking with the user \
         over voice. Keep answers short and spoken; never read file contents or long output \
         aloud, summarize them. When a tool call is answered as waiting for a confirmation, tell \
         the user in one sentence what the call would do and ask them to say yes or no, then \
         wait. Never repeat a call that is waiting: when the user answers, the harness runs or \
         refuses that call itself and gives you its result.",
        harness.runtime().workspace().root().display(),
        info.isolation,
        info.packs.join(", "),
        harness.runtime().instructions().render(harness.runtime().workspace_trusted()),
        mode_banner(mode),
    );
    if let Some(extra) = extra {
        instruction.push_str("\n\n");
        instruction.push_str(extra);
    }
    instruction
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Context window compression reaches the provider's session config only when asked for
    /// (spec 3.3 §5.6): a Gemini Live session sending video ends after about two minutes
    /// without it.
    #[tokio::test]
    async fn context_window_compression_reaches_the_provider() {
        use adk_realtime::scripted::ScriptedRealtimeModel;

        let dir = tempfile::tempdir().unwrap();
        let harness = Arc::new(
            HarnessAgent::builder("harness")
                .model(Arc::new(adk_model::ScriptedLlm::new(
                    "scripted",
                    Vec::<adk_model::ScriptedResponse>::new(),
                )))
                .workspace(adk_devtools::Workspace::new(dir.path()))
                .pack(crate::Pack::Voice)
                .build()
                .unwrap(),
        );
        for compressed in [false, true] {
            let model = ScriptedRealtimeModel::new(Vec::new());
            let mut config = VoiceConfig::new("adk-rust");
            if compressed {
                config = config.with_context_window_compression();
            }
            let surface = VoiceSurface::open(
                harness.clone(),
                Arc::new(model.clone()),
                Arc::new(adk_session::InMemorySessionService::new()),
                "ana",
                "voice-1",
                config,
            )
            .unwrap();
            surface.runner().connect().await.unwrap();
            let sent = model.log().configs()[0]
                .extra
                .as_ref()
                .and_then(|extra| extra.get("context_window_compression").cloned());
            assert_eq!(sent, compressed.then_some(serde_json::Value::Bool(true)));
        }
    }

    /// A tool of the harness's own: added with `.tool()` as `calendar`, and listed by a
    /// toolset as `time_now`.
    struct Named(&'static str);

    #[async_trait::async_trait]
    impl Tool for Named {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "A tool of the harness's own."
        }

        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value> {
            Ok(json!({ "ok": true }))
        }
    }

    /// A toolset of the harness's own, added with `.toolset()`.
    struct Clock;

    #[async_trait::async_trait]
    impl adk_core::Toolset for Clock {
        fn name(&self) -> &str {
            "clock"
        }

        async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
            Ok(vec![Arc::new(Named("time_now"))])
        }
    }

    fn voiced(dir: &std::path::Path, mode: HarnessMode) -> Arc<HarnessAgent> {
        Arc::new(
            HarnessAgent::builder("harness")
                .model(Arc::new(adk_model::ScriptedLlm::new(
                    "scripted",
                    Vec::<adk_model::ScriptedResponse>::new(),
                )))
                .workspace(adk_devtools::Workspace::new(dir).allow_bash(true))
                .mode(mode)
                .pack(crate::Pack::Voice)
                .tool(Arc::new(Named("calendar")))
                .toolset(Arc::new(Clock))
                .build()
                .unwrap(),
        )
    }

    /// The realtime session offers every tool the harness's model has: the harness toolset,
    /// `.tool()` and `.toolset()` (spec 3.3 §5.6).
    #[tokio::test]
    async fn the_session_offers_every_tool_the_model_has() {
        use adk_realtime::scripted::ScriptedRealtimeModel;

        let dir = tempfile::tempdir().unwrap();
        let harness = voiced(dir.path(), HarnessMode::Ask);
        let tools = harness.voice_tools("adk-rust", "ana", "voice-1").await;
        let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
        assert_eq!(names, ["calendar", "time_now"]);
        let model = ScriptedRealtimeModel::new(Vec::new());
        let surface = VoiceSurface::open(
            harness,
            Arc::new(model.clone()),
            Arc::new(adk_session::InMemorySessionService::new()),
            "ana",
            "voice-1",
            VoiceConfig::new("adk-rust").with_tools(tools),
        )
        .unwrap();
        surface.runner().connect().await.unwrap();
        let declared: Vec<String> =
            model.log().configs()[0].tools.iter().flatten().map(|tool| tool.name.clone()).collect();
        for name in ["bash", "calendar", "time_now"] {
            assert!(declared.iter().any(|declared| declared == name), "{name}: {declared:?}");
        }
    }

    /// A kill switch given to the session refuses its calls once frozen, as a runner's does
    /// a run's (spec 3.3 S-I10); without it the same call is held, approved by voice and runs.
    #[tokio::test]
    async fn a_frozen_kill_switch_refuses_the_sessions_calls() {
        use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
        use adk_session::{CreateRequest, SessionService};

        for frozen in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let harness = voiced(dir.path(), HarnessMode::Ask);
            let sessions = Arc::new(adk_session::InMemorySessionService::new());
            sessions
                .create(CreateRequest {
                    app_name: "adk-rust".into(),
                    user_id: "ana".into(),
                    session_id: Some("voice-1".into()),
                    state: Default::default(),
                })
                .await
                .unwrap();
            let model = ScriptedRealtimeModel::new(vec![
                ScriptedReply::new(scripted::calls(
                    "call-1",
                    "bash",
                    json!({ "command": "printf ran > ran.txt" }),
                ))
                .on_text(),
                ScriptedReply::new(scripted::spoken("Say yes or no.")).on_tool_output(),
                ScriptedReply::new(vec![scripted::input_transcript("yes")]).on_audio(),
                ScriptedReply::new(scripted::spoken("Done.")).on_tool_output(),
            ])
            .with_idle_timeout(Duration::from_millis(300));
            let stop = GovernanceControl::new();
            if frozen {
                stop.freeze("stop all");
            }
            let surface = VoiceSurface::open(
                harness,
                Arc::new(model.clone()),
                sessions,
                "ana",
                "voice-1",
                VoiceConfig::new("adk-rust").with_governance(stop),
            )
            .unwrap();
            surface.connect().await.unwrap();
            surface.say("run it").await.unwrap();
            let mut held = false;
            while let Some(event) = surface.next_event().await {
                if let Ok(VoiceEvent::Held { .. }) = event {
                    held = true;
                    let audio = adk_realtime::audio::AudioChunk::pcm16_16khz(vec![0; 640]);
                    surface.send_audio(&audio).await.unwrap();
                }
            }
            assert_eq!(held, !frozen, "frozen: {frozen}");
            assert_eq!(dir.path().join("ran.txt").exists(), !frozen, "frozen: {frozen}");
        }
    }

    #[test]
    fn plain_answers_decide_and_anything_else_waits() {
        for yes in ["yes", "Yes!", "yeah sure", "ok go ahead", "approve it", "do it"] {
            assert_eq!(parse_spoken_decision(yes), Some(SpokenDecision::Approve), "{yes}");
        }
        for no in ["no", "No.", "don't", "stop", "cancel that", "deny"] {
            assert_eq!(parse_spoken_decision(no), Some(SpokenDecision::Deny), "{no}");
        }
        for unclear in [
            "",
            "hmm",
            "what does it do",
            "yes no",
            "no wait yes",
            "tell me more about it first please now",
        ] {
            assert_eq!(parse_spoken_decision(unclear), None, "{unclear}");
        }
    }
}
