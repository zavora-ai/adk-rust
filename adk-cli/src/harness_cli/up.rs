//! `adk-rust up`: the harness as the agent kernel, in the foreground (spec 3.2 §8.1, §8.2).
//!
//! `up` builds what `serve` builds — the approval service with `operator` routing and
//! run-store wake-ups, background runs with the harness targets, the extractor and its
//! non-loopback refusal, the `--store` backend (§6 D-9) — and adds the kernel: a worker per
//! queue class, the scheduler, the process table and the control plane on the kernel
//! home's socket and under `/api/kernel` on HTTP. The kernel keeps its content-free
//! activity ledger beside its other stores, with the subject key in the home, and runs the
//! daily baseline pass, which the harness's model words (spec 3.2 §7.10).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_cli::harness::{HarnessLauncher, KernelSettings, OPERATOR_SCOPE};
use adk_core::audit_log::{AuditRecorder, AuditTrail};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, GovernanceControl, GovernanceStateStore,
    ScheduleStore, StoreApprovalService,
};
use adk_harness::HarnessMode;
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::proc::baselines::LlmPhraser;
use adk_kernel::proc::{Ledger, LedgerKey, LedgerStore};
use adk_kernel::{KernelConfig, ProcessStore, QueueClass};
use adk_server::background::{BackgroundState, DurableWorkerConfig};
use anyhow::{Context, Result, bail};

use super::{APP_NAME, AgentExtras, Setup, agents, user_config};
use crate::ResolvedModel;
use crate::cli::UpArgs;

mod commands;
#[cfg(feature = "mobile")]
mod phone;
#[cfg(feature = "channels")]
mod telegram;
mod webhooks;

/// Where `up` listens over HTTP when neither `--bind` nor `[kernel] bind` says.
pub(super) const DEFAULT_BIND: &str = "127.0.0.1:8080";
/// Why Go ahead is refused beyond loopback without a sandbox.
const AUTO_REFUSED: &str = "edits would run unattended on this host";

/// The kernel's own stores, in the backend the harness's stores are in.
struct KernelStores {
    processes: Arc<dyn ProcessStore>,
    agenda: Arc<dyn adk_kernel::AgendaStore>,
    /// The partition table and the crossing ledger (spec 3.2 §7.8).
    partitions: Arc<dyn adk_kernel::PartitionStore>,
    schedules: Arc<dyn ScheduleStore>,
    /// Mounted drivers and their parked calls (spec 3.2 §7.7).
    drivers: Arc<dyn adk_kernel::drivers::DriverStore>,
    /// The activity ledger, its observations and pass state (spec 3.2 §7.10).
    ledger: Arc<dyn LedgerStore>,
    /// Paired devices and their waiting codes, hashed (spec 3.3 §10 K2).
    devices: Arc<dyn adk_kernel::pairing::DeviceStore>,
    /// Settings changed while the kernel runs and each device's place in "While you were
    /// away" (spec 3.3 §10 K3, K4).
    control: Arc<dyn adk_kernel::control::ControlStore>,
    /// Where the cron wakes of sleepers left off (spec 3.2 §7.4).
    watermarks: Arc<dyn adk_kernel::CronWatermarkStore>,
    /// The agents a person added (spec 3.3 §5.7, §10 K7, L1).
    agents: Arc<dyn adk_kernel::agents::AgentStore>,
    /// Whether nothing outlives the kernel (`--ephemeral`).
    ephemeral: bool,
    /// The kill switch's shared state; `None` keeps it in this process.
    governance: Option<Arc<dyn GovernanceStateStore>>,
    /// Where they are, for the banner.
    label: String,
}

