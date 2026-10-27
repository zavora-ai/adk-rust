//! Agents a person adds, through the engine (spec 3.3 §5.7, §7.2): the roster, the commands
//! with their preview and digest, who may change agents, threads with an agent, the mode cap,
//! removal, the suggestion's card and the web routes.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApproverSelector, DurableRunStore, GovernanceControl, InMemoryApprovalStore, InMemoryRunStore,
    Llm, RunFilter, StoreApprovalService, ToolConfirmationRequest,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::agents::{
    AgentAssessment, AgentCeiling, AgentChoices, AgentFactory, AgentRecord, HostedAgent,
    ModelChoice,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::stores::InMemoryAgentStore;
use adk_kernel::{InMemoryProcessStore, KernelConfig, ProcessStore};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::{
    AgentDraft, AgentPreviewRequest, AgentStatus, Answer, Caller, Command, CommandOutcome, FileUse,
    LocalSource, Mode, Origin, ShellUse, SurfaceEngine, SurfaceKind, UsesDraft,
};
use async_trait::async_trait;
use common::{OWNER, eventually, web};

const APP: &str = "adk-rust";
use serde_json::json;

fn scripted() -> Arc<dyn Llm> {
    let replies: Vec<ScriptedResponse> = (0..32).map(|_| ScriptedResponse::text("done")).collect();
    Arc::new(ScriptedLlm::new("scripted", replies))
}

/// Builds each added agent as `adk-rust up` does: a harness lowered from its record, on the
/// kernel's sessions and kill switch.
struct Factory {
    dir: std::path::PathBuf,
    sessions: Arc<dyn SessionService>,
    control: GovernanceControl,
}

#[async_trait]
impl AgentFactory for Factory {
    fn assess(&self, record: &AgentRecord) -> AgentAssessment {
        adk_harness::assess_agent(record)
    }

    async fn ceiling(
        &self,
        _: &adk_core::AgentId,
        _: &str,
    ) -> adk_kernel::Result<Option<AgentCeiling>> {
        Ok(Some(AgentCeiling::new(
            adk_kernel::PartitionMode::Ask,
            adk_kernel::agents::AgentUses::default()
                .with_files(adk_kernel::agents::FileAccess::ReadWrite)
                .with_web(adk_kernel::agents::WebAccess::Any),
        )))
    }

    async fn choices(&self) -> adk_kernel::Result<AgentChoices> {
        Ok(AgentChoices::default().with_model(ModelChoice::new("anthropic", "claude-sonnet-5-5")))
    }

    async fn build(&self, record: &AgentRecord) -> adk_kernel::Result<HostedAgent> {
        let agent = Arc::new(
            HarnessAgent::builder("harness")
                .model(scripted())
                .workspace(adk_devtools::Workspace::new(&self.dir))
                .project_instructions(InstructionsConfig {
                    user_file: None,
                    require_trust: false,
                    ..InstructionsConfig::default()
                })
                .without_compaction()
                .mode(HarnessMode::Ask)
                .added_agent(record)
                .build()
                .map_err(|error| adk_kernel::KernelError::Config { reason: error.to_string() })?,
        );
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(Arc::clone(&self.sessions))
                    .governance(self.control.clone()),
            )
            .build()
            .map_err(|error| adk_kernel::KernelError::Config { reason: error.to_string() })?;
        let harness = Harness::with_runner(agent, runner)
            .map_err(|error| adk_kernel::KernelError::Config { reason: error.to_string() })?;
        Ok(harness.hosted_agent())
    }

    async fn retired(&self, _: &AgentRecord) -> adk_kernel::Result<u64> {
        Ok(0)
    }

    fn reserved_names(&self) -> Vec<String> {
        vec!["ADK-Rust".into()]
    }
}

/// The kernel's agent store, which a test can make fail as a backend that times out does.
#[derive(Default)]
struct Store {
    inner: InMemoryAgentStore,
    failing: std::sync::atomic::AtomicBool,
}

