use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPreview, CancellationBridge, ComputerUseRuntime,
    ControlLease, ExecutionCapability, ExecutionMode, ExecutionReceipt, LeaseBoundaries,
    PolicyDecision, ReceiptStatus, ScopeAuthorizer, TargetReservation, TargetReservationScope,
    build_reference_graph, build_reference_graph_with_checkpointer,
};
use adk_graph::{ExecutionConfig, GraphError, MemoryCheckpointer, State};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

struct FakeRuntime {
    blocker: Option<String>,
    physical_mutations: AtomicUsize,
    observation_concurrency: AtomicUsize,
    max_observation_concurrency: AtomicUsize,
    fail_after_first_commit: AtomicBool,
    fail_before_first_effect: AtomicBool,
    receipts: Mutex<HashMap<String, ExecutionReceipt>>,
    last_approval_grant: Mutex<Option<String>>,
    cancellation_log: Mutex<Vec<String>>,
    reservations: AtomicUsize,
    releases: AtomicUsize,
}

impl FakeRuntime {
    fn new(blocker: Option<&str>) -> Self {
        Self {
            blocker: blocker.map(str::to_string),
            physical_mutations: AtomicUsize::new(0),
            observation_concurrency: AtomicUsize::new(0),
            max_observation_concurrency: AtomicUsize::new(0),
            fail_after_first_commit: AtomicBool::new(false),
            fail_before_first_effect: AtomicBool::new(false),
            receipts: Mutex::new(HashMap::new()),
            last_approval_grant: Mutex::new(None),
            cancellation_log: Mutex::new(Vec::new()),
            reservations: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
        }
    }

    async fn observe(&self, kind: &str) -> Value {
        let active = self.observation_concurrency.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_observation_concurrency.fetch_max(active, Ordering::SeqCst);
        sleep(Duration::from_millis(15)).await;
        self.observation_concurrency.fetch_sub(1, Ordering::SeqCst);
        json!({ "kind": kind })
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
                target: None,
                resource: None,
                provenance: None,
                data_labels: vec!["private".into()],
                postcondition: None,
                reversible: true,
                external_side_effect: false,
                proposed_at: "2026-07-13T10:00:00Z".into(),
                expires_at: "2026-07-13T10:01:00Z".into(),
                args_digest: "digest-1".into(),
            },
            executable: self.blocker.is_none(),
            blocker: self.blocker.clone(),
            policy: PolicyDecision {
                decision: if self.blocker.is_some() { "confirm" } else { "allow" }.into(),
                policy_digest: "policy-1".into(),
                reasons: vec!["test".into()],
                grant_id: None,
            },
            capability: ExecutionCapability {
                app_id: "*".into(),
                operation: "write_clipboard".into(),
                backend: "none".into(),
                supported_modes: vec![
                    ExecutionMode::Shadow,
                    ExecutionMode::Background,
                    ExecutionMode::Foreground,
                ],
                interference: "none".into(),
                confidence: 1.0,
                verified_at: None,
                verification_source: "platform_rule".into(),
            },
        }
    }
}

#[async_trait]
impl ComputerUseRuntime for FakeRuntime {
    async fn discover_capabilities(&self) -> Result<Value, String> {
        Ok(self.observe("capability").await)
    }

    async fn observe_visual(&self) -> Result<Value, String> {
        Ok(self.observe("visual").await)
    }

    async fn observe_semantic(&self) -> Result<Value, String> {
        Ok(self.observe("semantic").await)
    }

    async fn preview_action(&self, _proposed_action: Value) -> Result<ActionPreview, String> {
        Ok(self.preview())
    }

