//! Background sub-agents on a real kernel: a parent process's `start_subagent` call runs
//! another agent as a `subagent` process whose parent is the caller, status reaches the
//! bus, the finish wakes the parent through a `triggered:subagent` process, and a kernel
//! killed while the sub-agent runs — its runtime dropped with no drain, as `kill -9`
//! leaves it — resumes the sub-agent on restart and tells the parent once.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_agent::subagents::SubagentTools;
use adk_core::subagent::{SubagentScope, SubagentTarget};
use adk_core::{
    Content, DurableRunStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, NewRun, Part,
    Result, RunState, SubagentHost, SubagentStatus,
};
use adk_kernel::{
    Kernel, KernelConfig, KernelEvent, ProcessFilter, ProcessKind, ProcessSpec, QueueClass,
    SUBAGENT_LABEL, SUBAGENT_NAME_LABEL, SubagentHandle,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorkerConfig, SubagentConfig};
use adk_session::{GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::json;

const APP: &str = "adk-rust";
const USER: &str = "ana";
const SESSION: &str = "s-1";

/// The lead starts `alpha` on the researcher once, then answers; a sub-agent notice gets
/// `ack`. The researcher reports on its task, after `gate` opens when one is set.
struct Model {
    lead: bool,
    gate: Option<Arc<tokio::sync::Notify>>,
    calls: AtomicUsize,
}

impl Model {
    fn lead() -> Arc<Self> {
        Arc::new(Self { lead: true, gate: None, calls: AtomicUsize::new(0) })
    }

    fn researcher(gate: Option<Arc<tokio::sync::Notify>>) -> Arc<Self> {
        Arc::new(Self { lead: false, gate, calls: AtomicUsize::new(0) })
    }

    fn answer(&self, req: &LlmRequest) -> Content {
        let texts: Vec<&str> =
            req.contents.iter().flat_map(|c| &c.parts).filter_map(Part::text).collect();
        if !self.lead {
            return Content::new("model").with_text(format!(
                "report on {}",
                texts.iter().rev().find(|t| !t.is_empty()).copied().unwrap_or_default()
            ));
        }
        if let Some(notice) = texts.iter().rev().find(|t| t.starts_with("[sub-agent ")) {
            return Content::new("model")
                .with_text(format!("ack: {}", notice.lines().next().unwrap_or_default()));
        }
        let started = req.contents.iter().flat_map(|c| &c.parts).any(|part| {
            matches!(part, Part::FunctionResponse { function_response, .. } if function_response.name == "start_subagent")
        });
        if started {
            return Content::new("model").with_text("started; working on other things");
        }
        Content {
            role: "model".into(),
            parts: vec![Part::FunctionCall {
                name: "start_subagent".into(),
                args: json!({ "task": "count the open bugs", "agent": "researcher", "name": "alpha" }),
                id: Some("call-start".into()),
                thought_signature: None,
            }],
        }
    }
}

#[async_trait]
impl Llm for Model {
    fn name(&self) -> &str {
        "subagent-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        let response = LlmResponse {
            content: Some(self.answer(&req)),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn runner(
    name: &str,
    model: Arc<Model>,
    sessions: &Arc<dyn SessionService>,
    tools: Option<&SubagentHandle>,
) -> Arc<Runner> {
    let mut agent = LlmAgentBuilder::new(name).model(model as Arc<dyn Llm>);
    if let Some(handle) = tools {
        for tool in SubagentTools::new(Arc::new(handle.clone())).tools() {
            agent = agent.tool(tool);
        }
    }
    Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent.build().unwrap()))
            .session_service(Arc::clone(sessions))
            .build()
            .unwrap(),
    )
}

fn config(home: &std::path::Path) -> KernelConfig {
    KernelConfig::default()
        .with_app_name(APP)
        .with_home(home)
        .with_worker(
            DurableWorkerConfig::default()
                .with_worker_id("k")
                .with_lease_ttl(Duration::from_millis(600))
                .with_poll_interval(Duration::from_millis(20)),
        )
        .with_scheduler_tick(Duration::from_millis(50))
        .with_control(adk_kernel::control::ControlPlaneConfig::default().without_socket())
}

fn kernel(
    builder: adk_kernel::KernelBuilder,
    home: &std::path::Path,
    sessions: Arc<dyn SessionService>,
    researcher: Arc<Model>,
) -> Kernel {
    let handle = SubagentHandle::new();
    let lead = runner("lead", Model::lead(), &sessions, Some(&handle));
    let researcher = runner("researcher", researcher, &sessions, None);
    builder
        .config(config(home))
        .session_service(sessions)
        .agent("lead", lead)
        .agent("researcher", researcher)
        .subagents(
            SubagentConfig::default().with_target(SubagentTarget::new("researcher", "researches")),
        )
        .subagent_handle(handle)
        .build()
        .unwrap()
}

