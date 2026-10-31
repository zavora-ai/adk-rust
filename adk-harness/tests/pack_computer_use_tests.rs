//! The computer-use pack (spec 3.1 §7.24, §5 H10, §9.2 I59, I60, §9.3
//! `harness.computer_use.*`).
//!
//! The runtime is an in-process double of adk-computer-use's `ComputerUseRuntime` that
//! counts every mutation. The pass conditions are on the policy's decisions, the approval
//! store, the runtime's counters and the audit records, never on model prose.

#![cfg(feature = "computer-use")]

use crate::common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPostcondition, ActionPreview, ComputerUseError,
    ComputerUseRuntime, ControlLease, ExecutionCapability, ExecutionMode, ExecutionReceipt,
    LeaseBoundaries, PolicyDecision as RuntimeDecision, ReceiptStatus, ScopeAuthorizer,
    TargetEvidence, VerificationOutcome,
};
use adk_core::{
    ActingContext, PolicyDecision, RunConfig, SubjectKind, SubjectRef, ToolApproval,
    ToolConfirmationRequest, ToolEffect, ToolOrigin, ToolPolicyRequest,
};
use adk_harness::approvals::{OriginPin, RuleScope, StandingDecision, StandingRule};
use adk_harness::packs::computer_use::{
    COMPUTER_USE_AUDIT_EXECUTED, COMPUTER_USE_AUDIT_PREVIEWED, COMPUTER_USE_EXECUTE,
    COMPUTER_USE_OBSERVE, COMPUTER_USE_PREVIEW, COMPUTER_USE_PREVIEW_MISMATCH_CODE,
    COMPUTER_USE_TOOL_NAMES, ComputerUsePack,
};
use adk_harness::{HarnessAgent, HarnessDecision, HarnessMode, Pack, ToolKind};
use adk_model::{ScriptedLlm, ScriptedResponse};
use common::{Fixture, Outcome, fixture, no_user_file, outcome, response_of};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const ACTION_DIGEST: &str = "digest-1";
const POLICY_DIGEST: &str = "policy-1";

/// A desktop runtime that previews one clipboard write and counts what it performs.
struct FakeRuntime {
    blocker: Option<String>,
    observations: AtomicUsize,
    mutations: AtomicUsize,
    receipts: Mutex<HashMap<String, ExecutionReceipt>>,
    last_grant: Mutex<Option<String>>,
}

impl FakeRuntime {
    fn new(blocker: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            blocker: blocker.map(str::to_string),
            observations: AtomicUsize::new(0),
            mutations: AtomicUsize::new(0),
            receipts: Mutex::new(HashMap::new()),
            last_grant: Mutex::new(None),
        })
    }

    fn mutations(&self) -> usize {
        self.mutations.load(Ordering::SeqCst)
    }

    fn preview(&self) -> ActionPreview {
        ActionPreview {
            envelope: ActionEnvelope {
                action_id: "action-1".into(),
                session_id: "session-1".into(),
                execution_group_id: Some("group-1".into()),
                principal_id: "principal-1".into(),
                agent_id: Some("executor".into()),
                tool: "write_clipboard".into(),
                operation: "write_clipboard".into(),
                action_class: ActionClass::EditReversible,
                requested_mode: ExecutionMode::Background,
                target: Some(TargetEvidence {
                    platform: "test".into(),
                    app_id: "app.reference".into(),
                    pid: None,
                    window_id: None,
                    window_title_digest: None,
                    display_id: None,
                    role: None,
                    label_digest: None,
                    bounds: None,
                    observation_id: "observation-1".into(),
                    screenshot_hash: None,
                    ui_tree_revision: None,
                    confidence: 1.0,
                    captured_at: "2026-07-13T10:00:00Z".into(),
                }),
                target_sensitivity: None,
                resource: None,
                provenance: None,
                data_labels: vec!["private".into()],
                postcondition: None,
                reversible: true,
                external_side_effect: false,
                proposed_at: "2026-07-13T10:00:00Z".into(),
                expires_at: "2099-01-01T00:01:00Z".into(),
                args_digest: ACTION_DIGEST.into(),
            },
            executable: self.blocker.is_none(),
            blocker: self.blocker.clone(),
            policy: RuntimeDecision {
                decision: if self.blocker.is_some() { "confirm" } else { "allow" }.into(),
                policy_digest: POLICY_DIGEST.into(),
                reasons: vec!["test".into()],
                grant_id: None,
            },
            capability: ExecutionCapability {
                app_id: "*".into(),
                operation: "write_clipboard".into(),
                backend: "none".into(),
                supported_modes: vec![ExecutionMode::Shadow, ExecutionMode::Background],
                interference: "none".into(),
                confidence: 1.0,
                verified_at: None,
                verification_source: "platform_rule".into(),
            },
        }
    }
}

