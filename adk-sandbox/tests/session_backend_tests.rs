//! `SessionBackend` over a real `SandboxClient` (the process-backed `LocalUnixClient`), the
//! default `SandboxSession::exec`, and `SandboxTool`'s kill switch and wall-time budget.
#![cfg(all(unix, feature = "workspace"))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{
    BudgetTracker, CallbackContext, Content, EventActions, GovernanceControl, ReadonlyContext,
    RunBudget, RunConfig, Tool, ToolContext,
};
use adk_sandbox::workspace::{
    CommandRequest, DirEntry, ExecOutput, LocalUnixClient, SandboxClient, SandboxSession,
    SessionBackend,
};
use adk_sandbox::{ExecRequest, Language, SandboxBackend, SandboxError, SandboxTool};
use async_trait::async_trait;
use serde_json::json;

fn local() -> (Arc<LocalUnixClient>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let client =
        Arc::new(LocalUnixClient::new(Some(dir.path().join("ws")), dir.path().join("snap")));
    (client, dir)
}

fn request(language: Language, code: &str, timeout: Duration) -> ExecRequest {
    ExecRequest {
        language,
        code: code.to_string(),
        stdin: None,
        timeout,
        memory_limit_mb: None,
        env: HashMap::new(),
    }
}

#[tokio::test]
async fn one_environment_serves_every_request_until_closed() {
    let (client, _dir) = local();
    let backend = SessionBackend::new(client.clone());
    assert_eq!(backend.name(), "local");
    assert_eq!(backend.capabilities().isolation_class, "session");

    let mut python = request(
        Language::Python,
        "import os, sys\nprint(sys.stdin.read().upper(), os.environ['MARK'])",
        Duration::from_secs(20),
    );
    python.stdin = Some("piped".to_string());
    python.env = HashMap::from([("MARK".to_string(), "m1".to_string())]);
    let result = backend.execute(python).await.unwrap();
    assert_eq!((result.stdout.as_str(), result.exit_code), ("PIPED m1\n", 0), "{}", result.stderr);

    // State persists between requests: the environment is the same one.
    backend
        .execute(request(Language::Command, "echo kept > state.txt", Duration::from_secs(5)))
        .await
        .unwrap();
    let result = backend
        .execute(request(
            Language::Command,
            "cat state.txt; ls -A .adk-exec | wc -l",
            Duration::from_secs(5),
        ))
        .await
        .unwrap();
    // Earlier programs are gone; the one listing is the program running this command.
    assert_eq!(result.stdout.split_whitespace().collect::<Vec<_>>(), ["kept", "1"]);

    let failed = backend
        .execute(request(Language::Command, "echo oops >&2; exit 4", Duration::from_secs(5)))
        .await
        .unwrap();
    assert_eq!((failed.stderr.as_str(), failed.exit_code), ("oops\n", 4));

    assert!(matches!(
        backend.execute(request(Language::Wasm, "", Duration::from_secs(5))).await,
        Err(SandboxError::InvalidRequest(_))
    ));

    let handle = backend.handle().await.unwrap();
    backend.close().await.unwrap();
    assert!(backend.handle().await.is_none());
    assert!(!client.sessions.read().await.contains_key(handle.as_str()));
}