/// `adk-rust up` (spec 3.2 §8.1): runs until Ctrl-C or `SIGTERM`, then drains and stops.
pub async fn up(model: ResolvedModel, instruction: Option<String>, up: UpArgs) -> Result<()> {
    #[cfg(feature = "mobile")]
    let (home_url, push_key) = (up.home_url.clone(), up.push_key.clone());
    #[cfg(all(feature = "mobile", feature = "scripted-model"))]
    let push_to_localhost = up.push_to_localhost;
    #[cfg(all(feature = "mobile", not(feature = "scripted-model")))]
    let push_to_localhost = false;
    #[cfg(feature = "genui-apps")]
    let apps_origin = up.apps_origin.clone();
    let UpArgs { home, bind, auth_token, store, mut harness, .. } = up;
    // The baseline pass's observations are worded by the harness's model (spec 3.2 §7.10).
    let phraser = Arc::new(LlmPhraser::new(model.llm.clone()));
    // The realtime models the home's voice view offers a harness marked for voice (spec 3.3
    // §5.6).
    #[cfg(all(feature = "voice", feature = "surface"))]
    let voice = crate::voice_cli::realtime_models(&model, harness.voice_script())?;
    // The personal preset's workspace is a folder under the home, so the current directory's
    // `.adk/harness.toml` plays no part (spec 3.3 §10 H1).
    let personal_home =
        harness.preset == Some(adk_harness::Preset::Personal) && harness.dir.is_none();
    // Only `max_concurrent_runs` of a workspace's [kernel] table applies, so the directory
    // the workspace resolves from is enough here.
    let mut settings = if personal_home {
        KernelSettings::resolve_user(user_config().as_deref())?
    } else {
        let workspace = match &harness.dir {
            Some(dir) => dir.clone(),
            None => std::env::current_dir().context("no current directory")?,
        };
        KernelSettings::resolve(user_config().as_deref(), &workspace)?
    };
    // The home every other command reads: `[kernel] home` in the user config, else the
    // default; `--home` is this kernel's alone.
    let shared_home = settings.home.clone().or_else(|| default_home().ok());
    let home = match home.or_else(|| shared_home.clone()) {
        Some(home) => home,
        None => default_home()?,
    };
    // The personal preset's answers live in the home, so the REPL, `code` and `serve` show
    // another `personal.toml` than this kernel keeps unless the user config names it (spec
    // 3.3 §13 Q7).
    let same = |a: &PathBuf, b: &PathBuf| {
        a.canonicalize().unwrap_or_else(|_| a.clone())
            == b.canonicalize().unwrap_or_else(|_| b.clone())
    };
    let home_note = match &shared_home {
        Some(shared)
            if harness.preset == Some(adk_harness::Preset::Personal) && !same(&home, shared) =>
        {
            Some(format!(
                "--home {} is this kernel's alone: the REPL, `code` and `serve` read the \
                 personal preset's answers in {}. Add `home = {:?}` under [kernel] in your user \
                 config so they read this kernel's",
                home.display(),
                shared.display(),
                home.display().to_string()
            ))
        }
        _ => None,
    };
    // Its own `.adk/harness.toml` may restrict the kernel as a project's does.
    if personal_home {
        let personal = adk_harness::personal::PersonalWorkspace::create(&home)?;
        settings = KernelSettings::resolve(user_config().as_deref(), personal.root())?;
        harness.dir = Some(personal.root().to_path_buf());
    }
    let bind = match bind.or(settings.bind) {
        Some(bind) => bind,
        None => DEFAULT_BIND.parse()?,
    };
    let token = auth_token.filter(|token| !token.trim().is_empty());
    // The phone at the home's public address (spec 3.3 §5.4).
    #[cfg(feature = "mobile")]
    let phone = match &home_url {
        Some(url) => {
            let mut phone = phone::Phone::open(url, push_key.as_deref(), &home, token.as_deref())?;
            if push_to_localhost {
                phone.setup = phone.setup.with_push_allowlist(
                    adk_surface::mobile::PushAllowlist::new(["localhost"])
                        .with_plain_http_to_localhost(),
                );
            }
            Some(phone)
        }
        None => None,
    };
    if !bind.ip().is_loopback() && token.is_none() {
        bail!(
            "serving the kernel on {bind} needs authentication: pass --auth-token or set \
             ADK_SERVE_TOKEN, or bind 127.0.0.1 (spec 3.2 K-I18)"
        );
    }

    // Shared with the agent factory, which builds each added agent with this composition
    // (spec 3.3 §10 L1).
    let setup = Arc::new(
        Setup::open(model, instruction, harness, HarnessMode::Plan, store.as_deref(), Some(&home))
            .await?,
    );
    // The agent table the primary agent's `propose_agent` suggests to (spec 3.3 §5.7).
    let agents_handle = adk_kernel::agents::AgentsHandle::new();
    setup.print_notes();
    setup.memory_notice();
    for note in settings.notes.iter().chain(&home_note) {
        println!("note: {note}");
    }
    // The first-run answers become the kernel's quiet hours and every partition's daily
    // spend cap (spec 3.3 §13 Q7).
    if let Some(personal) = &setup.personal {
        let user = KernelSettings::resolve_user(user_config().as_deref())?;
        apply_personal(&mut settings, &user, &personal.settings);
    }
    // Edits would run unattended on a network-reachable host: refused at start, and through
    // the home while the kernel runs (spec 3.3 §10 K3).
    let auto_refused = !bind.ip().is_loopback() && !setup.resolved.sandbox;
    if auto_refused && setup.resolved.mode == HarnessMode::Auto {
        bail!("--mode auto beyond loopback needs --sandbox: {AUTO_REFUSED}");
    }

    let (approval_store, run_store) = run_and_approval_stores(&setup);
    // Signed webhooks that wake the kernel's sleepers (spec 3.5 §10 X11).
    let hooks = webhooks::ingress(user_config().as_deref(), run_store.clone(), APP_NAME)?;
    // The Telegram chat thread, only when `TELEGRAM_BOT_TOKEN` is set (spec 3.3 §5.3).
    #[cfg(feature = "channels")]
    let chat = telegram::Chat::from_env(&setup, store.as_deref()).await?;
    // The person running the kernel is the operator: their own run's calls are theirs to
    // approve, as `serve` routes them.
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    // An agent's screen action reads as the kernel's words, low risk, Allow once · Edit · Deny
    // (spec 3.3 §2 item 7); every other call goes to the operator as before.
    #[cfg(feature = "verb-ui")]
    let routing = adk_surface::verbs::VerbRouter::catalogue(APP_NAME, routing);
    #[cfg(feature = "verb-ui")]
    let verb_approvals = approval_store.clone();
    let service = StoreApprovalService::new(approval_store)
        .with_router(routing)
        .with_run_store(run_store.clone());
    #[cfg(feature = "channels")]
    let service = match &chat {
        Some(chat) => service.with_notifier(chat.doorbell()),
        None => service,
    };
    // The phone's doorbell beside the chat's: a wake-up when a decision waits.
    #[cfg(feature = "mobile")]
    let service = match &phone {
        Some(phone) => service.with_notifier(phone.doorbell()),
        None => service,
    };
    let service: Arc<dyn ApprovalService> = Arc::new(service);
    let added_service = service.clone();
    let stores = kernel_stores(&setup, store.as_deref()).await?;
    // One kill switch for the runner, the kernel and the server, mirrored from the store
    // replicas share (K-I8).
    let governance = match &stores.governance {
        Some(state) => KernelConfig::mirrored_governance(state.clone()),
        None => GovernanceControl::new(),
    };
    let audit = kernel_audit(&home, &setup, store.as_deref()).await?;
    // The web home records its commands, and how each answer was given, on the same signed
    // log, so "You said yes in Telegram" survives a restart (spec 3.3 §11).
    #[cfg(feature = "surface")]
    let surface_records = audit.as_ref().map(|(recorder, logs, _)| {
        let answers: Arc<dyn adk_surface::AnswerLog> =
            Arc::new(adk_surface::AuditAnswerLog::new(logs.clone()));
        (AuditTrail::new(recorder.clone()), answers)
    });
    // Without a signed log, the chat thread and the home still share how each answer was
    // given while the process runs (spec 3.3 §8.1).
    #[cfg(feature = "channels")]
    let surface_records = match &chat {
        Some(_) if surface_records.is_none() => Some(telegram::answers_in_memory()),
        _ => surface_records,
    };
    let record = audit.as_ref().map(|(_, _, record)| record.clone());
    let audit = audit.map(|(recorder, _, _)| recorder);
    // Screen actions (spec 3.3 §2 item 7): the kernel's agenda through a handle bound when the
    // kernel is built, for every agent `up` hosts and for the person on every surface.
    #[cfg(feature = "verb-ui")]
    let verb_agenda = adk_kernel::AgendaHandle::new();
    #[cfg(feature = "verb-ui")]
    let verbs = {
        let processes = adk_kernel::ProcessTable::new(run_store.clone(), stores.processes.clone());
        let agenda = adk_surface::verbs::KernelAgenda::new(verb_agenda.clone(), processes);
        let mut registry = adk_surface::verbs::VerbRegistry::builder(APP_NAME, Arc::new(agenda))
            .approval_store(verb_approvals)
            .governance(governance.clone())
            .sessions(setup.sessions())
            .agent_name(APP_NAME, "ADK-Rust");
        if let Some((trail, answers)) = &surface_records {
            registry = registry.audit(trail.clone()).answers(answers.clone());
        }
        registry.build()?
    };
    if let Some(recorder) = &audit {
        governance.attach_audit(AuditTrail::new(recorder.clone()));
    }
    // Drivers (spec 3.2 §7.7): supervised MCP servers under the manifests the user mounted,
    // kept beside the kernel's other stores; their tools reach the harness with the classes
    // the manifests give them. A driver that may sample does so on the harness model,
    // metered against the run that called it.
    let sampling_model = setup.models.harness.llm.clone();
    let drivers = adk_kernel::drivers::DriverSupervisor::builder(stores.drivers.clone())
        .app_name(APP_NAME)
        .home(&home)
        .sampling_handlers(move |_| {
            Some(Arc::new(adk_tool::sampling::LlmSamplingHandler::new(sampling_model.clone()))
                as Arc<dyn adk_tool::sampling::SamplingHandler>)
        })
        .build();
    // The drivers `[kernel] drivers` lists mount when the kernel starts, each once the person
    // approves it here (spec 3.2 §7.7, §8.2, K-I13).
    crate::kernel_cli::approve_configured_drivers(&drivers, &settings.drivers, &setup.user).await?;
    // One harness per partition when `[[kernel.partitions]]` names any (spec 3.2 §7.8).
    let table = match settings.partitions.is_empty() {
        true => None,
        false => Some(
            open_partitions(&setup, &settings, stores.partitions.clone(), audit.as_ref()).await?,
        ),
    };
    let hosted = match &table {
        None => {
            let agent = Arc::new(setup.agent_with(
                setup.resolved.effort,
                APP_NAME,
                setup.resolved.mode,
                setup.resolved.sandbox,
                None,
                Some(service),
                AgentExtras {
                    budget: None,
                    audit: audit.clone(),
                    memory_with_inbox: true,
                    drivers: Some(drivers.clone()),
                    partition: None,
                    area_modes: Some((
                        stores.control.clone(),
                        adk_kernel::PartitionId::default(),
                        auto_refused,
                    )),
                    #[cfg(feature = "tui")]
                    local_console: false,
                    driver_crossings: Vec::new(),
                    agents: Some(agents_handle.clone()),
                    session_stamp: Some(adk_harness::PRIMARY_AGENT_STAMP.to_string()),
                    added: None,
                    #[cfg(feature = "verb-ui")]
                    verbs: Some((verbs.clone(), adk_kernel::PartitionId::default())),
                    #[cfg(feature = "genui")]
                    draws: true,
                },
            )?);
            vec![host(&setup, agent, None, &governance, &run_store)?]
        }
        Some(table) => {
            // Each partition's harness: its mode, identity, memory scope, crossing policy and
            // the drivers its own list names (spec 3.2 §7.7, §7.8).
            let extras = |partition: adk_kernel::Partition, coordinator: bool| AgentExtras {
                budget: None,
                audit: audit.clone(),
                memory_with_inbox: true,
                drivers: Some(drivers.clone()),
                area_modes: Some((stores.control.clone(), partition.id.clone(), auto_refused)),
                #[cfg(feature = "verb-ui")]
                verbs: Some((verbs.clone(), partition.id.clone())),
                partition: Some((partition, table.clone(), coordinator)),
                #[cfg(feature = "tui")]
                local_console: false,
                driver_crossings: Vec::new(),
                agents: Some(agents_handle.clone()),
                session_stamp: Some(adk_harness::PRIMARY_AGENT_STAMP.to_string()),
                added: None,
                #[cfg(feature = "genui")]
                draws: true,
            };
            partition_harnesses(&setup, &settings, table, &service, &governance, &run_store, extras)
                .await?
        }
    };
    let primary = &hosted[0];
    // The agents a person adds, built with the primary agent's composition in their own
    // area, on their own model, beside the primary (spec 3.3 §5.7, §10 L1).
    let factory = {
        let composed = setup.clone();
        let beside = primary.harness.clone();
        let audit = audit.clone();
        let drivers = drivers.clone();
        let control = stores.control.clone();
        let table = table.clone();
        let governance = governance.clone();
        let run_store = run_store.clone();
        let scripted = setup.model_id == "scripted";
        #[cfg(feature = "verb-ui")]
        let verbs = verbs.clone();
        let build: agents::BuildAgent = Arc::new(move |record: adk_kernel::agents::AgentRecord| {
            let setup = composed.clone();
            let beside = beside.clone();
            let service = added_service.clone();
            let audit = audit.clone();
            let drivers = drivers.clone();
            let control = control.clone();
            let table = table.clone();
            let governance = governance.clone();
            let run_store = run_store.clone();
            #[cfg(feature = "verb-ui")]
            let verbs = verbs.clone();
            Box::pin(async move {
                let model = agents::model_of(&record, scripted).await?;
                let partition = match &table {
                    Some(table) => table.get(&record.area).await?,
                    None => None,
                };
                let area_mode = partition
                    .as_ref()
                    .map_or(setup.resolved.mode, |partition| partition.default_mode.into());
                let modes = Arc::new(super::books::AreaModeStore::new(
                    agents::modes_of(&setup),
                    control.clone(),
                    record.area.clone(),
                    area_mode,
                    auto_refused,
                ));
                let added = agents::Added { record: record.clone(), beside, model, modes };
                let agent = Arc::new(setup.agent_with(
                    setup.resolved.effort,
                    &record.id,
                    area_mode,
                    setup.resolved.sandbox,
                    None,
                    Some(service),
                    AgentExtras {
                        audit,
                        memory_with_inbox: true,
                        drivers: Some(drivers),
                        partition: match (partition.clone(), table) {
                            (Some(partition), Some(table)) => Some((partition, table, false)),
                            _ => None,
                        },
                        area_modes: Some((control, record.area.clone(), auto_refused)),
                        #[cfg(feature = "verb-ui")]
                        verbs: Some((verbs, record.area.clone())),
                        added: Some(added),
                        #[cfg(feature = "genui")]
                        draws: true,
                        ..AgentExtras::default()
                    },
                )?);
                Ok(host(&setup, agent, partition.as_ref(), &governance, &run_store)?.harness)
            })
        });
        let mut choices = adk_kernel::agents::AgentChoices::default();
        for model in agents::model_choices(None, agents::has_key) {
            choices = choices.with_model(model);
        }
        for pack in &setup.resolved.packs {
            if let Some(name) = agents::pack_name(*pack) {
                choices = choices.with_pack(name);
            }
        }
        agents::HarnessAgentFactory::new(
            APP_NAME,
            setup.user.clone(),
            primary.harness.clone(),
            "ADK-Rust",
            choices,
            primary.harness.agent().runtime().standing_rules().clone(),
            build,
        )
        .with_areas(hosted.iter().filter_map(|hosted| {
            hosted.partition.clone().map(|area| (area, hosted.harness.clone()))
        }))
    };
    // Re-approving a driver carries the re-approving person's standing rules forward.
    drivers.set_rule_pins(primary.harness.driver_rule_pins());
    let mut worker = DurableWorkerConfig::default();
    if let Some(max) = settings.max_concurrent_runs {
        worker = worker.with_max_concurrent_runs(max);
    }
    // The consolidation tick: on in Ask and Auto with long-term memory (spec 3.2 §13 Q1),
    // for each partition in its own mode.
    let allowed = setup.resolved.consolidation && setup.memory_on();
    let mut ticking = Vec::new();
    for hosted in &hosted {
        if settings.enable_consolidation(hosted.agent.info().default_mode, allowed) {
            ticking.push(hosted);
        }
    }
    let tick = !ticking.is_empty();
    settings.consolidation = tick;
    let pairing = adk_kernel::pairing::Pairing::new(stores.devices.clone());
    #[cfg(feature = "mobile")]
    let pairing = match &phone {
        Some(phone) => phone.pairing(pairing),
        None => pairing,
    };
    #[cfg(feature = "channels")]
    let pairing = match &chat {
        Some(chat) => pairing.with_channel_bindings(chat.bindings()),
        None => pairing,
    };
    // The routes the home's Today and "Agent Memory and Rules" read and change (spec 3.3 §10
    // K3–K5): settings and cursors beside the kernel's other stores, the mode each area's
    // harness started in, its facts through the harness, the record through the signed log.
    let mut control = ControlPlaneConfig::default()
        .with_pairing(pairing)
        .with_control_store(stores.control.clone());
    if auto_refused {
        control = control.with_refused_mode(
            adk_kernel::PartitionMode::Auto,
            format!("Go ahead beyond loopback needs `adk-rust up --sandbox`: {AUTO_REFUSED}"),
        );
    }
    for hosted in &hosted {
        let mode = super::books::partition_mode(hosted.agent.info().default_mode);
        control = control.with_start_mode(hosted.partition.clone().unwrap_or_default(), mode);
    }
    // The facts in harness.db or the `--store` backend (spec 3.3 §12 phase 3).
    #[cfg(any(feature = "persist", feature = "postgres"))]
    if let Some(facts) = setup.fact_store().filter(|_| setup.memory_on()) {
        let areas = hosted
            .iter()
            .map(|hosted| (hosted.partition.clone().unwrap_or_default(), hosted.harness.clone()))
            .collect();
        control = control.with_memory(Arc::new(super::books::HarnessMemoryBook::new(areas, facts)));
    }
    if let Some(record) = record {
        control = control.with_record(record);
    }
    // The REPL's commands and "Always allow" for a terminal attached to this kernel, over the
    // primary harness the web home answers decisions over (spec 3.3 §5.2).
    let commands =
        adk_cli::harness::HarnessCommands::new(primary.harness.clone(), APP_NAME, setup.sessions())
            .agents(agents_handle.clone())
            .run_store(run_store.clone())
            .effort(setup.args.effort)
            .hooks_disabled(!setup.resolved.hooks);
    let host = commands::HarnessCommandHost::new(commands);
    // The agent table, bound once the kernel is built, so "Always allow" on an added agent's
    // card pins the rule to that agent (spec 3.3 S-I19).
    #[cfg(feature = "surface")]
    let host = host.with_rules(
        adk_surface::LocalSource::builder(APP_NAME)
            .harness(primary.harness.clone())
            .agents(agents_handle.clone())
            .build()?,
    );
    // `/snooze` for the terminal attached to this kernel, as its person, in this process (spec
    // 3.3 §5.2, owner question 27).
    #[cfg(feature = "verb-ui")]
    let host = {
        let source = adk_surface::LocalSource::builder(APP_NAME)
            .harness(primary.harness.clone())
            .sessions(setup.sessions())
            .build()?;
        let mut engine = adk_surface::SurfaceEngine::builder(Arc::new(source));
        if let Some((trail, answers)) = &surface_records {
            engine = engine.audit(trail.clone()).answers(answers.clone());
        }
        host.with_verbs(adk_surface::verbs::VerbGate::new(verbs.clone(), engine.build()))
    };
    control = control.with_commands(Arc::new(host));
    // The run store reads the system clock too, so leases, `not_before` and wake times agree.
    let config = settings.apply(
        KernelConfig::default()
            .with_app_name(APP_NAME)
            .with_home(&home)
            .with_worker(worker)
            // The socket at the kernel home, or on Windows the named pipe named after it
            // (spec 3.2 §10 W2), pairing devices beside the kernel's other stores (spec 3.3
            // §10 K2).
            .with_control(control)
            .with_clock(Arc::new(adk_core::SystemClock)),
    );
    // The subject key stays in the home; an ephemeral kernel's ledger and key end with it.
    let key = if stores.ephemeral {
        LedgerKey::generate()?
    } else {
        let path = home.join(LedgerKey::FILE);
        // A protected DACL stands in for mode 0600 (spec 3.2 §10 W4); the kernel fills the
        // empty file with the key.
        #[cfg(all(windows, any(feature = "audit", feature = "persist")))]
        if !path.exists() {
            std::fs::create_dir_all(&home)?;
            if let Err(error) = crate::owner_only::create_new(&path)
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error).with_context(|| format!("failed to create {}", path.display()));
            }
        }
        LedgerKey::load_or_create(&path)?
    };
    // The harness glue registers the runner as `adk-rust` and `harness.goal`, with the
    // harness's sessions, approval service, kill switch, audit trail and goal progress
    // (spec 3.2 §10 HK1); goals land on the background queue. A partition's harness runs as
    // `adk-rust@<partition>` and `harness.goal@<partition>`, bound to its partition; the
    // coordinator's also runs the CLI's own targets, so `attach`, `goal` and `/api/runs`
    // reach it.
    let (primary_goal, primary_runner) = primary.harness.goal_target();
    let mut builder = primary.harness.kernel();
    if let Some(partition) = &primary.partition {
        builder = builder
            .partition_agent(partition.clone(), APP_NAME, primary_runner.clone())
            .partition_agent(partition.clone(), adk_harness::GOAL_TARGET, primary_runner.clone());
    }
    for other in &hosted[1..] {
        builder = other.harness.host_partition(builder);
    }
    if let Some(table) = &table {
        builder = builder.partitions(table.clone());
    }
    let mut kernel = builder
        .config(config)
        .agent_store(stores.agents.clone())
        .agent_factory(Arc::new(factory))
        .agents_handle(agents_handle.clone())
        .process_store(stores.processes)
        .agenda_store(stores.agenda)
        .watermark_store(stores.watermarks)
        .schedule_store(stores.schedules)
        .drivers(drivers)
        .ledger(Ledger::new(stores.ledger, key))
        .observation_phraser(phraser);
    #[cfg(feature = "verb-ui")]
    {
        kernel = kernel.agenda_handle(verb_agenda.clone());
    }
    // Each tick distils this workspace's sessions of its own partition, and the kernel's own,
    // which name no workspace; an unpartitioned kernel's tick skips every partition's
    // (spec 3.2 §7.5, K-I15).
    let recorded = setup.resolved.workspace.display().to_string();
    let user = adk_core::UserId::new(setup.user.clone())?;
    for hosted in ticking {
        let filter = tick_sessions(recorded.clone(), hosted.partition.clone());
        let consolidator = hosted.harness.consolidator(&user)?.with_session_filter(filter);
        kernel = kernel
            .consolidator(hosted.partition.clone().unwrap_or_default(), Arc::new(consolidator));
    }
    if setup.memory_on() {
        // The pass's observations are put to the user; a confirmed one is remembered — each
        // partition's by its own harness, in its memory scope (spec 3.2 §7.8, §7.10).
        let user = adk_core::UserId::new(setup.user.clone())?;
        for hosted in &hosted {
            let audience = hosted.harness.observation_audience(&user)?;
            kernel = kernel.observation_audience(
                hosted.partition.clone().unwrap_or_default(),
                Arc::new(audience),
            );
        }
    }
    let kernel = kernel.build()?;
    // The inbox decides through the harness's approval service, which wakes the kernel's
    // approval sleepers; the service the builder was given would not.
    let decisions = primary
        .agent
        .runtime()
        .approval_service()
        .cloned()
        .context("the served harness has an approval service")?;
    // Chat cards link to the phone's home when it has one.
    #[cfg(all(feature = "channels", feature = "mobile"))]
    let chat = match (chat, &phone) {
        (Some(chat), Some(phone)) => Some(chat.linking_to(phone.home_link())),
        (chat, _) => chat,
    };
    // An agent's screen action is headed "Snooze something in Coming up" in chat too (spec
    // 3.3 §7.1).
    #[cfg(all(feature = "channels", feature = "verb-ui"))]
    let chat = chat.map(|chat| chat.with_card_hints(Arc::new(verbs.hints())));
    #[cfg(feature = "channels")]
    if let Some(chat) = chat {
        let host: Arc<dyn adk_kernel::channels::ChannelHost> = Arc::new(kernel.clone());
        chat.start(host, decisions.clone(), surface_records.clone(), bind).await?;
    }

    // `/api/runs` submissions land on the interactive queue, where the kernel's worker
    // claims them; the server runs no worker of its own.
    let mut background = BackgroundState::new()
        .with_run_store(run_store)
        .with_app_name(APP_NAME)
        .with_agent(APP_NAME, primary_runner.clone())
        .with_agent(adk_harness::GOAL_TARGET, primary_runner.clone())
        .with_agent(primary_goal, primary_runner)
        .with_worker_config(DurableWorkerConfig::default().with_queue(QueueClass::INTERACTIVE))
        .with_embedded_worker(false)
        .with_governance(governance);
    for hosted in &hosted {
        if let Some(partition) = &hosted.partition {
            let (goal, runner) = hosted.harness.goal_target();
            background = background
                .with_agent(adk_harness::partition_target(APP_NAME, partition), runner.clone())
                .with_agent(goal, runner);
        }
    }
    let partitions_note = hosted
        .iter()
        .filter_map(|hosted| hosted.partition.as_ref())
        .map(|id| match settings.coordinator.as_ref() == Some(id) {
            true => format!("{id} (coordinator)"),
            false => id.to_string(),
        })
        .collect::<Vec<_>>();
    let agent = primary.agent.clone();
    let mut launcher = HarnessLauncher::shared(agent)
        .app_name(APP_NAME)
        .with_session_service(setup.sessions())
        .with_user_id(setup.user.clone())
        .with_approval_service(decisions)
        .with_background_runs(background)
        .with_bind_address(bind.ip())
        .with_kernel(kernel);
    if let Some(token) = token {
        launcher = launcher.with_auth_token(token);
    }
    if let Some(hooks) = hooks {
        let paths: Vec<String> = hooks.names.iter().map(|name| format!("/hooks/{name}")).collect();
        println!("  hooks {} (signed webhooks wake the kernel's sleepers)", paths.join(", "));
        launcher = launcher.with_webhook_ingress(hooks.ingress);
    }
    #[cfg(feature = "surface")]
    if let Some((trail, answers)) = surface_records {
        launcher = launcher.with_surface_records(trail, answers);
    }
    #[cfg(feature = "verb-ui")]
    {
        launcher = launcher.with_verbs(verbs);
    }
    // Apps from MCP servers, framed on `--apps-origin` or the sandbox listener (spec 3.3 §6.1).
    #[cfg(feature = "genui-apps")]
    {
        launcher = launcher.with_apps_origin(apps_origin);
    }
    #[cfg(feature = "mobile")]
    if let Some(phone) = phone {
        println!("  phone {}/ (pairing, wake-ups, confirm it's you)", phone.setup.pairing_base());
        launcher = launcher.with_phone(phone.setup);
    }
    #[cfg(all(feature = "voice", feature = "surface"))]
    {
        let marked = setup.resolved.packs.contains(&adk_harness::Pack::Voice);
        launcher = crate::voice_cli::offer(launcher, voice, marked);
    }
    println!(
        "  store {}: sessions, approvals, runs, processes, agenda, schedules, drivers, partitions, the activity ledger, paired devices",
        stores.label
    );
    if !partitions_note.is_empty() {
        println!("  partitions {}", partitions_note.join(", "));
    }
    println!("  plans {}", setup.plans_note());
    // Both limits are shown on every start (spec 3.3 §13 Q7).
    if setup.personal.is_some() {
        println!(
            "  work  {} (personal preset: not a code repository, no host shell)",
            setup.resolved.workspace.display()
        );
        println!(
            "  quiet time {} · daily spending limit {}",
            quiet_time_label(settings.quiet_hours),
            spending_limit_label(settings.quotas.daily_spend_cap)
        );
    }
    println!(
        "  tick  {}",
        if tick {
            "consolidation on: distils missed sessions and tidies assumed facts (--no-consolidation turns it off)"
        } else {
            "consolidation off"
        }
    );
    launcher.run_serve_directly(bind.port()).await.map_err(Into::into)
}