impl Store {
    fn check(&self) -> adk_kernel::Result<()> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(adk_kernel::KernelError::Store {
                backend: "test",
                reason: "timed out".into(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl adk_kernel::agents::AgentStore for Store {
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> adk_kernel::Result<bool> {
        self.check()?;
        self.inner.put_if_version(record, expected).await
    }

    async fn get(&self, id: &str) -> adk_kernel::Result<Option<AgentRecord>> {
        self.check()?;
        self.inner.get(id).await
    }

    async fn list(&self) -> adk_kernel::Result<Vec<AgentRecord>> {
        self.check()?;
        self.inner.list().await
    }

    async fn delete(&self, id: &str) -> adk_kernel::Result<bool> {
        self.check()?;
        self.inner.delete(id).await
    }
}

struct World {
    _dir: tempfile::TempDir,
    kernel: adk_kernel::Kernel,
    /// The kernel's agent table through a handle, as `adk-rust up` gives the terminal's
    /// commands.
    handle: adk_kernel::agents::AgentsHandle,
    store: Arc<Store>,
    runs: Arc<dyn DurableRunStore>,
    service: Arc<dyn ApprovalService>,
    engine: SurfaceEngine,
    harness: Arc<Harness>,
    audit: Arc<adk_core::audit_log::InMemoryAuditRecorder>,
}

/// A kernel hosting a primary harness, with an agent store and a factory of added agents, and
/// the engine over it, as `adk-rust up` builds them.
async fn world() -> World {
    world_with(scripted()).await
}

/// [`world`] with the primary agent on `model`.
async fn world_with(model: Arc<dyn Llm>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(
                ApprovalPolicy::new([
                    ApproverSelector::group("owner"),
                    ApproverSelector::group("operator"),
                ])
                .allow_self_approval(),
            ),
    );
    let audit = Arc::new(adk_core::audit_log::InMemoryAuditRecorder::new());
    let agent = Arc::new(
        HarnessAgent::builder(APP)
            .audit_log(audit.clone())
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .session_stamp(adk_harness::PRIMARY_AGENT_STAMP)
            .approval_service(Arc::clone(&service))
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(Arc::clone(&sessions)),
        )
        .build()
        .unwrap();
    let harness =
        Arc::new(Harness::with_runner(agent, runner).unwrap().with_run_store(Arc::clone(&runs)));
    let factory = Factory {
        dir: dir.path().to_path_buf(),
        sessions: Arc::clone(&sessions),
        control: harness.runner().governance().clone(),
    };
    let store = Arc::new(Store::default());
    let handle = adk_kernel::agents::AgentsHandle::new();
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_millis(600))
                        .with_poll_interval(Duration::from_millis(20)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .agent_store(Arc::clone(&store) as Arc<dyn adk_kernel::agents::AgentStore>)
        .agents_handle(handle.clone())
        .agent_factory(Arc::new(factory))
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .harness(Arc::clone(&harness))
        .kernel(kernel.clone(), APP)
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .agent_name(APP, "ADK-Rust")
        .poll_interval(Duration::from_millis(20))
        .build();
    World { _dir: dir, kernel, handle, store, runs, service, engine, harness, audit }
}

fn finance() -> AgentDraft {
    let mut draft =
        AgentDraft::new("Finance", "default").with_mode(Mode::AskFirst).with_uses(UsesDraft {
            files: FileUse::Read,
            folder: Some("Invoices".into()),
            ..UsesDraft::default()
        });
    draft.description = "Reads invoices and drafts payment reminders".into();
    draft
}

async fn add(
    world: &World,
    caller: &Caller,
    draft: AgentDraft,
) -> adk_surface::Result<CommandOutcome> {
    let preview = world
        .engine
        .preview_agent(caller, AgentPreviewRequest::Create { draft: draft.clone(), replaces: None })
        .await?;
    world
        .engine
        .execute(
            caller,
            Command::AddAgent { draft: Box::new(draft), digest: preview.digest, replaces: None },
        )
        .await
}

fn terminal() -> Caller {
    Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard)
}

/// An agent added in the home is in the roster as Ready before it runs, with its own colour,
/// area and mode; paused it is Paused, and only the home or a paired local terminal resumes it
/// (spec 3.3 §5.7 "The roster", S-I10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_added_agent_is_ready_then_paused_in_the_roster() {
    let world = world().await;
    let CommandOutcome::AgentAdded { agent } = add(&world, &web(), finance()).await.unwrap() else {
        panic!("not added");
    };
    assert_eq!(agent.id, "finance");
    assert_eq!(agent.origin_text.split(" on ").next(), Some("Added by you"));
    let rows = world.engine.agents(&web()).await.unwrap();
    let row = rows.iter().find(|row| row.agent.name == "finance").expect("Finance is listed");
    assert_eq!((row.status, row.status_text.as_str()), (AgentStatus::Ready, "Ready"));
    assert!(row.added && row.can_change);
    assert_eq!(row.area.as_deref(), Some("Home"));
    assert_eq!(row.mode.as_ref().map(|mode| mode.status.as_str()), Some("Asks first"));
    assert_eq!(row.agent.avatar.color, agent.agent.avatar.color);

    let paused = world
        .engine
        .execute(&web(), Command::PauseAgent { agent: "finance".into() })
        .await
        .unwrap();
    assert!(matches!(paused, CommandOutcome::AgentPaused { .. }));
    let rows = world.engine.agents(&web()).await.unwrap();
    let row = rows.iter().find(|row| row.agent.name == "finance").unwrap();
    assert_eq!((row.status, row.status_text.as_str()), (AgentStatus::Paused, "Paused"));
    let refused =
        world.engine.execute(&terminal(), Command::ResumeAgent { agent: "finance".into() }).await;
    assert_eq!(refused.unwrap_err().code(), "surface.not_here");
    let resumed = world
        .engine
        .execute(&terminal().with_paired_local(), Command::ResumeAgent { agent: "finance".into() })
        .await
        .unwrap();
    assert!(matches!(resumed, CommandOutcome::AgentResumed { .. }));
    world.kernel.stop().await.unwrap();
}

/// Only a person at the home or the terminal changes agents: never an agent, chat, voice, the
/// tray, a phone or a paired device (spec 3.3 §5.7, §7.2, S-I11).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_a_person_at_the_home_or_the_terminal_changes_agents() {
    let world = world().await;
    let refusals = [
        Caller::human(OWNER, SurfaceKind::Web, Origin::Agent { agent: "adk-rust".into() }),
        Caller::human(OWNER, SurfaceKind::Phone, Origin::Touch),
        Caller::human(OWNER, SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() }),
        Caller::human(OWNER, SurfaceKind::Web, Origin::Voice),
        Caller::human(OWNER, SurfaceKind::Tray, Origin::Pointer),
        terminal().with_device("phone-1"),
    ];
    for caller in &refusals {
        let error = add(&world, caller, finance()).await.unwrap_err();
        assert_eq!(error.code(), "surface.not_here", "{caller:?}");
        assert!(!world.engine.can_add_agents(caller), "{caller:?}");
    }
    assert!(world.engine.can_add_agents(&web()));
    assert!(world.engine.agent(&web(), "finance").await.is_err(), "nothing was added");
    world.kernel.stop().await.unwrap();
}