#[async_trait::async_trait]
impl ComputerUseRuntime for FakeRuntime {
    async fn discover_capabilities(&self) -> Result<Value, ComputerUseError> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "kind": "capability" }))
    }
    async fn observe_visual(&self) -> Result<Value, ComputerUseError> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "kind": "visual" }))
    }
    async fn observe_semantic(&self) -> Result<Value, ComputerUseError> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "kind": "semantic" }))
    }
    async fn preview_action(&self, _: Value) -> Result<ActionPreview, ComputerUseError> {
        Ok(self.preview())
    }
    async fn acquire_lease(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<ControlLease, ComputerUseError> {
        Ok(ControlLease {
            lease_id: "lease-1".into(),
            revision: 1,
            session_id: envelope.session_id.clone(),
            principal_id: envelope.principal_id.clone(),
            agent_id: envelope.agent_id.clone(),
            kind: "cooperative".into(),
            execution_mode: envelope.requested_mode,
            state: "active".into(),
            acquired_at: None,
            expires_at: "2099-01-01T00:01:00Z".into(),
            action_budget: 1,
            actions_used: 0,
            boundaries: LeaseBoundaries::default(),
        })
    }
    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        _: &ControlLease,
        grant: Option<&str>,
    ) -> Result<ExecutionReceipt, ComputerUseError> {
        *self.last_grant.lock().await = grant.map(str::to_string);
        let mut receipts = self.receipts.lock().await;
        if let Some(receipt) = receipts.get(&envelope.action_id) {
            return Ok(receipt.clone());
        }
        self.mutations.fetch_add(1, Ordering::SeqCst);
        let receipt = ExecutionReceipt {
            receipt_id: "receipt-1".into(),
            session_id: envelope.session_id.clone(),
            action_id: envelope.action_id.clone(),
            action_digest: envelope.args_digest.clone(),
            attempt: 1,
            status: ReceiptStatus::Committed,
            created_at: None,
            updated_at: None,
            result: Some(json!({ "ok": true })),
            error: None,
        };
        receipts.insert(envelope.action_id.clone(), receipt.clone());
        Ok(receipt)
    }
    async fn verify(
        &self,
        receipt: &ExecutionReceipt,
        postcondition: Option<&ActionPostcondition>,
    ) -> Result<VerificationOutcome, ComputerUseError> {
        if receipt.status != ReceiptStatus::Committed {
            return Ok(VerificationOutcome::Failed { reason: "not committed".into() });
        }
        Ok(match postcondition {
            Some(_) => VerificationOutcome::Verified,
            None => VerificationOutcome::CommittedUnverified {
                reason: "no postcondition declared".into(),
            },
        })
    }
}

fn pack(runtime: &Arc<FakeRuntime>) -> ComputerUsePack {
    ComputerUsePack::new(
        runtime.clone(),
        ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]),
    )
}

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn proposed() -> Value {
    json!({ "action": { "tool": "write_clipboard", "operation": "write_clipboard", "text": "hello" } })
}

/// The summary `computer_use_preview` returns for the fake's action.
fn summary() -> Value {
    json!({
        "tool": "write_clipboard",
        "operation": "write_clipboard",
        "actionClass": "edit_reversible",
        "requestedMode": "background",
        "targetApp": "app.reference",
        "reversible": true,
        "externalSideEffect": false,
    })
}