/// The sessions a tick of `partition` distils: those of the workspace `recorded`, or naming
/// none, that `partition`'s harness ran — with no partition, those no partition's harness
/// ran (spec 3.2 §7.5, K-I15).
fn tick_sessions(
    recorded: String,
    partition: Option<adk_kernel::PartitionId>,
) -> adk_harness::SessionStateFilter {
    Arc::new(move |state: &dyn adk_session::State| {
        let workspace = match state.get("harness:workspace") {
            Some(value) => value.as_str() == Some(recorded.as_str()),
            None => true,
        };
        let ran = state.get(adk_harness::PARTITION_STATE_KEY);
        workspace
            && ran.as_ref().and_then(serde_json::Value::as_str)
                == partition.as_ref().map(adk_kernel::PartitionId::as_str)
    })
}

/// One harness `up` hosts: its agent, the harness over its runner, and its partition.
struct Hosted {
    agent: Arc<adk_harness::HarnessAgent>,
    harness: Arc<adk_harness::Harness>,
    partition: Option<adk_kernel::PartitionId>,
}

/// The harness over `agent`'s runner, with the run store and the background goal queue;
/// a partition's runner acts with the partition's class and tenant (spec 3.2 §7.8).
fn host(
    setup: &Setup,
    agent: Arc<adk_harness::HarnessAgent>,
    partition: Option<&adk_kernel::Partition>,
    governance: &GovernanceControl,
    run_store: &Arc<dyn adk_core::DurableRunStore>,
) -> Result<Hosted> {
    let mut runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .approval_store(agent.runtime().approval_store().clone())
        .governance(governance.clone());
    if let Some(partition) = partition {
        runner = runner.identity_source(
            partition.identity_source(APP_NAME, adk_core::Agent::name(agent.as_ref())),
        );
    }
    let harness = adk_harness::Harness::with_runner(agent.clone(), runner.build()?)?
        .with_run_store(run_store.clone())
        .with_goal_queue(QueueClass::BACKGROUND);
    let harness = Arc::new(harness);
    Ok(Hosted { agent, harness, partition: partition.map(|partition| partition.id.clone()) })
}

