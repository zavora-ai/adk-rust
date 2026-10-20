//! Table-driven tests of the harness decision function over every mode × class ×
//! isolation (spec 3.1 §7.4, §11 "Unit").

use std::sync::Arc;

use adk_core::{
    ActingContext, AdkError, PolicyDecision, Result, SubjectKind, ToolEffect, ToolOrigin,
    ToolPolicyRequest,
};
use adk_harness::{
    HarnessAgent, HarnessMode, InstructionsConfig, ModeRecord, ModeStore, PolicySwitches,
    TableDecision, TableRow, decision_cell,
};
use serde_json::{Value, json};

const A: &str = "Allow";
const D: &str = "Deny";
const R: &str = "RA";
const N: &str = "RA(NW)";

fn label(decision: TableDecision) -> &'static str {
    match decision {
        TableDecision::Allow => A,
        TableDecision::Deny => D,
        TableDecision::RequireApproval { non_waivable: false } => R,
        TableDecision::RequireApproval { non_waivable: true } => N,
    }
}

/// The table of spec 3.1 §7.4, transcribed: ReadOnly, Plan, Ask, Auto.
#[test]
fn the_mode_table_is_the_spec_table() {
    let off = PolicySwitches::default();
    let expected: [(TableRow, [&str; 4]); 13] = [
        (TableRow::ReadTodoDelegate, [A, A, A, A]),
        (TableRow::PlanExit, [D, N, D, D]),
        (TableRow::Edit, [D, D, R, A]),
        (TableRow::EditProtected, [D, D, N, N]),
        (TableRow::ShellOsSandbox, [D, D, R, R]),
        (TableRow::ShellContainer, [D, D, R, A]),
        (TableRow::ShellHost, [D, D, R, R]),
        (TableRow::ShellEscalation, [D, D, N, N]),
        (TableRow::MemoryWrite, [D, D, N, N]),
        (TableRow::MemoryPropose, [D, D, A, A]),
        (TableRow::Payment, [D, D, N, N]),
        (TableRow::ExternalReadOnlyTrusted, [A, A, A, A]),
        (TableRow::ExternalOther, [D, D, R, R]),
    ];
    for (row, cells) in expected {
        for (mode, cell) in HarnessMode::ALL.into_iter().zip(cells) {
            assert_eq!(label(decision_cell(mode, row).resolve(&off)), cell, "{row:?} in {mode}");
        }
    }
    // The conditional cells.
    let enforcer = PolicySwitches { enforcer_active: true, ..off };
    assert_eq!(
        label(decision_cell(HarnessMode::Plan, TableRow::ShellOsSandbox).resolve(&enforcer)),
        A
    );
    let s3 = PolicySwitches { sandbox_protects_paths: true, ..off };
    assert_eq!(label(decision_cell(HarnessMode::Auto, TableRow::ShellOsSandbox).resolve(&s3)), A);
    assert_eq!(label(decision_cell(HarnessMode::Ask, TableRow::ShellOsSandbox).resolve(&s3)), R);
    let auto_allow =
        PolicySwitches { auto_allow_sandboxed: true, sandbox_protects_paths: true, ..off };
    assert_eq!(
        label(decision_cell(HarnessMode::Ask, TableRow::ShellOsSandbox).resolve(&auto_allow)),
        A
    );
    assert_eq!(
        label(decision_cell(HarnessMode::Ask, TableRow::ShellContainer).resolve(&auto_allow)),
        A
    );
    let without_s3 = PolicySwitches { auto_allow_sandboxed: true, ..off };
    assert_eq!(
        label(decision_cell(HarnessMode::Ask, TableRow::ShellOsSandbox).resolve(&without_s3)),
        R
    );
    let host_in_auto = PolicySwitches { allow_host_shell_in_auto: true, ..off };
    assert_eq!(
        label(decision_cell(HarnessMode::Auto, TableRow::ShellHost).resolve(&host_in_auto)),
        A
    );
    assert_eq!(
        label(decision_cell(HarnessMode::Ask, TableRow::ShellHost).resolve(&host_in_auto)),
        R
    );
}

fn harness(
    mode: HarnessMode,
    configure: impl FnOnce(adk_harness::HarnessAgentBuilder) -> adk_harness::HarnessAgentBuilder,
) -> (tempfile::TempDir, HarnessAgent) {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        Vec::<adk_model::ScriptedResponse>::new(),
    ));
    let builder = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..Default::default()
        })
        .without_compaction()
        .mode(mode);
    let agent = configure(builder).build().unwrap();
    (dir, agent)
}

