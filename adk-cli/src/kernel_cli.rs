//! The running kernel's commands over its control plane (spec 3.2 §7.11, §8.1): `attach`,
//! `ps`, `top`, `kill`, `nice`, `suspend`, `resume`, `freeze`, `unfreeze`, `events`,
//! `agenda`, `wake` and `proc`, and the REPL `adk-rust` opens on a running kernel. Surfaces hold no state (K-I17): every command
//! reads the kernel, and detaching changes nothing in its process table.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use adk_cli::console::{ConsoleInput, ConsoleIo};
use adk_core::{ApprovalAction, ApprovalState, ErrorCategory, Event, Part, RunState};
use adk_harness::escape_control;
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{
    AgendaQuery, AttachRequest, LedgerParams, ObservationsParams, ProcessesQuery, SpawnRequest,
};
use adk_kernel::proc::baselines::BaselineSnapshot;
use adk_kernel::proc::{Direction, Phrasing};
use adk_kernel::{
    AgendaItem, AgendaState, AgendaStateKind, EventRecord, KernelEvent, PartitionId, Process,
    ProcessKind, QueueClass, Suspension, WakeSet,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use futures::StreamExt;

use crate::cli::{DriversCommands, KernelTarget, ProcView};
use adk_kernel::drivers::{DriverIsolation, DriverManifest, DriverProposal, DriverView};

/// The target a turn sent from the REPL runs: the harness, as `adk-rust up` registers it.
const APP_TARGET: &str = "adk-rust";

/// How often a following surface reads the session.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(300);

/// How long `events` without `--follow` waits for one more held event.
const EVENTS_IDLE: Duration = Duration::from_secs(1);

/// The kernel home commands reach by default: where `adk-rust up` keeps its socket.
fn default_home() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("could not determine the config directory; pass --home or --server")?
        .join("adk-rust"))
}

/// A client of the kernel `target` names: its HTTP control plane with `--server`, else the
/// socket of its home.
pub async fn client(target: &KernelTarget) -> Result<ControlClient> {
    if let Some(server) = &target.server {
        let mut client = ControlClient::http(server.trim_end_matches('/'));
        let token = target.token.clone().or_else(|| std::env::var("ADK_SERVE_TOKEN").ok());
        if let Some(token) = token.filter(|token| !token.trim().is_empty()) {
            client = client.with_bearer_token(token);
        }
        return Ok(client);
    }
    let home = match &target.home {
        Some(home) => home.clone(),
        None => default_home()?,
    };
    match ControlClient::connect_local(&home).await? {
        Some(client) => Ok(client),
        None => bail!(
            "no kernel runs at {}: start one with `adk-rust up`, or reach one over HTTP with \
             --server URL",
            home.display()
        ),
    }
}

/// The kernel running at the default home, if one answers: `adk-rust` with no subcommand
/// and `adk-rust goal` use it (spec 3.2 §7.11, §8.1).
pub async fn local() -> Result<Option<ControlClient>> {
    let Ok(home) = default_home() else {
        return Ok(None);
    };
    Ok(ControlClient::connect_local(&home).await?)
}

/// `adk-rust ps`.
pub async fn ps(
    target: &KernelTarget,
    partition: Option<String>,
    state: Option<RunState>,
    limit: Option<usize>,
) -> Result<()> {
    let client = client(target).await?;
    let query = ProcessesQuery { partition: partition.map(PartitionId::from), state, limit };
    let processes = client.processes(&query).await?;
    if processes.is_empty() {
        println!("No processes.");
        return Ok(());
    }
    println!("{}", header());
    for process in &processes {
        println!("{}", row(process));
    }
    Ok(())
}