/// The partition table in the kernel's store, with every `[[kernel.partitions]]` entry it
/// lacks added; an entry the table already holds keeps the table's record, which
/// `adk-rust partitions` changes (spec 3.2 §7.8, §8.1).
async fn open_partitions(
    setup: &Setup,
    settings: &KernelSettings,
    store: Arc<dyn adk_kernel::PartitionStore>,
    audit: Option<&Arc<dyn AuditRecorder>>,
) -> Result<adk_kernel::Partitions> {
    let mut table = adk_kernel::Partitions::new(store);
    if let Some(recorder) = audit {
        table = table.with_audit(AuditTrail::new(recorder.clone()));
    }
    let by =
        adk_core::audit_log::AuditActor::subject(adk_core::SubjectRef::human(setup.user.clone()));
    for partition in &settings.partitions {
        match table.get(&partition.id).await? {
            None => table.put(partition.clone(), by.clone()).await?,
            Some(stored) if stored != *partition => println!(
                "note: partition `{}` differs from its [[kernel.partitions]] entry; the \
                 partition table's record applies (change it with `adk-rust partitions`)",
                partition.id
            ),
            Some(_) => {}
        }
    }
    Ok(table)
}

/// One harness per partition in the table, the coordinator's first: it serves the REPL,
/// the web surface and the CLI's own targets (spec 3.2 §7.8).
async fn partition_harnesses(
    setup: &Setup,
    settings: &KernelSettings,
    table: &adk_kernel::Partitions,
    service: &Arc<dyn ApprovalService>,
    governance: &GovernanceControl,
    run_store: &Arc<dyn adk_core::DurableRunStore>,
    extras: impl Fn(adk_kernel::Partition, bool) -> AgentExtras,
) -> Result<Vec<Hosted>> {
    let mut partitions = table.list().await?;
    let coordinator = settings.coordinator.as_ref();
    partitions.sort_by_key(|partition| Some(&partition.id) != coordinator);
    let mut hosted = Vec::with_capacity(partitions.len());
    let ids: Vec<adk_kernel::PartitionId> =
        partitions.iter().map(|partition| partition.id.clone()).collect();
    for partition in partitions {
        let is_coordinator = coordinator == Some(&partition.id);
        let mut extra = extras(partition.clone(), is_coordinator);
        // The coordinator reaches the other partitions' drivers as crossings (spec 3.2 §7.8).
        if is_coordinator {
            extra.driver_crossings =
                ids.iter().filter(|id| **id != partition.id).cloned().collect();
        }
        let agent = Arc::new(setup.agent_with(
            setup.resolved.effort,
            APP_NAME,
            partition.default_mode.into(),
            setup.resolved.sandbox,
            None,
            Some(service.clone()),
            extra,
        )?);
        hosted.push(host(setup, agent, Some(&partition), governance, run_store)?);
    }
    Ok(hosted)
}