async fn eventually<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    let started = tokio::time::Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(started.elapsed() < Duration::from_secs(15), "never: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn parent_turn() -> NewRun {
    NewRun::new(
        QueueClass::INTERACTIVE,
        APP,
        USER,
        "lead",
        json!({ "text": "start a researcher and keep working" }),
    )
    .with_session_id(SESSION)
}

async fn acks(sessions: &Arc<dyn SessionService>) -> Vec<String> {
    let session = sessions
        .get(GetRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    session
        .events()
        .all()
        .iter()
        .filter(|e| e.author == "lead")
        .filter_map(|e| e.llm_response.content.as_ref())
        .flat_map(|c| c.parts.iter().filter_map(Part::text).map(str::to_string))
        .filter(|t| t.starts_with("ack: "))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_is_a_process_of_its_parent_and_its_finish_wakes_the_parent() {
    let home = tempfile::tempdir().unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(adk_core::InMemoryRunStore::new());
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let kernel = kernel(
        Kernel::builder().run_store(Arc::clone(&runs)),
        home.path(),
        Arc::clone(&sessions),
        Model::researcher(None),
    );
    let mut events = kernel.events();
    kernel.start().await.unwrap();
    let parent = kernel
        .processes()
        .spawn(parent_turn(), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let host = kernel.subagents().expect("the kernel hosts sub-agents").clone();
    let scope = SubagentScope::new(APP, USER, SESSION);
    let done = eventually("the sub-agent completed", async || {
        host.list(&scope).await.unwrap().into_iter().find(|r| r.status.is_terminal())
    })
    .await;
    assert_eq!(done.status, SubagentStatus::Completed);
    assert_eq!(done.parent.run_id.as_deref(), Some(parent.meta.pid.as_str()));
    let header = format!("[sub-agent alpha ({}) completed · run {}]", done.id, done.run_id);
    let acked = eventually("the parent acknowledged", async || {
        let acks = acks(&sessions).await;
        (!acks.is_empty()).then_some(acks)
    })
    .await;
    assert_eq!(acked, vec![format!("ack: {header}")]);

    let processes = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
    let child = processes.iter().find(|p| p.meta.pid == done.run_id).expect("listed");
    assert_eq!(child.meta.kind, ProcessKind::Subagent);
    assert_eq!(child.meta.parent.as_deref(), Some(parent.meta.pid.as_str()));
    assert_eq!(child.meta.labels.get(SUBAGENT_LABEL), Some(&done.id));
    assert_eq!(child.meta.labels.get(SUBAGENT_NAME_LABEL).map(String::as_str), Some("alpha"));
    let notice = processes
        .iter()
        .find(|p| p.meta.kind == ProcessKind::Triggered { source: "subagent".into() })
        .expect("the notice ran as a process");
    assert_eq!(notice.meta.parent.as_deref(), Some(done.run_id.as_str()));
    assert_eq!(notice.run.state, RunState::Completed);

    let mut statuses = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(200), events.recv()).await
    {
        if let KernelEvent::Subagent { id, status, .. } = &record.event {
            assert_eq!(id, &done.id);
            statuses.push(*status);
        }
    }
    for status in [SubagentStatus::Queued, SubagentStatus::Running, SubagentStatus::Completed] {
        assert!(statuses.contains(&status), "{status} reached the bus: {statuses:?}");
    }
    assert_eq!(statuses.last(), Some(&SubagentStatus::Completed));
    kernel.stop().await.unwrap();
}

#[cfg(feature = "sqlite")]
#[test]
fn a_killed_kernel_resumes_the_sub_agent_and_tells_the_parent_once() {
    let home = tempfile::tempdir().unwrap();
    let kernel_url = format!("sqlite://{}?mode=rwc", home.path().join("kernel.db").display());
    let session_url = format!("sqlite://{}?mode=rwc", home.path().join("sessions.db").display());
    let open = |kernel_url: String, session_url: String| async move {
        let stores = adk_kernel::stores::KernelStores::open(&kernel_url).await.unwrap();
        let sessions = adk_session::SqliteSessionService::new(&session_url).await.unwrap();
        sessions.migrate().await.unwrap();
        (stores, Arc::new(sessions) as Arc<dyn SessionService>)
    };

    // The first kernel starts the sub-agent and is killed while its model call is in flight.
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let reached = Arc::clone(&reached);
        let (kernel_url, session_url) = (kernel_url.clone(), session_url.clone());
        let home = home.path().to_path_buf();
        runtime.block_on(async move {
            let (stores, sessions) = open(kernel_url, session_url).await;
            let never = Arc::new(tokio::sync::Notify::new());
            let researcher = Model::researcher(Some(never));
            let kernel = kernel(
                Kernel::builder().stores(&stores),
                &home,
                Arc::clone(&sessions),
                Arc::clone(&researcher),
            );
            kernel.start().await.unwrap();
            kernel
                .processes()
                .spawn(parent_turn(), ProcessSpec::new(ProcessKind::Interactive))
                .await
                .unwrap();
            eventually("the sub-agent's model call is in flight", async || {
                (researcher.calls.load(Ordering::SeqCst) > 0).then_some(())
            })
            .await;
            let host = kernel.subagents().unwrap().clone();
            let scope = SubagentScope::new(APP, USER, SESSION);
            let running = host.list(&scope).await.unwrap();
            assert_eq!(running.len(), 1);
            assert_eq!(running[0].status, SubagentStatus::Running);
            reached.store(true, Ordering::SeqCst);
        });
        // No stop, no drain: the runtime and every task it ran are dropped mid-call.
        runtime.shutdown_timeout(Duration::ZERO);
    }
    assert!(reached.load(Ordering::SeqCst));

    // The second kernel, over the same files, recovers the run once its lease expires.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let (stores, sessions) = open(kernel_url, session_url).await;
        let researcher = Model::researcher(None);
        let kernel = kernel(
            Kernel::builder().stores(&stores),
            home.path(),
            Arc::clone(&sessions),
            Arc::clone(&researcher),
        );
        kernel.start().await.unwrap();
        let host = kernel.subagents().unwrap().clone();
        let scope = SubagentScope::new(APP, USER, SESSION);
        let done = eventually("the sub-agent completed after the restart", async || {
            host.list(&scope).await.unwrap().into_iter().find(|r| r.status.is_terminal())
        })
        .await;
        assert_eq!(done.status, SubagentStatus::Completed);
        assert_eq!(done.result.as_deref(), Some("report on count the open bugs"));
        let acked = eventually("the parent was told", async || {
            let acks = acks(&sessions).await;
            (!acks.is_empty()).then_some(acks)
        })
        .await;
        assert_eq!(acked.len(), 1);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(acks(&sessions).await.len(), 1, "told once");
        kernel.stop().await.unwrap();
    });
}

