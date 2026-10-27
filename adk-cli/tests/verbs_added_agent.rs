//! Screen actions on `adk-rust up` (spec 3.3 §2 item 7, §12 "One verb, three origins"): an
//! agent a person added asks to snooze something in Coming up, its request waits through a
//! `kill -9` and a restart on SQLite (`harness.db`), Allow once on the phone runs it once, and
//! the terminal attached to the kernel snoozes in up's own process.
//!
//! ```sh
//! cargo nextest run -p adk-cli --features verb-ui,scripted-model --test verbs_added_agent
//! ```

#![cfg(all(unix, feature = "scripted-model", feature = "persist", feature = "verb-ui"))]

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Timelike, Utc};
use serde_json::{Value, json};

/// `adk-rust up` as a child process, killed when dropped, so a failing test leaves nothing
/// running.
struct Up(Option<Child>);

impl Up {
    /// `kill -9`.
    fn kill(mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Up {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Place {
    home: tempfile::TempDir,
    ws: tempfile::TempDir,
    kernel: PathBuf,
    port: u16,
}

impl Place {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let kernel = home.path().join("kernel");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        Self { home, ws, kernel, port }
    }

    /// `adk-rust up` in Ask first with `script`; returns once it listens.
    fn up(&self, script: &str) -> Up {
        let (kernel, dir, bind) = (
            self.kernel.display().to_string(),
            self.ws.path().display().to_string(),
            format!("127.0.0.1:{}", self.port),
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
        command
            .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
            .args(["--model-script", script, "--mode", "ask"])
            .args(["--no-memory", "--no-consolidation"])
            .current_dir(self.ws.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        common::isolate(&mut command, self.home.path());
        command.env_remove("TELEGRAM_BOT_TOKEN");
        let mut child = command.spawn().expect("adk-rust starts");
        let started = Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            if child.try_wait().unwrap().is_some() || started.elapsed() > common::TIMEOUT {
                let _ = child.kill();
                panic!("up did not listen");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Up(Some(child))
    }

    fn api(&self) -> Api {
        Api { base: format!("http://127.0.0.1:{}", self.port), client: reqwest::Client::new() }
    }
}

struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    async fn get(&self, path: &str) -> Value {
        let response = self.client.get(format!("{}{path}", self.base)).send().await.unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "GET {path}: {status} {body}");
        body
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response =
            self.client.post(format!("{}{path}", self.base)).json(&body).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// `command` from `surface`, given as `how`.
    async fn command_from(&self, surface: &str, how: &str, command: Value) -> Value {
        let (status, body) = self
            .post(
                "/api/surface/commands",
                json!({ "surface": surface, "origin": { "kind": how }, "command": command }),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn decisions(&self) -> Vec<Value> {
        self.get("/api/surface/decisions?surface=web").await["decisions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    async fn agenda(&self, state: &str) -> Vec<Value> {
        self.get(&format!("/api/kernel/agenda?state={state}")).await["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// When the scheduled item titled `title` wakes.
    async fn wake_of(&self, title: &str) -> Option<DateTime<Utc>> {
        let item =
            self.agenda("scheduled").await.into_iter().find(|item| item["title"] == title)?;
        item["state"]["scheduled"]["wake"]["anyOf"][0]["at"].as_str()?.parse().ok()
    }

    async fn verb_lines(&self) -> Vec<Value> {
        self.get("/api/kernel/record?limit=500").await["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|line| line["action"] == "surface.verb")
            .cloned()
            .collect()
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = Instant::now();
    while started.elapsed() <= Duration::from_secs(30) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_added_agents_snooze_waits_through_a_restart_and_runs_once() {
    let place = Place::new();
    let api = place.api();
    let at = (Utc::now() + chrono::Duration::hours(8))
        .with_minute(0)
        .and_then(|at| at.with_second(0))
        .and_then(|at| at.with_nanosecond(0))
        .unwrap();

    // 1. ADK-Rust suggests Finance and a follow-up, "News digest", at a time.
    let first = place.up(&json!([
        { "call": { "name": "propose_agent", "args": {
            "name": "Finance",
            "whatItDoes": "Keeps an eye on the evening",
            "instructions": "Tidy the evening's schedule.",
            "mode": "askFirst"
        } } },
        { "call": { "name": "propose_followup", "args": {
            "title": "News digest",
            "rationale": "The evening edition lands at 18:30",
            "when": { "at": at.to_rfc3339() }
        } } },
        { "text": "Both wait for you." },
        { "text": "Done." }
    ])
    .to_string());
    api.command_from(
        "web",
        "pointer",
        json!({ "type": "send", "sessionId": "ask-1", "text": "Set up my evening." }),
    )
    .await;
    let cards = eventually("both suggestions", || async {
        let cards = api.decisions().await;
        (cards.len() >= 2).then_some(cards)
    })
    .await;
    for card in cards {
        api.command_from(
            "web",
            "pointer",
            json!({ "type": "decide", "decisionId": card["id"], "version": card["version"], "answer": "allowOnce", "expanded": true }),
        )
        .await;
    }
    eventually("News digest scheduled", || async {
        (api.wake_of("News digest").await == Some(at)).then_some(())
    })
    .await;
    let item = api
        .agenda("scheduled")
        .await
        .into_iter()
        .find(|item| item["title"] == "News digest")
        .unwrap();
    let id = item["id"].as_str().unwrap().to_string();
    // Finance's detail lists what the home grants it.
    let finance = api.get("/api/surface/agents/finance?surface=web").await;
    assert_eq!(
        finance["granted"],
        json!([{ "id": "verb-ui", "text": "Ask to change things on your screens — always asks you first" }])
    );
    first.kill();

    // 2. After a restart, Finance reads Coming up and asks to snooze News digest: held.
    let to = at + chrono::Duration::hours(1);
    let second = place.up(&json!([
        { "call": { "name": "verb_coming_up", "args": {} } },
        { "call": { "name": "verb_snooze", "args": { "item": id, "to": to.to_rfc3339() } } },
        { "text": "It waits for you." },
        { "text": "Done." }
    ])
    .to_string());
    api.command_from(
        "web",
        "pointer",
        json!({ "type": "newThread", "agent": "finance", "sessionId": "finance-1", "text": "Tidy my evening." }),
    )
    .await;
    let card = eventually("Finance's request", || async {
        api.decisions().await.into_iter().find(|card| card["call"]["tool"] == "verb_snooze")
    })
    .await;
    assert!(card["heading"].as_str().unwrap().starts_with("Move News digest to "), "{card}");
    assert_eq!(card["riskText"], "Low risk");
    assert_eq!(card["reversibilityText"], "You can change it back");
    let answers: Vec<&str> = card["allowed"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["answer"].as_str().unwrap())
        .collect();
    assert_eq!(answers, ["allowOnce", "edit", "deny"], "never Always allow");
    assert_eq!(api.wake_of("News digest").await, Some(at), "nothing moves before an answer");
    // The terminal attached to the kernel draws the same card (spec 3.3 §7.1).
    let kernel = place.kernel.display().to_string();
    let attached =
        common::run(&["--home", &kernel, "--output", "json"], place.home.path(), place.ws.path());
    let shown = String::from_utf8_lossy(&attached.stdout).into_owned();
    let drawn = shown
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["type"] == "decisions")
        .flat_map(|line| line["cards"].as_array().cloned().unwrap_or_default())
        .find(|drawn| drawn["call"]["tool"] == "verb_snooze")
        .unwrap_or_else(|| panic!("no screen-action card in:\n{shown}"));
    assert_eq!(drawn["heading"], card["heading"], "{drawn}");
    assert_eq!(drawn["reversibilityText"], "You can change it back");
    assert_eq!(drawn["detail"]["text"], card["detail"]["text"]);
    second.kill();

    // 3. After another restart the card still waits; Allow once on the phone runs it once.
    let _third = place.up(&json!([{ "text": "Done." }, { "text": "Done." }]).to_string());
    let card = eventually("the card after the restart", || async {
        api.decisions().await.into_iter().find(|card| card["call"]["tool"] == "verb_snooze")
    })
    .await;
    api.command_from(
        "phone",
        "touch",
        json!({ "type": "decide", "decisionId": card["id"], "version": card["version"], "answer": "allowOnce", "expanded": true }),
    )
    .await;
    eventually("the snooze", || async {
        (api.wake_of("News digest").await == Some(to)).then_some(())
    })
    .await;
    let lines = eventually("its record", || async {
        let lines = api.verb_lines().await;
        (!lines.is_empty()).then_some(lines)
    })
    .await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "snooze");
    assert_eq!(lines[0]["target"], "News digest");
    assert_eq!(lines[0]["decision"], "approved");
    assert_eq!(lines[0]["surface"], "phone");
    assert_eq!(lines[0]["agent"], "urn:adk:agent:adk-rust/finance");

    // 4. The terminal attached to this kernel snoozes in up's process, as its person.
    let (status, ran) = api
        .post(
            "/api/kernel/commands/run",
            json!({ "name": "snooze", "args": "News digest", "sessionId": "terminal" }),
        )
        .await;
    assert_eq!(status, 200, "{ran}");
    assert!(ran["text"].as_str().unwrap().starts_with("✓ snoozed News digest → "), "{ran}");
    let later = to + chrono::Duration::hours(1);
    eventually("the terminal's snooze", || async {
        (api.wake_of("News digest").await == Some(later)).then_some(())
    })
    .await;
    let lines = api.verb_lines().await;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["surface"], "terminal", "newest first");
    assert_eq!(lines[0]["how"], "keyboard");
}