/// Lays the personal preset's first-run answers over `settings` (spec 3.3 §13 Q7): the user
/// config's `[kernel]` table, `user`, wins over them, and a workspace's
/// `.adk/harness.toml` may only lower the daily spending limit, never raise it.
pub(super) fn apply_personal(
    settings: &mut KernelSettings,
    user: &KernelSettings,
    personal: &adk_harness::personal::PersonalSettings,
) {
    if user.quiet_hours.is_none() {
        settings.quiet_hours = personal.quiet_time.map(Into::into);
    }
    if user.quotas.daily_spend_cap.is_none() {
        settings.quotas.daily_spend_cap =
            match (personal.daily_spending_limit_micro_usd(), settings.quotas.daily_spend_cap) {
                (Some(answer), Some(workspace)) => Some(answer.min(workspace)),
                (answer, workspace) => answer.or(workspace),
            };
    }
}

/// Quiet time as the banners show it.
pub(super) fn quiet_time_label(quiet: Option<adk_kernel::QuietHours>) -> String {
    quiet.map_or_else(
        || "off".to_string(),
        |quiet| format!("{}–{}", quiet.start.format("%H:%M"), quiet.end.format("%H:%M")),
    )
}

/// The daily spending limit as the banners show it.
pub(super) fn spending_limit_label(micro_usd: Option<u64>) -> String {
    micro_usd.map_or_else(
        || "none".to_string(),
        |micro| format!("${:.2} a day", micro as f64 / 1_000_000.0),
    )
}