/// Each status change reaches a `WebhookSink` filtered to `subagent` events as a signed
/// webhook the ingress verifies.
#[cfg(feature = "webhooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_status_change_is_a_signed_webhook() {
    use adk_core::webhook::{WebhookHeaders, WebhookSecret, WebhookVerifier};
    use adk_kernel::{EventFilter, WebhookSink};
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};

    type Seen = Arc<std::sync::Mutex<Vec<(HeaderMap, Bytes)>>>;
    async fn receive(State(seen): State<Seen>, headers: HeaderMap, body: Bytes) -> StatusCode {
        seen.lock().unwrap().push((headers, body));
        StatusCode::OK
    }
    let seen: Seen = Arc::default();
    let app = axum::Router::new()
        .route("/hooks/subagents", axum::routing::post(receive))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let secret = || WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
    let sink = WebhookSink::new(&format!("http://{addr}/hooks/subagents"), secret())
        .unwrap()
        .with_filter(EventFilter::only([adk_kernel::EventKind::Subagent]));

    let home = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let kernel = kernel(
        Kernel::builder()
            .run_store(Arc::new(adk_core::InMemoryRunStore::new()))
            .event_sink(Arc::new(sink)),
        home.path(),
        Arc::clone(&sessions),
        Model::researcher(None),
    );
    kernel.start().await.unwrap();
    kernel
        .processes()
        .spawn(parent_turn(), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let completed = eventually("a signed `completed` webhook", async || {
        let deliveries = seen.lock().unwrap().clone();
        deliveries.into_iter().find(|(_, body)| {
            let json: serde_json::Value = serde_json::from_slice(body).unwrap();
            json["data"]["event"]["status"] == "completed"
        })
    })
    .await;
    let (headers, body) = completed;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["type"], "subagent");
    assert_eq!(json["data"]["event"]["name"], "alpha");
    let lookup =
        WebhookHeaders::from_lookup(|name| headers.get(name).and_then(|v| v.to_str().ok()))
            .unwrap();
    WebhookVerifier::new(secret()).verify(&lookup, &body).expect("signed with the sink's secret");
    assert!(
        seen.lock().unwrap().iter().all(|(_, body)| {
            serde_json::from_slice::<serde_json::Value>(body).unwrap()["type"] == "subagent"
        }),
        "the filter sends sub-agent events only"
    );
    kernel.stop().await.unwrap();
}