/// `adk-rust top`: `ps` of the unfinished processes by burn rate, refreshed every
/// `interval` until Ctrl-C (spec 3.2 §7.10, §8.1).
pub async fn top(target: &KernelTarget, partition: Option<String>, interval: u64) -> Result<()> {
    let client = client(target).await?;
    let interval = Duration::from_secs(interval.max(1));
    let terminal = std::io::stdout().is_terminal();
    loop {
        let health = client.health().await?;
        let query = ProcessesQuery {
            partition: partition.clone().map(PartitionId::from),
            ..ProcessesQuery::default()
        };
        let mut processes = client.processes(&query).await?;
        processes.retain(|process| !process.run.state.is_terminal());
        processes.sort_by(|a, b| {
            let rate = |process: &Process| process.view.burn_rate.unwrap_or(-1.0);
            rate(b).total_cmp(&rate(a))
        });
        if terminal {
            print!("\x1b[2J\x1b[H");
        }
        let spend: u64 = processes.iter().map(|process| process.view.spend_today).sum();
        println!(
            "kernel {} · {}{} · {} process(es) · {} today · {}",
            client.endpoint(),
            health.state,
            if health.frozen { " · FROZEN" } else { "" },
            processes.len(),
            dollars(spend),
            Utc::now().format("%H:%M:%S")
        );
        println!("{}  {:>9}  {:>9}", header(), "BURN/MIN", "BY 24:00");
        for process in &processes {
            let burn = process.view.burn_rate.map(|rate| dollars(rate.round() as u64));
            let projection = process.view.projection.map(dollars);
            println!(
                "{}  {:>9}  {:>9}",
                row(process),
                burn.as_deref().unwrap_or("-"),
                projection.as_deref().unwrap_or("-")
            );
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// What `kill`, `suspend` and `resume` do.
pub enum Action {
    Kill(Option<String>),
    Suspend,
    Resume,
}

/// `adk-rust kill`, `suspend` and `resume`.
pub async fn act(target: &KernelTarget, pid: &str, action: Action) -> Result<()> {
    let client = client(target).await?;
    let pid = resolve_pid(&client, pid).await?;
    let (verb, process) = match action {
        Action::Kill(reason) => ("killed", client.kill(&pid, reason.as_deref()).await?),
        Action::Suspend => ("suspended", client.suspend(&pid).await?),
        Action::Resume => ("resumed", client.resume(&pid).await?),
    };
    println!("{verb} {pid}: now {}{}", state_name(process.run.state), note(&process));
    Ok(())
}

/// `adk-rust nice`: moves a queued or sleeping process to `queue`. The run store has no
/// queue change, so the moved process has a new pid.
pub async fn nice(target: &KernelTarget, pid: &str, queue: QueueClass) -> Result<()> {
    let client = client(target).await?;
    let pid = resolve_pid(&client, pid).await?;
    let moved = client.nice(&pid, queue.clone()).await?;
    println!("moved {pid} to {queue}: now {} ({})", moved.meta.pid, state_name(moved.run.state));
    Ok(())
}

/// What `adk-rust wake` sleeps on, as the command line gave it.
pub struct WakeOn {
    /// The wake set.
    pub wake: WakeSet,
    /// The process kind it records.
    pub kind: ProcessKind,
    /// What `wake` prints about it.
    pub described: String,
}

/// `adk-rust wake`: a process asleep on `on` that runs `task` when it wakes (spec 3.2 §8.1).
pub async fn wake(
    target: &KernelTarget,
    on: WakeOn,
    task: &str,
    session: Option<String>,
) -> Result<()> {
    let client = client(target).await?;
    let mut request = SpawnRequest::new(APP_TARGET, serde_json::json!({ "text": task }))
        .with_queue(QueueClass::Background)
        .with_kind(on.kind)
        .with_wake(on.wake);
    if let Some(session) = session {
        request = request.with_session_id(session);
    }
    let process = client.spawn(&request).await?;
    println!("{} sleeps until {}; `adk-rust ps` shows it.", process.meta.pid, on.described);
    Ok(())
}

/// The wake set of `adk-rust wake`: one of `file` (a glob, relative to the current
/// directory), `cron` (an expression in UTC; the kernel wakes the process at each
/// occurrence, re-arming it after every run) or `webhook` (a hook an ingress on the
/// kernel's store serves).
pub fn wake_on(file: Option<&str>, cron: Option<&str>, webhook: Option<&str>) -> Result<WakeOn> {
    match (file, cron, webhook) {
        (Some(glob), None, None) => {
            let cwd = std::env::current_dir().context("no current directory")?;
            let path = cwd.join(glob);
            let (root, pattern) = match (path.parent(), path.file_name()) {
                (Some(root), Some(name)) => {
                    (root.to_path_buf(), name.to_string_lossy().into_owned())
                }
                _ => bail!(
                    "--file {glob}: give a file name or pattern, such as Cargo.lock or src/*.rs"
                ),
            };
            let root = std::fs::canonicalize(&root)
                .with_context(|| format!("--file {glob}: {} does not exist", root.display()))?;
            let described = format!("{pattern} changes under {}", root.display());
            Ok(WakeOn {
                wake: WakeSet::any_of([adk_kernel::Wake::FileChange {
                    root,
                    glob: pattern,
                    debounce: adk_kernel::FileWakeSource::DEFAULT_DEBOUNCE,
                }]),
                kind: ProcessKind::Triggered { source: "file".into() },
                described,
            })
        }
        (None, Some(expr), None) => {
            // Checked here, so a typo fails now rather than never waking.
            let at = next_occurrence(expr)?;
            Ok(WakeOn {
                wake: WakeSet::any_of([adk_kernel::Wake::Cron {
                    expr: expr.to_string(),
                    missed: adk_core::MissedFirePolicy::Skip,
                }]),
                kind: ProcessKind::Scheduled,
                described: format!(
                    "`{expr}` (UTC) fires, first at {}, and again after each run until killed",
                    at.format("%Y-%m-%d %H:%M:%S")
                ),
            })
        }
        (None, None, Some(hook)) => Ok(WakeOn {
            wake: WakeSet::any_of([adk_kernel::Wake::Webhook { hook: hook.to_string() }]),
            kind: ProcessKind::Triggered { source: format!("hook:{hook}") },
            described: format!(
                "the webhook `{hook}` delivers (an ingress serving /hooks/{hook} on this kernel's store)"
            ),
        }),
        _ => bail!("give exactly one of --file GLOB, --cron EXPR or --webhook NAME"),
    }
}

/// The wake set of `adk-rust agenda schedule`: `--at` an instant, or `--cron` its next
/// occurrence in UTC.
pub fn schedule_wake(at: Option<&str>, cron: Option<&str>) -> Result<WakeSet> {
    let at = match (at, cron) {
        (Some(at), None) => parse_at(at, Utc::now())?,
        (None, Some(expr)) => next_occurrence(expr)?,
        _ => bail!("give exactly one of --at TIME or --cron EXPR"),
    };
    Ok(WakeSet::any_of([adk_kernel::Wake::At(at)]))
}

/// The next time the cron expression `expr` fires, in UTC.
fn next_occurrence(expr: &str) -> Result<DateTime<Utc>> {
    let spec =
        adk_core::schedule::ScheduleSpec::Cron { expr: expr.to_string(), timezone: "UTC".into() };
    spec.next_after(Utc::now())
        .map_err(|error| anyhow::anyhow!("--cron {expr}: {error}"))?
        .with_context(|| format!("--cron {expr} never fires again"))
}

/// An instant: RFC 3339 (`2026-10-24T07:00:00Z`), `HH:MM` local time (today, or tomorrow
/// once it passed), or `+30m`, `+2h`, `+1d` from `now`.
fn parse_at(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&Utc));
    }
    if let Some(offset) = text.strip_prefix('+') {
        let (number, unit) = offset.split_at(offset.len().saturating_sub(1));
        let number: i64 =
            number.parse().with_context(|| format!("--at {text}: write +30m, +2h or +1d"))?;
        let step = match unit {
            "m" => chrono::Duration::minutes(number),
            "h" => chrono::Duration::hours(number),
            "d" => chrono::Duration::days(number),
            _ => bail!("--at {text}: write +30m, +2h or +1d"),
        };
        return Ok(now + step);
    }
    if let Ok(time) = chrono::NaiveTime::parse_from_str(text, "%H:%M") {
        use chrono::TimeZone as _;
        let local_now = now.with_timezone(&chrono::Local);
        for days in 0..=1 {
            let date = local_now.date_naive() + chrono::Days::new(days);
            if let Some(at) = chrono::Local.from_local_datetime(&date.and_time(time)).earliest()
                && at.with_timezone(&Utc) > now
            {
                return Ok(at.with_timezone(&Utc));
            }
        }
    }
    bail!("--at {text}: write an RFC 3339 time (2026-10-24T07:00:00Z), HH:MM, or +30m, +2h, +1d")
}

/// What `adk-rust agenda approve`, `reject` and `schedule` do.
pub enum AgendaAction {
    Approve,
    Reject(Option<String>),
    Schedule(WakeSet),
}