fn request(tool: &str, args: Value, effect: ToolEffect) -> ToolPolicyRequest {
    ToolPolicyRequest::new(tool, args)
        .with_effect(effect)
        .with_agent_name("harness")
        .with_identity("app", "alice", "s1")
        .with_invocation_id("inv-1")
        .with_acting_context(ActingContext::derived("app", "harness", "alice", SubjectKind::Human))
}

fn kind_of(decision: &PolicyDecision) -> &'static str {
    match decision {
        PolicyDecision::Allow => A,
        PolicyDecision::Deny { .. } => D,
        PolicyDecision::RequireApproval { reason } if reason.contains("one-time approval") => N,
        PolicyDecision::RequireApproval { .. } => R,
    }
}

#[tokio::test]
async fn the_decision_function_reaches_every_class_in_every_mode() {
    let static_origin = ToolOrigin::new("static", &json!({ "name": "x" }), None);
    let mcp_origin = ToolOrigin::new("mcp:fs", &json!({ "name": "x" }), None);
    #[rustfmt::skip]
    let calls: Vec<(&str, Value, ToolEffect, &ToolOrigin, [&str; 4])> = vec![
        ("read_file", json!({ "path": "a.rs" }), ToolEffect::ReadOnly, &static_origin, [A, A, A, A]),
        ("write_todos", json!({ "todos": [] }), ToolEffect::Idempotent, &static_origin, [A, A, A, A]),
        ("exit_plan_mode", json!({ "plan": "p" }), ToolEffect::NonIdempotent, &static_origin, [D, N, D, D]),
        ("write_file", json!({ "path": "src/a.rs", "content": "" }), ToolEffect::NonIdempotent, &static_origin, [D, D, R, A]),
        ("edit_file", json!({ "path": "Agents.MD", "old_string": "a", "new_string": "b" }), ToolEffect::NonIdempotent, &static_origin, [D, D, N, N]),
        ("write_file", json!({ "path": ".git/hooks/pre-commit", "content": "" }), ToolEffect::NonIdempotent, &static_origin, [D, D, N, N]),
        ("bash", json!({ "command": "ls" }), ToolEffect::NonIdempotent, &static_origin, [D, D, R, R]),
        // An unsandboxed retry is refused outright on the host: there is no sandboxed call.
        ("bash", json!({ "command": "ls", "unsandboxed": true, "retry_of": "c1", "reason": "r" }), ToolEffect::NonIdempotent, &static_origin, [D, D, D, D]),
        // An MCP server's readOnlyHint is not trusted by default.
        ("search", json!({}), ToolEffect::ReadOnly, &mcp_origin, [D, D, R, R]),
        // A tool the operator attached that declares itself read-only is.
        ("lookup", json!({}), ToolEffect::ReadOnly, &static_origin, [A, A, A, A]),
        ("deploy", json!({}), ToolEffect::NonIdempotent, &static_origin, [D, D, R, R]),
        // An MCP tool that shadows a harness tool's name is External, not Read.
        ("read_file", json!({ "path": "a.rs" }), ToolEffect::ReadOnly, &mcp_origin, [D, D, R, R]),
        // A path outside the workspace is refused in every mode.
        ("read_file", json!({ "path": "../../etc/passwd" }), ToolEffect::ReadOnly, &static_origin, [D, D, D, D]),
    ];
    for mode in HarnessMode::ALL {
        let (_dir, agent) = harness(mode, |b| b);
        let policy = agent.runtime().policy();
        for (tool, args, effect, origin, expected) in &calls {
            let column = HarnessMode::ALL.iter().position(|m| *m == mode).unwrap();
            let decision =
                policy.decide(&request(tool, args.clone(), *effect), Some(origin), Some("c")).await;
            assert_eq!(
                kind_of(&decision),
                expected[column],
                "{tool} {args} from {} in {mode}: {decision:?}",
                origin.source
            );
        }
    }
}

#[tokio::test]
async fn mcp_read_only_hints_are_trusted_only_when_opted_in() {
    let mcp = ToolOrigin::new("mcp:fs", &json!({ "name": "search" }), None);
    let (_dir, agent) = harness(HarnessMode::Plan, |b| b.trust_mcp_read_only_hints(true));
    let decision = agent
        .runtime()
        .policy()
        .decide(&request("search", json!({}), ToolEffect::ReadOnly), Some(&mcp), None)
        .await;
    assert_eq!(decision, PolicyDecision::Allow);
}

