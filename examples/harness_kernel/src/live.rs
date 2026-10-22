//! K3 and K5 on a live model through the `adk-rust up` binary (spec 3.2 §11): the kernel
//! runs as its own process, and this one reaches it over the control plane like any other
//! surface. Keys come from the environment the binary inherits and are never printed.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{AttachRequest, ProcessesQuery, SpawnRequest};
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
        let root = tempfile::tempdir()?;
        let (home, workspace) = (root.path().join("kernel"), root.path().join("ws"));
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("README.md"), "a project\n")?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let log = std::fs::File::create(root.path().join("up.log"))?;
        let mut command = Command::new(bin);
        command
            .args(["up", "--home"])
            .arg(&home)
            .args(["--bind", &format!("127.0.0.1:{port}"), "--mode", mode, "--dir"])
            .arg(&workspace)
            .args(["--provider", provider, "--model", model, "--no-memory"])
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
        // SIGTERM drains the kernel; a stuck one is killed.
        let _ = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).status();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(40) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
            Ok(!terminal.attach("k5-live", &AttachRequest::default()).await?.pending.is_empty())
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
    let up = Up::start(bin, provider, model, "plan").await?;
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
    checks.check("the woken turn completed", finished);
    checks.check(
        "it read the file",
        calls.iter().any(|call| call.starts_with("read_file") && call.contains("notes.md")),
    );
    checks.check("and answered from it", text.to_lowercase().contains("heliotrope"));
    Ok(checks)
}