/// A high-risk agent needs presence: confirmed in the home, or the paired local terminal
/// (spec 3.3 §7.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_high_risk_agent_needs_presence() {
    let world = world().await;
    let host = finance().with_uses(UsesDraft { shell: ShellUse::Host, ..UsesDraft::default() });
    let preview = world
        .engine
        .preview_agent(&web(), AgentPreviewRequest::Create { draft: host.clone(), replaces: None })
        .await
        .unwrap();
    assert_eq!(preview.risk_text, "High risk");
    assert!(preview.needs_presence);
    assert!(
        preview.abilities.iter().any(|ability| ability == "It can run commands on this computer.")
    );
    assert!(add(&world, &web(), host.clone()).await.is_err());
    let added = add(&world, &web().with_presence_confirmed(), host).await.unwrap();
    assert!(matches!(added, CommandOutcome::AgentAdded { .. }));
    world.kernel.stop().await.unwrap();
}

/// A change against an older version is shown again with the agent as it is now, never
/// applied (spec 3.3 §5.7 "Changing an agent").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_against_an_older_version_is_out_of_date() {
    let world = world().await;
    add(&world, &web(), finance()).await.unwrap();
    let seen = world.engine.agent(&web(), "finance").await.unwrap();
    let mut bills = seen.draft.clone();
    bills.display_name = "Bills".into();
    let preview = world
        .engine
        .preview_agent(
            &web(),
            AgentPreviewRequest::Change {
                agent: "finance".into(),
                version: seen.version,
                draft: bills.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(preview.fields, ["displayName"]);
    let saved = world
        .engine
        .execute(
            &web(),
            Command::ChangeAgent {
                agent: "finance".into(),
                version: seen.version,
                draft: Box::new(bills.clone()),
                digest: preview.digest.clone(),
            },
        )
        .await
        .unwrap();
    let CommandOutcome::AgentChanged { agent } = saved else { panic!("{saved:?}") };
    assert_eq!(agent.agent.display_name, "Bills");
    let mut again = bills;
    again.mode = Mode::Suggest;
    let stale = world
        .engine
        .execute(
            &web(),
            Command::ChangeAgent {
                agent: "finance".into(),
                version: seen.version,
                draft: Box::new(again),
                digest: preview.digest,
            },
        )
        .await
        .unwrap();
    let CommandOutcome::AgentOutOfDate { agent, text } = stale else { panic!("{stale:?}") };
    assert_eq!(agent.version, seen.version + 1);
    // Wave 3 finding 37: it names where the change was made.
    assert!(text.starts_with("Bills was changed in the home app at"), "{text}");
    world.kernel.stop().await.unwrap();
}

async fn targets(runs: &Arc<dyn DurableRunStore>, session: &str) -> Vec<String> {
    let mut found: Vec<(chrono::DateTime<chrono::Utc>, String)> = runs
        .list(RunFilter::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.session_id.as_deref() == Some(session))
        .map(|run| (run.created_at, run.target))
        .collect();
    found.sort();
    found.into_iter().map(|(_, target)| target).collect()
}

/// A new thread with an agent runs on it; the thread's later messages follow it, and another
/// thread goes to the primary agent (spec 3.3 §5.7 "Threads").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_with_an_agent_stays_with_it() {
    let world = world().await;
    add(&world, &web(), finance()).await.unwrap();
    world
        .engine
        .execute(
            &web(),
            Command::NewThread {
                agent: "finance".into(),
                session_id: "t-finance".into(),
                text: "hello".into(),
            },
        )
        .await
        .unwrap();
    eventually("the first run", Duration::from_secs(10), || async {
        let found = targets(&world.runs, "t-finance").await;
        (found.len() == 1).then_some(())
    })
    .await;
    // The agent stamped its thread.
    let stamp = world.engine.source().agent_of_session(OWNER, "t-finance").await.unwrap();
    assert_eq!(stamp.as_deref(), Some("finance"));
    eventually("the first run done", Duration::from_secs(10), || async {
        (!world.engine.source().busy(OWNER, "t-finance").await.unwrap()).then_some(())
    })
    .await;
    world
        .engine
        .execute(&web(), Command::Send { session_id: "t-finance".into(), text: "again".into() })
        .await
        .unwrap();
    world
        .engine
        .execute(&web(), Command::Send { session_id: "t-other".into(), text: "hi".into() })
        .await
        .unwrap();
    let finance_runs = eventually("both runs of the thread", Duration::from_secs(10), || async {
        let found = targets(&world.runs, "t-finance").await;
        (found.len() == 2).then_some(found)
    })
    .await;
    assert_eq!(finance_runs, ["finance", "finance"]);
    let other = eventually("the other thread's run", Duration::from_secs(10), || async {
        let found = targets(&world.runs, "t-other").await;
        (!found.is_empty()).then_some(found)
    })
    .await;
    assert_eq!(other, [APP]);
    world.kernel.stop().await.unwrap();
}