fn execute_args(action_digest: &str, policy_digest: &str, action: Value) -> Value {
    json!({ "actionDigest": action_digest, "policyDigest": policy_digest, "action": action })
}

fn request(tool: &str, args: Value) -> ToolPolicyRequest {
    ToolPolicyRequest::new(tool, args)
        .with_effect(ToolEffect::NonIdempotent)
        .with_agent_name("harness")
        .with_identity(common::APP, "alice", "s1")
        .with_invocation_id("inv-1")
        .with_acting_context(ActingContext::derived(
            common::APP,
            "harness",
            "alice",
            SubjectKind::Human,
        ))
}

fn static_origin() -> ToolOrigin {
    ToolOrigin::new("static", &json!({ "name": "x" }), None)
}

fn records(fixture: &Fixture, action: &str) -> Vec<Value> {
    fixture
        .records(action)
        .into_iter()
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

// --- Classes and the mode table ---------------------------------------------------------

#[tokio::test]
async fn the_three_tools_are_decided_as_the_table_says_in_every_mode() {
    for mode in HarnessMode::ALL {
        let runtime = FakeRuntime::new(Some("approval_required"));
        let fixture = fixture(Vec::new(), |b| b.mode(mode).pack_computer_use(pack(&runtime))).await;
        let policy = fixture.harness.agent().runtime().policy();
        for tool in [COMPUTER_USE_OBSERVE, COMPUTER_USE_PREVIEW] {
            assert_eq!(
                policy.classify(&request(tool, json!({})), Some(&static_origin())),
                ToolKind::Read,
                "{tool}"
            );
            assert_eq!(
                policy.decide(&request(tool, json!({})), Some(&static_origin()), Some("c")).await,
                PolicyDecision::Allow,
                "{tool} in {mode}"
            );
        }
        let args = execute_args(ACTION_DIGEST, POLICY_DIGEST, summary());
        assert_eq!(
            policy.classify(&request(COMPUTER_USE_EXECUTE, args.clone()), Some(&static_origin())),
            ToolKind::ComputerUse
        );
        let decision = policy
            .decide(&request(COMPUTER_USE_EXECUTE, args), Some(&static_origin()), Some("c"))
            .await;
        match mode {
            HarnessMode::ReadOnly | HarnessMode::Plan => {
                assert!(matches!(decision, PolicyDecision::Deny { .. }), "{mode}: {decision:?}");
            }
            HarnessMode::Ask | HarnessMode::Auto => match &decision {
                PolicyDecision::RequireApproval { reason } => {
                    assert!(reason.contains("one-time approval"), "{mode}: {reason}");
                    assert!(reason.contains("a desktop action"), "{mode}: {reason}");
                }
                other => panic!("{mode}: {other:?}"),
            },
            _ => unreachable!(),
        }
        let info = fixture.harness.agent().info();
        assert_eq!(info.packs, vec!["computer-use".to_string()]);
        for tool in COMPUTER_USE_TOOL_NAMES {
            assert!(
                info.tools.contains(&(*tool).to_string()),
                "{tool} missing from {:?}",
                info.tools
            );
        }
    }
}

#[test]
fn pack_computer_use_alone_is_refused_at_build() {
    let dir = tempfile::tempdir().unwrap();
    let error = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .pack(Pack::ComputerUse)
        .build()
        .unwrap_err();
    assert!(error.to_string().contains("pack_computer_use"), "{error}");
}

#[tokio::test]
async fn without_the_pack_the_names_are_external() {
    let fixture = fixture(Vec::new(), |b| b).await;
    let policy = fixture.harness.agent().runtime().policy();
    let kind = policy.classify(&request(COMPUTER_USE_EXECUTE, json!({})), Some(&static_origin()));
    assert!(matches!(kind, ToolKind::External { .. }), "{kind:?}");
    assert!(fixture.harness.agent().info().packs.is_empty());
}

// --- Never waivable (spec 3.1 §9.2 I59) --------------------------------------------------

#[tokio::test]
async fn a_standing_rule_or_a_run_wide_approval_never_covers_a_desktop_action() {
    let runtime = FakeRuntime::new(Some("approval_required"));
    let fixture = fixture(Vec::new(), |b| b.pack_computer_use(pack(&runtime))).await;
    let rules = fixture.harness.standing_rules().clone();
    rules
        .grant(StandingRule {
            id: "rule-desktop".into(),
            scope: RuleScope::User {
                app_name: common::APP.to_string(),
                user_id: "alice".to_string(),
                subject_kind: SubjectKind::Human,
            },
            tool: COMPUTER_USE_EXECUTE.to_string(),
            origin: OriginPin::AnyDeclaration { source: "static".to_string() },
            agent: None,
            predicates: Vec::new(),
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human("alice"),
            created_at: chrono::Utc::now(),
            expires_at: None,
            label: "always allow desktop actions".into(),
            source_fingerprint: None,
        })
        .await
        .unwrap();
    assert!(!ToolKind::ComputerUse.rule_eligible());
    let policy = fixture.harness.agent().runtime().policy();
    let args = execute_args(ACTION_DIGEST, POLICY_DIGEST, summary());
    let decision = policy
        .decide(&request(COMPUTER_USE_EXECUTE, args.clone()), Some(&static_origin()), Some("c"))
        .await;
    assert!(
        matches!(&decision, PolicyDecision::RequireApproval { reason } if reason.contains("no standing rule applies")),
        "{decision:?}"
    );
    let mut config = RunConfig::default();
    let fingerprint = ToolConfirmationRequest::new(COMPUTER_USE_EXECUTE, args).fingerprint();
    config.tool_approvals.insert(fingerprint.clone(), ToolApproval::approve());
    fixture.harness.agent().runtime().layer().apply(&mut config);
    assert!(!config.tool_approvals.contains_key(&fingerprint));
}

#[tokio::test]
async fn in_auto_a_desktop_action_is_held_and_in_plan_denied() {
    let runtime = FakeRuntime::new(Some("approval_required"));
    let auto = fixture(
        vec![call(COMPUTER_USE_EXECUTE, execute_args(ACTION_DIGEST, POLICY_DIGEST, summary()))],
        |b| b.mode(HarnessMode::Auto).pack_computer_use(pack(&runtime)),
    )
    .await;
    let events = auto.send("Write hello to the clipboard.").await;
    assert_eq!(outcome(&response_of(&events, COMPUTER_USE_EXECUTE)), Outcome::Held);
    assert_eq!(runtime.mutations(), 0);
    assert_eq!(auto.harness.pending_approvals(&auto.user, &auto.session).await.unwrap().len(), 1);

    let runtime = FakeRuntime::new(Some("approval_required"));
    let plan = fixture(
        vec![
            call(COMPUTER_USE_EXECUTE, execute_args(ACTION_DIGEST, POLICY_DIGEST, summary())),
            done(),
        ],
        |b| b.mode(HarnessMode::Plan).pack_computer_use(pack(&runtime)),
    )
    .await;
    let events = plan.send("Write hello to the clipboard.").await;
    assert_eq!(outcome(&response_of(&events, COMPUTER_USE_EXECUTE)), Outcome::Denied);
    assert_eq!(runtime.mutations(), 0);
    assert!(records(&plan, COMPUTER_USE_AUDIT_EXECUTED).is_empty(), "the tool never ran");
}

// --- H10: one desktop action, after a digest-bound approval -------------------------------

#[tokio::test]
async fn h10_a_desktop_action_runs_once_after_the_digest_bound_approval() {
    let runtime = FakeRuntime::new(Some("approval_required"));
    let fixture = fixture(
        vec![call(COMPUTER_USE_OBSERVE, json!({})), call(COMPUTER_USE_PREVIEW, proposed()), done()],
        |b| b.pack_computer_use(pack(&runtime)),
    )
    .await;
    let (user, session) = (fixture.user.clone(), fixture.session.clone());

    // Run 1: observe and preview run; the model learns the digests.
    let events = fixture.send("Write hello to the clipboard.").await;
    let observed = response_of(&events, COMPUTER_USE_OBSERVE);
    assert_eq!(observed["visual"]["kind"], "visual", "{observed}");
    let preview = response_of(&events, COMPUTER_USE_PREVIEW);
    assert_eq!(preview["actionDigest"], ACTION_DIGEST, "{preview}");
    assert_eq!(preview["policyDigest"], POLICY_DIGEST);
    assert_eq!(preview["route"], "approval");
    assert_eq!(preview["action"], summary());
    assert_eq!(runtime.mutations(), 0);
    let previewed = records(&fixture, COMPUTER_USE_AUDIT_PREVIEWED);
    assert_eq!(previewed.len(), 1, "{previewed:?}");
    assert_eq!(previewed[0]["route"], "approval");
    assert_eq!(previewed[0]["actionClass"], "edit_reversible");

    // Run 2: the execution is held; nothing moves; the inbox lists it with the digests.
    fixture.model.push(call(
        COMPUTER_USE_EXECUTE,
        execute_args(ACTION_DIGEST, POLICY_DIGEST, preview["action"].clone()),
    ));
    fixture.model.push(done());
    let events = fixture.send("Go ahead.").await;
    assert_eq!(outcome(&response_of(&events, COMPUTER_USE_EXECUTE)), Outcome::Held);
    assert_eq!(runtime.mutations(), 0);
    let pending = fixture.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.tool_name, COMPUTER_USE_EXECUTE);
    assert_eq!(pending[0].request.args["actionDigest"], ACTION_DIGEST);

    // The person approves; the graph's interrupt is answered by that decision; one mutation.
    fixture
        .harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    let events = fixture.resume().await;
    let executed = response_of(&events, COMPUTER_USE_EXECUTE);
    assert_eq!(outcome(&executed), Outcome::Executed, "{executed}");
    assert_eq!(executed["receiptId"], "receipt-1");
    assert_eq!(executed["committed"], true);
    assert_eq!(executed["verified"], false, "no postcondition was declared");
    assert_eq!(executed["status"], "committed_unverified");
    assert_eq!(runtime.mutations(), 1);
    assert_eq!(
        *runtime.last_grant.lock().await,
        None,
        "the harness decision is the authority; no bearer passed"
    );
    let records_executed = records(&fixture, COMPUTER_USE_AUDIT_EXECUTED);
    assert_eq!(records_executed.len(), 1, "{records_executed:?}");
    assert_eq!(records_executed[0]["receiptId"], "receipt-1");
    assert_eq!(records_executed[0]["actionDigest"], ACTION_DIGEST);
    assert_eq!(records_executed[0]["committed"], true);

    // Run 3: nothing runs again; the approval and the preview are spent.
    fixture.model.push(done());
    fixture.send("Thanks.").await;
    assert_eq!(runtime.mutations(), 1);
    assert!(fixture.harness.pending_approvals(&user, &session).await.unwrap().is_empty());
    assert!(common::last_system_text(&fixture.model).contains("packs: computer-use"));
}

