//! The routes the surfaces need, on a real kernel's control plane over HTTP on loopback as
//! `adk-rust up` serves it (spec 3.3 §10 K3, K4, K5):
//!
//! - K3: quiet time, the daily spending limit and the mode read their start values, change
//!   while the kernel runs, are recorded, survive a restart of the kernel over the same
//!   store, and take effect at the next admission;
//! - K4: "While you were away" counts, spends, says whether quiet time was respected, links
//!   each line to its record, and moves on only when a viewer marks it seen, each device on
//!   its own cursor, exactly across a restart;
//! - K5: memory with provenance, corrected only by the person; the record and its check;
//! - every route's scope: a device with `read` reads, no device changes a setting or a
//!   memory, and a caller without the `operator` scope does neither.

#![cfg(feature = "proc")]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{
    AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail, InMemoryAuditRecorder,
};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalPolicy, ApproverSelector, ErrorCategory, GovernanceControl,
    InMemoryApprovalStore, RequestContext, StoreApprovalService,
};
use adk_kernel::control::api::{AwayKind, AwayQuery, IssueRequest, SpawnRequest};
use adk_kernel::control::{
    ControlClient, ControlPlane, ControlPlaneConfig, ControlStore, FactOrigin,
    InMemoryControlStore, MemoryBook, RecordBook, RecordCheck, RecordLine, RememberedFact,
    SqliteControlStore, TidyUp, TidyUpUndo,
};
use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing};
use adk_kernel::proc::{
    Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerSink, LedgerStore,
    SqliteLedgerStore, activity_for_audit,
};
use adk_kernel::{
    Kernel, KernelConfig, PartitionId, PartitionMode, ProcessKind, QueueClass, QuietHours, Quotas,
};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use chrono::{Timelike as _, Utc};
use common::{APP, World};
use serde_json::json;

fn category(error: adk_kernel::KernelError) -> ErrorCategory {
    AdkError::from(error).category
}

/// The facts of one person, a guess among them, and one tidy-up.
#[derive(Default)]
struct Notebook {
    facts: Mutex<Vec<RememberedFact>>,
    undone: Mutex<Vec<String>>,
}

impl Notebook {
    fn with_facts() -> Arc<Self> {
        let now = Utc::now();
        let book = Self::default();
        *book.facts.lock().unwrap() = vec![
            RememberedFact::new(
                "f1",
                "You bill [CLIENT] at [RATE] an hour.",
                FactOrigin::YouSaidThis,
                now,
            ),
            RememberedFact::new(
                "f2",
                "Your accountant is [NAME].",
                FactOrigin::YouConfirmedThis,
                now,
            ),
            RememberedFact::new(
                "f3",
                "You send invoices on the last working day.",
                FactOrigin::Guess,
                now,
            )
            .with_noticed_by("finance")
            .with_area("work"),
        ];
        Arc::new(book)
    }

    fn mine(&self, user: &str, id: &str) -> adk_kernel::Result<usize> {
        let facts = self.facts.lock().unwrap();
        match (user, facts.iter().position(|fact| fact.id == id)) {
            ("ana", Some(index)) => Ok(index),
            _ => Err(adk_kernel::KernelError::Core(AdkError::new(
                adk_core::ErrorComponent::Agent,
                ErrorCategory::NotFound,
                "harness.memory.fact_not_found",
                format!("no fact {id}"),
            ))),
        }
    }
}

#[async_trait::async_trait]
impl MemoryBook for Notebook {
    async fn facts(
        &self,
        user: &str,
        area: Option<&PartitionId>,
    ) -> adk_kernel::Result<Vec<RememberedFact>> {
        let facts = self.facts.lock().unwrap();
        Ok(facts
            .iter()
            .filter(|_| user == "ana")
            .filter(|fact| area.is_none_or(|area| fact.area.as_ref() == Some(area)))
            .cloned()
            .collect())
    }

    async fn confirm(
        &self,
        user: &str,
        id: &str,
        _: &SubjectRef,
    ) -> adk_kernel::Result<RememberedFact> {
        let index = self.mine(user, id)?;
        let mut facts = self.facts.lock().unwrap();
        facts[index].origin = FactOrigin::YouConfirmedThis;
        Ok(facts[index].clone())
    }

