//! The running kernel's commands over its control plane (spec 3.2 §7.11, §8.1): `attach`,
//! `ps`, `top`, `kill`, `suspend`, `resume`, `freeze`, `unfreeze` and `events`, and the
//! REPL `adk-rust` opens on a running kernel. Surfaces hold no state (K-I17): every command
//! reads the kernel, and detaching changes nothing in its process table.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use adk_cli::console::{ConsoleInput, ConsoleIo};
use adk_core::{ApprovalAction, ApprovalState, ErrorCategory, Event, Part, RunState};
use adk_harness::escape_control;
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{AttachRequest, ProcessesQuery, SpawnRequest};
use adk_kernel::{EventRecord, KernelEvent, PartitionId, Process, Suspension};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use futures::StreamExt;

use crate::cli::KernelTarget;

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

/// `adk-rust freeze`.
pub async fn freeze(target: &KernelTarget, reason: Option<&str>) -> Result<()> {
    let answer = client(target).await?.freeze(reason).await?;
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
        KernelEvent::Wake { pid, source, key } => {
            format!("{source} {key} → {}", pid.as_deref().unwrap_or("nothing"))
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