/// In an added agent's own thread the mode can only make it ask more (spec 3.3 §5.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mode_in_an_agents_thread_never_loosens_past_its_own() {
    let world = world().await;
    add(&world, &web(), finance().with_mode(Mode::Suggest)).await.unwrap();
    world
        .engine
        .execute(
            &web(),
            Command::NewThread {
                agent: "finance".into(),
                session_id: "t-f".into(),
                text: "hello".into(),
            },
        )
        .await
        .unwrap();
    eventually("the stamp", Duration::from_secs(10), || async {
        world
            .engine
            .source()
            .agent_of_session(OWNER, "t-f")
            .await
            .unwrap()
            .filter(|agent| agent == "finance")
    })
    .await;
    let looser = world
        .engine
        .execute(&web(), Command::SetMode { session_id: "t-f".into(), mode: Mode::AskFirst })
        .await
        .unwrap_err();
    assert_eq!(looser.code(), "surface.not_here");
    assert_eq!(looser.to_string(), "Finance suggests at most. Change it in Finance's settings.");
    let stricter = world
        .engine
        .execute(&web(), Command::SetMode { session_id: "t-f".into(), mode: Mode::JustLook })
        .await
        .unwrap();
    assert!(matches!(stricter, CommandOutcome::ModeSet { .. }));
    world.kernel.stop().await.unwrap();
}