    async fn fix(
        &self,
        user: &str,
        id: &str,
        text: &str,
        _: &SubjectRef,
    ) -> adk_kernel::Result<RememberedFact> {
        let index = self.mine(user, id)?;
        let mut facts = self.facts.lock().unwrap();
        let fixed = RememberedFact::new(
            format!("{id}-fixed"),
            text,
            FactOrigin::YouConfirmedThis,
            Utc::now(),
        );
        facts[index] = fixed.clone();
        Ok(fixed)
    }

    async fn forget(&self, user: &str, id: &str, _: &SubjectRef) -> adk_kernel::Result<()> {
        let index = self.mine(user, id)?;
        self.facts.lock().unwrap().remove(index);
        Ok(())
    }

    async fn tidy_ups(&self, _: &str) -> adk_kernel::Result<Vec<TidyUp>> {
        let at = Utc::now() - chrono::Duration::hours(6);
        let done = TidyUp::new("t2", "work", at).with_counts(1, 2, 0, 0).finished(at);
        let done = match self.undone.lock().unwrap().is_empty() {
            true => done,
            false => done.undone(Utc::now()),
        };
        Ok(vec![TidyUp::new("t3", "work", Utc::now()), done])
    }

    async fn undo_tidy_up(
        &self,
        _: &str,
        id: &str,
        _: &SubjectRef,
    ) -> adk_kernel::Result<TidyUpUndo> {
        self.undone.lock().unwrap().push(id.to_string());
        Ok(TidyUpUndo::new(id, vec!["a".into(), "b".into()], vec!["m".into()], Vec::new()))
    }
}

/// A record of three actions that checks as `valid`.
struct Logbook {
    valid: bool,
}

#[async_trait::async_trait]
impl RecordBook for Logbook {
    async fn recent(&self, limit: usize) -> adk_kernel::Result<Vec<RecordLine>> {
        let now = Utc::now();
        let lines = vec![
            RecordLine::new(3, now, "approval")
                .with_agent("finance")
                .with_how("chat", Some("telegram")),
            RecordLine::new(2, now, "approval").with_agent("code").with_how("touch", None),
            RecordLine::new(1, now, "kernel.control").with_subject("ana"),
        ];
        Ok(lines.into_iter().take(limit).collect())
    }

    async fn check(&self) -> adk_kernel::Result<RecordCheck> {
        let check = RecordCheck::new(self.valid, 3, Utc::now());
        Ok(match self.valid {
            true => check,
            false => check.with_problem("entry 2 does not match its checkpoint"),
        })
    }
}

/// Admits `Bearer viewer` as `ana` without the `operator` scope.
struct ViewerExtractor;

#[async_trait::async_trait]
impl RequestContextExtractor for ViewerExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        match parts.headers.get("authorization").and_then(|value| value.to_str().ok()) {
            Some("Bearer viewer") => Ok(RequestContext {
                user_id: "ana".into(),
                scopes: Vec::new(),
                metadata: HashMap::new(),
            }),
            Some("Bearer owner") => Ok(RequestContext {
                user_id: "ana".into(),
                scopes: vec!["operator".into()],
                metadata: HashMap::new(),
            }),
            _ => Err(RequestContextError::MissingAuth),
        }
    }
}

/// A kernel with a control plane on loopback, device pairing, a ledger, an audit trail,
/// the books and a control store.
struct Plane {
    kernel: Kernel,
    plane: ControlPlane,
    recorder: InMemoryAuditRecorder,
    ledger: Ledger,
    _world: World,
}

struct Setup {
    config: KernelConfig,
    control: ControlPlaneConfig,
    store: Arc<dyn ControlStore>,
    ledger: Arc<dyn LedgerStore>,
    notebook: Option<Arc<Notebook>>,
    record_valid: bool,
    agent: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            config: KernelConfig::default().with_app_name(APP),
            control: ControlPlaneConfig::default()
                .without_socket()
                .with_http("127.0.0.1:0".parse().unwrap()),
            store: Arc::new(InMemoryControlStore::new()),
            ledger: Arc::new(InMemoryLedgerStore::new()),
            notebook: Some(Notebook::with_facts()),
            record_valid: true,
            agent: false,
        }
    }
}

