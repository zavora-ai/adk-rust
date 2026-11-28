//! Realtime runners govern every tool call, configured or not.
//!
//! A `RealtimeRunner` or `IntegratedRealtimeRunner` built without `tool_governance` executed
//! every call it was sent, outside `adk_core::authorize_tool_call`: no kill switch, no
//! confirmation, no acting identity. These tests build runners with no governance and prove
//! the governed path applies: an allowed call runs, a call that needs confirmation is refused
//! (or held where a decision can reach it), and a freeze stops calls in a session that is
//! already open.

#![cfg(feature = "scripted")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    ApprovalStore, DeclarativePolicy, GovernanceControl, InMemoryApprovalStore, RunConfig,
    ToolApproval, ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest,
};
use adk_realtime::RealtimeRunner;
use adk_realtime::config::ToolDefinition;
use adk_realtime::runner::RealtimeRunnerBuilder;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use serde_json::{Value, json};

/// The words a refused call that nobody can approve carries to the model.
const NO_APPROVER: &str = "Nobody can approve it in this session";

/// A confirmation handler that answers every request with `decision`.
#[derive(Debug)]
struct Answers(ToolConfirmationDecision);

#[async_trait::async_trait]
impl ToolConfirmationHandler for Answers {
    async fn decide(
        &self,
        _request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        Ok(self.0)
    }
}

/// A connected runner whose `pay` handler counts executions, configured by `configure`.
async fn runner_with(
    executions: &Arc<AtomicUsize>,
    configure: impl FnOnce(RealtimeRunnerBuilder) -> RealtimeRunnerBuilder,
) -> (RealtimeRunner, ScriptedRealtimeModel) {
    let model = ScriptedRealtimeModel::new(vec![]).with_idle_timeout(Duration::from_millis(50));
    let executions = Arc::clone(executions);
    let builder = RealtimeRunner::builder().model(Arc::new(model.clone())).tool_fn(
        ToolDefinition::new("pay"),
        move |_call| {
            executions.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "paid": true }))
        },
    );
    let runner = configure(builder).build().unwrap();
    runner.connect().await.unwrap();
    (runner, model)
}

/// The output the model received for the last tool call.
fn last_output(model: &ScriptedRealtimeModel) -> Value {
    model.log().tool_outputs().last().expect("the call is answered").output.clone()
}

#[tokio::test]
async fn a_runner_without_governance_runs_an_allowed_call_under_its_kill_switch() {
    let executions = Arc::new(AtomicUsize::new(0));
    let (runner, model) = runner_with(&executions, |builder| builder).await;

    runner.dispatch_tool_call("c1", "pay", "{}").await.unwrap();

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(last_output(&model), json!({ "paid": true }));
    assert!(
        runner.run_config().governance.iter().any(|control| control.same_as(runner.governance())),
        "the default governance carries the runner's kill switch"
    );
}

#[tokio::test]
async fn a_runner_without_governance_refuses_a_call_that_needs_confirmation() {
    let executions = Arc::new(AtomicUsize::new(0));
    let (runner, model) =
        runner_with(&executions, |builder| builder.require_tool_confirmation("pay")).await;

    runner.dispatch_tool_call("c1", "pay", "{}").await.unwrap();

    assert_eq!(executions.load(Ordering::SeqCst), 0, "nobody approved the call");
    let output = last_output(&model);
    let error = output["error"].as_str().unwrap_or_default();
    assert!(error.contains("Tool 'pay' requires confirmation"), "{output}");
    assert!(error.contains(NO_APPROVER), "the refusal says why, in words to speak: {output}");
}

#[tokio::test]
async fn a_confirmation_handler_decides_a_runner_call() {
    for (decision, expected) in
        [(ToolConfirmationDecision::Approve, 1), (ToolConfirmationDecision::Deny, 0)]
    {
        let executions = Arc::new(AtomicUsize::new(0));
        let config =
            RunConfig::builder().tool_confirmation_handler(Arc::new(Answers(decision))).build();
        let (runner, model) = runner_with(&executions, |builder| {
            builder
                .tool_governance(config, adk_core::ApprovalScope::new("voice", "ana", "call-1"))
                .require_tool_confirmation("pay")
        })
        .await;

        runner.dispatch_tool_call("c1", "pay", "{}").await.unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), expected, "{decision:?}");
        assert!(!last_output(&model).to_string().contains(NO_APPROVER));
    }
}

#[tokio::test]
async fn a_call_that_needs_confirmation_waits_in_the_approval_store() {
    let executions = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryApprovalStore::new());
    let scope = adk_core::ApprovalScope::new("voice", "ana", "call-1");
    let config = RunConfig::builder().approval_store(store.clone()).build();
    let (runner, model) = runner_with(&executions, |builder| {
        builder.tool_governance(config, scope.clone()).require_tool_confirmation("pay")
    })
    .await;

    runner.dispatch_tool_call("c1", "pay", "{}").await.unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let output = last_output(&model);
    assert!(!output.to_string().contains(NO_APPROVER), "a decision can reach it: {output}");
    let pending = store.pending(&scope).await.unwrap();
    let [held] = pending.as_slice() else { panic!("one held call: {pending:?}") };

    store.decide(&scope, &held.fingerprint, ToolApproval::approve()).await.unwrap();
    runner.dispatch_tool_call("c2", "pay", "{}").await.unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 1, "the approved call runs once asked again");
}

