//! Lifecycle contract tests for `SandboxRunner`.
#![cfg(feature = "sandbox-runner")]

use adk_core::{
    AdkError, Agent, Content, ErrorCategory, Event, EventStream, InvocationContext,
    Result as AdkResult,
};
use adk_runner::Runner;
use adk_runner::sandbox_runner::SandboxRunner;
use adk_sandbox::SandboxError;
use adk_sandbox::workspace::{
    Capability, DirEntry, ExecOutput, Manifest, SandboxClient, SandboxConfig, SandboxSession,
    SessionHandle, SnapshotId,
};
use adk_session::{
    CreateRequest, DeleteRequest, Event as SessionEvent, GetRequest, InMemorySessionService,
    ListRequest, Session, SessionService,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
enum AgentBehavior {
    Events(usize),
    Fail,
    Delay(Duration),
}

struct TestAgent {
    behavior: AgentBehavior,
    calls: Arc<Mutex<Vec<&'static str>>>,
    tools_seen: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for TestAgent {
    fn name(&self) -> &str {
        "sandbox_test_agent"
    }

    fn description(&self) -> &str {
        "exercises the sandbox runner lifecycle"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        self.calls.lock().unwrap_or_else(|error| error.into_inner()).push("run");

        let mut tool_count = 0;
        for runtime in &ctx.run_config().runtime_toolsets {
            tool_count += runtime.toolset().tools(ctx.clone()).await?.len();
        }
        self.tools_seen.store(tool_count, Ordering::SeqCst);

        match self.behavior {
            AgentBehavior::Events(count) => {
                let events = (0..count)
                    .map(|index| {
                        let mut event =
                            Event::with_id(format!("event-{index}"), "agent-invocation");
                        event.author = self.name().to_string();
                        Ok(event)
                    })
                    .collect::<Vec<AdkResult<Event>>>();
                Ok(Box::pin(futures::stream::iter(events)))
            }
            AgentBehavior::Fail => Err(AdkError::agent("agent execution failed")),
            AgentBehavior::Delay(duration) => {
                tokio::time::sleep(duration).await;
                Ok(Box::pin(futures::stream::empty()))
            }
        }
    }
}

struct FakeSession;

#[async_trait]
impl SandboxSession for FakeSession {
    async fn exec_command(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        Ok(ExecOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration: Duration::from_millis(1),
            timed_out: false,
        })
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

#[derive(Clone, Copy, Default)]
struct ClientFailures {
    provision: bool,
    start: bool,
    snapshot: bool,
    stop: bool,
}

struct FakeClient {
    calls: Arc<Mutex<Vec<&'static str>>>,
    failures: ClientFailures,
}

impl FakeClient {
    fn fail(stage: &str) -> SandboxError {
        SandboxError::ExecutionFailed(format!("{stage} failed"))
    }

    fn record(&self, stage: &'static str) {
        self.calls.lock().unwrap_or_else(|error| error.into_inner()).push(stage);
    }
}

#[async_trait]
impl SandboxClient for FakeClient {
    async fn provision(&self, _manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        self.record("provision");
        if self.failures.provision {
            return Err(Self::fail("provision"));
        }
        Ok(SessionHandle("fake-handle".to_string()))
    }

    async fn start(
        &self,
        _handle: &SessionHandle,
    ) -> Result<Box<dyn SandboxSession>, SandboxError> {
        self.record("start");
        if self.failures.start {
            return Err(Self::fail("start"));
        }
        Ok(Box::new(FakeSession))
    }

    async fn stop(&self, _handle: &SessionHandle) -> Result<(), SandboxError> {
        self.record("stop");
        if self.failures.stop {
            return Err(Self::fail("stop"));
        }
        Ok(())
    }

    async fn snapshot(&self, _handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        self.record("snapshot");
        if self.failures.snapshot {
            return Err(Self::fail("snapshot"));
        }
        Ok(SnapshotId("fake-snapshot".to_string()))
    }

    async fn resume(&self, _snapshot_id: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        Ok(SessionHandle("resumed-handle".to_string()))
    }
}

struct CountingSessionService {
    inner: InMemorySessionService,
    creates: AtomicUsize,
    fail_get: bool,
}

impl CountingSessionService {
    fn new(fail_get: bool) -> Self {
        Self { inner: InMemorySessionService::new(), creates: AtomicUsize::new(0), fail_get }
    }

    fn create_count(&self) -> usize {
        self.creates.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SessionService for CountingSessionService {
    async fn create(&self, req: CreateRequest) -> AdkResult<Box<dyn Session>> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        self.inner.create(req).await
    }

    async fn get(&self, req: GetRequest) -> AdkResult<Box<dyn Session>> {
        if self.fail_get {
            return Err(AdkError::session("session backend unavailable"));
        }
        self.inner.get(req).await
    }

    async fn list(&self, req: ListRequest) -> AdkResult<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> AdkResult<()> {
        self.inner.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: SessionEvent) -> AdkResult<()> {
        self.inner.append_event(session_id, event).await
    }
}

fn build_harness(
    behavior: AgentBehavior,
    failures: ClientFailures,
    snapshot_on_stop: bool,
    session_timeout: Duration,
    session_service: Arc<CountingSessionService>,
) -> (SandboxRunner, SandboxConfig, Arc<Mutex<Vec<&'static str>>>, Arc<AtomicUsize>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tools_seen = Arc::new(AtomicUsize::new(0));
    let agent =
        Arc::new(TestAgent { behavior, calls: calls.clone(), tools_seen: tools_seen.clone() });
    let runner = Runner::builder()
        .app_name("sandbox-runner-test")
        .agent(agent)
        .session_service(session_service)
        .build()
        .expect("runner builds");
    let config = SandboxConfig {
        client: Arc::new(FakeClient { calls: calls.clone(), failures }),
        manifest: Manifest::new(Vec::new()),
        capabilities: [Capability::Shell, Capability::Filesystem].into_iter().collect(),
        command_timeout: Duration::from_secs(5),
        session_timeout,
        snapshot_on_stop,
    };

    (SandboxRunner::new(runner), config, calls, tools_seen)
}

fn content() -> Content {
    Content::new("user").with_text("do the thing")
}

fn recorded(calls: &Arc<Mutex<Vec<&'static str>>>) -> Vec<&'static str> {
    calls.lock().unwrap_or_else(|error| error.into_inner()).clone()
}

fn cleanup_stages(error: &AdkError) -> Vec<String> {
    error
        .details
        .metadata
        .get("sandbox.cleanupErrors")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|failure| failure.get("stage").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn invalid_identity_has_no_side_effects() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures::default(),
        false,
        Duration::from_secs(1),
        sessions.clone(),
    );

    let error = runner.run(&config, "", "session-1", content()).await.expect_err("ID is invalid");

    assert_eq!(error.category, ErrorCategory::InvalidInput);
    assert!(recorded(&calls).is_empty());
    assert_eq!(sessions.create_count(), 0);
}

#[tokio::test]
async fn session_lookup_failure_does_not_create_or_provision() {
    let sessions = Arc::new(CountingSessionService::new(true));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures::default(),
        false,
        Duration::from_secs(1),
        sessions.clone(),
    );

    let error = runner
        .run(&config, "user-1", "session-1", content())
        .await
        .expect_err("lookup failure is preserved");

    assert_eq!(error.message, "session backend unavailable");
    assert!(recorded(&calls).is_empty());
    assert_eq!(sessions.create_count(), 0);
}

#[tokio::test]
async fn provision_failure_does_not_create_or_stop() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures { provision: true, ..ClientFailures::default() },
        false,
        Duration::from_secs(1),
        sessions.clone(),
    );

    let error =
        runner.run(&config, "user-1", "session-1", content()).await.expect_err("provision fails");

    assert!(error.message.contains("provision failed"));
    assert_eq!(recorded(&calls), vec!["provision"]);
    assert_eq!(sessions.create_count(), 0);
}

#[tokio::test]
async fn start_error_precedes_stop_cleanup_error() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures { start: true, stop: true, ..ClientFailures::default() },
        true,
        Duration::from_secs(1),
        sessions.clone(),
    );

    let error =
        runner.run(&config, "user-1", "session-1", content()).await.expect_err("start fails");

    assert!(error.message.contains("start failed"));
    assert_eq!(cleanup_stages(&error), vec!["stop"]);
    assert_eq!(recorded(&calls), vec!["provision", "start", "stop"]);
    assert_eq!(sessions.create_count(), 0);
}