impl Plane {
    async fn start(setup: Setup) -> Self {
        let world = World::answer();
        let control = GovernanceControl::new();
        let recorder = InMemoryAuditRecorder::new();
        let approvals = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
            ),
        );
        let mut plane = setup
            .control
            .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new())))
            .with_control_store(setup.store)
            .with_record(Arc::new(Logbook { valid: setup.record_valid }));
        if let Some(notebook) = setup.notebook {
            plane = plane.with_memory(notebook);
        }
        let worker = adk_server::background::DurableWorkerConfig::default()
            .with_poll_interval(Duration::from_millis(20));
        let config = setup
            .config
            .with_governance(control.clone())
            .with_worker(worker)
            .with_scheduler_tick(Duration::from_millis(50))
            .with_control(plane);
        let ledger = Ledger::new(setup.ledger, LedgerKey::from_bytes([3; 32]));
        let mut builder = Kernel::builder()
            .config(config)
            .run_store(world.runs.clone())
            .process_store(world.processes.clone())
            .session_service(world.sessions.clone())
            .approval_service(approvals)
            .audit(AuditTrail::new(Arc::new(recorder.clone())))
            .ledger(ledger.clone());
        if setup.agent {
            builder = builder.agent(common::TARGET, world.runner(&control));
        }
        let kernel = builder.build().unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        let ledger = kernel.ledger().cloned().unwrap();
        Self { kernel, plane, recorder, ledger, _world: world }
    }

    fn base(&self) -> String {
        format!("http://{}", self.plane.http_addr().unwrap())
    }

    /// The person at the kernel: a loopback caller without a token is the operator.
    fn operator(&self) -> ControlClient {
        ControlClient::http(self.base())
    }

    /// Pairs a device with `scopes` and returns a client holding its token, and its id.
    async fn device(&self, scopes: &[DeviceScope]) -> (ControlClient, String) {
        let request = IssueRequest {
            name: Some("phone".into()),
            scopes: scopes.to_vec(),
            expires_in_days: None,
        };
        let issued = self.operator().issue_pairing_code(&request).await.unwrap();
        let claimed =
            ControlClient::http(self.base()).claim_pairing(&issued.code, None).await.unwrap();
        (ControlClient::http(self.base()).with_bearer_token(claimed.token), claimed.device.id)
    }

    /// The `kernel.control` records of `action`, as their extension fields.
    fn controls(&self, action: &str) -> Vec<serde_json::Value> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == adk_kernel::audit::CONTROL)
            .filter(|record| record.ext["action"] == action)
            .map(|record| serde_json::to_value(&record.ext).unwrap())
            .collect()
    }

    async fn row(&self, activity: Activity) {
        self.ledger.record(activity).await.unwrap();
    }
}

fn window(text: &str) -> QuietHours {
    text.parse::<QuietHours>().unwrap().with_utc_offset_minutes(0)
}

/// A quiet window in UTC holding now, two hours long.
fn window_around_now() -> QuietHours {
    let now = Utc::now();
    let hour = |offset: i64| (now + chrono::Duration::hours(offset)).hour();
    window(&format!("{:02}:00-{:02}:00", hour(-1), hour(1)))
}

// ---------------------------------------------------------------------------------------
// K3: quiet time, the daily spending limit and the mode
// ---------------------------------------------------------------------------------------