#[tokio::test]
async fn a_shared_kill_switch_and_the_policy_govern_the_runner() {
    let executions = Arc::new(AtomicUsize::new(0));
    let control = GovernanceControl::new();
    let config = RunConfig::builder()
        .tool_policy(Arc::new(DeclarativePolicy::builder().allow("get_*").build()))
        .build();
    let (runner, model) = runner_with(&executions, |builder| {
        builder
            .tool_governance(config, adk_core::ApprovalScope::new("voice", "ana", "call-1"))
            .governance(control.clone())
    })
    .await;
    assert!(runner.governance().same_as(&control));

    runner.dispatch_tool_call("c1", "pay", "{}").await.unwrap();
    assert!(last_output(&model)["error"].as_str().unwrap_or_default().contains("denied by policy"));

    control.freeze("incident 4012");
    let error = runner.dispatch_tool_call("c2", "pay", "{}").await.unwrap_err();
    assert!(error.to_string().contains("incident 4012"), "{error}");
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

/// An operator freezes the runner while its session is open and the event loop is running:
/// the next call the model makes is not run, and `run` ends with the freeze.
#[tokio::test]
async fn an_operator_freeze_reaches_an_open_runner_session() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls("call-1", "pay", json!({}))).on_text(),
        ScriptedReply::new(scripted::calls("call-2", "pay", json!({}))).on_text(),
    ])
    .with_idle_timeout(Duration::from_secs(5));
    let executions = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(tokio::sync::Notify::new());
    let runner = {
        let (executions, ran) = (Arc::clone(&executions), Arc::clone(&ran));
        RealtimeRunner::builder()
            .model(Arc::new(model.clone()))
            .tool_fn(ToolDefinition::new("pay"), move |_call| {
                executions.fetch_add(1, Ordering::SeqCst);
                ran.notify_one();
                Ok(json!({ "paid": true }))
            })
            .build()
            .unwrap()
    };
    let runner = Arc::new(runner);
    runner.connect().await.unwrap();
    let running = tokio::spawn({
        let runner = Arc::clone(&runner);
        async move { runner.run().await }
    });

    runner.send_text("pay the first invoice").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), ran.notified()).await.expect("call-1 runs");
    runner.governance().freeze("operator stop");
    runner.send_text("pay the second invoice").await.unwrap();

    let ended = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the freeze ends the run")
        .unwrap();
    let error = ended.expect_err("the run ends with the freeze");
    assert!(error.to_string().contains("operator stop"), "{error}");
    assert_eq!(executions.load(Ordering::SeqCst), 1, "call-2 never runs");
    let outputs: Vec<String> =
        model.log().tool_outputs().into_iter().map(|output| output.call_id).collect();
    assert_eq!(outputs, ["call-1"]);
}

#[cfg(feature = "integration")]
mod integrated {
    use super::*;

    use std::collections::HashMap;

    use adk_core::{Tool, ToolContext, ToolEffect};
    use adk_realtime::ToolCall;
    use adk_realtime::integration::{IntegratedRealtimeRunner, IntegratedRealtimeRunnerBuilder};
    use adk_session::{CreateRequest, InMemorySessionService, SessionService};
    use async_trait::async_trait;

    const APP: &str = "voice";
    const USER: &str = "ana";
    const SESSION: &str = "s1";

    /// A `bash`-like tool that counts its executions.
    struct Shell {
        executions: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for Shell {
        fn name(&self) -> &str {
            "bash"
        }
        fn description(&self) -> &str {
            "runs a command"
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::NonIdempotent
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            args: Value,
        ) -> adk_core::Result<Value> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ran": args["command"] }))
        }
    }

    fn builder(
        model: ScriptedRealtimeModel,
        executions: &Arc<AtomicUsize>,
    ) -> IntegratedRealtimeRunnerBuilder {
        IntegratedRealtimeRunner::builder()
            .model(Arc::new(model))
            .identity(APP, USER, SESSION)
            .adk_tool(Arc::new(Shell { executions: Arc::clone(executions) }))
    }

    fn idle_model() -> ScriptedRealtimeModel {
        ScriptedRealtimeModel::new(vec![]).with_idle_timeout(Duration::from_millis(50))
    }

    fn bash_call(call_id: &str) -> ToolCall {
        ToolCall {
            call_id: call_id.to_string(),
            name: "bash".to_string(),
            arguments: json!({ "command": "ls" }),
        }
    }

    async fn dispatch(
        runner: &IntegratedRealtimeRunner,
        call_id: &str,
    ) -> adk_realtime::Result<Value> {
        let tool: Arc<dyn Tool> = Arc::new(Shell { executions: Arc::new(AtomicUsize::new(0)) });
        runner.execute_tool_with_plugins_for_test(&tool, &bash_call(call_id)).await
    }