#[tokio::test]
async fn successful_run_buffers_events_and_orders_snapshot_before_stop() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, tools_seen) = build_harness(
        AgentBehavior::Events(2),
        ClientFailures::default(),
        true,
        Duration::from_secs(1),
        sessions.clone(),
    );

    let result = runner.run(&config, "user-1", "session-1", content()).await.expect("run succeeds");

    assert_eq!(result.events.len(), 2);
    assert_eq!(result.events[0].id, "event-0");
    assert_eq!(result.events[1].id, "event-1");
    assert_eq!(result.snapshot_id, Some(SnapshotId("fake-snapshot".to_string())));
    assert!(tools_seen.load(Ordering::SeqCst) >= 2);
    assert_eq!(recorded(&calls), vec!["provision", "start", "run", "snapshot", "stop"]);
    assert_eq!(sessions.create_count(), 1);
}

#[tokio::test]
async fn existing_session_is_not_created_again() {
    let sessions = Arc::new(CountingSessionService::new(false));
    sessions
        .create(CreateRequest {
            app_name: "sandbox-runner-test".to_string(),
            user_id: "user-1".to_string(),
            session_id: Some("session-1".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("session is seeded");
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures::default(),
        false,
        Duration::from_secs(1),
        sessions.clone(),
    );

    runner.run(&config, "user-1", "session-1", content()).await.expect("run succeeds");

    assert_eq!(sessions.create_count(), 1);
    assert_eq!(recorded(&calls), vec!["provision", "start", "run", "stop"]);
}

#[tokio::test]
async fn execution_error_precedes_snapshot_and_stop_errors() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Fail,
        ClientFailures { snapshot: true, stop: true, ..ClientFailures::default() },
        true,
        Duration::from_secs(1),
        sessions,
    );

    let error =
        runner.run(&config, "user-1", "session-1", content()).await.expect_err("agent fails");

    assert_eq!(error.message, "agent execution failed");
    assert_eq!(cleanup_stages(&error), vec!["snapshot", "stop"]);
    assert_eq!(recorded(&calls), vec!["provision", "start", "run", "snapshot", "stop"]);
}