/// The settings read the values the kernel started with; each change replaces its value,
/// is recorded with what it was and what it became, and the others keep theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_start_as_configured_and_each_change_replaces_its_value() {
    let setup = Setup {
        config: KernelConfig::default()
            .with_app_name(APP)
            .with_quiet_hours(window("22:00-07:00"))
            .with_quotas(Quotas::new().with_daily_spend_cap(5_000_000)),
        control: Setup::default().control.with_start_mode("default", PartitionMode::Ask),
        ..Setup::default()
    };
    let plane = Plane::start(setup).await;
    let operator = plane.operator();

    let areas = operator.settings().await.unwrap();
    assert_eq!(areas.len(), 1, "a kernel without partitions has one area");
    let area = &areas[0];
    assert_eq!(area.partition.as_str(), "default");
    assert_eq!(area.quiet_time, [window("22:00-07:00")]);
    assert_eq!((area.daily_spending_limit, area.spent_today), (Some(5_000_000), 0));
    assert_eq!((area.mode, area.changed.as_ref()), (Some(PartitionMode::Ask), None));

    let area = operator.set_quiet_time("default", Some(window("23:00-06:00"))).await.unwrap();
    assert_eq!(area.quiet_time, [window("23:00-06:00")]);
    assert_eq!(area.daily_spending_limit, Some(5_000_000), "the limit keeps its value");
    let area = operator.set_spending_limit("default", Some(2_000_000)).await.unwrap();
    assert_eq!(area.daily_spending_limit, Some(2_000_000));
    let area = operator.set_area_mode("default", PartitionMode::Auto).await.unwrap();
    assert_eq!(area.mode, Some(PartitionMode::Auto));
    assert_eq!(area.quiet_time, [window("23:00-06:00")], "quiet time keeps its change");
    let changed = area.changed.expect("the changes are listed");
    assert_eq!(changed.mode.map(|change| change.value), Some(PartitionMode::Auto));
    assert!(changed.quiet_time.is_some() && changed.daily_spending_limit.is_some());

    let area = operator.set_quiet_time("default", None).await.unwrap();
    assert!(area.quiet_time.is_empty(), "quiet time is off");
    let area = operator.set_spending_limit("default", None).await.unwrap();
    assert_eq!(area.daily_spending_limit, None, "no limit");

    let quiet = plane.controls("settings.quiet_time");
    assert_eq!(quiet.len(), 2);
    assert_eq!(quiet[0]["target"], "default");
    assert_eq!(
        quiet[0]["from"],
        json!([{ "start": "22:00", "end": "07:00", "utcOffsetMinutes": 0 }])
    );
    assert_eq!(quiet[0]["to"], json!({ "start": "23:00", "end": "06:00", "utcOffsetMinutes": 0 }));
    assert_eq!(quiet[1]["to"], json!(null));
    let limit = plane.controls("settings.spending_limit");
    assert_eq!(
        (limit[0]["from"].clone(), limit[0]["to"].clone()),
        (json!(5_000_000), json!(2_000_000))
    );
    let mode = plane.controls("settings.mode");
    assert_eq!((mode[0]["from"].clone(), mode[0]["to"].clone()), (json!("ask"), json!("auto")));

    let error = operator.set_quiet_time("garden", Some(window("22:00-07:00"))).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::NotFound, "an unknown area is refused");
    let error = operator.set_quiet_time("default", Some(window("07:00-07:00"))).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::InvalidInput, "an empty window is refused");
}

/// A change lives in the control store: a kernel restarted over the same store reads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_settings_survive_a_restart_over_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("harness.db").display());
    let open = || async {
        let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
        let store = SqliteControlStore::new(pool);
        store.migrate().await.unwrap();
        Arc::new(store) as Arc<dyn ControlStore>
    };
    let config = || {
        KernelConfig::default()
            .with_app_name(APP)
            .with_quotas(Quotas::new().with_daily_spend_cap(5_000_000))
    };
    {
        let plane =
            Plane::start(Setup { config: config(), store: open().await, ..Setup::default() }).await;
        plane.operator().set_spending_limit("default", Some(1_500_000)).await.unwrap();
        plane.plane.shutdown().await.unwrap();
    }
    let plane =
        Plane::start(Setup { config: config(), store: open().await, ..Setup::default() }).await;
    let area = plane.operator().area_settings("default").await.unwrap();
    assert_eq!(area.daily_spending_limit, Some(1_500_000));
}

/// A device with `read` reads the settings and changes none, whatever else it may do; a
/// device without `read` reads none; a caller without the `operator` scope does neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_reads_the_settings_and_never_changes_them() {
    let plane = Plane::start(Setup::default()).await;
    let (reader, _) = plane.device(&[DeviceScope::Read]).await;
    assert_eq!(reader.settings().await.unwrap().len(), 1);
    reader.area_settings("default").await.unwrap();
    let (everything, _) = plane.device(&DeviceScope::ALL).await;
    for refused in [
        everything.set_quiet_time("default", Some(window("22:00-07:00"))).await.unwrap_err(),
        everything.set_spending_limit("default", Some(1)).await.unwrap_err(),
        everything.set_area_mode("default", PartitionMode::Auto).await.unwrap_err(),
        reader.set_area_mode("default", PartitionMode::Auto).await.unwrap_err(),
    ] {
        assert_eq!(category(refused), ErrorCategory::Forbidden);
    }
    let (stopper, _) = plane.device(&[DeviceScope::Stop]).await;
    assert_eq!(category(stopper.settings().await.unwrap_err()), ErrorCategory::Forbidden);
    assert!(plane.controls("settings.mode").is_empty(), "nothing changed, nothing recorded");
    assert_eq!(plane.operator().area_settings("default").await.unwrap().changed, None);

    let setup = Setup {
        control: Setup::default().control.with_extractor(Arc::new(ViewerExtractor)),
        ..Setup::default()
    };
    let guarded = Plane::start(setup).await;
    let viewer = ControlClient::http(guarded.base()).with_bearer_token("viewer");
    assert_eq!(category(viewer.settings().await.unwrap_err()), ErrorCategory::Forbidden);
    let error = viewer.set_spending_limit("default", Some(1)).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::Forbidden);
    let owner = ControlClient::http(guarded.base()).with_bearer_token("owner");
    let area = owner.set_spending_limit("default", Some(1)).await.expect("the home's owner may");
    assert_eq!(area.changed.unwrap().daily_spending_limit.unwrap().by, "ana");
}

