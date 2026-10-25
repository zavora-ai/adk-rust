//! `adk-rust up --home-url … --auth-token …` (spec 3.3 §5.4, §10 K2): the phone on the kernel.
//! A request without the token is refused, on loopback too; a phone paired with `kind: phone`
//! reads the surface routes with its device token and sends no command; pairing links open in
//! the installed app's scope; the page serves `/ui/pair`; the push key is its owner's only. A
//! home URL without a token is refused at start.
//!
//! The binary runs as a process with a scripted model on SQLite (`harness.db`).

#![cfg(all(unix, feature = "scripted-model", feature = "persist", feature = "mobile"))]

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TOKEN: &str = "operator-token-for-phone-up";
const REPLY: &str = r#"[{"text": "Hello."}]"#;

/// `adk-rust up` as a child process, stopped when dropped, so a failing test leaves nothing
/// running.
struct Up(Option<Child>);

impl Drop for Up {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn command(home: &Path, kernel: &Path, ws: &Path, port: u16, extra: &[&str]) -> Command {
    let (kernel, dir, bind) =
        (kernel.display().to_string(), ws.display().to_string(), format!("127.0.0.1:{port}"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
        .args(["--model-script", REPLY, "--mode", "ask"])
        .args(extra)
        .current_dir(ws)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::isolate(&mut command, home);
    command
        .env_remove("TELEGRAM_BOT_TOKEN")
        .env_remove("TELEGRAM_API_BASE")
        .env_remove("TELEGRAM_HOME_URL");
    command
}

/// `adk-rust up` serving the phone at `http://localhost:<port>`; returns once it listens.
fn up(home: &Path, kernel: &Path, ws: &Path, port: u16) -> Up {
    let home_url = format!("http://localhost:{port}");
    let extra = ["--home-url", home_url.as_str(), "--auth-token", TOKEN];
    let mut child = command(home, kernel, ws, port, &extra).spawn().expect("adk-rust starts");
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

fn workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    ws
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn up_serves_the_phone_at_its_home_url() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let _up = up(home.path(), &kernel, ws.path(), port);
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Behind a proxy every request comes from loopback: none is the operator without the token.
    let refused =
        client.get(format!("{base}/api/surface/status?surface=web")).send().await.unwrap();
    assert_eq!(refused.status(), 401);

    // The operator pairs a phone; its link opens inside the installed app's scope.
    let issued: Value = client
        .post(format!("{base}/api/kernel/pairing"))
        .bearer_auth(TOKEN)
        .json(&json!({ "name": "Your phone", "kind": "phone" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = issued["code"].as_str().expect("a code").to_string();
    let url = issued["url"].as_str().expect("a link");
    assert!(url.starts_with(&format!("http://localhost:{port}/ui/pair#code=")), "{url}");
    let claimed: Value = client
        .post(format!("{base}/api/kernel/pairing/claim"))
        .json(&json!({ "code": code }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(claimed["device"]["kind"], "phone", "{claimed}");
    let device = claimed["token"].as_str().expect("a device token").to_string();

    // The phone reads the surface with its own token, as the phone.
    let decisions = client
        .get(format!("{base}/api/surface/decisions?surface=phone"))
        .bearer_auth(&device)
        .send()
        .await
        .unwrap();
    assert_eq!(decisions.status(), 200);
    let phone: Value = client
        .get(format!("{base}/api/surface/phone"))
        .bearer_auth(&device)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(phone["rpId"], "localhost", "{phone}");
    assert!(phone["pushKey"].as_str().is_some_and(|key| key.len() > 80), "{phone}");
    // Commands stay closed to devices.
    let command = client
        .post(format!("{base}/api/surface/commands"))
        .bearer_auth(&device)
        .json(&json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": { "type": "resumeAll" } }))
        .send()
        .await
        .unwrap();
    assert!(matches!(command.status().as_u16(), 401 | 403), "{}", command.status());

    // The page serves the pairing screen.
    let page = client.get(format!("{base}/ui/pair")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("manifest.webmanifest"));

    // The push key is kept in the kernel home, its owner's only.
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(kernel.join("push.key")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_home_url_needs_the_auth_token() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let output = command(
        home.path(),
        &kernel,
        ws.path(),
        port,
        &["--home-url", "https://home.example.ts.net"],
    )
    .output()
    .expect("adk-rust runs");
    assert!(!output.status.success());
    let text = common::text(&output);
    assert!(text.contains("--home-url needs --auth-token"), "{text}");
}
