//! K3, K4, K5, K6 and K10 on a live model through the `adk-rust up` binary (spec 3.2 §11):
//! the kernel runs as its own process, and this one reaches it over the control plane like
//! any other surface. Keys come from the environment the binary inherits and are never
//! printed.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{AgendaQuery, AttachRequest, ProcessesQuery, SpawnRequest};
use adk_kernel::{Wake, WakeSet};
use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::Checks;

/// The model each provider runs on.
pub fn model_for(provider: &str) -> Option<&'static str> {
    let (model, keys): (&str, &[&str]) = match provider {
        "anthropic" => ("claude-sonnet-5-5", &["ANTHROPIC_API_KEY"]),
        "gemini" => ("gemini-3.5-flash-lite", &["GOOGLE_API_KEY", "GEMINI_API_KEY"]),
        _ => return None,
    };
    keys.iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()))
        .then_some(model)
}

pub async fn run(name: &str, bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    match name {
        "live_k5_approve_from_another_process" => k5(bin, provider, model).await,
        "live_k3_file_change_wakes" => k3(bin, provider, model).await,
        "live_k4_a_proposal_waits_for_approval" => k4(bin, provider, model).await,
        "live_k6_supervised_driver" => k6(bin, provider, model).await,
        "live_parked_question_after_restart" => parked_question(bin, provider, model).await,
        "live_k10_proc" => k10(bin, provider, model).await,
        other => bail!("unknown scenario {other}"),
    }
}

/// `adk-rust up` running in a private home, stopped when dropped.
struct Up {
    child: Child,
    root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    port: u16,
}

impl Up {
    async fn start(bin: &Path, provider: &str, model: &str, mode: &str) -> Result<Self> {
        Self::start_seeded(bin, provider, model, mode, false, |_| async { Ok(()) }).await
    }

    /// Starts `adk-rust up` after `seed` wrote what the kernel home holds before it, with
    /// long-term memory when `memory` is set.
    async fn start_seeded<F, Fut>(
        bin: &Path,
        provider: &str,
        model: &str,
        mode: &str,
        memory: bool,
        seed: F,
    ) -> Result<Self>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let root = tempfile::tempdir()?;
        let (home, workspace) = (root.path().join("kernel"), root.path().join("ws"));
        std::fs::create_dir_all(&home)?;
        seed(home.clone()).await?;
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("README.md"), "a project\n")?;
        Self::boot(bin, provider, model, mode, root, home, workspace).await
    }

    /// Stops this kernel with `SIGTERM` and starts it again on the same home.
    async fn restart(self, bin: &Path, provider: &str, model: &str, mode: &str) -> Result<Self> {
        let mut this = std::mem::ManuallyDrop::new(self);
        stop(&mut this.child);
        // SAFETY: each field is read once and `this` is never used or dropped again.
        let (root, home, workspace) = unsafe {
            (std::ptr::read(&this.root), std::ptr::read(&this.home), std::ptr::read(&this.workspace))
        };
        Self::boot(bin, provider, model, mode, root, home, workspace).await
    }

    async fn boot(
        bin: &Path,
        provider: &str,
        model: &str,
        mode: &str,
        root: tempfile::TempDir,
        home: PathBuf,
        workspace: PathBuf,
    ) -> Result<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.path().join("up.log"))?;
        let mut command = Command::new(bin);
        command
            .args(["up", "--home"])
            .arg(&home)
            .args(["--bind", &format!("127.0.0.1:{port}"), "--mode", mode, "--dir"])
            .arg(&workspace)
            .args(["--provider", provider, "--model", model])
            .args((!memory).then_some("--no-memory"))
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        isolate(&mut command, root.path());
        let child =
            command.spawn().with_context(|| format!("failed to start {}", bin.display()))?;
        let mut up = Self { child, root, home, workspace, port };
        let started = Instant::now();
        loop {
            if let Some(status) = up.child.try_wait()? {
                bail!("adk-rust up exited with {status}:\n{}", up.log());
            }
            if ControlClient::connect_local(&up.home).await?.is_some() {
                return Ok(up);
            }
            if started.elapsed() > Duration::from_secs(60) {
                bail!("adk-rust up did not open its socket within a minute:\n{}", up.log());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn client(&self) -> Result<ControlClient> {
        ControlClient::connect_local(&self.home).await?.context("the kernel's socket")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.path().join("up.log")).unwrap_or_default()
    }
}