#[tokio::test]
async fn host_shell_in_auto_follows_its_switch() {
    let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
    let (_dir, agent) = harness(HarnessMode::Auto, |b| b.allow_host_shell_in_auto());
    let decision = agent
        .runtime()
        .policy()
        .decide(
            &request("bash", json!({ "command": "ls" }), ToolEffect::NonIdempotent),
            Some(&origin),
            None,
        )
        .await;
    assert_eq!(decision, PolicyDecision::Allow);
}

#[derive(Debug)]
struct BrokenModes(Option<Value>);

#[async_trait::async_trait]
impl ModeStore for BrokenModes {
    async fn get(&self, _app: &str, _user: &str, _session: &str) -> Result<Option<ModeRecord>> {
        match &self.0 {
            Some(value) => Ok(Some(ModeRecord::from_json_lenient(value))),
            None => Err(AdkError::session("mode store down")),
        }
    }
    async fn set(
        &self,
        _app: &str,
        _user: &str,
        _session: &str,
        _record: ModeRecord,
    ) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn an_unparseable_or_unreadable_mode_is_read_only() {
    let origin = ToolOrigin::new("static", &json!({ "name": "write_file" }), None);
    for store in [
        BrokenModes(None),
        BrokenModes(Some(json!({ "mode": "yolo" }))),
        BrokenModes(Some(json!("auto"))),
    ] {
        let (_dir, agent) = harness(HarnessMode::Auto, |b| b.mode_store(Arc::new(store)));
        let decision = agent
            .runtime()
            .policy()
            .decide(
                &request(
                    "write_file",
                    json!({ "path": "a", "content": "" }),
                    ToolEffect::NonIdempotent,
                ),
                Some(&origin),
                None,
            )
            .await;
        assert!(matches!(decision, PolicyDecision::Deny { .. }), "{decision:?}");
    }
}

#[tokio::test]
async fn a_member_of_another_agent_gets_no_harness_class() {
    let origin = ToolOrigin::new("static", &json!({ "name": "read_file" }), None);
    let (_dir, agent) = harness(HarnessMode::Plan, |b| b);
    let decision = agent
        .runtime()
        .policy()
        .decide(
            &request("read_file", json!({ "path": "a" }), ToolEffect::NonIdempotent)
                .with_agent_name("intruder"),
            Some(&origin),
            None,
        )
        .await;
    assert!(matches!(decision, PolicyDecision::Deny { .. }), "{decision:?}");
}

#[test]
fn provider_shell_builtins_are_rejected_at_build() {
    struct NativeBash;
    #[async_trait::async_trait]
    impl adk_core::Tool for NativeBash {
        fn name(&self) -> &str {
            "bash_20250124"
        }
        fn description(&self) -> &str {
            "provider bash"
        }
        fn is_builtin(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> Result<Value> {
            Ok(Value::Null)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        Vec::<adk_model::ScriptedResponse>::new(),
    ));
    let error = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(InstructionsConfig { user_file: None, ..Default::default() })
        .tool(Arc::new(NativeBash))
        .build()
        .unwrap_err();
    assert!(error.message.contains("provider-executed shell"), "{error}");
}

#[tokio::test]
async fn a_second_plan_exit_in_the_approving_invocation_is_refused() {
    let store: Arc<dyn ModeStore> = Arc::new(adk_harness::InMemoryModeStore::new());
    let mut record = ModeRecord::new(HarnessMode::Ask, adk_harness::ModeSource::PlanApproval);
    record.effective_from = Some("inv-1".into());
    store.set("app", "alice", "s1", record).await.unwrap();
    let (_dir, agent) = harness(HarnessMode::Plan, |b| b.mode_store(store));
    let origin = ToolOrigin::new("static", &json!({ "name": "exit_plan_mode" }), None);
    let policy = agent.runtime().policy();
    let again = request("exit_plan_mode", json!({ "plan": "p" }), ToolEffect::NonIdempotent);
    assert!(matches!(
        policy.decide(&again, Some(&origin), None).await,
        PolicyDecision::Deny { .. }
    ));
    // The invocation still runs under Plan: an edit stays refused.
    let edit =
        request("write_file", json!({ "path": "a", "content": "" }), ToolEffect::NonIdempotent);
    assert!(matches!(policy.decide(&edit, Some(&origin), None).await, PolicyDecision::Deny { .. }));
    // The next invocation is in Ask.
    let next = edit.with_invocation_id("inv-2");
    assert!(matches!(
        policy.decide(&next, Some(&origin), None).await,
        PolicyDecision::RequireApproval { .. }
    ));
}