/// Removing names what ends, then the agent leaves the roster (spec 3.3 §5.7, S-I16).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_an_agent_names_what_ends() {
    let world = world().await;
    add(&world, &web(), finance()).await.unwrap();
    // It has run, so its finished run is a roster row until it is removed.
    world
        .engine
        .execute(
            &web(),
            Command::NewThread {
                agent: "finance".into(),
                session_id: "t-finance".into(),
                text: "hello".into(),
            },
        )
        .await
        .unwrap();
    eventually("its run done", Duration::from_secs(10), || async {
        let ran = !targets(&world.runs, "t-finance").await.is_empty();
        (ran && !world.engine.source().busy(OWNER, "t-finance").await.unwrap()).then_some(())
    })
    .await;
    let seen = world.engine.agent(&web(), "finance").await.unwrap();
    let preview = world
        .engine
        .preview_agent(
            &web(),
            AgentPreviewRequest::Remove { agent: "finance".into(), version: seen.version },
        )
        .await
        .unwrap();
    let ended = preview.ended.clone().expect("what removing ends");
    assert_eq!(ended.text, "Removing Finance ends nothing that is running. Its threads stay.");
    let removed = world
        .engine
        .execute(
            &web(),
            Command::RemoveAgent {
                agent: "finance".into(),
                version: seen.version,
                digest: preview.digest,
            },
        )
        .await
        .unwrap();
    assert!(matches!(removed, CommandOutcome::AgentRemoved { .. }));
    let rows = world.engine.agents(&web()).await.unwrap();
    assert!(!rows.iter().any(|row| row.agent.name == "finance"), "{rows:?}");
    // Wave 3 findings 10, 11 and 39: its threads stay readable under "Removed agents".
    let removed = world.engine.removed_agents(&web()).await.unwrap();
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert_eq!(removed[0].agent.display_name, "Finance");
    assert!(removed[0].removed_text.starts_with("Removed on "), "{removed:?}");
    let threads: Vec<&str> =
        removed[0].threads.iter().map(|thread| thread.session_id.as_str()).collect();
    assert_eq!(threads, ["t-finance"]);
    world.kernel.stop().await.unwrap();
}

/// Wave 3 findings 12 and 13: copying one of the person's rules to an agent goes through the
/// harness, which records `harness.rule.created` before the rule is stored; a suggestion,
/// never added, gets no rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copying_a_rule_to_an_agent_is_recorded() {
    let world = world().await;
    add(&world, &web(), finance()).await.unwrap();
    let rule = adk_harness::StandingRule {
        id: "rule-read".into(),
        scope: adk_harness::RuleScope::User {
            app_name: APP.into(),
            user_id: OWNER.into(),
            subject_kind: adk_core::SubjectKind::Human,
        },
        tool: "read_file".into(),
        origin: adk_harness::OriginPin::exact(&adk_core::ToolOrigin::new(
            "static",
            &json!({ "name": "read_file" }),
            None,
        )),
        agent: None,
        predicates: vec![],
        decision: adk_harness::StandingDecision::Allow,
        created_by: adk_core::SubjectRef::human(OWNER),
        created_at: chrono::Utc::now(),
        expires_at: None,
        label: "Reading files".into(),
        source_fingerprint: None,
    };
    world.harness.agent().runtime().standing_rules().grant(rule).await.unwrap();
    let copied = world
        .engine
        .execute(
            &web(),
            Command::CopyRuleToAgent { agent: "finance".into(), rule_id: "rule-read".into() },
        )
        .await
        .unwrap();
    assert!(matches!(copied, CommandOutcome::RuleCopied { .. }), "{copied:?}");
    let created: Vec<_> = world
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == "harness.rule.created")
        .collect();
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0].ext["surface"], "web");
    world.kernel.stop().await.unwrap();
}