/// The kernel home when neither `--home` nor `[kernel] home` names one: the directory
/// `harness.db` is in.
pub(super) fn default_home() -> Result<PathBuf> {
    #[cfg(feature = "persist")]
    return crate::sessions::default_dir();
    #[cfg(not(feature = "persist"))]
    Ok(dirs::config_dir()
        .context("could not determine the config directory; pass --home")?
        .join("adk-rust"))
}

/// The approval and run stores `serve` uses: the `--store` backend, `harness.db`, or memory.
pub(super) fn run_and_approval_stores(
    setup: &Setup,
) -> (Arc<dyn adk_core::ApprovalStore>, Arc<dyn adk_core::DurableRunStore>) {
    if let Some(shared) = &setup.shared {
        return (shared.approvals.clone(), shared.runs.clone());
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        return (db.approvals.clone(), db.runs.clone());
    }
    (Arc::new(adk_core::InMemoryApprovalStore::new()), Arc::new(adk_core::InMemoryRunStore::new()))
}

/// The process, schedule and governance stores, beside the harness's (spec 3.2 §7.1, §13
/// Q8): on `harness.db`'s pool with their own migration tables, in the `--store` backend,
/// or in memory with `--ephemeral`.
async fn kernel_stores(setup: &Setup, store: Option<&str>) -> Result<KernelStores> {
    if let Some(url) = store {
        return shared_kernel_stores(url).await;
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        let processes = adk_kernel::SqliteProcessStore::new(db.pool.clone());
        processes.migrate().await?;
        let agenda = adk_kernel::SqliteAgendaStore::new(db.pool.clone());
        agenda.migrate().await?;
        let partitions = adk_kernel::SqlitePartitionStore::new(db.pool.clone());
        partitions.migrate().await?;
        let schedules = adk_session::SqliteScheduleStore::from_pool(db.pool.clone());
        schedules.migrate().await?;
        let governance = adk_session::SqliteGovernanceStateStore::from_pool(db.pool.clone());
        governance.migrate().await?;
        let drivers = adk_kernel::stores::SqliteDriverStore::new(db.pool.clone());
        drivers.migrate().await?;
        let ledger = adk_kernel::proc::SqliteLedgerStore::new(db.pool.clone());
        ledger.migrate().await?;
        let devices = adk_kernel::pairing::SqliteDeviceStore::new(db.pool.clone());
        devices.migrate().await?;
        let control = adk_kernel::control::SqliteControlStore::new(db.pool.clone());
        control.migrate().await?;
        let watermarks = adk_kernel::SqliteCronWatermarkStore::new(db.pool.clone());
        watermarks.migrate().await?;
        let agents = adk_kernel::stores::SqliteAgentStore::new(db.pool.clone());
        agents.migrate().await?;
        return Ok(KernelStores {
            agents: Arc::new(agents),
            processes: Arc::new(processes),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            schedules: Arc::new(schedules),
            drivers: Arc::new(drivers),
            ledger: Arc::new(ledger),
            devices: Arc::new(devices),
            control: Arc::new(control),
            watermarks: Arc::new(watermarks),
            ephemeral: false,
            governance: Some(Arc::new(governance)),
            label: "harness.db".into(),
        });
    }
    let _ = setup;
    Ok(KernelStores {
        processes: Arc::new(adk_kernel::InMemoryProcessStore::new()),
        agenda: Arc::new(adk_kernel::InMemoryAgendaStore::new()),
        partitions: Arc::new(adk_kernel::InMemoryPartitionStore::new()),
        schedules: Arc::new(adk_core::InMemoryScheduleStore::new()),
        drivers: Arc::new(adk_kernel::stores::InMemoryDriverStore::new()),
        ledger: Arc::new(adk_kernel::proc::InMemoryLedgerStore::new()),
        devices: Arc::new(adk_kernel::pairing::InMemoryDeviceStore::new()),
        control: Arc::new(adk_kernel::control::InMemoryControlStore::new()),
        watermarks: Arc::new(adk_kernel::InMemoryCronWatermarkStore::new()),
        agents: Arc::new(adk_kernel::stores::InMemoryAgentStore::new()),
        ephemeral: true,
        governance: None,
        label: "memory (--ephemeral): nothing outlives the kernel".into(),
    })
}