    async fn acquire_lease(&self, envelope: &ActionEnvelope) -> Result<ControlLease, String> {
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
            expires_at: "2026-07-13T10:01:00Z".into(),
            action_budget: 1,
            actions_used: 0,
            boundaries: LeaseBoundaries::default(),
        })
    }

    async fn reserve_target(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<Option<TargetReservation>, String> {
        self.reservations.fetch_add(1, Ordering::SeqCst);
        Ok(Some(TargetReservation {
            reservation_id: "reservation-1".into(),
            revision: 1,
            intent_id: envelope.action_id.clone(),
            session_id: envelope.session_id.clone(),
            principal_id: envelope.principal_id.clone(),
            execution_group_id: envelope.execution_group_id.clone(),
            agent_id: envelope.agent_id.clone(),
            scope: TargetReservationScope { app_id: "app.reference".into(), window_id: None },
            state: "active".into(),
            acquired_at: "2026-07-13T10:00:00Z".into(),
            expires_at: "2026-07-13T10:01:00Z".into(),
            terminal_reason: None,
        }))
    }

    async fn release_target(&self, _reservation: &TargetReservation) -> Result<(), String> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        _lease: &ControlLease,
        approval_grant_id: Option<&str>,
    ) -> Result<ExecutionReceipt, String> {
        *self.last_approval_grant.lock().await = approval_grant_id.map(str::to_string);
        let mut receipts = self.receipts.lock().await;
        if let Some(receipt) = receipts.get(&envelope.action_id) {
            return Ok(receipt.clone());
        }
        if self.fail_before_first_effect.swap(false, Ordering::SeqCst) {
            return Err("injected crash before v8 effect".into());
        }
        self.physical_mutations.fetch_add(1, Ordering::SeqCst);
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
        if self.fail_after_first_commit.swap(false, Ordering::SeqCst) {
            return Err("injected crash after v8 commit".into());
        }
        Ok(receipt)
    }

    async fn verify(&self, receipt: &ExecutionReceipt) -> Result<bool, String> {
        Ok(receipt.status == ReceiptStatus::Committed)
    }

    async fn pause_session(&self, session_id: &str, reason: &str) -> Result<(), String> {
        self.cancellation_log.lock().await.push(format!("pause:{session_id}:{reason}"));
        Ok(())
    }

    async fn stop_session(&self, session_id: &str, reason: &str) -> Result<(), String> {
        self.cancellation_log.lock().await.push(format!("stop:{session_id}:{reason}"));
        Ok(())
    }

    async fn emergency_stop(&self, reason: &str) -> Result<(), String> {
        self.cancellation_log.lock().await.push(format!("emergency:{reason}"));
        Ok(())
    }
}

fn input() -> State {
    let mut state = State::new();
    state.insert("proposed_action".into(), json!({ "tool": "write_clipboard" }));
    state
}

fn authorizer() -> Arc<ScopeAuthorizer> {
    Arc::new(ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]))
}