/// The process a spawn makes, once admission has decided on it: its labels.
async fn admitted_labels(
    plane: &Plane,
    request: SpawnRequest,
) -> (String, std::collections::BTreeMap<String, String>) {
    let process = plane.operator().spawn(&request).await.unwrap();
    let pid = process.meta.pid.clone();
    let started = tokio::time::Instant::now();
    loop {
        let process = plane.kernel.processes().get(&pid).await.unwrap().unwrap();
        if process.meta.labels.contains_key("kernel.admission") || process.run.state.is_terminal() {
            return (pid, process.meta.labels);
        }
        assert!(started.elapsed() < Duration::from_secs(10), "admission decided on {pid}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A limit changed while the kernel runs holds background work at its next admission, and
/// raising it lets that work run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_limit_holds_background_work_at_its_next_step() {
    let plane = Plane::start(Setup { agent: true, ..Setup::default() }).await;
    plane.kernel.start().await.unwrap();
    plane.operator().set_spending_limit("default", Some(0)).await.unwrap();
    let goal = SpawnRequest::new(common::TARGET, json!({ "text": "tidy the inbox" }))
        .with_queue(QueueClass::Background)
        .with_kind(ProcessKind::Goal);
    let (pid, labels) = admitted_labels(&plane, goal).await;
    assert_eq!(labels.get("kernel.admission").map(String::as_str), Some("budget"));
    assert_eq!(plane._world.model_calls(), 0, "held work makes no model call");

    plane.operator().set_spending_limit("default", None).await.unwrap();
    let started = tokio::time::Instant::now();
    while !plane.kernel.processes().get(&pid).await.unwrap().unwrap().run.state.is_terminal() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the held work runs once the limit is lifted"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(plane._world.model_calls(), 1);
    plane.kernel.stop().await.unwrap();
}

/// Quiet time changed while the kernel runs holds a suggestion's work at its next
/// admission; the start setting, no quiet time, did not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_quiet_time_holds_proactive_work_at_its_next_step() {
    let plane = Plane::start(Setup { agent: true, ..Setup::default() }).await;
    plane.kernel.start().await.unwrap();
    let suggestion = || {
        SpawnRequest::new(common::TARGET, json!({ "text": "pay the supplier" }))
            .with_queue(QueueClass::Background)
            .with_kind(ProcessKind::Proposed)
    };
    let (_, labels) = admitted_labels(&plane, suggestion()).await;
    assert_eq!(labels.get("kernel.admission"), None, "no quiet time at start: it runs");

    plane.operator().set_quiet_time("default", Some(window_around_now())).await.unwrap();
    let (pid, labels) = admitted_labels(&plane, suggestion()).await;
    assert_eq!(labels.get("kernel.admission").map(String::as_str), Some("quiet_hours"));
    let calls = plane._world.model_calls();

    // Turning quiet time off releases the held work at once, not when the old window ends.
    plane.operator().set_quiet_time("default", None).await.unwrap();
    let started = tokio::time::Instant::now();
    while !plane.kernel.processes().get(&pid).await.unwrap().unwrap().run.state.is_terminal() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the held work runs once quiet time is off"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(plane._world.model_calls(), calls + 1);
    plane.kernel.stop().await.unwrap();
}

// ---------------------------------------------------------------------------------------
// K4: while you were away
// ---------------------------------------------------------------------------------------

/// One night: two routines finish, one fails, a suggestion, a decision, a question, spend,
/// and a suggestion's work that started inside quiet time; wakes and other rows are no
/// lines.
async fn one_night(plane: &Plane) {
    let agent = |kind| Activity::new(kind, "finance");
    plane.row(agent(LedgerKind::Process).with_effect("completed")).await;
    plane.row(Activity::new(LedgerKind::Wake, "kernel").with_effect("woke")).await;
    plane.row(agent(LedgerKind::Process).with_effect("completed")).await;
    plane.row(agent(LedgerKind::Process).with_effect("failed")).await;
    plane.row(Activity::new(LedgerKind::Process, "kernel").with_effect("completed")).await;
    plane.row(Activity::new(LedgerKind::Proposal, "kernel").with_effect("proposed")).await;
    // A person's answer reaches the ledger as its audit record's row, as the harness's
    // `LedgerAuditTap` writes it.
    let answer = AuditRecordDraft::new(AuditRecordKind::Approval, "send_email")
        .decision(AuditDecision::Approved);
    plane.row(activity_for_audit(&answer).unwrap()).await;
    plane.row(Activity::new(LedgerKind::Observation, "kernel")).await;
    plane.row(agent(LedgerKind::Spend).with_count("micro_usd", 400_000)).await;
    plane.row(agent(LedgerKind::Spend).with_count("micro_usd", 220_000)).await;
    plane.row(agent(LedgerKind::ToolCall).with_effect("read_only")).await;
}

/// The digest counts each kind of line, sums the spend, links each line to its record, and
/// moves on only once the viewer marks it seen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn away_counts_spends_links_and_moves_on_once_seen() {
    let plane = Plane::start(Setup::default()).await;
    one_night(&plane).await;
    let operator = plane.operator();
    let page = operator.away(&AwayQuery::default()).await.unwrap();
    assert_eq!(page.viewer, "home");
    assert_eq!(
        page.lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
        [
            AwayKind::Done,
            AwayKind::Done,
            AwayKind::Failed,
            AwayKind::Suggestion,
            AwayKind::Decided,
            AwayKind::Question
        ]
    );
    assert_eq!(page.lines[4].effect.as_deref(), Some("approve"), "the answer says which it was");
    assert_eq!(
        (
            page.counts.done,
            page.counts.failed,
            page.counts.suggestions,
            page.counts.decided,
            page.counts.questions
        ),
        (2, 1, 1, 1, 1)
    );
    assert_eq!(page.spent_micro_usd, 620_000);
    assert!(page.quiet_time.respected);
    assert_eq!((page.through, page.more), (Some(11), false));
    let record = operator.away_line(page.lines[2].seq).await.unwrap();
    assert_eq!(page.lines[2].record, format!("/api/kernel/away/lines/{}", page.lines[2].seq));
    assert_eq!(
        (record.entry.kind.as_str(), record.entry.effect.as_deref()),
        ("process", Some("failed"))
    );
    assert_eq!(category(operator.away_line(99).await.unwrap_err()), ErrorCategory::NotFound);

    let again = operator.away(&AwayQuery::default()).await.unwrap();
    assert_eq!(again.lines, page.lines, "nothing moves until it is marked seen");
    let cursor = operator.mark_away_seen(None, 11).await.unwrap();
    assert_eq!(cursor.seq, 11);
    let after = operator.away(&AwayQuery::default()).await.unwrap();
    assert!(after.lines.is_empty());
    assert_eq!((after.after, after.through, after.spent_micro_usd), (Some(11), Some(11), 0));
    assert_eq!(after.since, cursor.seen_at);

    plane.row(Activity::new(LedgerKind::Governance, "kernel").with_effect("frozen")).await;
    let next = operator.away(&AwayQuery::default()).await.unwrap();
    assert_eq!(
        next.lines.iter().map(|line| (line.seq, line.kind)).collect::<Vec<_>>(),
        [(12, AwayKind::StoppedAll)]
    );
    let back = operator.mark_away_seen(None, 3).await.unwrap();
    assert_eq!(back.seq, 11, "a cursor never moves back");
    let error = operator.mark_away_seen(None, 40).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::InvalidInput, "no cursor passes the last row");
}