/// `adk-rust agenda` and `agenda list` (spec 3.2 §8.1).
pub async fn agenda_list(
    target: &KernelTarget,
    partition: Option<String>,
    state: Option<AgendaStateKind>,
    limit: Option<usize>,
) -> Result<()> {
    let client = client(target).await?;
    let query = AgendaQuery { partition: partition.map(PartitionId::from), state, limit };
    let items = client.agenda(&query).await?;
    if items.is_empty() {
        println!("The agenda is empty.");
        return Ok(());
    }
    println!("{:<36}  {:<10}  {:<13}  {:>7}  TITLE", "ID", "STATE", "ORIGIN", "AGE");
    let now = Utc::now();
    for item in &items {
        let age_of = (now - item.created_at).to_std().unwrap_or_default();
        println!(
            "{:<36}  {:<10}  {:<13}  {:>7}  {}",
            item.id,
            item.state.kind().as_str(),
            item.origin.kind(),
            age(age_of),
            escape_control(&one_line(&item.title))
        );
    }
    let proposed: Vec<&AgendaItem> =
        items.iter().filter(|item| item.state == AgendaState::Proposed).collect();
    let questions = proposed.iter().filter(|item| item.origin.is_question()).count();
    let waiting = proposed.len() - questions;
    if waiting > 0 {
        println!(
            "
{waiting} proposed: `adk-rust agenda approve ID`, `reject ID` or `schedule ID --at TIME`."
        );
    }
    if questions > 0 {
        println!(
            "
{questions} observation(s) asked as questions: `adk-rust agenda approve ID` confirms one and \
             remembers it, `reject ID` dismisses it."
        );
    }
    Ok(())
}

/// `adk-rust agenda approve`, `reject` and `schedule`.
pub async fn agenda_act(target: &KernelTarget, id: &str, action: AgendaAction) -> Result<()> {
    let client = client(target).await?;
    let id = resolve_item(&client, id).await?;
    let (verb, item) = match action {
        AgendaAction::Approve => ("approved", client.approve_item(&id).await?),
        AgendaAction::Reject(reason) => {
            ("rejected", client.reject_item(&id, reason.as_deref()).await?)
        }
        AgendaAction::Schedule(wake) => ("scheduled", client.schedule_item(&id, wake).await?),
    };
    match &item.answer {
        Some(answer) if answer.confirmed => match &answer.fact {
            Some(fact) => println!("confirmed {id}: remembered as known fact {fact}"),
            None => println!("confirmed {id}"),
        },
        Some(_) => println!("dismissed {id}: nothing is remembered"),
        None => println!("{verb} {id}: now {}", item_state(&item)),
    }
    Ok(())
}

/// An item's state as `agenda` shows it.
fn item_state(item: &AgendaItem) -> String {
    match &item.state {
        AgendaState::Scheduled { wake } => format!("scheduled on {} wake(s)", wake.any_of.len()),
        AgendaState::Running { pid } => format!("running as {pid}"),
        other => other.kind().to_string(),
    }
}

/// The id of the agenda item `id` names: itself, or the one item whose id starts with it.
async fn resolve_item(client: &ControlClient, id: &str) -> Result<String> {
    let items = client.agenda(&AgendaQuery::default()).await?;
    if items.iter().any(|item| item.id == id) {
        return Ok(id.to_string());
    }
    let matching: Vec<&str> = items
        .iter()
        .map(|item| item.id.as_str())
        .filter(|candidate| candidate.starts_with(id))
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).to_string()),
        // The kernel answers an unknown id with the remedy.
        [] => Ok(id.to_string()),
        many => bail!(
            "{id} matches {} agenda items ({}); give more of the id",
            many.len(),
            many.join(", ")
        ),
    }
}

/// `adk-rust freeze`.
pub async fn freeze(target: &KernelTarget, reason: Option<&str>) -> Result<()> {
    let answer = client(target).await?.freeze(reason).await?;
    if let Some(error) = &answer.audit_error {
        eprintln!("warning: the freeze's audit record could not be written: {error}");
    }
    println!(
        "frozen ({}); {} running process(es) parked on this replica, every replica parks its \
         own. Lift it with `adk-rust unfreeze`.",
        answer.reason.as_deref().unwrap_or("no reason given"),
        answer.parked_runs.unwrap_or(0)
    );
    Ok(())
}

/// `adk-rust unfreeze`.
pub async fn unfreeze(target: &KernelTarget) -> Result<()> {
    let answer = client(target).await?.unfreeze().await?;
    if answer.frozen {
        bail!("the kernel is still frozen: {}", answer.reason.unwrap_or_default());
    }
    println!("unfrozen; the processes the freeze parked resume.");
    Ok(())
}

