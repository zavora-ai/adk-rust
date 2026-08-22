//! A tool guardrail must stop a call before the tool runs, and be able to narrow its arguments.
//!
//! `Guardrail` validates `Content` and never sees a tool call, and `ToolConfirmationPolicy`
//! decides per tool *name*. Neither can express "this tool may run, but not with these
//! arguments", so argument-level policy had nowhere to live inside the framework.
//!
//! These tests assert the property that matters for a destructive tool: when a guardrail denies,
//! the tool's `execute` is never entered.

#![cfg(feature = "guardrails")]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{
    DeniedArgumentPattern, PathAllowList, Severity, ToolGuardrail, ToolGuardrailResult,
    ToolGuardrailSet,
};
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State, Tool, ToolConfirmationDecision,
    ToolContext, tool_call_fingerprint,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

// --- Harness ---

/// Emits one function call, then plain text.
///
/// A model that returns the same function call on every turn makes the agent loop to its
/// iteration cap, so the tool would run 100 times and an exact call count could not be asserted.
struct MockModel {
    call: LlmResponse,
    done: LlmResponse,
    turns: AtomicUsize,
}

fn response(content: Content) -> LlmResponse {
    LlmResponse {
        content: Some(content),
        usage_metadata: None,
        finish_reason: Some(FinishReason::Stop),
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
    }
}

impl MockModel {
    fn calling(name: &str, args: Value) -> Self {
        Self {
            call: response(Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: name.to_string(),
                    args,
                    id: Some(format!("call_{name}")),
                    thought_signature: None,
                }],
            }),
            done: response(Content::new("model").with_text("done")),
            turns: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Llm for MockModel {
    fn name(&self) -> &str {
        "mock-model"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let first = self.turns.fetch_add(1, Ordering::SeqCst) == 0;
        let response = if first { self.call.clone() } else { self.done.clone() };
        Ok(Box::pin(async_stream::stream! { yield Ok(response); }))
    }
}

/// Records whether it ran and with which arguments, so a denial can be proven rather than assumed.
struct RecordingTool {
    name: &'static str,
    runs: Arc<AtomicUsize>,
    last_args: Arc<std::sync::Mutex<Option<Value>>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "records that it ran"
    }
    fn parameters_schema(&self) -> Option<Value> {
        None
    }
    fn response_schema(&self) -> Option<Value> {
        None
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        *self.last_args.lock().expect("lock") = Some(args);
        Ok(json!({ "status": "ok" }))
    }
}

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
    run_config: RunConfig,
}

impl MockContext {
    fn new(run_config: RunConfig) -> Self {
        Self {
            session: MockSession,
            user_content: Content::new("user").with_text("start"),
            run_config,
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

/// Runs an agent that calls `tool_name` with `args` under `guardrails`.
///
/// Returns how many times the tool executed, the arguments it saw, and every event text.
async fn run_with_guardrails(
    tool_name: &'static str,
    args: Value,
    guardrails: ToolGuardrailSet,
) -> (usize, Option<Value>, String) {
    run_with_options(tool_name, args, guardrails, false, RunConfig::default()).await
}

async fn run_with_options(
    tool_name: &'static str,
    args: Value,
    guardrails: ToolGuardrailSet,
    require_confirmation: bool,
    run_config: RunConfig,
) -> (usize, Option<Value>, String) {
    let runs = Arc::new(AtomicUsize::new(0));
    let last_args = Arc::new(std::sync::Mutex::new(None));
    let tool = Arc::new(RecordingTool {
        name: tool_name,
        runs: Arc::clone(&runs),
        last_args: Arc::clone(&last_args),
    });

    let mut builder = LlmAgentBuilder::new("test-agent")
        .model(Arc::new(MockModel::calling(tool_name, args.clone())))
        .tool(tool)
        .tool_guardrails(guardrails);
    if require_confirmation {
        builder = builder.require_tool_confirmation(tool_name);
    }
    let agent = builder.build().expect("agent builds");

    let mut stream = agent.run(Arc::new(MockContext::new(run_config))).await.expect("run starts");
    let mut transcript = String::new();
    while let Some(event) = stream.next().await {
        if let Ok(event) = event {
            transcript.push_str(&serde_json::to_string(&event).unwrap_or_default());
        }
    }

    let seen = last_args.lock().expect("lock").clone();
    (runs.load(Ordering::SeqCst), seen, transcript)
}

/// Denies every call, to prove the tool is never entered.
struct DenyEverything;

#[async_trait]
impl ToolGuardrail for DenyEverything {
    fn name(&self) -> &str {
        "deny-everything"
    }
    async fn validate_call(&self, _tool: &str, _args: &Value) -> ToolGuardrailResult {
        ToolGuardrailResult::deny("refused for the test", Severity::Critical)
    }
}

// --- Tests ---

#[tokio::test]
async fn a_denied_call_never_reaches_the_tool() {
    let (runs, seen, transcript) = run_with_guardrails(
        "delete_everything",
        json!({ "path": "/" }),
        ToolGuardrailSet::new().with(DenyEverything),
    )
    .await;

    assert_eq!(runs, 0, "a denied tool must not execute");
    assert!(seen.is_none(), "the tool must not observe the arguments either");
    assert!(
        transcript.contains("deny-everything"),
        "the denial should name the guardrail so the model can adjust: {transcript}"
    );
}

#[tokio::test]
async fn a_denied_call_does_not_request_confirmation() {
    let (runs, _, transcript) = run_with_options(
        "delete_everything",
        json!({ "path": "/" }),
        ToolGuardrailSet::new().with(DenyEverything),
        true,
        RunConfig::default(),
    )
    .await;

    assert_eq!(runs, 0);
    assert!(transcript.contains("deny-everything"), "got {transcript}");
    assert!(!transcript.contains("confirmation required"), "got {transcript}");
}

#[tokio::test]
async fn a_revision_is_fingerprinted_and_confirmed_after_screening() {
    struct ForceDryRun;

    #[async_trait]
    impl ToolGuardrail for ForceDryRun {
        fn name(&self) -> &str {
            "force-dry-run"
        }
        async fn validate_call(&self, _tool: &str, args: &Value) -> ToolGuardrailResult {
            let mut revised = args.clone();
            revised
                .as_object_mut()
                .expect("object arguments")
                .insert("dry_run".to_string(), json!(true));
            ToolGuardrailResult::revise(revised, "dry-run is mandatory")
        }
    }

    let call_id = "call_prune_cache".to_string();
    let revised = json!({ "path": "/var/cache", "dry_run": true });
    let run_config = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(
            call_id.clone(),
            ToolConfirmationDecision::Approve,
        )]))
        .tool_confirmation_fingerprints(HashMap::from([(
            call_id,
            tool_call_fingerprint("prune_cache", &revised),
        )]))
        .build();