/// A suggestion's work that started inside quiet time is a breach: quiet time was not
/// respected, and the digest names the row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn away_says_when_quiet_time_was_not_respected() {
    let plane = Plane::start(Setup::default()).await;
    let operator = plane.operator();
    operator.set_quiet_time("default", Some(window_around_now())).await.unwrap();
    plane.row(Activity::new(LedgerKind::Proposal, "kernel").with_effect("running")).await;
    let outside = Utc::now() - chrono::Duration::hours(3);
    plane
        .row(Activity::new(LedgerKind::Proposal, "kernel").with_effect("running").at(outside))
        .await;
    let page = operator.away(&AwayQuery::default()).await.unwrap();
    assert!(!page.quiet_time.respected);
    assert_eq!(page.quiet_time.breaches, [1]);
}

/// Each device reads and marks its own cursor: one device marking seen leaves the other's
/// lines, and a device cannot read as another viewer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_device_moves_its_own_cursor() {
    let plane = Plane::start(Setup::default()).await;
    one_night(&plane).await;
    let (phone, phone_id) = plane.device(&[DeviceScope::Read]).await;
    let (tablet, tablet_id) = plane.device(&[DeviceScope::Read, DeviceScope::Decide]).await;
    let page = phone
        .away(&AwayQuery { viewer: Some("home".into()), ..AwayQuery::default() })
        .await
        .unwrap();
    assert_eq!(page.viewer, format!("device:{phone_id}"), "a device reads as itself");
    let cursor = phone.mark_away_seen(Some("home"), page.through.unwrap()).await.unwrap();
    assert_eq!(cursor.viewer, format!("device:{phone_id}"));
    assert!(phone.away(&AwayQuery::default()).await.unwrap().lines.is_empty());
    let other = tablet.away(&AwayQuery::default()).await.unwrap();
    assert_eq!((other.viewer, other.lines.len()), (format!("device:{tablet_id}"), 6));
    assert_eq!(plane.operator().away(&AwayQuery::default()).await.unwrap().lines.len(), 6);
    tablet.away_line(page.lines[0].seq).await.expect("a device with read follows a line's record");

    let (stopper, _) = plane.device(&[DeviceScope::Stop]).await;
    assert_eq!(
        category(stopper.away(&AwayQuery::default()).await.unwrap_err()),
        ErrorCategory::Forbidden
    );
    assert_eq!(
        category(stopper.mark_away_seen(None, 1).await.unwrap_err()),
        ErrorCategory::Forbidden
    );
    let named = plane
        .operator()
        .away(&AwayQuery { viewer: Some("no spaces".into()), ..AwayQuery::default() })
        .await;
    assert_eq!(category(named.unwrap_err()), ErrorCategory::InvalidInput);
}