#[tokio::test]
async fn an_allowed_route_still_needs_the_harness_approval_and_runs_once() {
    let runtime = FakeRuntime::new(None);
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let fixture = fixture(
        vec![
            call(COMPUTER_USE_PREVIEW, proposed()),
            call(COMPUTER_USE_EXECUTE, execute_args(ACTION_DIGEST, POLICY_DIGEST, summary())),
            done(),
        ],
        |b| b.pack_computer_use(pack(&runtime)).prompter(prompter.clone()),
    )
    .await;
    let events = fixture.send("Write hello to the clipboard.").await;
    assert_eq!(response_of(&events, COMPUTER_USE_PREVIEW)["route"], "allowed");
    let executed = response_of(&events, COMPUTER_USE_EXECUTE);
    assert_eq!(outcome(&executed), Outcome::Executed, "{executed}");
    assert_eq!(prompter.count(), 1, "the person approved the desktop action once");
    assert_eq!(runtime.mutations(), 1);
}

#[tokio::test]
async fn a_changed_digest_runs_nothing_even_after_approval() {
    let runtime = FakeRuntime::new(Some("approval_required"));
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let fixture = fixture(
        vec![
            call(COMPUTER_USE_PREVIEW, proposed()),
            call(COMPUTER_USE_EXECUTE, execute_args("digest-other", POLICY_DIGEST, summary())),
            done(),
        ],
        |b| b.pack_computer_use(pack(&runtime)).prompter(prompter.clone()),
    )
    .await;
    let events = fixture.send("Write hello to the clipboard.").await;
    let response = response_of(&events, COMPUTER_USE_EXECUTE);
    assert!(
        response["error"].as_str().unwrap_or_default().contains(COMPUTER_USE_PREVIEW_MISMATCH_CODE),
        "{response}"
    );
    assert_eq!(prompter.count(), 1);
    assert_eq!(runtime.mutations(), 0);
    let executed = records(&fixture, COMPUTER_USE_AUDIT_EXECUTED);
    assert_eq!(executed.len(), 1, "{executed:?}");
    assert_eq!(executed[0]["refusal"], "previewMismatch");
    assert_eq!(executed[0]["actionDigest"], "digest-other");
}