impl Drop for Up {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// `SIGTERM`, which drains the kernel; a stuck one is killed.
fn stop(child: &mut Child) {
    let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(40) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A private `HOME` and config directory, so nothing reads or writes the user's own.
fn isolate(command: &mut Command, root: &Path) {
    command
        .env("HOME", root.join("user"))
        .env("XDG_CONFIG_HOME", root.join("user/.config"))
        .env("RUST_LOG", "warn")
        .env_remove("ADK_SERVE_TOKEN");
}

/// Waits until `check` holds, polling every 250 ms, for at most `limit`.
async fn until<F, Fut>(limit: Duration, mut check: F) -> Result<bool>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check().await? {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(false)
}

/// The text and the calls of a transcript, for the checks.
fn transcript_text(events: &[adk_core::Event]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for event in events {
        let Some(content) = &event.llm_response.content else { continue };
        for part in &content.parts {
            match part {
                adk_core::Part::Text { text: chunk } if event.author != "user" => {
                    text.push_str(chunk)
                }
                adk_core::Part::FunctionCall { name, args, .. } => {
                    calls.push(format!("{name} {args}"))
                }
                _ => {}
            }
        }
    }
    (text, calls)
}

/// K5 live: a held `bash` call, approved over the control plane by another process
/// (`adk-rust approvals --server`), runs once, and the turn completes without the client
/// that started it.
async fn k5(bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let up = Up::start(bin, provider, model, "ask").await?;
    let terminal = up.client().await?;
    let request = SpawnRequest::new(
        "adk-rust",
        json!({ "text": "Use the bash tool to run exactly this command, once: echo kernel >> marker.txt\nThen reply with the single word: done" }),
    )
    .with_session_id("k5-live");
    let pid = terminal.spawn(&request).await?.meta.pid;
    let held = until(Duration::from_secs(120), || {
        let terminal = terminal.clone();
        async move {
            match terminal.attach("k5-live", &AttachRequest::default()).await {
                Ok(attached) => Ok(!attached.pending.is_empty()),
                // The session exists once the kernel's worker claims the turn.
                Err(error) if error.category() == adk_core::ErrorCategory::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
    })
    .await?;
    checks.check("the model's bash call is held for a decision", held);
    checks.check("nothing ran before the decision", !up.workspace.join("marker.txt").exists());
    drop(terminal);

    let items = up.client().await?.approvals().await?;
    let Some(item) = items.iter().find(|item| item.scope.session_id == "k5-live") else {
        checks.check("the held call is in the inbox", false);
        return Ok(checks);
    };
    let mut approve = Command::new(bin);
    approve
        .args([
            "approvals",
            "--server",
            &format!("http://127.0.0.1:{}", up.port),
            "approve",
            &item.id,
        ])
        .stdin(Stdio::null());
    isolate(&mut approve, up.root.path());
    let approved = approve.output()?;
    checks.check("another process approved it over the control plane", approved.status.success());

    let observer = up.client().await?;
    let finished = until(Duration::from_secs(180), || {
        let observer = observer.clone();
        let pid = pid.clone();
        async move {
            let processes = observer.processes(&ProcessesQuery::default()).await?;
            Ok(processes
                .iter()
                .any(|process| process.meta.pid == pid && process.run.state.is_terminal()))
        }
    })
    .await?;
    let processes = observer.processes(&ProcessesQuery::default()).await?;
    let state =
        processes.iter().find(|process| process.meta.pid == pid).map(|process| process.run.state);
    checks.check(
        "the turn completed without the terminal that started it",
        finished && state == Some(adk_core::RunState::Completed),
    );
    checks.check(
        "the command ran once",
        std::fs::read_to_string(up.workspace.join("marker.txt")).unwrap_or_default() == "kernel\n",
    );
    let attached = observer.attach("k5-live", &AttachRequest::default()).await?;
    let (_, calls) = transcript_text(&attached.transcript);
    checks.check(
        "a new client attaches and sees the call in the transcript",
        calls.iter().any(|call| call.starts_with("bash") && call.contains("marker.txt")),
    );
    Ok(checks)
}

/// K3 live: a process asleep on a file wake makes no model call; saving the file wakes it
/// within two seconds, and the turn reads the file.
async fn k3(bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let up = Up::start(bin, provider, model, "ask").await?;
    let client = up.client().await?;
    let wake = WakeSet::any_of([Wake::FileChange {
        root: up.workspace.clone(),
        glob: "notes.md".into(),
        debounce: Duration::from_millis(300),
    }]);
    let request = SpawnRequest::new(
        "adk-rust",
        json!({ "text": "Read notes.md with the read_file tool and reply with the codeword it contains, nothing else." }),
    )
    .with_session_id("k3-live")
    .with_wake(wake);
    let pid = client.spawn(&request).await?.meta.pid;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let asleep = client.processes(&ProcessesQuery::default()).await?;
    let sleeping = asleep.iter().find(|process| process.meta.pid == pid).is_some_and(|process| {
        process.run.state == adk_core::RunState::Waiting && process.view.model_calls == 0
    });
    checks.check("the process sleeps on its wake set with no model call", sleeping);

    std::fs::write(up.workspace.join("notes.md"), "codeword: heliotrope\n")?;
    let saved = Instant::now();
    let woke = until(Duration::from_secs(10), || {
        let client = client.clone();
        let pid = pid.clone();
        async move {
            let processes = client.processes(&ProcessesQuery::default()).await?;
            Ok(processes.iter().any(|process| {
                process.meta.pid == pid && process.run.state != adk_core::RunState::Waiting
            }))
        }
    })
    .await?;
    let latency = saved.elapsed();
    checks.check("saving the file woke it", woke);
    checks.check("within two seconds", woke && latency <= Duration::from_secs(2));
    let finished = until(Duration::from_secs(180), || {
        let client = client.clone();
        let pid = pid.clone();
        async move {
            let processes = client.processes(&ProcessesQuery::default()).await?;
            Ok(processes
                .iter()
                .any(|process| process.meta.pid == pid && process.run.state.is_terminal()))
        }
    })
    .await?;
    let attached = client.attach("k3-live", &AttachRequest::default()).await?;
    let (text, calls) = transcript_text(&attached.transcript);
    if !finished {
        let processes = client.processes(&ProcessesQuery::default()).await?;
        let process = processes.iter().find(|process| process.meta.pid == pid);
        println!(
            "    state {:?}, error {:?}, held {:?}, calls {calls:?}",
            process.map(|process| process.run.state),
            process.and_then(|process| process.run.error.clone()),
            attached.pending.iter().map(|request| request.tool_name.clone()).collect::<Vec<_>>(),
        );
    }
    checks.check("the woken turn completed", finished);
    checks.check(
        "it read the file",
        calls.iter().any(|call| call.starts_with("read_file") && call.contains("notes.md")),
    );
    checks.check("and answered from it", text.to_lowercase().contains("heliotrope"));
    Ok(checks)
}

/// K4 live: the model proposes a follow-up with `propose_followup`; in Ask it waits on the
/// agenda and nothing runs until another process approves it with `adk-rust agenda
/// approve`; then it runs as a `Proposed` process whose parent is the proposing turn.
async fn k4(bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let up = Up::start(bin, provider, model, "ask").await?;
    let client = up.client().await?;
    let request = SpawnRequest::new(
        "adk-rust",
        json!({ "text": "The dependency bump lands tonight. Do not do anything about it now. \
            Call the propose_followup tool once, with the title \"Write a status note about the dependency bump\" \
            and a one-sentence rationale, asking for a reply-only note that needs no tools. \
            Then reply with the single word: proposed" }),
    )
    .with_session_id("k4-live");
    let pid = client.spawn(&request).await?.meta.pid;
    let item = |client: ControlClient, pid: String| async move {
        let items = client.agenda(&AgendaQuery::default()).await?;
        Ok::<_, anyhow::Error>(items.into_iter().find(|item| item.proposer() == Some(pid.as_str())))
    };
    let proposed = until(Duration::from_secs(120), || {
        let (client, pid) = (client.clone(), pid.clone());
        async move { Ok(item(client, pid).await?.is_some()) }
    })
    .await?;
    checks.check("the model proposed a follow-up", proposed);
    let Some(proposal) = item(client.clone(), pid.clone()).await? else { return Ok(checks) };
    tokio::time::sleep(Duration::from_secs(3)).await;
    let followup = |client: ControlClient, id: String| async move {
        let processes = client.processes(&ProcessesQuery::default()).await?;
        Ok::<_, anyhow::Error>(
            processes
                .into_iter()
                .find(|process| process.meta.agenda_item.as_deref() == Some(id.as_str())),
        )
    };
    let held = item(client.clone(), pid.clone()).await?;
    checks.check(
        "in Ask it waits proposed and nothing runs",
        held.is_some_and(|item| item.state == adk_kernel::AgendaState::Proposed)
            && followup(client.clone(), proposal.id.clone()).await?.is_none(),
    );
    let mut list = Command::new(bin);
    list.args(["agenda", "--home"]).arg(&up.home).stdin(Stdio::null());
    isolate(&mut list, up.root.path());
    let listed = list.output()?;
    println!("    $ adk-rust agenda\n{}", indent(&String::from_utf8_lossy(&listed.stdout)));
    let mut approve = Command::new(bin);
    approve
        .args(["agenda", "--home"])
        .arg(&up.home)
        .args(["approve", &proposal.id[..8]])
        .stdin(Stdio::null());
    isolate(&mut approve, up.root.path());
    let approved = approve.output()?;
    checks.check(
        "another process approved it with `adk-rust agenda approve`",
        approved.status.success(),
    );
    println!(
        "    $ adk-rust agenda approve {}\n{}",
        &proposal.id[..8],
        indent(&String::from_utf8_lossy(&approved.stdout))
    );
    // The follow-up runs in Ask too (K-I7): a call it holds waits for a person, who
    // rejects it here, as the inbox's operator would.
    let follow_session = adk_kernel::agenda::item_session(&proposal.id);
    let rejected = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = until(Duration::from_secs(240), || {
        let (client, pid) = (client.clone(), pid.clone());
        let (follow_session, rejected) = (follow_session.clone(), rejected.clone());
        async move {
            for held in client.approvals().await? {
                if held.scope.session_id == follow_session {
                    let action = adk_core::ApprovalAction::Reject;
                    client.decide(&held.id, held.version, action, Some("not now")).await?;
                    rejected.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Ok(item(client, pid)
                .await?
                .is_some_and(|item| item.state == adk_kernel::AgendaState::Done))
        }
    })
    .await?;
    checks.check("the follow-up ran", done);
    let rejected = rejected.load(std::sync::atomic::Ordering::SeqCst);
    if rejected > 0 {
        println!("    (the follow-up held {rejected} call(s) for a person, in Ask; rejected)");
    }
    let process = followup(client.clone(), proposal.id.clone()).await?;
    checks.check(
        "as a `Proposed` process whose parent is the proposing turn",
        process.as_ref().is_some_and(|process| {
            process.meta.kind == adk_kernel::ProcessKind::Proposed
                && process.meta.parent.as_deref() == Some(pid.as_str())
                && process.run.state == adk_core::RunState::Completed
        }),
    );
    if let Some(process) = &process {
        let session = process.run.session_id.clone().unwrap_or_default();
        let attached = client.attach(&session, &AttachRequest::default()).await?;
        let (text, calls) = transcript_text(&attached.transcript);
        checks.check("the follow-up answered in its own session", !text.trim().is_empty());
        if !done {
            let held: Vec<&str> =
                attached.pending.iter().map(|request| request.tool_name.as_str()).collect();
            println!(
                "    (follow-up {} is {:?}; held calls {held:?}; calls made {calls:?})",
                process.meta.pid, process.run.state
            );
        }
    }
    Ok(checks)
}

/// `text` indented for the scenario's output.
fn indent(text: &str) -> String {
    text.lines().map(|line| format!("      {line}")).collect::<Vec<_>>().join("\n")
}

/// Runs `adk-rust <command> --home <home> <args>` against the kernel of `up` and returns
/// its output.
fn cli(bin: &Path, up: &Up, command_and_args: &[&str]) -> Result<(bool, String)> {
    let (name, args) = command_and_args.split_first().context("a command")?;
    let mut command = Command::new(bin);
    command.arg(name).arg("--home").arg(&up.home).args(args).stdin(Stdio::null());
    isolate(&mut command, up.root.path());
    let output = command.output()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), text))
}

/// Writes the notes driver's manifest under `up`'s root and mounts it with
/// `adk-rust drivers mount --yes`.
fn mount_notes(bin: &Path, up: &Up, instance: &str) -> Result<(bool, String)> {
    let notes = up.root.path().join("notes");
    std::fs::create_dir_all(&notes)?;
    let manifest = up.root.path().join("notes.toml");
    std::fs::write(&manifest, crate::notes_driver::manifest(&std::env::current_exe()?, &notes, instance))?;
    let path = manifest.display().to_string();
    cli(bin, up, &["drivers", "mount", &path, "--yes"])
}

/// Spawns a turn of `text` in `session` and waits for it to reach `state`.
async fn turn_until(up: &Up, session: &str, text: &str, done: impl Fn(adk_core::RunState) -> bool) -> Result<(String, bool)> {
    let client = up.client().await?;
    let pid = client.spawn(&SpawnRequest::new("adk-rust", json!({ "text": text })).with_session_id(session)).await?.meta.pid;
    let reached = until(Duration::from_secs(180), || {
        let client = client.clone();
        let pid = pid.clone();
        let done = &done;
        async move {
            let processes = client.processes(&ProcessesQuery::default()).await?;
            Ok(processes.iter().any(|process| process.meta.pid == pid && done(process.run.state)))
        }
    })
    .await?;
    Ok((pid, reached))
}

/// K6 live: the notes driver mounted with `adk-rust drivers mount`, used by the model,
/// killed and restarted with backoff, redeclared and held until `drivers reapprove`.
async fn k6(bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let up = Up::start(bin, provider, model, "ask").await?;
    let instance = format!("live-k6-{}", std::process::id());
    let (mounted, shown) = mount_notes(bin, &up, &instance)?;
    checks.check(
        "`drivers mount` shows the command line, partitions and classes, and mounts",
        mounted && shown.contains("read_note") && shown.contains("send_external") && shown.contains("mounted notes"),
    );
    let ask = "Call the read_note tool once and reply with the codeword it returns, nothing else.";
    let (_, finished) = turn_until(&up, "k6-first", ask, |state| state.is_terminal()).await?;
    let attached = up.client().await?.attach("k6-first", &AttachRequest::default()).await?;
    let (text, calls) = transcript_text(&attached.transcript);
    checks.check("the model used the driver's read tool without approval", finished && calls.iter().any(|call| call.starts_with("read_note")));
    checks.check("and answered from it", text.contains(crate::notes_driver::CODEWORD));

    let client = up.client().await?;
    let mut events = client.events(None).await?;
    crate::notes_driver::kill(&instance)?;
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !seen.ends_with(&["running".to_string()]) {
        if let Ok(Some(Ok(record))) = tokio::time::timeout(Duration::from_secs(2), futures_next(&mut events)).await
            && let adk_kernel::KernelEvent::Driver { id, state, .. } = record.event
            && id == "notes"
        {
            seen.push(state.as_str().to_string());
        }
    }
    checks.check(
        "a killed driver restarts with backoff",
        seen.iter().any(|state| state == "crashed")
            && seen.iter().any(|state| state == "restarting")
            && seen.last().is_some_and(|state| state == "running"),
    );

    std::fs::write(up.root.path().join("notes/declaration.txt"), "it now also forwards the note")?;
    let (restarted, _) = cli(bin, &up, &["drivers", "restart", "notes"])?;
    let (_, listed) = cli(bin, &up, &["drivers", "list"])?;
    checks.check(
        "a redeclared tool needs re-approval",
        restarted && listed.contains("needsReapproval") && listed.contains("changed since approval: read_note"),
    );
    let (pid, held) = turn_until(&up, "k6-stale", ask, |state| state == adk_core::RunState::Waiting).await?;
    let items = up.client().await?.approvals().await?;
    checks.check("its call is held for a person", held && items.iter().any(|item| item.tool_name == "read_note"));
    if let Some(item) = items.iter().find(|item| item.tool_name == "read_note") {
        let client = up.client().await?;
        client.decide(&item.id, item.version, adk_core::ApprovalAction::Reject, Some("checked".into())).await?;
    }
    let _ = pid;
    let (reapproved, _) = cli(bin, &up, &["drivers", "reapprove", "notes"])?;
    let (_, listed) = cli(bin, &up, &["drivers", "list"])?;
    checks.check("`drivers reapprove` restores it", reapproved && listed.contains("running") && !listed.contains("changed since approval"));
    let (_, finished) = turn_until(&up, "k6-after", ask, |state| state.is_terminal()).await?;
    let attached = up.client().await?.attach("k6-after", &AttachRequest::default()).await?;
    let (text, _) = transcript_text(&attached.transcript);
    checks.check("after re-approval the tool runs without asking", finished && attached.pending.is_empty() && text.contains(crate::notes_driver::CODEWORD));
    Ok(checks)
}

/// A parked driver question answered after a kernel restart: the model's `send_note` is
/// approved, its server asks before sending, the kernel stops and starts again, and the
/// answer in the inbox completes the call once.
async fn parked_question(bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let up = Up::start(bin, provider, model, "ask").await?;
    let instance = format!("live-q9-{}", std::process::id());
    let (mounted, _) = mount_notes(bin, &up, &instance)?;
    checks.check("the notes driver is mounted", mounted);
    let ask = "Use the send_note tool once with the text: kernel restart check. Then reply with the single word: done";
    let client = up.client().await?;
    let pid = client
        .spawn(&SpawnRequest::new("adk-rust", json!({ "text": ask })).with_session_id("q9-live"))
        .await?
        .meta
        .pid;
    let running = &up;
    let item = |tool: &'static str| async move {
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline {
            let found = match running.client().await {
                Ok(client) => client.approvals().await.ok().and_then(|items| {
                    items.into_iter().find(|item| item.tool_name == tool)
                }),
                Err(_) => None,
            };
            if found.is_some() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        None
    };
    let call = item("send_note").await;
    checks.check("the send is held for approval", call.is_some());
    if let Some(call) = call {
        client.decide(&call.id, call.version, adk_core::ApprovalAction::Approve, None).await?;
    }
    let question = item("driver:notes/elicitation").await;
    checks.check("the driver's question waits in the inbox", question.is_some());
    checks.check("nothing was sent before the answer", !up.root.path().join("notes/notes.log").exists());

    let up = up.restart(bin, provider, model, "ask").await?;
    let client = up.client().await?;
    let pending = client.approvals().await?;
    let question = pending.iter().find(|item| item.tool_name == "driver:notes/elicitation");
    checks.check("the question survives the restart", question.is_some());
    if let Some(question) = question {
        client.decide(&question.id, question.version, adk_core::ApprovalAction::Approve, None).await?;
    }
    let finished = until(Duration::from_secs(180), || {
        let client = client.clone();
        let pid = pid.clone();
        async move {
            let processes = client.processes(&ProcessesQuery::default()).await?;
            Ok(processes.iter().any(|process| process.meta.pid == pid && process.run.state.is_terminal()))
        }
    })
    .await?;
    let processes = client.processes(&ProcessesQuery::default()).await?;
    let state = processes.iter().find(|process| process.meta.pid == pid).map(|process| process.run.state);
    checks.check("the turn completed after the restart", finished && state == Some(adk_core::RunState::Completed));
    let log = std::fs::read_to_string(up.root.path().join("notes/notes.log")).unwrap_or_default();
    checks.check("the note was sent exactly once", log.lines().filter(|line| line.contains("kernel restart check")).count() == 1);
    checks.check("no further approval was asked", client.approvals().await?.is_empty());
    Ok(checks)
}

async fn futures_next(
    events: &mut futures::stream::BoxStream<'static, adk_kernel::Result<adk_kernel::EventRecord>>,
) -> Option<adk_kernel::Result<adk_kernel::EventRecord>> {
    use futures::StreamExt as _;
    events.next().await
}

/// Six weeks of a person's working days ending yesterday, written to the ledger in
/// `home`'s `harness.db` under the key in `home`, as a kernel that ran them would have: four
/// weeks ending around 17:30 with twelve tool calls, then two of late finishes ending around
/// 20:30 with twenty-four.
async fn seed_routine(home: PathBuf) -> Result<()> {
    use adk_kernel::proc::{Activity, Ledger, LedgerKey, LedgerKind, LedgerSink};
    use chrono::{Datelike as _, Days, Local, TimeZone as _, Weekday};

    let url = format!("sqlite://{}?mode=rwc", home.join("harness.db").display());
    let sessions = adk_session::SqliteSessionService::new(&url).await?;
    sessions.migrate().await?;
    let store =
        std::sync::Arc::new(adk_kernel::proc::SqliteLedgerStore::new(sessions.pool().clone()));
    store.migrate().await?;
    let ledger = Ledger::new(store, LedgerKey::load_or_create(&home.join(LedgerKey::FILE))?);
    let agent = "urn:adk:agent:adk-rust/harness";
    let yesterday = Local::now().date_naive().pred_opt().context("a yesterday")?;
    for back in (0..42u64).rev() {
        let day = yesterday - Days::new(back);
        let late = back < 14;
        let (calls, last) = match day.weekday() {
            Weekday::Sat | Weekday::Sun => (2, 11 * 60 + 30),
            _ if late => (24, 20 * 60 + 30),
            _ => (12, 17 * 60 + 30),
        };
        let at = |minute: u32| -> Result<chrono::DateTime<chrono::Utc>> {
            let time = day.and_hms_opt(minute / 60, minute % 60, 0).context("a time")?;
            Ok(Local.from_local_datetime(&time).earliest().context("a local time")?.to_utc())
        };
        let first = 9 * 60;
        ledger.record(Activity::new(LedgerKind::Intent, agent).at(at(first)?)).await?;
        for call in 1..=calls {
            let minute = first + (last - first) * call / (calls + 1);
            ledger
                .record(
                    Activity::new(LedgerKind::ToolCall, agent)
                        .with_effect("read_only")
                        .with_duration(Duration::from_millis(120 + u64::from(call) * 7))
                        .with_subject(format!("read_file\u{1f}{{\"path\":\"notes/{day}.md\"}}"))
                        .at(at(minute)?),
                )
                .await?;
        }
        ledger.record(Activity::new(LedgerKind::Intent, agent).at(at(last)?)).await?;
    }
    Ok(())
}

/// K10 live (spec 3.2 §5, §7.10): the kernel keeps a content-free ledger of a live turn,
/// runs the pass of the day that ended over six weeks of history, and the live model words
/// what the deterministic test found as a neutral question; the ledger exports as the JSONL
/// `cargo adk eval --ledger` reads.
async fn k10(bin: &Path, provider: &str, model: &str) -> Result<Checks> {
    use adk_kernel::control::api::{LedgerParams, ObservationsParams};
    use adk_kernel::proc::{LedgerEntry, Phrasing, is_neutral_question};

    let mut checks = Checks::default();
    let up = Up::start_seeded(bin, provider, model, "ask", true, seed_routine).await?;
    let client = up.client().await?;

    // A live turn: its activity reaches the ledger, its content does not.
    let request = SpawnRequest::new(
        "adk-rust",
        json!({ "text": "Use the read_file tool to read README.md, then say in one short sentence what it describes." }),
    )
    .with_session_id("k10-live");
    let pid = client.spawn(&request).await?.meta.pid;
    let finished = until(Duration::from_secs(180), || {
        let client = client.clone();
        let pid = pid.clone();
        async move {
            let processes = client.processes(&ProcessesQuery::default()).await?;
            Ok(processes
                .iter()
                .any(|process| process.meta.pid == pid && process.run.state.is_terminal()))
        }
    })
    .await?;
    checks.check("the live turn completed", finished);
    let today = chrono::Utc::now() - chrono::Duration::hours(1);
    let rows = until(Duration::from_secs(10), || {
        let client = client.clone();
        async move {
            let page = client
                .ledger(&LedgerParams { from: Some(today), ..LedgerParams::default() })
                .await?;
            let kinds: Vec<&str> = page.entries.iter().map(|row| row.kind.as_str()).collect();
            Ok(["intent", "tool_call", "turn", "process"].iter().all(|kind| kinds.contains(kind)))
        }
    })
    .await?;
    checks.check("the turn's intent, tool call, turn and process rows are in the ledger", rows);
    let page =
        client.ledger(&LedgerParams { from: Some(today), ..LedgerParams::default() }).await?;
    let tool = page.entries.iter().find(|row| row.kind == "tool_call");
    checks.check(
        "its tool call has an effect class, a duration and a hashed subject",
        tool.is_some_and(|row| {
            row.effect.as_deref() == Some("read_only")
                && row.duration_ms.is_some()
                && row.subject_hash.as_deref().is_some_and(|hash| hash.starts_with("hmac-sha256:"))
        }),
    );
    let lines: Vec<String> =
        page.entries.iter().map(LedgerEntry::to_jsonl).collect::<Result<_, _>>()?;
    checks.check(
        "no row holds the prompt, the file or the answer",
        lines.iter().all(|line| {
            !line.contains("README") && !line.contains("a project") && !line.contains("describes")
        }),
    );

    // The day that ended: the pass over six weeks, worded by the live model.
    let observed = until(Duration::from_secs(120), || {
        let client = client.clone();
        async move { Ok(!client.observations(&ObservationsParams::default()).await?.is_empty()) }
    })
    .await?;
    checks.check("the pass of the day that ended recorded an observation", observed);
    let observations = client.observations(&ObservationsParams::default()).await?;
    for observation in &observations {
        println!("    {} [{:?}] {}", observation.as_of, observation.phrasing, observation.question);
    }
    checks.check(
        "every observation is a neutral question",
        !observations.is_empty()
            && observations.iter().all(|observation| is_neutral_question(&observation.question)),
    );
    let worded = observations
        .iter()
        .any(|observation| matches!(observation.phrasing, Phrasing::Model { .. }));
    if !worded {
        // Why the kernel fell back to its template, from its own log.
        for line in up.log().lines().filter(|line| line.contains("observation")) {
            println!("    log: {line}");
        }
    }
    checks.check("the live model worded it", worded);
    checks.check(
        "it names the late finishes",
        observations.iter().any(|observation| {
            observation
                .dimensions
                .iter()
                .any(|dimension| dimension.dimension == "activity.last_minute")
        }),
    );

    // The observation is a question for the user, answered and never scheduled: confirmed
    // through the binary, it is remembered as a known fact with the person as its source.
    let asked = until(Duration::from_secs(30), || {
        let client = client.clone();
        async move {
            let items = client.agenda(&AgendaQuery::default()).await?;
            Ok(items.iter().any(|item| {
                item.origin.is_question() && item.state == adk_kernel::AgendaState::Proposed
            }))
        }
    })
    .await?;
    checks.check("the observation waits in the agenda as a question for the user", asked);
    let items = client.agenda(&AgendaQuery::default()).await?;
    let question = items.iter().find(|item| item.origin.is_question()).cloned();
    let processes = client.processes(&ProcessesQuery::default()).await?;
    checks.check(
        "and no process runs for it",
        question.as_ref().is_some_and(|item| {
            processes.iter().all(|process| process.meta.agenda_item.as_deref() != Some(&item.id))
        }),
    );
    let mut list = Command::new(bin);
    list.args(["agenda", "--home"]).arg(&up.home).env("RUST_LOG", "warn").stdin(Stdio::null());
    isolate(&mut list, up.root.path());
    let listed = String::from_utf8_lossy(&list.output()?.stdout).to_string();
    println!("    $ adk-rust agenda\n{}", indent(&listed));
    checks.check(
        "`adk-rust agenda` lists it as a question",
        listed.contains("observation") && listed.contains("asked as questions"),
    );
    let mut remembered = false;
    if let Some(item) = &question {
        let mut schedule = Command::new(bin);
        schedule
            .args(["agenda", "--home"])
            .arg(&up.home)
            .args(["schedule", &item.id, "--at", "+1h"])
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        isolate(&mut schedule, up.root.path());
        let refused = schedule.output()?;
        checks.check(
            "`adk-rust agenda schedule` on it is refused with the reason",
            !refused.status.success()
                && String::from_utf8_lossy(&refused.stderr).contains("never scheduled"),
        );
        let mut approve = Command::new(bin);
        approve
            .args(["agenda", "--home"])
            .arg(&up.home)
            .args(["approve", &item.id[..12]])
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        isolate(&mut approve, up.root.path());
        let approved = approve.output()?;
        let said = String::from_utf8_lossy(&approved.stdout).to_string();
        println!("    $ adk-rust agenda approve {}\n{}", &item.id[..12], indent(&said));
        let answered = client.agenda(&AgendaQuery::default()).await?;
        let closed = answered.iter().find(|candidate| candidate.id == item.id);
        let fact = closed.and_then(|item| item.answer.as_ref()).and_then(|a| a.fact.clone());
        checks.check(
            "`adk-rust agenda approve` confirms it and closes the item",
            approved.status.success()
                && said.contains("remembered as known fact")
                && closed.is_some_and(|item| item.state == adk_kernel::AgendaState::Done),
        );
        if let Some(fact) = fact {
            use adk_harness::FactStore as _;
            let url = format!("sqlite://{}?mode=rw", up.home.join("harness.db").display());
            let sessions = adk_session::SqliteSessionService::new(&url).await?;
            let store = adk_harness::SqliteFactStore::new(sessions.pool().clone());
            let facts =
                store.list(adk_harness::FactFilter::user("adk-rust", &item.subject.id)).await?;
            remembered = facts.iter().any(|known| {
                known.id == fact
                    && known.text == item.rationale
                    && known.status == adk_harness::FactStatus::Known
                    && known.provenance.source == adk_harness::FactSource::UserConfirmed
            });
        }
    }
    checks.check("its statement is a known fact whose source is the person", remembered);

    // The export, through the binary as a person runs it.
    let export = up.root.path().join("ledger.jsonl");
    let mut command = Command::new(bin);
    command
        .args(["proc", "ledger", "--home"])
        .arg(&up.home)
        .arg("--export")
        .arg(&export)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    isolate(&mut command, up.root.path());
    let status = command.status()?;
    let exported = std::fs::read_to_string(&export).unwrap_or_default();
    let parsed: Vec<LedgerEntry> =
        exported.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    checks.check(
        "`adk-rust proc ledger --export` writes every row as JSONL",
        status.success() && parsed.len() == exported.lines().count() && parsed.len() > 400,
    );
    let mut command = Command::new(bin);
    command.args(["proc", "--home"]).arg(&up.home).env("RUST_LOG", "warn");
    isolate(&mut command, up.root.path());
    let shown = command.output()?;
    let shown = String::from_utf8_lossy(&shown.stdout);
    checks.check(
        "`adk-rust proc` offers the observation's question",
        observations.iter().any(|observation| shown.contains(&observation.question)),
    );
    Ok(checks)
}