/// The cursor and the ledger outlive the kernel: rows read before a restart and marked seen
/// are not shown again, rows written after it are, and a page read but not marked is read
/// again in full.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn away_is_exact_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("harness.db").display());
    let stores = || async {
        let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
        let control = SqliteControlStore::new(pool.clone());
        control.migrate().await.unwrap();
        let ledger = SqliteLedgerStore::new(pool);
        ledger.migrate().await.unwrap();
        (Arc::new(control) as Arc<dyn ControlStore>, Arc::new(ledger) as Arc<dyn LedgerStore>)
    };
    let start = |(store, ledger)| Plane::start(Setup { store, ledger, ..Setup::default() });

    let plane = start(stores().await).await;
    one_night(&plane).await;
    let first = plane.operator().away(&AwayQuery::default()).await.unwrap();
    plane.operator().mark_away_seen(None, first.through.unwrap()).await.unwrap();
    plane.row(Activity::new(LedgerKind::Process, "code").with_effect("completed")).await;
    let unmarked = plane.operator().away(&AwayQuery::default()).await.unwrap();
    plane.plane.shutdown().await.unwrap();

    let plane = start(stores().await).await;
    let again = plane.operator().away(&AwayQuery::default()).await.unwrap();
    assert_eq!(again.lines, unmarked.lines, "a page not marked seen is read again in full");
    plane.row(Activity::new(LedgerKind::Observation, "kernel")).await;
    let page = plane.operator().away(&AwayQuery::default()).await.unwrap();
    let seen: Vec<u64> = first.lines.iter().chain(&page.lines).map(|line| line.seq).collect();
    let mut unique = seen.clone();
    unique.dedup();
    assert_eq!(seen, unique, "no line twice");
    assert_eq!(
        page.lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
        [AwayKind::Done, AwayKind::Question]
    );
}

// ---------------------------------------------------------------------------------------
// K5: memory and record
// ---------------------------------------------------------------------------------------

