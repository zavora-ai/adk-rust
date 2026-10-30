//! `adk-rust up` mounts a `WebhookIngress` for the kernel's webhook wakes (spec 3.5 §8.2,
//! §10 X11): a process made with `adk-rust wake --webhook github` sleeps until a signed
//! delivery to `/hooks/github` wakes it; an unsigned one is refused and a redelivery wakes
//! nothing twice.
//!
//! The binary runs as a process with a scripted model.

#![cfg(all(unix, feature = "scripted-model", feature = "persist", feature = "harness"))]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use adk_core::webhook::{WebhookSecret, WebhookSigner};

const REPLY: &str = r#"[{"text": "Audited the new dependencies."}]"#;

/// `adk-rust up` as a child process, stopped when dropped.
struct Up(Option<Child>);

impl Drop for Up {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Up {
    fn finish(mut self) -> String {
        let mut child = self.0.take().expect("up runs");
        let _ = child.kill();
        common::text(&child.wait_with_output().unwrap())
    }
}

/// The user config `adk-rust` reads under `home`.
fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/adk-rust")
    } else {
        home.join(".config/adk-rust")
    }
}

fn up(home: &Path, kernel: &Path, ws: &Path, port: u16) -> Up {
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["up", "--home"])
        .arg(kernel)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--dir"])
        .arg(ws)
        .args(["--model-script", REPLY, "--mode", "ask"])
        .current_dir(ws)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::isolate(&mut command, home);
    command.env_remove("TELEGRAM_BOT_TOKEN");
    let mut child = command.spawn().expect("adk-rust starts");
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if child.try_wait().unwrap().is_some() || started.elapsed() > common::TIMEOUT {
            let _ = child.kill();
            panic!("up did not listen:\n{}", common::text(&child.wait_with_output().unwrap()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Up(Some(child))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The row `adk-rust ps` prints for `pid`.
fn ps_row(home: &Path, kernel: &Path, ws: &Path, pid: &str) -> String {
    let kernel = kernel.display().to_string();
    let output = common::run(&["ps", "--home", &kernel], home, ws);
    common::text(&output).lines().find(|line| line.starts_with(pid)).unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_webhook_wakes_a_sleeping_process_once() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    let kernel = home.path().join("kernel");
    let secret = WebhookSecret::from_bytes(b"a shared signing key for github".to_vec()).unwrap();
    let config = config_dir(home.path());
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("github.whsec"), secret.to_whsec()).unwrap();
    std::fs::write(
        config.join("harness.toml"),
        "[[kernel.webhooks]]\nname = \"github\"\nsecret = \"file:github.whsec\"\n",
    )
    .unwrap();
    let port = free_port();
    let child = up(home.path(), &kernel, ws.path(), port);
    let kernel_arg = kernel.display().to_string();

    let output = common::run(
        &["wake", "--webhook", "github", "--home", &kernel_arg, "--", "audit the new dependencies"],
        home.path(),
        ws.path(),
    );
    let printed = common::text(&output);
    assert!(output.status.success(), "{printed}");
    let pid = printed.split_whitespace().next().unwrap().to_string();
    // A sleeper parks at its first claim, before any model call (spec 3.2 §7.4, K-I2), so
    // `ps` can show it queued or running for a moment first.
    let started = Instant::now();
    let asleep = loop {
        let row = ps_row(home.path(), &kernel, ws.path(), &pid);
        if row.contains("waiting") {
            break row;
        }
        assert!(
            !["completed", "failed", "cancelled"].iter().any(|state| row.contains(state)),
            "it ran before any webhook: {row}"
        );
        assert!(started.elapsed() < common::TIMEOUT, "asleep first: {row}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(asleep.contains("asleep on 1 wake(s)"), "{asleep}");
    assert_eq!(asleep.split_whitespace().nth(6), Some("0/0"), "no model call yet: {asleep}");

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/hooks/github");
    let body = br#"{"action":"opened","pull_request":{"number":7}}"#;
    let unsigned = client.post(&url).body(body.to_vec()).send().await.unwrap();
    assert_eq!(unsigned.status().as_u16(), 401);
    let signed = WebhookSigner::new(secret).sign_now("msg_1", body);
    let deliver = || {
        let mut request = client.post(&url).header("content-type", "application/json");
        for (name, value) in signed.header_pairs() {
            request = request.header(name, value);
        }
        request.body(body.to_vec()).send()
    };
    let first = deliver().await.unwrap();
    assert_eq!(first.status().as_u16(), 202, "{:?}", first.text().await);
    let again = deliver().await.unwrap();
    assert_eq!(again.status().as_u16(), 200, "a redelivery answers with the first run");
    let duplicate: serde_json::Value = again.json().await.unwrap();
    assert_eq!(duplicate["duplicate"], true);

    let started = Instant::now();
    let woken = loop {
        let row = ps_row(home.path(), &kernel, ws.path(), &pid);
        if !row.contains("waiting") && !row.is_empty() {
            break row;
        }
        assert!(started.elapsed() < common::TIMEOUT, "the process never woke: {row}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(woken.contains("completed") || woken.contains("running"), "{woken}");
    let log = child.finish();
    assert!(log.contains("hooks /hooks/github"), "{log}");
}
