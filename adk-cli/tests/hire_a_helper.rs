//! Spec 3.3 §12 phase 4 "Hire a helper" (§5.7): `adk-rust up` on SQLite with a scripted model.
//! ADK-Rust suggests a Finance agent; nothing is added until a person says yes in the web home;
//! Finance is in the roster as Ready; after `kill -9` and a restart it is still listed under the
//! same id, answers in its own thread, and ADK-Rust refuses that thread; renamed "Bills" and set
//! to Suggest in the home; removed with `/agent` over the kernel's command route, naming what
//! ends; a new Finance gets `finance-2`; the record holds every change in order.
//!
//! Then `adk-rust agents` on the kernel's socket (spec 3.3 §10 L1): it previews every change and
//! asks on the terminal; with no terminal a change is refused; `--yes` applies one that is not
//! high risk and is refused at high risk; `/agent` refuses a code that does not confirm the change.

#![cfg(all(unix, feature = "scripted-model", feature = "persist"))]

mod common;

use common::{Terminal, run, text};

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn up(home: &Path, kernel: &Path, ws: &Path, port: u16, script: &str, mode: &str) -> Up {
    let (kernel, dir, bind) =
        (kernel.display().to_string(), ws.display().to_string(), format!("127.0.0.1:{port}"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
        .args(["--model-script", script, "--mode", mode])
        .current_dir(ws)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    common::isolate(&mut command, home);
    command.env_remove("TELEGRAM_BOT_TOKEN");
    let mut child = command.spawn().expect("adk-rust starts");
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if child.try_wait().unwrap().is_some() || started.elapsed() > common::TIMEOUT {
            let _ = child.kill();
            panic!("up did not listen");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Up(Some(child))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
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

    async fn command(&self, command: Value) -> Value {
        let (status, body) = self
            .post(
                "/api/surface/commands",
                json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": command }),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    /// `/agent args` on the kernel, as the terminal runs it.
    async fn agent(&self, args: &str) -> Value {
        let (status, body) = self
            .post(
                "/api/kernel/commands/run",
                json!({ "name": "agent", "args": args, "sessionId": "terminal" }),
            )
            .await;
        assert_eq!(status, 200, "/agent {args}: {body}");
        serde_json::from_str(body["text"].as_str().unwrap_or_default()).unwrap_or(body)
    }

    async fn roster(&self) -> Vec<Value> {
        self.get("/api/surface/agents?surface=web").await["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    async fn row(&self, id: &str) -> Option<Value> {
        self.roster().await.into_iter().find(|row| row["agent"]["name"] == id)
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

const SUGGEST: &str = r#"[
  {"call": {"name": "propose_agent", "args": {
    "name": "Finance",
    "whatItDoes": "Reads your invoices and drafts payment reminders",
    "instructions": "Read the invoices in Invoices. Draft a polite reminder for each overdue one. Never send them.",
    "mode": "askFirst",
    "uses": {"files": "read", "folder": "Invoices"}
  }}},
  {"text": "I suggested a Finance agent. It waits for you in Decisions."}
]"#;

const HELLO: &str = r#"[{"text": "Finance here."}, {"text": "Still Finance."}, {"text": "Done."}]"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hire_a_helper() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join("Invoices")).unwrap();
    std::fs::write(ws.path().join("Invoices/march.txt"), "INV-7 due 31 March").unwrap();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let api = Api { base: format!("http://127.0.0.1:{port}"), client: reqwest::Client::new() };

    // 1–2. The person asks; ADK-Rust suggests. Nothing is added: no Finance in the roster.
    let first = up(home.path(), &kernel, ws.path(), port, SUGGEST, "ask");
    api.command(json!({
        "type": "send",
        "sessionId": "ask-1",
        "text": "I need a Finance agent that reads my invoices and drafts payment reminders."
    }))
    .await;
    let card = eventually("the suggestion's card", || async {
        let decisions = api.get("/api/surface/decisions?surface=web").await;
        decisions["decisions"].as_array().and_then(|cards| {
            cards.iter().find(|card| card["call"]["tool"] == "kernel:agent.create").cloned()
        })
    })
    .await;
    assert_eq!(card["heading"], "Add an agent called Finance");
    assert_eq!(card["riskText"], "Medium risk");
    let labels: Vec<&str> = card["allowed"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Add this agent", "Don't add"], "no Always allow");
    assert_eq!(card["addsAgent"]["id"], "finance");
    assert!(api.row("finance").await.is_none(), "nothing is added before a person says yes");

    // 3. Add this agent, in the web home: Finance is Ready in the roster.
    api.command(json!({
        "type": "decide",
        "decisionId": card["id"],
        "version": card["version"],
        "answer": "allowOnce",
        "expanded": true
    }))
    .await;
    let row = eventually("Finance in the roster", || async { api.row("finance").await }).await;
    assert_eq!(row["statusText"], "Ready");
    assert_eq!(row["added"], true);
    let color = row["agent"]["avatar"]["color"].clone();

    // 4. kill -9 and start again: the same id, and a message runs as Finance.
    first.kill();
    let second = up(home.path(), &kernel, ws.path(), port, HELLO, "ask");
    let row = eventually("Finance after the restart", || async { api.row("finance").await }).await;
    assert_eq!(row["agent"]["avatar"]["color"], color, "the same colour");
    api.command(json!({ "type": "newThread", "agent": "finance", "sessionId": "finance-1", "text": "hello" }))
        .await;
    let processes = eventually("Finance's run", || async {
        let processes = api.get("/api/kernel/processes").await;
        let found: Vec<Value> = processes["processes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|process| process["run"]["sessionId"] == "finance-1")
            .cloned()
            .collect();
        found.iter().any(|process| process["run"]["state"] == "completed").then_some(found)
    })
    .await;
    assert!(processes.iter().all(|process| process["run"]["target"] == "finance"));
    // ADK-Rust refuses Finance's thread.
    let (status, spawned) = api
        .post(
            "/api/kernel/processes",
            json!({ "target": "adk-rust", "sessionId": "finance-1", "input": { "text": "mine now" } }),
        )
        .await;
    assert_eq!(status, 200, "{spawned}");
    let pid = spawned["process"]["meta"]["pid"].as_str().unwrap().to_string();
    let refused = eventually("ADK-Rust's refused run", || async {
        let processes = api.get("/api/kernel/processes").await;
        processes["processes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|process| {
                process["meta"]["pid"] == pid.as_str() && process["run"]["state"] == "failed"
            })
            .cloned()
    })
    .await;
    assert!(refused["run"]["error"].to_string().contains("is finance's thread"), "{refused}");

    // 5. In the web home: renamed Bills, set to Suggest.
    let detail = api.get("/api/surface/agents/finance?surface=web").await;
    let mut draft = detail["draft"].clone();
    draft["displayName"] = json!("Bills");
    draft["mode"] = json!("suggest");
    let (status, preview) = api
        .post(
            "/api/surface/agents/preview?surface=web",
            json!({ "action": "change", "agent": "finance", "version": detail["version"], "draft": draft }),
        )
        .await;
    assert_eq!(status, 200, "{preview}");
    let changed = api
        .command(json!({
            "type": "changeAgent",
            "agent": "finance",
            "version": detail["version"],
            "draft": draft,
            "digest": preview["digest"]
        }))
        .await;
    assert_eq!(changed["type"], "agentChanged", "{changed}");
    let row = api.row("finance").await.unwrap();
    assert_eq!(row["agent"]["displayName"], "Bills");

    // 7. Removed from the terminal with `/agent`, naming what ends; a new Finance is finance-2.
    let preview = api.agent("remove finance --json").await;
    let code = preview["code"].as_str().expect("a confirming code").to_string();
    let removed = api.agent(&format!("remove finance --confirm {code} --json")).await;
    assert_eq!(removed["outcome"]["outcome"], "removed", "{removed}");
    assert!(api.row("finance").await.is_none());
    let preview = api.agent("new --name Finance --asks just-look --json").await;
    assert_eq!(preview["preview"]["id"], "finance-2", "a removed id is never given again");
    let code = preview["code"].as_str().unwrap().to_string();
    api.agent(&format!("new --name Finance --asks just-look --confirm {code} --json")).await;
    eventually("the new Finance", || async { api.row("finance-2").await }).await;

    // 8. The record holds every change, in order (it answers newest first).
    let record = api.get("/api/kernel/record?limit=500").await;
    let mut lines: Vec<(u64, String)> = record["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| Some((line["seq"].as_u64()?, line["action"].as_str()?.to_string())))
        .filter(|(_, action)| action.starts_with("kernel.agent."))
        .collect();
    lines.sort();
    let actions: Vec<&str> = lines.iter().map(|(_, action)| action.as_str()).collect();
    let order = [
        "kernel.agent.proposed",
        "kernel.agent.created",
        "kernel.agent.changed",
        "kernel.agent.removing",
        "kernel.agent.removed",
    ];
    let positions: Vec<Option<usize>> =
        order.iter().map(|action| actions.iter().position(|seen| seen == action)).collect();
    assert!(positions.iter().all(Option::is_some), "{actions:?}");
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{actions:?}");
    drop(second);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adk_rust_agents_previews_and_asks_before_a_change() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join("Invoices")).unwrap();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let api = Api { base: format!("http://127.0.0.1:{port}"), client: reqwest::Client::new() };
    // Go ahead in the area, so an agent that goes ahead is within its limits and high risk.
    let up = up(home.path(), &kernel, ws.path(), port, HELLO, "auto");
    let at = kernel.display().to_string();
    let agents = |args: &[&str]| {
        let mut line = vec!["agents", "--home", at.as_str()];
        line.extend_from_slice(args);
        run(&line, home.path(), ws.path())
    };
    let ids = || {
        let listed = agents(&["--output", "json", "list"]);
        assert!(listed.status.success(), "{}", text(&listed));
        let views: Vec<Value> = serde_json::from_slice(&listed.stdout).unwrap();
        views
            .iter()
            .map(|view| view["agent"]["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    // The kernel's socket opens beside its HTTP listener, a moment later.
    let started = Instant::now();
    let listed = loop {
        let listed = agents(&["list"]);
        if listed.status.success() || started.elapsed() > common::TIMEOUT {
            break listed;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(text(&listed).contains("No agents of your own yet"), "{}", text(&listed));

    // With no terminal to ask on, a change is refused after its preview.
    let finance = ["new", "--name", "Finance", "--does", "Reads invoices", "--asks", "ask-first"];
    let refused = agents(&finance);
    assert!(!refused.status.success());
    assert!(text(&refused).contains("Here is what Finance can do"), "{}", text(&refused));
    assert!(text(&refused).contains("no terminal to ask on"), "{}", text(&refused));
    assert!(ids().is_empty(), "nothing is added");

    // --yes applies a change that is not high risk.
    let added = agents(
        &[&["--yes"][..], &finance[..], &["--files", "read", "--folder", "Invoices"][..]].concat(),
    );
    assert!(added.status.success(), "{}", text(&added));
    assert!(text(&added).contains("Added Finance."), "{}", text(&added));
    assert_eq!(ids(), ["finance"]);

    // --yes is refused at high risk; on the kernel's terminal, the person confirms it.
    let ops = ["new", "--name", "Ops", "--does", "Keeps the build green", "--asks", "go-ahead"];
    let refused = agents(&[&["--yes"][..], &ops[..]].concat());
    assert!(!refused.status.success());
    assert!(text(&refused).contains("High risk"), "{}", text(&refused));
    assert!(text(&refused).contains("--yes is refused at high risk"), "{}", text(&refused));
    assert_eq!(ids(), ["finance"]);
    let mut terminal = Terminal::spawn_in(
        &[&["agents", "--home", at.as_str()][..], &ops[..]].concat(),
        home.path(),
        ws.path(),
    );
    terminal.expect("Confirm? [y/N]");
    terminal.send("y\r");
    terminal.expect("Added Ops.");
    let (code, output) = terminal.wait();
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(ids(), ["finance", "ops"]);

    // `/agent` applies a change only with the code its preview showed.
    let preview = api.agent("pause ops --json").await;
    let code = preview["code"].as_str().unwrap().to_string();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let (status, body) = api
        .post(
            "/api/kernel/commands/run",
            json!({ "name": "agent", "args": format!("pause ops --confirm {wrong}"), "sessionId": "terminal" }),
        )
        .await;
    assert_ne!(status, 200, "{body}");
    assert!(body.to_string().contains("does not confirm this change"), "{body}");
    let paused = api.agent(&format!("pause ops --confirm {code} --json")).await;
    assert_eq!(paused["outcome"]["outcome"], "paused", "{paused}");
    drop(up);
}