#[tokio::test]
async fn a_misdescribed_action_runs_nothing_even_after_approval() {
    let runtime = FakeRuntime::new(Some("approval_required"));
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let mut lie = summary();
    lie["operation"] = json!("read_clipboard");
    let fixture = fixture(
        vec![
            call(COMPUTER_USE_PREVIEW, proposed()),
            call(COMPUTER_USE_EXECUTE, execute_args(ACTION_DIGEST, POLICY_DIGEST, lie)),
            done(),
        ],
        |b| b.pack_computer_use(pack(&runtime)).prompter(prompter.clone()),
    )
    .await;
    let events = fixture.send("Write hello to the clipboard.").await;
    let response = response_of(&events, COMPUTER_USE_EXECUTE);
    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains(COMPUTER_USE_PREVIEW_MISMATCH_CODE)
            && error.contains("differs from the preview"),
        "{response}"
    );
    assert_eq!(runtime.mutations(), 0);
}

#[tokio::test]
async fn an_execution_without_a_kept_preview_runs_nothing() {
    let runtime = FakeRuntime::new(None);
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let fixture = fixture(
        vec![
            call(COMPUTER_USE_EXECUTE, execute_args(ACTION_DIGEST, POLICY_DIGEST, summary())),
            done(),
        ],
        |b| b.pack_computer_use(pack(&runtime)).prompter(prompter.clone()),
    )
    .await;
    let events = fixture.send("Write hello to the clipboard.").await;
    let response = response_of(&events, COMPUTER_USE_EXECUTE);
    assert!(
        response["error"].as_str().unwrap_or_default().contains("no preview is kept"),
        "{response}"
    );
    assert_eq!(runtime.mutations(), 0);
}

#[tokio::test]
async fn a_member_never_gets_a_desktop_tool() {
    let runtime = FakeRuntime::new(None);
    let members = Arc::new(ScriptedLlm::new("member", vec![ScriptedResponse::text("looked")]));
    let member_model = members.clone();
    let fixture = fixture(
        vec![call("general", json!({ "request": "look at the screen" })), done()],
        move |b| b.pack_computer_use(pack(&runtime)).member_model(member_model),
    )
    .await;
    fixture.send("Have general look at the screen.").await;
    let tools: Vec<String> = members.requests()[0].tools.keys().cloned().collect();
    for tool in COMPUTER_USE_TOOL_NAMES {
        assert!(!tools.iter().any(|t| t == tool), "{tool} offered to a member: {tools:?}");
    }
    assert!(tools.iter().any(|tool| tool == "write_file"), "{tools:?}");
}