/// The memory lists the caller's facts with provenance and the tidy-ups; "That's right",
/// "Fix it", "Forget it" and "Undo" reach the book and are recorded by id, never by text.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_lists_provenance_and_takes_the_persons_corrections() {
    let setup = Setup {
        control: Setup::default().control.with_extractor(Arc::new(ViewerExtractor)),
        ..Setup::default()
    };
    let plane = Plane::start(setup).await;
    let owner = ControlClient::http(plane.base()).with_bearer_token("owner");
    let memory = owner.memory(None).await.unwrap();
    assert_eq!(
        memory.facts.iter().map(|fact| fact.origin).collect::<Vec<_>>(),
        [FactOrigin::YouSaidThis, FactOrigin::YouConfirmedThis, FactOrigin::Guess]
    );
    assert_eq!(memory.facts[2].noticed_by.as_deref(), Some("finance"));
    assert_eq!(owner.memory(Some("work")).await.unwrap().facts.len(), 1);
    assert_eq!(memory.tidy_ups.iter().filter(|tidy_up| tidy_up.undoable()).count(), 1);

    assert_eq!(owner.confirm_fact("f3").await.unwrap().origin, FactOrigin::YouConfirmedThis);
    let fixed = owner.fix_fact("f2", "Your accountant is [OTHER].").await.unwrap();
    assert_eq!(
        (fixed.text.as_str(), fixed.origin),
        ("Your accountant is [OTHER].", FactOrigin::YouConfirmedThis)
    );
    owner.forget_fact("f1").await.unwrap();
    assert_eq!(owner.memory(None).await.unwrap().facts.len(), 2);
    let undo = owner.undo_tidy_up(None).await.unwrap();
    assert_eq!(
        (undo.id.as_str(), undo.restored.len()),
        ("t2", 2),
        "the latest tidy-up that can be undone"
    );
    let error = owner.undo_tidy_up(None).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::NotFound, "nothing is left to undo");
    assert_eq!(category(owner.forget_fact("nope").await.unwrap_err()), ErrorCategory::NotFound);

    for (action, target) in [
        ("memory.confirm", "f3"),
        ("memory.fix", "f2"),
        ("memory.forget", "f1"),
        ("memory.undo", "t2"),
    ] {
        let records = plane.controls(action);
        assert_eq!(records[0]["target"], target, "{action}");
        assert!(!records[0].to_string().contains("accountant"), "{action} keeps no fact text");
    }
}

/// A device with `read` reads the memory and corrects nothing; a caller without the
/// `operator` scope reads nothing; a kernel without a memory book says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_person_corrects_the_memory() {
    let plane = Plane::start(Setup::default()).await;
    let (device, _) = plane.device(&DeviceScope::ALL).await;
    device.memory(None).await.expect("a device with read reads the memory");
    for refused in [
        device.confirm_fact("f3").await.map(|_| ()).unwrap_err(),
        device.fix_fact("f3", "x").await.map(|_| ()).unwrap_err(),
        device.forget_fact("f3").await.unwrap_err(),
        device.undo_tidy_up(None).await.map(|_| ()).unwrap_err(),
    ] {
        assert_eq!(category(refused), ErrorCategory::Forbidden);
    }
    assert!(plane.controls("memory.forget").is_empty());

    let setup = Setup {
        control: Setup::default().control.with_extractor(Arc::new(ViewerExtractor)),
        notebook: None,
        ..Setup::default()
    };
    let guarded = Plane::start(setup).await;
    let viewer = ControlClient::http(guarded.base()).with_bearer_token("viewer");
    assert_eq!(category(viewer.memory(None).await.unwrap_err()), ErrorCategory::Forbidden);
    let owner = ControlClient::http(guarded.base()).with_bearer_token("owner");
    assert_eq!(
        category(owner.memory(None).await.unwrap_err()),
        ErrorCategory::InvalidInput,
        "no memory book"
    );
}

/// The record's lines and its check come from the book, to a device with `read` too; a
/// check that fails says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_record_and_its_check_reach_every_reader() {
    let plane = Plane::start(Setup::default()).await;
    let lines = plane.operator().record(Some(2)).await.unwrap();
    assert_eq!(lines.iter().map(|line| line.seq).collect::<Vec<_>>(), [3, 2]);
    assert_eq!(lines[0].channel.as_deref(), Some("telegram"));
    let (device, _) = plane.device(&[DeviceScope::Read]).await;
    let check = device.check_record().await.unwrap();
    assert!(check.valid && check.problem.is_none());
    assert_eq!(check.entries, 3);
    let (stopper, _) = plane.device(&[DeviceScope::Stop]).await;
    assert_eq!(category(stopper.record(None).await.unwrap_err()), ErrorCategory::Forbidden);

    let broken = Plane::start(Setup { record_valid: false, ..Setup::default() }).await;
    let check = broken.operator().check_record().await.unwrap();
    assert!(!check.valid);
    assert!(check.problem.unwrap().contains("checkpoint"));
}