/// The kernel's stores in the `--store` backend every replica shares.
async fn shared_kernel_stores(url: &str) -> Result<KernelStores> {
    let label = adk_harness::redact_store_url(url);
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        #[cfg(feature = "postgres")]
        Some("postgres" | "postgresql") => {
            let processes = adk_kernel::PostgresProcessStore::connect(url).await?;
            processes.migrate().await?;
            let agenda = adk_kernel::PostgresAgendaStore::new(processes.pool().clone());
            agenda.migrate().await?;
            let partitions = adk_kernel::PostgresPartitionStore::new(processes.pool().clone());
            partitions.migrate().await?;
            let schedules = adk_session::PostgresScheduleStore::new(url).await?;
            schedules.migrate().await?;
            let governance = adk_session::PostgresGovernanceStateStore::new(url).await?;
            governance.migrate().await?;
            let drivers = adk_kernel::stores::PostgresDriverStore::connect(url).await?;
            drivers.migrate().await?;
            let ledger = adk_kernel::proc::PostgresLedgerStore::new(processes.pool().clone());
            ledger.migrate().await?;
            let devices = adk_kernel::pairing::PostgresDeviceStore::new(processes.pool().clone());
            devices.migrate().await?;
            let control = adk_kernel::control::PostgresControlStore::new(processes.pool().clone());
            control.migrate().await?;
            let watermarks = adk_kernel::PostgresCronWatermarkStore::new(processes.pool().clone());
            watermarks.migrate().await?;
            let agents = adk_kernel::stores::PostgresAgentStore::new(processes.pool().clone());
            agents.migrate().await?;
            Ok(KernelStores {
                agents: Arc::new(agents),
                processes: Arc::new(processes),
                agenda: Arc::new(agenda),
                partitions: Arc::new(partitions),
                schedules: Arc::new(schedules),
                drivers: Arc::new(drivers),
                ledger: Arc::new(ledger),
                devices: Arc::new(devices),
                control: Arc::new(control),
                watermarks: Arc::new(watermarks),
                ephemeral: false,
                governance: Some(Arc::new(governance)),
                label,
            })
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let processes = adk_kernel::RedisProcessStore::connect(url).await?;
            processes.migrate().await?;
            let agenda = adk_kernel::RedisAgendaStore::from_client(processes.client().clone());
            agenda.migrate().await?;
            let partitions =
                adk_kernel::RedisPartitionStore::from_client(processes.client().clone());
            partitions.migrate().await?;
            let schedules = adk_session::RedisScheduleStore::new(url).await?;
            schedules.migrate().await?;
            let governance = adk_session::RedisGovernanceStateStore::new(url).await?;
            governance.migrate().await?;
            let drivers = adk_kernel::stores::RedisDriverStore::connect(url).await?;
            drivers.migrate().await?;
            let ledger =
                adk_kernel::proc::RedisLedgerStore::from_client(processes.client().clone());
            ledger.migrate().await?;
            let devices =
                adk_kernel::pairing::RedisDeviceStore::from_client(processes.client().clone());
            devices.migrate().await?;
            let control =
                adk_kernel::control::RedisControlStore::from_client(processes.client().clone());
            control.migrate().await?;
            let watermarks =
                adk_kernel::RedisCronWatermarkStore::from_client(processes.client().clone());
            watermarks.migrate().await?;
            let agents =
                adk_kernel::stores::RedisAgentStore::from_client(processes.client().clone());
            agents.migrate().await?;
            Ok(KernelStores {
                agents: Arc::new(agents),
                processes: Arc::new(processes),
                agenda: Arc::new(agenda),
                partitions: Arc::new(partitions),
                schedules: Arc::new(schedules),
                drivers: Arc::new(drivers),
                ledger: Arc::new(ledger),
                devices: Arc::new(devices),
                control: Arc::new(control),
                watermarks: Arc::new(watermarks),
                ephemeral: false,
                governance: Some(Arc::new(governance)),
                label,
            })
        }
        // `Setup::open` already refused a scheme this build cannot open.
        _ => bail!("--store {label}: no kernel store backend for it in this build"),
    }
}

/// The kernel replica's audit recorder, the log store under it, and the record book the
/// control plane reads and checks it through (spec 3.3 §10 K5).
type KernelAudit = (
    Arc<dyn AuditRecorder>,
    Arc<dyn adk_core::audit_log::AuditLogStore>,
    Arc<dyn adk_kernel::control::RecordBook>,
);