/// `adk-rust events`: what the kernel still holds after `after`, then, with `follow`, the
/// live events until Ctrl-C.
pub async fn events(
    target: &KernelTarget,
    follow: bool,
    after: Option<u64>,
    json: bool,
) -> Result<()> {
    let client = client(target).await?;
    let mut events = client.events(Some(after.unwrap_or(0))).await?;
    loop {
        let next = if follow {
            tokio::select! {
                next = events.next() => next,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        } else {
            match tokio::time::timeout(EVENTS_IDLE, events.next()).await {
                Ok(next) => next,
                Err(_) => return Ok(()),
            }
        };
        match next {
            Some(Ok(record)) if json => println!("{}", serde_json::to_string(&record)?),
            Some(Ok(record)) => println!("{}", event_line(&record)),
            Some(Err(error)) => return Err(error.into()),
            None => return Ok(()),
        }
    }
}

/// `adk-rust drivers` (spec 3.2 §7.7, §8.1).
pub async fn drivers(target: &KernelTarget, command: DriversCommands) -> Result<()> {
    let client = client(target).await?;
    match command {
        DriversCommands::List => {
            let drivers = client.drivers().await?;
            if drivers.is_empty() {
                println!("No drivers mounted. Mount one with `adk-rust drivers mount <file>`.");
                return Ok(());
            }
            for driver in &drivers {
                println!("{}", driver_line(driver));
            }
        }
        DriversCommands::Mount { file, yes } => {
            let mut manifest = DriverManifest::from_file(&file)?;
            // A workspace's manifest of the same driver may only restrict it (spec 3.2 §7.7).
            let project = std::env::current_dir()?.join(".adk").join("drivers");
            if let Some(narrower) =
                adk_kernel::drivers::load_dir(&project)?.into_iter().find(|m| m.id == manifest.id)
            {
                manifest = manifest.restrict_with(&narrower)?;
                println!("restricted by {}", project.display());
            }
            let text = std::fs::read_to_string(&file)?;
            let toml = if manifest == DriverManifest::from_toml(&text)? {
                text
            } else {
                restricted_toml(&manifest)?
            };
            let shown = client.mount_driver(&manifest.id, &toml, false).await?;
            print_proposal(&shown.proposal);
            if !yes && !confirm("Mount this driver?")? {
                println!("Not mounted.");
                return Ok(());
            }
            let mounted = client.mount_driver(&manifest.id, &toml, true).await?;
            match mounted.mounted {
                Some(driver) => println!("mounted {}", driver_line(&driver)),
                None => bail!("the kernel did not mount {}", manifest.id),
            }
        }
        DriversCommands::Unmount { id } => {
            if client.unmount_driver(&id).await? {
                println!("unmounted {id}");
            } else {
                bail!("no driver {id} is mounted");
            }
        }
        DriversCommands::Restart { id } => {
            println!("restarted {}", driver_line(&client.restart_driver(&id).await?));
        }
        DriversCommands::Reapprove { id } => {
            println!("re-approved {}", driver_line(&client.reapprove_driver(&id).await?));
        }
    }
    Ok(())
}

/// One driver as `drivers list` shows it.
fn driver_line(driver: &DriverView) -> String {
    let record = &driver.record;
    let isolation = match &record.isolation {
        Some(DriverIsolation::OsSandbox { enforcer, .. }) => format!("sandboxed ({enforcer})"),
        Some(DriverIsolation::Unsandboxed { reason }) => format!("unsandboxed: {reason}"),
        Some(_) => "isolated".to_string(),
        None => format!("{} (not started)", record.manifest.sandbox.as_str()),
    };
    let partitions: Vec<&str> =
        record.manifest.partitions.iter().map(PartitionId::as_str).collect();
    let mut line = format!(
        "{:<16} {:<16} [{}] {}  {}",
        record.id(),
        driver.state.as_str(),
        partitions.join(","),
        isolation,
        escape_control(&driver.command_line)
    );
    if !record.stale.is_empty() {
        let stale: Vec<&str> = record.stale.keys().map(String::as_str).collect();
        line.push_str(&format!(
            "\n  changed since approval: {} — `adk-rust drivers reapprove {}`",
            stale.join(", "),
            record.id()
        ));
    }
    line
}

/// What a mount asks the person to approve.
fn print_proposal(proposal: &DriverProposal) {
    println!("driver {}", proposal.id);
    println!("  command     {}", escape_control(&proposal.command_line));
    let partitions: Vec<&str> = proposal.partitions.iter().map(PartitionId::as_str).collect();
    println!("  partitions  {}", partitions.join(", "));
    println!("  sandbox     {}", proposal.sandbox.as_str());
    println!("  network     {}", if proposal.access.allow_network { "allowed" } else { "denied" });
    for (name, reference) in &proposal.secrets {
        println!("  secret      {name} ← {reference} (through the credential broker)");
    }
    println!("  tools (any other tool is `{}`):", proposal.unlisted_tools);
    for (tool, class) in &proposal.effect_classes {
        println!("    {tool:<24} {class}");
    }
    if proposal.sampling {
        println!("  may sample the harness model");
    }
}

/// Asks a yes/no question on the terminal; no answer is no.
fn confirm(question: &str) -> Result<bool> {
    use std::io::Write as _;
    if !std::io::stdin().is_terminal() {
        bail!("{question} needs an answer: run in a terminal or pass --yes");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// The TOML of a manifest a workspace restricted, as the kernel reads one.
fn restricted_toml(manifest: &DriverManifest) -> Result<String> {
    let mut table = toml::Table::new();
    let mut put = |key: &str, value: toml::Value| {
        table.insert(key.to_string(), value);
    };
    let strings = |items: Vec<String>| {
        toml::Value::Array(items.into_iter().map(toml::Value::String).collect())
    };
    put("id", toml::Value::String(manifest.id.clone()));
    put("command", toml::Value::String(manifest.server.command.clone()));
    put("args", strings(manifest.server.args.clone()));
    put("env", toml::Value::try_from(&manifest.server.env)?);
    put(
        "partitions",
        strings(manifest.partitions.iter().map(|p| p.as_str().to_string()).collect()),
    );
    put("lifecycle", toml::Value::try_from(manifest.lifecycle)?);
    put("protocol", toml::Value::try_from(manifest.server.lifecycle)?);
    put("sandbox", toml::Value::String(manifest.sandbox.as_str().to_string()));
    let paths = |items: &[std::path::PathBuf]| {
        strings(items.iter().map(|p| p.display().to_string()).collect())
    };
    put("allow_read", paths(&manifest.access.allow_read));
    put("allow_write", paths(&manifest.access.allow_write));
    put("allow_network", toml::Value::Boolean(manifest.access.allow_network));
    put("sampling", toml::Value::Boolean(manifest.sampling));
    put("data_classes", strings(manifest.data_classes.clone()));
    if let Some(restart) = &manifest.server.restart_policy {
        let mut policy = toml::Table::new();
        policy.insert(
            "max_attempts".into(),
            toml::Value::Integer(restart.max_restart_attempts.into()),
        );
        policy.insert(
            "initial_delay_ms".into(),
            toml::Value::Integer(restart.initial_delay_ms.try_into()?),
        );
        policy
            .insert("max_delay_ms".into(), toml::Value::Integer(restart.max_delay_ms.try_into()?));
        policy.insert("backoff_multiplier".into(), toml::Value::Float(restart.backoff_multiplier));
        put("restart", toml::Value::Table(policy));
    }
    let tools: toml::Table = manifest
        .tools
        .iter()
        .map(|(tool, class)| (tool.clone(), toml::Value::String(class.as_str().to_string())))
        .collect();
    put("tools", toml::Value::Table(tools));
    Ok(toml::to_string(&table)?)
}

/// `adk-rust attach`: the REPL on the running kernel.
pub async fn attach(target: &KernelTarget, session: Option<String>) -> Result<()> {
    repl(client(target).await?, session).await
}

/// The REPL on a running kernel (spec 3.2 §7.11): each line is a process the kernel runs
/// in `session`; the transcript is read back by cursor; a held call is answered here
/// through the approvals inbox, or from any other surface. Detaching changes nothing.
pub async fn repl(client: ControlClient, session: Option<String>) -> Result<()> {
    let console = ConsoleIo::terminal()?;
    let health = client.health().await?;
    let session = session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    println!(
        "Attached to the kernel at {} ({}{}).",
        client.endpoint(),
        health.state,
        if health.frozen { ", frozen" } else { "" }
    );
    println!(
        "Session {session}: each turn runs as a process the kernel owns. `exit` or Ctrl-D \
         detaches; the kernel keeps running.\n"
    );
    let mut cursor = None;
    match client.attach(&session, &AttachRequest::default()).await {
        Ok(attached) => {
            render(&attached.transcript, true);
            cursor = attached.cursor;
            if attached.process.is_some() {
                report(follow(&client, Some(&console), &session, &mut cursor, None).await?);
            }
        }
        // A new session: the first turn creates it.
        Err(error) if error.category() == ErrorCategory::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    loop {
        let line = match console.read_line("\x1b[36mYou >\x1b[0m ").await {
            ConsoleInput::Line(line) => line,
            ConsoleInput::Interrupted => {
                println!("(exit or Ctrl-D detaches)");
                continue;
            }
            ConsoleInput::Eof => break,
        };
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if matches!(text, "exit" | "quit" | "/exit" | "/quit") {
            break;
        }
        console.add_history(&line);
        let request = SpawnRequest::new(APP_TARGET, serde_json::json!({ "text": text }))
            .with_session_id(&session);
        let process = client.spawn(&request).await?;
        println!();
        let followed =
            follow(&client, Some(&console), &session, &mut cursor, Some(&process.meta.pid)).await?;
        report(followed);
    }
    println!(
        "\nDetached. Session {session} stays on the kernel: `adk-rust attach --session {session}`."
    );
    Ok(())
}

/// How following a session ended.
pub enum Followed {
    /// Nothing runs in the session any more; the process followed, when one was, as it
    /// finished.
    Done(Option<Box<Process>>),
    /// Calls wait for a decision and no one at this surface gave one.
    Held(usize),
    /// The person pressed Ctrl-C; the kernel carries on.
    Detached,
}

fn report(followed: Followed) {
    match followed {
        Followed::Done(Some(process)) if process.run.state == RunState::Failed => println!(
            "\nThe turn failed: {}\n",
            process.run.error.as_deref().map(escape_control).unwrap_or_default()
        ),
        Followed::Done(Some(process)) if process.run.state == RunState::Cancelled => {
            println!("\nThe turn was cancelled.\n");
        }
        Followed::Done(_) => println!(),
        Followed::Held(calls) => println!(
            "\n{calls} call(s) wait for a decision: `adk-rust approvals --server …` or the inbox.\n"
        ),
        Followed::Detached => {
            println!("\nDetached from the turn; it carries on in the kernel.\n");
        }
    }
}

/// Reads `session` from `cursor` until nothing runs in it, printing what it records and,
/// with a terminal `console`, asking for each held call (spec 3.2 §7.11, K5). Without one,
/// a held call ends the follow with [`Followed::Held`].
pub async fn follow(
    client: &ControlClient,
    console: Option<&ConsoleIo>,
    session: &str,
    cursor: &mut Option<DateTime<Utc>>,
    pid: Option<&str>,
) -> Result<Followed> {
    let mut asked = HashSet::new();
    let mut said_waiting = false;
    let mut said_frozen = false;
    loop {
        let request = AttachRequest { after: *cursor, ..AttachRequest::default() };
        let attached = match client.attach(session, &request).await {
            Ok(attached) => attached,
            // A new session exists once the kernel's worker claims its first turn.
            Err(error) if error.category() == ErrorCategory::NotFound && pid.is_some() => {
                let process = match pid {
                    Some(pid) => process_of(client, pid).await?,
                    None => None,
                };
                if let Some(process) = process.filter(|process| process.run.state.is_terminal()) {
                    return Ok(Followed::Done(Some(Box::new(process))));
                }
                tokio::select! {
                    () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
                    _ = tokio::signal::ctrl_c() => return Ok(Followed::Detached),
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        render(&attached.transcript, false);
        if attached.cursor.is_some() {
            *cursor = attached.cursor;
        }
        let Some(process) = attached.process else {
            let finished = match pid {
                Some(pid) => process_of(client, pid).await?.map(Box::new),
                None => None,
            };
            return Ok(Followed::Done(finished));
        };
        if process.run.state == RunState::Waiting {
            if process.meta.suspended_by == Some(Suspension::Freeze) && !said_frozen {
                println!("(frozen: this resumes on `adk-rust unfreeze`)");
                said_frozen = true;
            }
            if !attached.pending.is_empty() {
                let Some(console) = console.filter(|console| console.is_terminal()) else {
                    return Ok(Followed::Held(attached.pending.len()));
                };
                if let Some(ended) = decide_pending(client, console, session, &mut asked).await? {
                    return Ok(ended);
                }
                if !said_waiting {
                    println!("(waiting for a decision; any surface may give it — Ctrl-C detaches)");
                    said_waiting = true;
                }
            }
        }
        tokio::select! {
            () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
            _ = tokio::signal::ctrl_c() => return Ok(Followed::Detached),
        }
    }
}

/// The process `pid` as the kernel lists it now.
async fn process_of(client: &ControlClient, pid: &str) -> Result<Option<Process>> {
    let processes = client.processes(&ProcessesQuery::default()).await?;
    Ok(processes.into_iter().find(|process| process.meta.pid == pid))
}

/// Asks at `console` about each pending call of `session` not asked about yet, and decides
/// it through the approvals inbox. `Some` when the follow ends here.
async fn decide_pending(
    client: &ControlClient,
    console: &ConsoleIo,
    session: &str,
    asked: &mut HashSet<String>,
) -> Result<Option<Followed>> {
    let items = match client.approvals().await {
        Ok(items) => items,
        Err(error) if error.category() == ErrorCategory::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let waiting: Vec<_> = items
        .into_iter()
        .filter(|item| {
            item.scope.session_id == session
                && item.state == ApprovalState::Pending
                && !asked.contains(&item.id)
        })
        .collect();
    for item in waiting {
        asked.insert(item.id.clone());
        println!(
            "\n⏸ {} {}",
            escape_control(&item.tool_name),
            one_line(&escape_control(&item.args_redacted.to_string()))
        );
        if !item.summary.trim().is_empty() {
            println!("  {}", escape_control(&item.summary));
        }
        let answer = console.read_line("Approve? [y]es, [n]o, or a reason to refuse > ").await;
        let (action, reason) = match answer {
            ConsoleInput::Line(line) => match line.trim() {
                "y" | "yes" => (ApprovalAction::Approve, None),
                "" | "n" | "no" => (ApprovalAction::Reject, None),
                reason => (ApprovalAction::Reject, Some(reason.to_string())),
            },
            ConsoleInput::Interrupted | ConsoleInput::Eof => return Ok(Some(Followed::Detached)),
        };
        client.decide(&item.id, item.version, action, reason.as_deref()).await?;
        println!("{}", if action == ApprovalAction::Approve { "approved" } else { "refused" });
    }
    Ok(None)
}

/// Prints the model's text and the calls it made; the person's own lines only when
/// `with_user`, for a transcript read back on attach.
fn render(events: &[Event], with_user: bool) {
    for event in events {
        let Some(content) = &event.llm_response.content else {
            continue;
        };
        for part in &content.parts {
            match part {
                Part::Text { text } if text.trim().is_empty() => {}
                Part::Text { text } if event.author == "user" && with_user => {
                    println!("\x1b[36mYou >\x1b[0m {}", escape_control(text.trim()));
                }
                Part::Text { .. } if event.author == "user" => {}
                Part::Text { text } => println!("{}", escape_control(text.trim_end())),
                Part::FunctionCall { name, args, .. } => println!(
                    "\x1b[2m· {} {}\x1b[0m",
                    escape_control(name),
                    one_line(&escape_control(&args.to_string()))
                ),
                Part::FunctionResponse { function_response, .. }
                    if function_response.response.get("error").is_some() =>
                {
                    println!(
                        "\x1b[2m  ↳ {}: {}\x1b[0m",
                        escape_control(&function_response.name),
                        one_line(&escape_control(&function_response.response["error"].to_string()))
                    );
                }
                _ => {}
            }
        }
    }
}

/// The id of the process `pid` names: itself, or the one process whose id starts with it.
async fn resolve_pid(client: &ControlClient, pid: &str) -> Result<String> {
    let processes = client.processes(&ProcessesQuery::default()).await?;
    if processes.iter().any(|process| process.meta.pid == pid) {
        return Ok(pid.to_string());
    }
    let matching: Vec<&str> = processes
        .iter()
        .map(|process| process.meta.pid.as_str())
        .filter(|candidate| candidate.starts_with(pid))
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).to_string()),
        // The kernel answers an unknown id with the remedy.
        [] => Ok(pid.to_string()),
        many => bail!(
            "{pid} matches {} processes ({}); give more of the id",
            many.len(),
            many.join(", ")
        ),
    }
}

fn header() -> String {
    format!(
        "{:<36}  {:<9}  {:<13}  {:<11}  {:>7}  {:>9}  {:>9}  NOTE",
        "PID", "STATE", "KIND", "QUEUE", "AGE", "TODAY", "CALLS"
    )
}

fn row(process: &Process) -> String {
    let view = &process.view;
    format!(
        "{:<36}  {:<9}  {:<13}  {:<11}  {:>7}  {:>9}  {:>9}  {}",
        process.meta.pid,
        state_name(process.run.state),
        process.meta.kind.to_string(),
        process.meta.queue.as_str(),
        age(view.age),
        dollars(view.spend_today),
        format!("{}/{}", view.model_calls, view.tool_calls),
        note(process)
    )
}

/// What `ps` says about a process beyond its columns: suspension, sleep, goal, loop guard.
fn note(process: &Process) -> String {
    let mut notes = Vec::new();
    if let Some(suspension) = &process.meta.suspended_by {
        notes.push(format!("suspended: {suspension}"));
    } else if let Some(wake) = &process.meta.wake
        && process.run.state == RunState::Waiting
    {
        notes.push(format!("asleep on {} wake(s)", wake.any_of.len()));
    }
    if let Some(goal) = &process.view.goal {
        let mut text = format!("goal {}/{}", goal.iteration, goal.max_iterations);
        if let Some(exit) = goal.last_check_exit {
            text.push_str(&format!(", check exit {exit}"));
        }
        notes.push(text);
    }
    if let Some(verdict) = &process.view.loop_guard {
        notes.push(format!("loop guard: {verdict}"));
    }
    if process.run.state == RunState::Failed
        && let Some(error) = &process.run.error
    {
        notes.push(one_line(error));
    }
    if notes.is_empty() { String::new() } else { escape_control(&notes.join(" · ")) }
}

fn event_line(record: &EventRecord) -> String {
    let summary = match &record.event {
        KernelEvent::Process { pid, from, to, reason } => format!(
            "{pid} {} → {}{}",
            state_name(*from),
            state_name(*to),
            reason.as_deref().map(|reason| format!(" ({reason})")).unwrap_or_default()
        ),
        KernelEvent::Wake { pid, source, key, outcome } => {
            let what = match outcome {
                Some(adk_kernel::WakeOutcome::Woke { .. }) => " (woke)",
                Some(adk_kernel::WakeOutcome::Spawned { .. }) => " (spawned)",
                Some(adk_kernel::WakeOutcome::Deduplicated { .. }) => " (already used)",
                Some(adk_kernel::WakeOutcome::Expired { .. }) => " (expired)",
                _ => "",
            };
            format!("{source} {key} → {}{what}", pid.as_deref().unwrap_or("nothing"))
        }
        KernelEvent::Held { pid, request } => format!("{pid} holds {}", request.tool_name),
        KernelEvent::Decided { pid, by, .. } => format!("{pid} decided by {}", by.id),
        KernelEvent::RuleHit { pid, rule_id } => format!("{pid} allowed by rule {rule_id}"),
        KernelEvent::Spend { partition, delta, .. } => {
            format!("{partition} +{}", dollars(delta.micro_usd))
        }
        KernelEvent::Governance { frozen: true, reason } => {
            format!("frozen ({})", reason.as_deref().unwrap_or("no reason given"))
        }
        KernelEvent::Governance { frozen: false, .. } => "unfrozen".to_string(),
        KernelEvent::Health(health) => format!("kernel {}", health.state),
        KernelEvent::Agenda { item, from, to, reason } => format!(
            "{item} {} → {}{}",
            from.kind(),
            to.kind(),
            reason.as_deref().map(|reason| format!(" ({reason})")).unwrap_or_default()
        ),
        KernelEvent::Review(report) => format!(
            "{}: {} rule(s), {} sleeper(s), {} freeze(s) older than {}",
            report.partition,
            report.rules.len(),
            report.sleepers.len(),
            report.frozen.len(),
            age(report.review_period)
        ),
        KernelEvent::Observation { id, partition } => {
            format!("{partition} observation {id}: `adk-rust proc` shows its question")
        }
        other => other.name().to_string(),
    };
    format!(
        "#{:<6} {}  {:<10} {}",
        record.cursor,
        record.at.format("%H:%M:%S"),
        record.event.name(),
        escape_control(&summary)
    )
}

fn state_name(state: RunState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{state:?}"))
}

/// Micro-USD as dollars.
fn dollars(micro_usd: u64) -> String {
    format!("${:.4}", micro_usd as f64 / 1_000_000.0)
}

/// A duration as `45s`, `3m12s`, `2h05m` or `3d04h`.
fn age(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        3_600..86_400 => format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60),
        _ => format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600),
    }
}

fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(120).collect();
    if cut.len() < line.len() { format!("{cut}…") } else { cut }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_times_read_three_ways() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-23T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_at("2026-10-24T07:00:00Z", now).unwrap().to_rfc3339(),
            "2026-10-24T07:00:00+00:00"
        );
        assert_eq!(parse_at("+90m", now).unwrap(), now + chrono::Duration::minutes(90));
        assert_eq!(parse_at("+1d", now).unwrap(), now + chrono::Duration::days(1));
        let local = parse_at("07:00", now).unwrap();
        assert!(local > now && local - now <= chrono::Duration::days(1), "the next 07:00");
        assert!(parse_at("tomorrow", now).is_err());
        assert!(parse_at("+5y", now).is_err());
        let wake = schedule_wake(None, Some("0 0 3 * * *")).unwrap();
        assert!(matches!(wake.any_of.as_slice(), [adk_kernel::Wake::At(_)]));
        assert!(schedule_wake(Some("+1h"), Some("0 0 3 * * *")).is_err());
        assert!(wake_on(None, None, None).is_err());
        let cron = wake_on(None, Some("0 0 8 * * Mon-Fri"), None).unwrap();
        assert!(
            matches!(cron.wake.any_of.as_slice(), [adk_kernel::Wake::Cron { expr, .. }] if expr == "0 0 8 * * Mon-Fri")
        );
        assert!(wake_on(None, Some("not cron"), None).is_err(), "a bad expression fails at once");
        let hook = wake_on(None, None, Some("github")).unwrap();
        assert_eq!(hook.wake.any_of, [adk_kernel::Wake::Webhook { hook: "github".into() }]);
    }

    #[test]
    fn a_restricted_manifest_reads_back_as_the_kernel_parses_it() {
        let user = DriverManifest::from_toml(
            "id = \"email\"\ncommand = \"mcp-email\"\nargs = [\"--imap\"]\n\
             env = { EMAIL_TOKEN = \"secret://email/token\" }\npartitions = [\"work\", \"home\"]\n\
             sandbox = \"unsandboxed\"\nallow_read = [\"/srv/mail\"]\nsampling = true\n\
             restart = { max_attempts = 3 }\n[tools]\nsend_draft = \"send_external\"\n",
        )
        .unwrap();
        let project = DriverManifest::from_toml(
            "id = \"email\"\ncommand = \"mcp-email\"\nargs = [\"--imap\"]\n\
             env = { EMAIL_TOKEN = \"secret://email/token\" }\npartitions = [\"work\"]\n\
             sandbox = \"os\"\nallow_network = false\nrestart = { max_attempts = 3 }\n\
             [tools]\nsend_draft = \"publish_public\"\n",
        )
        .unwrap();
        let restricted = user.restrict_with(&project).unwrap();
        let text = restricted_toml(&restricted).unwrap();
        assert_eq!(DriverManifest::from_toml(&text).unwrap(), restricted, "{text}");
    }

    #[test]
    fn ages_and_amounts_read_at_a_glance() {
        let cases = [(0, "0s"), (59, "59s"), (192, "3m12s"), (7_500, "2h05m"), (273_600, "3d04h")];
        for (seconds, shown) in cases {
            assert_eq!(age(Duration::from_secs(seconds)), shown);
        }
        assert_eq!(dollars(12_345), "$0.0123");
        assert_eq!(state_name(RunState::Waiting), "waiting");
    }

    #[test]
    fn a_process_row_names_its_suspension_and_goal() {
        let run = adk_core::RunRecord::queued(
            "run-1",
            adk_core::NewRun::new(
                "background",
                "adk-rust",
                "ana",
                "harness.goal",
                serde_json::json!({}),
            ),
            Utc::now(),
        );
        let meta = adk_kernel::ProcessMeta::new(
            "run-1",
            adk_kernel::QueueClass::Background,
            adk_kernel::ProcessKind::Goal,
        )
        .with_suspension(Suspension::Freeze);
        let mut view = adk_kernel::ProcessView::default();
        view.goal = Some(adk_kernel::GoalProgress::new(3, 10).with_last_check(1, "1 failed"));
        let process = Process::new(run, meta).with_view(view);
        let shown = row(&process);
        assert!(shown.starts_with("run-1"), "{shown}");
        assert!(shown.contains("suspended: freeze · goal 3/10, check exit 1"), "{shown}");
        assert!(shown.contains("background"), "{shown}");
    }
}