#[tokio::test]
async fn graph_parallelizes_observation_and_has_one_executor_effect() {
    let runtime = Arc::new(FakeRuntime::new(None));
    let graph = build_reference_graph(runtime.clone(), authorizer()).unwrap();
    let output = graph.invoke(input(), ExecutionConfig::new("thread-1")).await.unwrap();

    assert_eq!(output.get("verified"), Some(&json!(true)));
    assert!(runtime.max_observation_concurrency.load(Ordering::SeqCst) >= 2);
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.reservations.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.releases.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn approval_route_interrupts_before_lease_or_execution() {
    let runtime = Arc::new(FakeRuntime::new(Some("approval_required")));
    let graph = build_reference_graph(runtime.clone(), authorizer()).unwrap();
    let error = graph.invoke(input(), ExecutionConfig::new("thread-approval")).await.unwrap_err();

    assert!(matches!(error, GraphError::Interrupted(_)));
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.reservations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn approval_resume_executes_only_the_original_digest_with_the_exact_grant() {
    let runtime = Arc::new(FakeRuntime::new(Some("approval_required")));
    let graph = build_reference_graph_with_checkpointer(
        runtime.clone(),
        authorizer(),
        Some(Arc::new(MemoryCheckpointer::new())),
    )
    .unwrap();
    let interrupted =
        graph.invoke(input(), ExecutionConfig::new("thread-resume")).await.unwrap_err();
    let checkpoint_id = match interrupted {
        GraphError::Interrupted(value) => value.checkpoint_id,
        other => panic!("expected interrupt, got {other:?}"),
    };

    let mut resumed_input = State::new();
    resumed_input.insert(
        "approval".into(),
        json!({ "actionDigest": "digest-1", "policyDigest": "policy-1", "grantId": "grant-exact" }),
    );
    let output = graph
        .invoke(
            resumed_input,
            ExecutionConfig::new("thread-resume").with_resume_from(&checkpoint_id),
        )
        .await
        .unwrap();
    assert_eq!(output.get("verified"), Some(&json!(true)));
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.last_approval_grant.lock().await.as_deref(), Some("grant-exact"));
}

#[tokio::test]
async fn approval_resume_rejects_a_changed_action_digest_before_mutation() {
    let runtime = Arc::new(FakeRuntime::new(Some("approval_required")));
    let graph = build_reference_graph_with_checkpointer(
        runtime.clone(),
        authorizer(),
        Some(Arc::new(MemoryCheckpointer::new())),
    )
    .unwrap();
    let checkpoint_id =
        match graph.invoke(input(), ExecutionConfig::new("thread-mismatch")).await.unwrap_err() {
            GraphError::Interrupted(value) => value.checkpoint_id,
            other => panic!("expected interrupt, got {other:?}"),
        };
    let mut resumed_input = State::new();
    resumed_input.insert(
        "approval".into(),
        json!({ "actionDigest": "changed", "policyDigest": "policy-1", "grantId": "grant-wrong" }),
    );
    let error = graph
        .invoke(
            resumed_input,
            ExecutionConfig::new("thread-mismatch").with_resume_from(&checkpoint_id),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not match"));
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn approval_resume_rejects_a_changed_policy_digest_before_mutation() {
    let runtime = Arc::new(FakeRuntime::new(Some("approval_required")));
    let graph = build_reference_graph_with_checkpointer(
        runtime.clone(),
        authorizer(),
        Some(Arc::new(MemoryCheckpointer::new())),
    )
    .unwrap();
    let checkpoint_id = match graph
        .invoke(input(), ExecutionConfig::new("thread-policy-mismatch"))
        .await
        .unwrap_err()
    {
        GraphError::Interrupted(value) => value.checkpoint_id,
        other => panic!("expected interrupt, got {other:?}"),
    };
    let mut resumed_input = State::new();
    resumed_input.insert(
        "approval".into(),
        json!({ "actionDigest": "digest-1", "policyDigest": "changed-policy", "grantId": "grant-wrong" }),
    );
    let error = graph
        .invoke(
            resumed_input,
            ExecutionConfig::new("thread-policy-mismatch").with_resume_from(&checkpoint_id),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("policy digests"));
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_bridge_revokes_v8_before_interrupting_adk() {
    let runtime = Arc::new(FakeRuntime::new(None));
    let runtime_for_interrupt = runtime.clone();
    let bridge = CancellationBridge::new(
        runtime,
        Arc::new(move |_session: &str| {
            // try_lock succeeds only after the awaited v8 pause released the mutex.
            runtime_for_interrupt
                .cancellation_log
                .try_lock()
                .map(|log| !log.is_empty())
                .unwrap_or(false)
        }),
    );
    assert!(bridge.pause("v8-session", "adk-session", "user_takeover").await.unwrap());
}

#[tokio::test]
async fn graph_retry_after_post_commit_crash_does_not_duplicate_mutation() {
    let runtime = Arc::new(FakeRuntime::new(None));
    runtime.fail_after_first_commit.store(true, Ordering::SeqCst);
    let graph = build_reference_graph(runtime.clone(), authorizer()).unwrap();

    assert!(graph.invoke(input(), ExecutionConfig::new("thread-crash-1")).await.is_err());
    let output = graph.invoke(input(), ExecutionConfig::new("thread-crash-2")).await.unwrap();

    assert_eq!(output.get("verified"), Some(&json!(true)));
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn graph_retry_after_pre_effect_crash_executes_exactly_once() {
    let runtime = Arc::new(FakeRuntime::new(None));
    runtime.fail_before_first_effect.store(true, Ordering::SeqCst);
    let graph = build_reference_graph(runtime.clone(), authorizer()).unwrap();

    assert!(graph.invoke(input(), ExecutionConfig::new("thread-pre-effect-1")).await.is_err());
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 0);
    let output = graph.invoke(input(), ExecutionConfig::new("thread-pre-effect-2")).await.unwrap();

    assert_eq!(output.get("verified"), Some(&json!(true)));
    assert_eq!(runtime.physical_mutations.load(Ordering::SeqCst), 1);
}