    async fn sessions() -> Arc<InMemorySessionService> {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(SESSION.to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        sessions
    }

    #[tokio::test]
    async fn an_integrated_runner_without_governance_runs_an_allowed_call() {
        let executions = Arc::new(AtomicUsize::new(0));
        let runner = builder(idle_model(), &executions).build().unwrap();
        let tool: Arc<dyn Tool> = Arc::new(Shell { executions: Arc::clone(&executions) });

        let result =
            runner.execute_tool_with_plugins_for_test(&tool, &bash_call("c1")).await.unwrap();

        assert_eq!(result, json!({ "ran": "ls" }));
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(
            runner
                .run_config()
                .governance
                .iter()
                .any(|control| control.same_as(runner.governance())),
            "the default governance carries the runner's kill switch"
        );
    }

    #[tokio::test]
    async fn without_an_approver_a_call_that_needs_confirmation_is_refused() {
        let executions = Arc::new(AtomicUsize::new(0));
        let runner =
            builder(idle_model(), &executions).require_tool_confirmation("bash").build().unwrap();
        let tool: Arc<dyn Tool> = Arc::new(Shell { executions: Arc::clone(&executions) });

        let result =
            runner.execute_tool_with_plugins_for_test(&tool, &bash_call("c1")).await.unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 0);
        let error = result["error"].as_str().unwrap_or_default();
        assert!(error.contains("Tool 'bash' requires confirmation"), "{result}");
        assert!(error.contains(NO_APPROVER), "{result}");
        assert!(!adk_core::is_confirmation_pending(&result), "nothing holds it: {result}");
    }

    /// With a session, a call that needs confirmation is held there, and a later runner on
    /// the session with a decision resumes it.
    #[tokio::test]
    async fn with_a_session_the_call_is_held_and_resumes_on_a_decision() {
        let sessions = sessions().await;
        let executions = Arc::new(AtomicUsize::new(0));
        let runner = builder(idle_model(), &executions)
            .session_service(sessions.clone())
            .require_tool_confirmation("bash")
            .build()
            .unwrap();
        let tool: Arc<dyn Tool> = Arc::new(Shell { executions: Arc::clone(&executions) });

        let result =
            runner.execute_tool_with_plugins_for_test(&tool, &bash_call("call-1")).await.unwrap();
        assert!(adk_core::is_confirmation_pending(&result), "{result}");
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        let held = runner.held_calls().await.unwrap();
        let [held] = held.as_slice() else { panic!("one held call: {held:?}") };

        let approved = RunConfig::builder()
            .tool_approval(held.request.fingerprint(), ToolApproval::approve())
            .build();
        let later = builder(idle_model(), &executions)
            .session_service(sessions)
            .require_tool_confirmation("bash")
            .tool_governance(approved)
            .build()
            .unwrap();
        later.connect().await.unwrap();
        let resumed = later.resume_held_calls().await.unwrap();

        assert_eq!(resumed.len(), 1, "{resumed:?}");
        assert_eq!(resumed[0].call_id, "call-1");
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_shared_kill_switch_freezes_the_integrated_runner() {
        let executions = Arc::new(AtomicUsize::new(0));
        let control = GovernanceControl::new();
        let runner =
            builder(idle_model(), &executions).governance(control.clone()).build().unwrap();
        assert!(runner.governance().same_as(&control));

        control.freeze("incident 4012");
        let error = dispatch(&runner, "c1").await.unwrap_err();

        assert!(error.to_string().contains("incident 4012"), "{error}");
    }

    /// An operator freezes the runner mid-session: the model's next call is not run.
    #[tokio::test]
    async fn an_operator_freeze_reaches_an_open_integrated_session() {
        let model = ScriptedRealtimeModel::new(vec![
            ScriptedReply::new(scripted::calls("call-1", "bash", json!({ "command": "ls" })))
                .on_text(),
            ScriptedReply::new(scripted::calls("call-2", "bash", json!({ "command": "rm" })))
                .on_text(),
        ])
        .with_idle_timeout(Duration::from_millis(200));
        let executions = Arc::new(AtomicUsize::new(0));
        let runner = builder(model.clone(), &executions).build().unwrap();
        runner.connect().await.unwrap();

        runner.send_text("list the files").await.unwrap();
        while executions.load(Ordering::SeqCst) == 0 {
            let event = runner.next_event().await.expect("the session is open").unwrap();
            if let adk_realtime::ServerEvent::FunctionCallDone { call_id, .. } = &event {
                assert_eq!(call_id, "call-1");
            }
        }
        runner.governance().freeze("operator stop");
        runner.send_text("delete them").await.unwrap();
        while let Some(event) = runner.next_event().await {
            event.unwrap();
        }

        assert_eq!(executions.load(Ordering::SeqCst), 1, "call-2 never runs");
        let outputs: Vec<String> =
            model.log().tool_outputs().into_iter().map(|output| output.call_id).collect();
        assert_eq!(outputs, ["call-1"]);
        let _ = runner.close().await;
    }
}