/// What `adk-rust proc` selects (spec 3.2 §7.10, §8.1).
#[derive(Debug, Clone, Default)]
pub struct ProcQuery {
    pub partition: Option<String>,
    pub kind: Option<String>,
    pub agent: Option<String>,
    pub since: Option<String>,
    pub limit: Option<usize>,
    pub export: Option<PathBuf>,
}

/// `adk-rust proc [ledger|baselines|observations]` (spec 3.2 §7.10, §8.1).
pub async fn proc(target: &KernelTarget, view: ProcView, query: ProcQuery) -> Result<()> {
    let client = client(target).await?;
    match view {
        ProcView::Ledger => proc_ledger(&client, query).await,
        ProcView::Baselines => proc_baselines(&client, query.partition.as_deref()).await,
        ProcView::Observations => {
            let params = ObservationsParams {
                partition: query.partition.map(PartitionId::from),
                limit: Some(query.limit.unwrap_or(20)),
                ..ObservationsParams::default()
            };
            let observations = client.observations(&params).await?;
            if observations.is_empty() {
                println!(
                    "No observations. The kernel offers one when a partition's routine changes \
                     for long enough against its 28-day baseline."
                );
            }
            for observation in observations {
                let worded = match &observation.phrasing {
                    Phrasing::Model { model } => format!("worded by {model}"),
                    _ => "worded by the kernel".to_string(),
                };
                println!(
                    "{}  {}  {}",
                    observation.as_of,
                    observation.partition,
                    escape_control(&observation.question)
                );
                for dimension in &observation.dimensions {
                    let arrow = match dimension.direction {
                        Direction::Higher => "up",
                        _ => "down",
                    };
                    let days = if dimension.days == 1 { "day" } else { "days" };
                    println!(
                        "    {} {arrow} since {} ({} {days})",
                        dimension.dimension, dimension.since, dimension.days
                    );
                }
                println!("    {worded}; id {}", observation.id);
            }
            Ok(())
        }
    }
}