#[tokio::test]
async fn a_timeout_is_a_timeout_error() {
    let (client, _dir) = local();
    let backend = SessionBackend::new(client);
    let started = Instant::now();
    let result =
        backend.execute(request(Language::Command, "sleep 30", Duration::from_millis(300))).await;
    assert!(matches!(result, Err(SandboxError::Timeout { .. })), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_cancelled_request_destroys_the_environment() {
    let (client, _dir) = local();
    let backend = Arc::new(SessionBackend::new(client.clone()));
    backend.execute(request(Language::Command, "true", Duration::from_secs(5))).await.unwrap();
    let handle = backend.handle().await.unwrap();

    let running = backend.clone();
    let task = tokio::spawn(async move {
        running.execute(request(Language::Command, "sleep 30", Duration::from_secs(60))).await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    task.abort();
    let _ = task.await;

    for _ in 0..100 {
        if !client.sessions.read().await.contains_key(handle.as_str()) {
            assert!(backend.handle().await.is_none());
            // The next request provisions a new environment.
            backend
                .execute(request(Language::Command, "true", Duration::from_secs(5)))
                .await
                .unwrap();
            assert_ne!(backend.handle().await.unwrap(), handle);
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the cancelled request left its environment running");
}

/// A session that only implements `exec_command`, so `exec` is the trait's default.
#[derive(Default)]
struct Recording {
    commands: Mutex<Vec<String>>,
    delay: Duration,
}

#[async_trait]
impl SandboxSession for Recording {
    async fn exec_command(
        &self,
        command: &str,
        _dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        self.commands.lock().unwrap().push(command.to_string());
        tokio::time::sleep(self.delay).await;
        Ok(ExecOutput::new("done", "", 0, self.delay, false))
    }
    async fn read_file(&self, _path: &str) -> Result<Vec<u8>, SandboxError> {
        Ok(Vec::new())
    }
    async fn write_file(&self, _path: &str, _content: &[u8]) -> Result<(), SandboxError> {
        Ok(())
    }
    async fn list_dir(&self, _path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        Ok(Vec::new())
    }
    async fn apply_patch(&self, _patch: &str) -> Result<(), SandboxError> {
        Ok(())
    }
}

#[tokio::test]
async fn the_default_exec_exports_quoted_env_and_enforces_the_timeout() {
    let session = Recording::default();
    session.exec(CommandRequest::new("run").env("A", "it's").env("B_2", "x y")).await.unwrap();
    assert_eq!(session.commands.lock().unwrap()[0], "export A='it'\\''s'; export B_2='x y'; run");
    assert!(matches!(
        session.exec(CommandRequest::new("run").env("BAD;rm", "x")).await,
        Err(SandboxError::InvalidRequest(_))
    ));

    let slow = Recording { delay: Duration::from_secs(30), ..Recording::default() };
    let output =
        slow.exec(CommandRequest::new("run").timeout(Duration::from_millis(100))).await.unwrap();
    assert!(output.timed_out);
}

#[tokio::test]
async fn the_local_session_applies_per_command_env_and_timeout() {
    let (client, _dir) = local();
    let handle = client.provision(&adk_sandbox::workspace::Manifest::new(vec![])).await.unwrap();
    let session = client.start(&handle).await.unwrap();
    let output = session
        .exec(CommandRequest::new("printf %s \"$GREETING\"").env("GREETING", "hello there"))
        .await
        .unwrap();
    assert_eq!(output.stdout, "hello there");
    let output = session
        .exec(CommandRequest::new("sleep 30").timeout(Duration::from_millis(200)))
        .await
        .unwrap();
    assert!(output.timed_out);
    assert!(
        matches!(client.pause(&handle).await, Err(SandboxError::Unsupported { provider, .. }) if provider == "local")
    );
    client.stop(&handle).await.unwrap();
}

/// A tool context carrying a run configuration, as a runner gives one.
struct RunContext {
    content: Content,
    config: RunConfig,
}

#[async_trait]
impl ReadonlyContext for RunContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
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
impl CallbackContext for RunContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for RunContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
    fn run_config(&self) -> Option<&RunConfig> {
        Some(&self.config)
    }
}

#[tokio::test]
async fn a_freeze_stops_a_running_call_and_destroys_its_environment() {
    let (client, _dir) = local();
    let backend = Arc::new(SessionBackend::new(client.clone()));
    let tool = SandboxTool::new(backend.clone());
    let control = GovernanceControl::new();
    let ctx: Arc<dyn ToolContext> = Arc::new(RunContext {
        content: Content::new("user"),
        config: RunConfig::builder().governance(control.clone()).build(),
    });
    let freezer = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        freezer.freeze("incident 77");
    });
    let started = Instant::now();
    let error = tool
        .execute(
            ctx.clone(),
            json!({ "language": "command", "code": "sleep 30", "timeout_secs": 60 }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, adk_core::GOVERNANCE_FROZEN_CODE);
    assert!(started.elapsed() < Duration::from_secs(5), "the freeze did not end the call");
    for _ in 0..100 {
        if client.sessions.read().await.is_empty() {
            // A frozen run refuses the next call before it starts.
            let refused = tool
                .execute(ctx, json!({ "language": "command", "code": "true" }))
                .await
                .unwrap_err();
            assert_eq!(refused.code, adk_core::GOVERNANCE_FROZEN_CODE);
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the frozen call left its environment running");
}

#[tokio::test]
async fn the_wall_time_budget_caps_the_call() {
    let (client, _dir) = local();
    let tool = SandboxTool::new(Arc::new(SessionBackend::new(client)));
    let config = RunConfig {
        budget_tracker: Some(Arc::new(BudgetTracker::new(
            RunBudget::new().max_wall_time(Duration::from_secs(1)),
        ))),
        ..RunConfig::default()
    };
    let ctx: Arc<dyn ToolContext> = Arc::new(RunContext { content: Content::new("user"), config });
    let started = Instant::now();
    let result = tool
        .execute(
            ctx.clone(),
            json!({ "language": "command", "code": "sleep 20", "timeout_secs": 30 }),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "timeout", "{result}");
    assert!(started.elapsed() < Duration::from_secs(5));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let spent =
        tool.execute(ctx, json!({ "language": "command", "code": "true" })).await.unwrap_err();
    assert_eq!(spent.code, "budget.wall_time");
}