    let (runs, seen, transcript) = run_with_options(
        "prune_cache",
        json!({ "path": "/var/cache" }),
        ToolGuardrailSet::new().with(ForceDryRun),
        true,
        run_config,
    )
    .await;

    assert_eq!(runs, 1, "revised approved call should execute: {transcript}");
    assert_eq!(seen, Some(revised));
}

#[tokio::test]
async fn an_allowed_call_still_reaches_the_tool() {
    let (runs, seen, _) =
        run_with_guardrails("read_file", json!({ "path": "/tmp/ok" }), ToolGuardrailSet::new())
            .await;

    assert_eq!(runs, 1, "an empty guardrail set must not change behaviour");
    assert_eq!(seen, Some(json!({ "path": "/tmp/ok" })));
}

#[tokio::test]
async fn a_revision_is_what_the_tool_actually_receives() {
    /// Forces a dry run, the narrowing case a guardrail exists for.
    struct ForceDryRun;

    #[async_trait]
    impl ToolGuardrail for ForceDryRun {
        fn name(&self) -> &str {
            "force-dry-run"
        }
        async fn validate_call(&self, _tool: &str, args: &Value) -> ToolGuardrailResult {
            let mut revised = args.clone();
            if let Some(object) = revised.as_object_mut() {
                object.insert("dry_run".to_string(), json!(true));
            }
            ToolGuardrailResult::revise(revised, "dry-run is mandatory")
        }
    }

    let (runs, seen, _) = run_with_guardrails(
        "prune_cache",
        json!({ "path": "/var/cache" }),
        ToolGuardrailSet::new().with(ForceDryRun),
    )
    .await;

    assert_eq!(runs, 1, "a revision allows the call");
    assert_eq!(
        seen,
        Some(json!({ "path": "/var/cache", "dry_run": true })),
        "the tool must receive the revised arguments, not the originals"
    );
}

#[tokio::test]
async fn a_path_outside_the_allow_list_is_blocked() {
    let guardrails = ToolGuardrailSet::new().with(
        PathAllowList::new("agents-only", ["path"], ["/Users/me/Library/LaunchAgents"])
            .on_tools(["plist_write"]),
    );

    let (runs, _, transcript) = run_with_guardrails(
        "plist_write",
        json!({ "path": "/Library/LaunchDaemons/root.plist" }),
        guardrails,
    )
    .await;

    assert_eq!(runs, 0, "writing outside the allowed root must be refused");
    assert!(transcript.contains("agents-only"), "got {transcript}");
}

#[tokio::test]
async fn a_path_inside_the_allow_list_is_permitted() {
    let root = tempfile::tempdir().expect("allowed root");
    let guardrails = ToolGuardrailSet::new()
        .with(PathAllowList::new("agents-only", ["path"], [root.path()]).on_tools(["plist_write"]));

    let (runs, _, _) = run_with_guardrails(
        "plist_write",
        json!({ "path": root.path().join("ai.zavora.sysadmin.plist") }),
        guardrails,
    )
    .await;

    assert_eq!(runs, 1, "the allowed root must remain usable");
}

#[tokio::test]
async fn a_guardrail_scoped_to_other_tools_does_not_interfere() {
    let guardrails = ToolGuardrailSet::new().with(
        DeniedArgumentPattern::new("no-recursive", r"-rf\b", Severity::Critical)
            .expect("valid pattern")
            .on_tools(["run_command"]),
    );

    let (runs, _, _) =
        run_with_guardrails("read_file", json!({ "flags": "-rf" }), guardrails).await;

    assert_eq!(runs, 1, "a guardrail scoped to run_command must not gate read_file");
}

#[tokio::test]
async fn a_denied_argument_pattern_blocks_the_matching_tool() {
    let guardrails = ToolGuardrailSet::new().with(
        DeniedArgumentPattern::new("no-recursive", r"-rf\b", Severity::Critical)
            .expect("valid pattern")
            .on_tools(["run_command"]),
    );

    let (runs, _, transcript) =
        run_with_guardrails("run_command", json!({ "cmd": "rm -rf /" }), guardrails).await;

    assert_eq!(runs, 0);
    assert!(transcript.contains("no-recursive"), "got {transcript}");
}