/// The instant `--since` names: an RFC 3339 instant, or the start of a day in UTC.
fn since(value: &str) -> Result<DateTime<Utc>> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(value) {
        return Ok(instant.with_timezone(&Utc));
    }
    let day = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("--since {value}: use an RFC 3339 instant or YYYY-MM-DD"))?;
    Ok(day.and_time(chrono::NaiveTime::MIN).and_utc())
}

async fn proc_ledger(client: &ControlClient, query: ProcQuery) -> Result<()> {
    let from = match (&query.since, &query.export) {
        (Some(value), _) => Some(since(value)?),
        (None, Some(_)) => None,
        (None, None) => Some(Utc::now() - chrono::Duration::hours(24)),
    };
    let mut params = LedgerParams {
        partition: query.partition.map(PartitionId::from),
        kind: query.kind,
        agent: query.agent,
        from,
        ..LedgerParams::default()
    };
    if let Some(path) = query.export {
        use std::io::Write as _;
        let mut out: Box<dyn std::io::Write> = if path.as_os_str() == "-" {
            Box::new(std::io::stdout().lock())
        } else {
            Box::new(std::io::BufWriter::new(
                std::fs::File::create(&path)
                    .with_context(|| format!("could not create {}", path.display()))?,
            ))
        };
        params.limit = Some(LedgerParams::MAX_LIMIT);
        let mut rows = 0_usize;
        loop {
            let page = client.ledger(&params).await?;
            for entry in &page.entries {
                writeln!(out, "{}", entry.to_jsonl()?)?;
            }
            rows += page.entries.len();
            if query.limit.is_some_and(|limit| rows >= limit) {
                break;
            }
            match page.next {
                Some(next) => params.after = Some(next),
                None => break,
            }
        }
        out.flush()?;
        if path.as_os_str() != "-" {
            eprintln!(
                "exported {rows} ledger rows to {}; check them with `cargo adk eval --ledger {}`",
                path.display(),
                path.display()
            );
        }
        return Ok(());
    }
    params.limit = Some(query.limit.unwrap_or(100));
    let page = client.ledger(&params).await?;
    if page.entries.is_empty() {
        println!("No ledger rows in that window.");
        return Ok(());
    }
    println!(
        "{:<26}  {:<10}  {:<12}  {:<16}  {:>8}  AGENT",
        "TIME", "PARTITION", "KIND", "EFFECT", "MS"
    );
    for entry in &page.entries {
        println!(
            "{:<26}  {:<10}  {:<12}  {:<16}  {:>8}  {}",
            entry.ts.format("%Y-%m-%d %H:%M:%S %:z"),
            entry.partition,
            entry.kind,
            entry.effect.as_deref().unwrap_or("-"),
            entry.duration_ms.map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            entry.agent
        );
    }
    if page.next.is_some() {
        println!("… more rows: raise --limit, or write them all with --export FILE");
    }
    Ok(())
}