/// Wave 3 findings 3 and 38: "Always allow" on an added agent's card makes a rule for that
/// agent alone, also from a source that reaches the kernel's agent table only through a
/// handle — the one `adk-rust up` serves an attached terminal's commands with; when the table
/// cannot be read, no rule is offered or made, rather than one for every agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_on_an_added_agents_card_pins_the_rule_to_it() {
    let model: Arc<dyn Llm> = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "Invoices/a.txt", "content": "INV-1" }),
            ),
            ScriptedResponse::text("Written."),
        ],
    ));
    let world = world_with(model).await;
    add(&world, &web(), finance()).await.unwrap();
    world
        .engine
        .execute(&web(), Command::Send { session_id: "s-hold".into(), text: "write it".into() })
        .await
        .unwrap();
    let approvals = Arc::clone(world.harness.agent().runtime().approval_store());
    let scope = ApprovalScope::new(APP, OWNER, "s-hold");
    let held = eventually("the held write", Duration::from_secs(20), || async {
        approvals.pending(&scope).await.unwrap().into_iter().next()
    })
    .await;
    // The same call, as Finance makes it.
    let mut call = held.request.clone();
    call.acting = Some(Box::new(adk_core::ActingContext::derived(
        APP,
        "finance",
        OWNER,
        adk_core::SubjectKind::Human,
    )));
    approvals.record_pending(&scope, &call).await.unwrap();
    let request = ApprovalRequest::new(
        "apr-finance",
        scope.clone(),
        &call,
        ApprovalPolicy::new([ApproverSelector::human(OWNER)]),
    )
    .with_acting(adk_core::ActingContext::derived(
        APP,
        "finance",
        OWNER,
        adk_core::SubjectKind::Human,
    ));

    // No kernel of its own: the table's handle, as `adk-rust up` builds it.
    let source = LocalSource::builder(APP)
        .sessions(Arc::new(InMemorySessionService::new()))
        .harness(Arc::clone(&world.harness))
        .agents(world.handle.clone())
        .build()
        .unwrap();
    use adk_surface::Source as _;
    assert!(source.always_allow_offer(&request).await.is_some());
    source.prepare_always_allow(&terminal(), &request).await.unwrap().commit().await.unwrap();
    let rules = world.harness.agent().runtime().standing_rules().list(APP, OWNER).await.unwrap();
    assert_eq!(
        rules.iter().map(|rule| rule.agent.as_deref()).collect::<Vec<_>>(),
        [Some("urn:adk:agent:adk-rust/finance")],
        "{rules:?}"
    );

    // The agent table cannot be read: nothing is offered, and nothing is made.
    world.store.failing.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(source.always_allow_offer(&request).await.is_none());
    assert!(source.prepare_always_allow(&terminal(), &request).await.is_err());
    world.store.failing.store(false, std::sync::atomic::Ordering::SeqCst);
    world.kernel.stop().await.unwrap();
}

/// Wave 3 finding 35: a suggestion approved in the web home is added as the web home, so its
/// record names where the person said yes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suggestion_approved_in_the_home_is_added_as_the_home() {
    let world = world().await;
    let primary = adk_core::AgentPrincipal::derived(APP, APP).id;
    let proposal = world
        .kernel
        .agents()
        .propose(
            finance().into_definition(),
            &adk_kernel::agents::AgentProposer::new(primary, OWNER, "s-suggest"),
        )
        .await
        .unwrap();
    let card = world.engine.decision(&web(), &proposal.request_id).await.unwrap();
    // No tick may find the answer first: the answer is applied as the surface gave it.
    world.kernel.stop().await.unwrap();
    let answered = world
        .engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(answered, CommandOutcome::Answered { .. }), "{answered:?}");
    let added = world.kernel.agents().get("finance").await.unwrap().unwrap().agent;
    assert_eq!(added.state, adk_kernel::agents::AgentRecordState::Active);
    assert_eq!(added.changed_on.as_deref(), Some("web"));
    // The web home reaches the engine over HTTP, and the record says so (spec 3.3 S-I18).
    // Fails on `origin/main`, whose record of a web home's change has no transport.
    let created = world
        .audit
        .records()
        .into_iter()
        .find(|record| record.action == adk_kernel::audit::AGENT_CREATED)
        .unwrap();
    assert_eq!(
        (&created.ext["surface"], &created.ext["how"], &created.ext["transport"]),
        (&json!("web"), &json!("pointer"), &json!("http"))
    );
}