#[tokio::test]
async fn snapshot_error_is_returned_and_stop_still_runs() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures { snapshot: true, ..ClientFailures::default() },
        true,
        Duration::from_secs(1),
        sessions,
    );

    let error =
        runner.run(&config, "user-1", "session-1", content()).await.expect_err("snapshot fails");

    assert!(error.message.contains("snapshot failed"));
    assert!(cleanup_stages(&error).is_empty());
    assert_eq!(recorded(&calls), vec!["provision", "start", "run", "snapshot", "stop"]);
}

#[tokio::test]
async fn stop_error_is_returned_after_successful_execution() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Events(0),
        ClientFailures { stop: true, ..ClientFailures::default() },
        false,
        Duration::from_secs(1),
        sessions,
    );

    let error =
        runner.run(&config, "user-1", "session-1", content()).await.expect_err("stop fails");

    assert!(error.message.contains("stop failed"));
    assert_eq!(recorded(&calls), vec!["provision", "start", "run", "stop"]);
}

#[tokio::test]
async fn timeout_still_snapshots_and_stops() {
    let sessions = Arc::new(CountingSessionService::new(false));
    let (runner, config, calls, _) = build_harness(
        AgentBehavior::Delay(Duration::from_millis(200)),
        ClientFailures::default(),
        true,
        Duration::from_millis(20),
        sessions,
    );

    let error =
        runner.run(&config, "user-1", "session-1", content()).await.expect_err("agent times out");

    assert!(error.is_timeout());
    assert_eq!(recorded(&calls), vec!["provision", "start", "run", "snapshot", "stop"]);
}