/// A dimension's value as it reads: minutes of the day as a time, others as numbers.
fn dimension_value(dimension: &str, value: f64) -> String {
    if dimension.ends_with("_minute") {
        let minutes = value.round().clamp(0.0, 1439.0) as u32;
        format!("{:02}:{:02}", minutes / 60, minutes % 60)
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

async fn proc_baselines(client: &ControlClient, partition: Option<&str>) -> Result<()> {
    let states = client.baselines().await?;
    let states: Vec<_> = states
        .into_iter()
        .filter(|state| partition.is_none_or(|partition| state.partition.as_str() == partition))
        .collect();
    if states.is_empty() {
        println!("No baselines yet: the kernel tests each day once it has ended.");
        return Ok(());
    }
    for state in states {
        println!("{} — tested through {}", state.partition, state.last_day);
        let Ok(snapshot) = serde_json::from_value::<BaselineSnapshot>(state.baselines) else {
            println!("    (no baselines recorded)");
            continue;
        };
        println!(
            "    {:<28}  {:<8}  {:>8}  {:>7}  {:>8}  {:>8}  STATE",
            "DIMENSION", "DAYS", "MEDIAN", "MAD", "LAST 7", "LAST 14"
        );
        for test in &snapshot.dimensions {
            for class in &test.classes {
                let Some(baseline) = class.baseline else { continue };
                let window = |days: u32| {
                    class
                        .windows
                        .iter()
                        .find(|window| window.days == days)
                        .and_then(|window| window.recent_median)
                        .map_or_else(
                            || "-".to_string(),
                            |value| dimension_value(&test.dimension, value),
                        )
                };
                let state = if class.deviating {
                    "changed"
                } else if !test.warm_up.is_complete() {
                    "learning"
                } else {
                    "usual"
                };
                println!(
                    "    {:<28}  {:<8}  {:>8}  {:>7}  {:>8}  {:>8}  {state}",
                    test.dimension,
                    format!("{:?}", class.class).to_lowercase(),
                    dimension_value(&test.dimension, baseline.median),
                    format!("{:.1}", baseline.mad),
                    window(7),
                    window(14)
                );
            }
        }
    }
    Ok(())
}