fn suggestion(instructions: &str) -> ApprovalRequest {
    let definition = finance().into_definition().with_instructions(instructions);
    let call = ToolConfirmationRequest::new(
        adk_kernel::agents::AGENT_CREATE_TOOL_NAME,
        serde_json::to_value(&definition).unwrap(),
    );
    ApprovalRequest::new(
        "agent-finance-0123456789ab",
        ApprovalScope::new(APP, OWNER, "s-suggest"),
        &call,
        ApprovalPolicy::new([ApproverSelector::human(OWNER), ApproverSelector::group("operator")]),
    )
    .with_summary("Add an agent called Finance")
    .with_risk(ApprovalRisk::Medium)
    .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject])
}

/// The card of a suggested agent is the kernel's: its heading, the definition in full, Add
/// this agent and Don't add, never Always allow or Edit; long instructions are expanded before
/// it can be added; chat offers Don't add only (spec 3.3 §5.7, §7.2, S-I11, S-I13).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suggested_agents_card_is_the_kernels() {
    let world = world().await;
    world.service.submit(suggestion("Draft reminders; never send them.")).await.unwrap();
    let card = world
        .engine
        .decisions(&web())
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.id.starts_with("agent-finance"))
        .expect("the suggestion's card");
    assert_eq!(card.heading, "Add an agent called Finance");
    assert_eq!(card.reversibility_text, "You can remove it later");
    let view = card.adds_agent.as_ref().expect("the definition");
    assert_eq!(view.id, "finance");
    assert!(view.lines.iter().any(|line| line.value == "Reads files in Invoices"));
    assert_eq!(view.allowed_without_asking, "Nothing yet");
    let labels: Vec<(Answer, &str)> =
        card.allowed.options.iter().map(|option| (option.answer, option.label.as_str())).collect();
    assert_eq!(labels, [(Answer::AllowOnce, "Add this agent"), (Answer::Deny, "Don't add")]);
    let chat = common::telegram();
    let in_chat = world.engine.decision(&chat, &card.id).await.unwrap();
    let labels: Vec<&str> =
        in_chat.allowed.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Don't add"]);

    let long = "Read the invoices. ".repeat(200);
    let mut request = suggestion(&long);
    request.id = "agent-finance-ffffffffffff".into();
    world.service.submit(request).await.unwrap();
    let card = world.engine.decision(&web(), "agent-finance-ffffffffffff").await.unwrap();
    assert!(card.adds_agent.as_ref().unwrap().instructions_truncated);
    assert!(card.allowed.option(Answer::AllowOnce).unwrap().needs_expanded);
    world.kernel.stop().await.unwrap();
}

/// The web routes read an agent, the choices and a preview; the roster says agents can be
/// added; a change goes through the commands route (spec 3.3 §5.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_routes_read_and_preview() {
    let world = world().await;
    let api = common::api(&world.kernel, &world.engine);
    let (status, roster) = common::call(&api, "GET", "/api/surface/agents", None).await;
    assert_eq!(status, 200);
    assert_eq!(roster["canAdd"], true);
    let (status, choices) = common::call(&api, "GET", "/api/surface/agents/choices", None).await;
    assert_eq!(status, 200, "{choices}");
    assert_eq!(choices["models"][0]["model"], "claude-sonnet-5-5");
    assert_eq!(choices["colors"].as_array().unwrap().len(), 8);
    let draft = serde_json::to_value(finance()).unwrap();
    let (status, preview) = common::call(
        &api,
        "POST",
        "/api/surface/agents/preview",
        Some(json!({ "action": "create", "draft": draft })),
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["heading"], "Here is what Finance can do");
    let (status, outcome) = common::call(
        &api,
        "POST",
        "/api/surface/commands",
        Some(json!({ "command": { "type": "addAgent", "draft": draft, "digest": preview["digest"] }, "origin": { "kind": "pointer" } })),
    )
    .await;
    assert_eq!(status, 200, "{outcome}");
    assert_eq!(outcome["type"], "agentAdded");
    let (status, detail) = common::call(&api, "GET", "/api/surface/agents/finance", None).await;
    assert_eq!(status, 200);
    assert_eq!(detail["draft"]["displayName"], "Finance");
    world.kernel.stop().await.unwrap();
}
