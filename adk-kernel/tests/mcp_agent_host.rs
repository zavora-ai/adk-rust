//! A kernel-hosted agent served over MCP (`adk_server::mcp`): the host enqueues each call on the
//! kernel's run store, the kernel's interactive worker runs it, and the task parks on the held
//! call, asks about it, and finishes once the caller approves.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    DurableRunStore, InMemoryActionLedger, InMemoryApprovalStore, InMemoryRunStore, Llm, RunConfig,
    Tool, ToolContext, ToolEffect,
};
use adk_kernel::{InMemoryProcessStore, Kernel, KernelConfig, ProcessStore, QueueClass};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_server::mcp::{McpAgent, McpAgentHost};
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::rmcp::model::{InputResponses, TaskPayload};
use adk_tool::mcp::server::{McpCallExecutor, McpCaller, McpTaskHost, McpToolCall, McpToolOutcome};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "watchdog";

/// A report the agent may only file with a person's approval.
struct FileReport {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for FileReport {
    fn name(&self) -> &str {
        "file_report"
    }

    fn description(&self) -> &str {
        "files the folder report"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "filed" }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kernel_hosted_agent_is_a_task_that_asks_and_finishes() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let sessions = Arc::new(InMemorySessionService::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("file_report", json!({ "folder": "inbox" })),
            ScriptedResponse::text("filed"),
        ],
    ));
    let agent = LlmAgentBuilder::new("watchdog")
        .model(model as Arc<dyn Llm>)
        .tool(Arc::new(FileReport { calls: calls.clone() }))
        .require_tool_confirmation("file_report")
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().approval_store(approvals.clone()).build())
        .action_ledger(Arc::new(InMemoryActionLedger::new()))
        .build()
        .unwrap();
    let kernel = Kernel::builder()
        .config(
            KernelConfig::default().with_app_name(APP).with_worker(
                DurableWorkerConfig::default()
                    .with_lease_ttl(Duration::from_secs(2))
                    .with_poll_interval(Duration::from_millis(20)),
            ),
        )
        .run_store(runs.clone())
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .session_service(sessions as Arc<dyn SessionService>)
        .agent(APP, Arc::new(runner))
        .build()
        .unwrap();
    kernel.start().await.unwrap();

    let host = McpAgentHost::builder(APP, runs.clone())
        .agent(McpAgent::new(APP, "Files folder reports"))
        .queue(QueueClass::INTERACTIVE)
        .approval_store(approvals)
        .poll_interval(Duration::from_millis(20))
        .build()
        .unwrap();
    let ana = McpCaller::authenticated("ana");
    let tool = host.tools().remove(0);
    let call =
        McpToolCall::new(tool, json!({ "message": "file the inbox report" }), ana.clone(), "1")
            .with_task_support();
    let McpToolOutcome::Task(task) = host.execute(call).await.unwrap() else {
        panic!("expected a task");
    };

    let started = tokio::time::Instant::now();
    let key = loop {
        let current = host.get(&task.task_id, &ana).await.unwrap();
        if let TaskPayload::InputRequired { input_requests } = &current.payload {
            break input_requests.keys().next().unwrap().clone();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "never asked: {current:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let answers: InputResponses = [(key, json!({ "action": "accept" }))].into();
    host.update(&task.task_id, &ana, answers).await.unwrap();

    let finished = loop {
        let current = host.get(&task.task_id, &ana).await.unwrap();
        if let TaskPayload::Completed { result } = &current.payload {
            break result.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "never finished: {current:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(finished["structuredContent"]["text"], "filed");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the approved call runs once");
    kernel.stop().await.unwrap();
}