/// The kernel replica's signed audit log (spec 3.2 §9.3, K-I16): one hash chain per
/// replica in the store the harness's are in, signed with a key kept owner-only in the
/// kernel home. The harness's tool-call records and the kernel's `kernel.*` records share
/// it. `None` with `--ephemeral`, whose records would not outlive the kernel.
#[cfg(feature = "audit")]
async fn kernel_audit(
    home: &Path,
    setup: &Setup,
    store: Option<&str>,
) -> Result<Option<KernelAudit>> {
    use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
    use adk_core::audit_log::AuditLogStore;

    let logs: Arc<dyn AuditLogStore> = match store {
        Some(url) => shared_audit_store(url).await?,
        None => match harness_db_audit_store(setup).await? {
            Some(logs) => logs,
            None => return Ok(None),
        },
    };
    std::fs::create_dir_all(home)
        .with_context(|| format!("failed to create {}", home.display()))?;
    let id = kernel_id(home)?;
    let key_path = home.join("kernel-audit.key");
    let signer = match std::fs::read_to_string(&key_path) {
        Ok(key) => Ed25519Signer::from_note_key(key.trim())
            .with_context(|| format!("{} is not a signer key", key_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let signer = Ed25519Signer::generate(format!("kernel-{id}"))?;
            write_private(&key_path, signer.to_note_key().as_str())?;
            signer
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", key_path.display()));
        }
    };
    // The record routes check the log with the key `adk-rust audit verify` is given (spec 3.3
    // §10 K5).
    let verifier = signer
        .verifier_key()
        .parse::<NoteVerifier>()
        .context("the kernel's audit key has no verifier key")?;
    let record =
        super::books::SignedRecordBook::new(logs.clone(), format!("kernel-{id}"), verifier);
    let log =
        AuditLog::builder(logs.clone(), format!("kernel-{id}"), Arc::new(signer)).open().await?;
    Ok(Some((Arc::new(log), logs, Arc::new(record))))
}

#[cfg(not(feature = "audit"))]
async fn kernel_audit(
    _home: &Path,
    _setup: &Setup,
    _store: Option<&str>,
) -> Result<Option<KernelAudit>> {
    Ok(None)
}

/// The audit log store on `harness.db`'s pool, or `None` without `harness.db`.
#[cfg(all(feature = "audit", feature = "persist"))]
async fn harness_db_audit_store(
    setup: &Setup,
) -> Result<Option<Arc<dyn adk_core::audit_log::AuditLogStore>>> {
    let Some(db) = &setup.db else {
        return Ok(None);
    };
    let logs = adk_session::SqliteAuditLogStore::from_pool(db.pool.clone());
    logs.migrate().await?;
    Ok(Some(Arc::new(logs)))
}

#[cfg(all(feature = "audit", not(feature = "persist")))]
async fn harness_db_audit_store(
    _setup: &Setup,
) -> Result<Option<Arc<dyn adk_core::audit_log::AuditLogStore>>> {
    Ok(None)
}

#[cfg(feature = "audit")]
async fn shared_audit_store(url: &str) -> Result<Arc<dyn adk_core::audit_log::AuditLogStore>> {
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        #[cfg(feature = "postgres")]
        Some("postgres" | "postgresql") => {
            let logs = adk_session::PostgresAuditLogStore::new(url).await?;
            logs.migrate().await?;
            Ok(Arc::new(logs))
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let logs = adk_session::RedisAuditLogStore::new(url).await?;
            logs.migrate().await?;
            Ok(Arc::new(logs))
        }
        _ => bail!(
            "--store {}: no audit log backend for it in this build",
            adk_harness::redact_store_url(url)
        ),
    }
}

/// This replica's id, kept in the kernel home so its audit chain continues across restarts.
#[cfg(feature = "audit")]
fn kernel_id(home: &Path) -> Result<String> {
    let path = home.join("kernel-id");
    match std::fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => Ok(id.trim().to_string()),
        Ok(_) => bail!("{} is empty; remove it to start a new audit chain", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = uuid::Uuid::new_v4().simple().to_string();
            write_private(&path, &id)?;
            Ok(id)
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Writes `contents` to a new owner-only file at `path`: mode `0600` on Unix, a DACL granting
/// the owner alone access on Windows (spec 3.2 §10 W4).
#[cfg(feature = "audit")]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    #[cfg(not(windows))]
    let created = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    };
    #[cfg(windows)]
    let created = crate::owner_only::create_new(path);
    let mut file = created.with_context(|| format!("failed to create {}", path.display()))?;
    writeln!(file, "{contents}").with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(all(test, feature = "audit"))]
mod tests {
    use super::*;

    /// The replica id and the audit signing key in the kernel home are readable by their
    /// owner only (spec 3.2 §10 W4).
    #[test]
    fn the_kernel_home_files_are_owner_only() {
        let home = tempfile::tempdir().unwrap();
        let id = kernel_id(home.path()).unwrap();
        assert_eq!(kernel_id(home.path()).unwrap(), id, "the id survives a restart");
        let key = home.path().join("kernel-audit.key");
        write_private(&key, "secret").unwrap();
        for path in [home.path().join("kernel-id"), key] {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", path.display());
            }
            #[cfg(windows)]
            assert!(crate::owner_only::is_owner_only(&path).unwrap(), "{}", path.display());
        }
    }
}

#[cfg(test)]
mod tick_tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use super::tick_sessions;

    struct State(HashMap<String, Value>);

    impl adk_session::State for State {
        fn get(&self, key: &str) -> Option<Value> {
            self.0.get(key).cloned()
        }

        fn set(&mut self, key: String, value: Value) {
            self.0.insert(key, value);
        }

        fn all(&self) -> HashMap<String, Value> {
            self.0.clone()
        }
    }

    #[test]
    fn a_tick_distils_only_its_own_partitions_sessions() {
        let state = |entries: &[(&str, Value)]| {
            State(entries.iter().map(|(key, value)| (key.to_string(), value.clone())).collect())
        };
        let workspace = ("harness:workspace", json!("/ws"));
        let home = ("harness:partition", json!("home"));
        let work = ("harness:partition", json!("work"));
        let home_tick = tick_sessions("/ws".into(), Some("home".into()));
        let plain_tick = tick_sessions("/ws".into(), None);
        assert!(home_tick(&state(&[workspace.clone(), home.clone()])));
        assert!(
            home_tick(&state(std::slice::from_ref(&home))),
            "a kernel session names no workspace"
        );
        assert!(!home_tick(&state(&[workspace.clone(), work.clone()])), "work's session");
        assert!(!home_tick(&state(std::slice::from_ref(&workspace))), "no partition's session");
        assert!(!home_tick(&state(&[("harness:workspace", json!("/other")), home])));
        assert!(plain_tick(&state(std::slice::from_ref(&workspace))));
        assert!(plain_tick(&state(&[])));
        assert!(!plain_tick(&state(&[workspace, work])), "a partition's session");
    }
}
