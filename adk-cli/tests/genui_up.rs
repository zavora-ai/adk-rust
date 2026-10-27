//! Spec 3.3 §12 "Governed form" step 8 (§7.4 rule 5, wave 4 decisions 13 and 14): `adk-rust
//! up` with a scripted model gives every harness it hosts the render tools. A form drawn by
//! the agent a person added, Bookkeeper, is held as Bookkeeper; one drawn in the Home area is
//! held in that area's target; Allow once runs each in its own target, and nothing is answered
//! "not run".
//!
//! Run with `cargo nextest run -p adk-cli --features genui,scripted-model --test genui_up`.

#![cfg(all(unix, feature = "scripted-model", feature = "persist", feature = "genui"))]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// `adk-rust up` as a child process, killed when dropped, so a failing test leaves nothing
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

/// Every agent shares the scripted model, one step a call: Bookkeeper draws its note form
/// (1–2), the Home area draws its own (3–4), and each answers once its call ran (5–6).
const SCRIPT: &str = r#"[
  {"call": {"name": "render_form", "args": {
    "surfaceId": "note", "title": "Bookkeeper's note",
    "fields": [{"name": "content", "label": "Note"}],
    "actions": [{"id": "save", "label": "Save", "tool": "write_file",
                 "args": {"path": "bookkeeper.txt", "content": {"path": "/content"}}}]
  }}},
  {"text": "Here is the note."},
  {"call": {"name": "render_form", "args": {
    "surfaceId": "note", "title": "Home note",
    "fields": [{"name": "content", "label": "Note"}],
    "actions": [{"id": "save", "label": "Save", "tool": "write_file",
                 "args": {"path": "home.txt", "content": {"path": "/content"}}}]
  }}},
  {"text": "Here is the note."},
  {"text": "Saved."},
  {"text": "Saved."},
  {"text": "Done."}
]"#;

/// Two areas: Work, which coordinates, and Home.
fn user_config(home: &Path) {
    let config = "[kernel]\ncoordinator = \"work\"\n\n[[kernel.partitions]]\nid = \"work\"\n\n\
                  [[kernel.partitions]]\nid = \"home\"\n";
    for dir in [".config/adk-rust", "Library/Application Support/adk-rust"] {
        let dir = home.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("harness.toml"), config).unwrap();
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
        user_config(home.path());
        let kernel = home.path().join("kernel");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        Self { home, ws, kernel, port }
    }

    fn up(&self) -> Up {
        let (kernel, dir, bind) = (
            self.kernel.display().to_string(),
            self.ws.path().display().to_string(),
            format!("127.0.0.1:{}", self.port),
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
        command
            .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
            .args(["--model-script", SCRIPT, "--mode", "ask"])
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
                let _ = child.wait();
                panic!("up did not listen");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Up(Some(child))
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

    async fn processes(&self) -> Vec<Value> {
        self.get("/api/kernel/processes").await["processes"].as_array().cloned().unwrap_or_default()
    }

    /// The runs of `session`, once there are `count` and none is still to run or running.
    async fn settled(&self, session: &str, count: usize) -> Vec<Value> {
        eventually(&format!("{count} settled runs in {session}"), || async {
            let runs: Vec<Value> = self
                .processes()
                .await
                .into_iter()
                .filter(|process| process["run"]["sessionId"] == session)
                .map(|process| process["run"].clone())
                .collect();
            let settled = |run: &Value| {
                matches!(
                    run["state"].as_str(),
                    Some("completed" | "failed" | "cancelled" | "waiting")
                )
            };
            (runs.len() >= count && runs.iter().all(settled)).then_some(runs)
        })
        .await
    }

    /// The drawn surface of `session`.
    async fn drawn(&self, session: &str) -> Value {
        eventually(&format!("the drawing in {session}"), || async {
            // The session exists once its first run starts.
            let url = format!("{}/api/surface/sessions/{session}/thread", self.base);
            let response = self.client.get(url).send().await.ok()?;
            let thread: Value = response.json().await.ok()?;
            thread["entries"]
                .as_array()?
                .iter()
                .flat_map(|entry| entry["items"].as_array().cloned().unwrap_or_default())
                .find(|item| item["kind"] == "drawn")
        })
        .await
    }

    /// The open decision of `session`.
    async fn decision(&self, session: &str) -> Value {
        eventually(&format!("the decision of {session}"), || async {
            let page =
                self.get(&format!("/api/surface/decisions?surface=web&session={session}")).await;
            page["decisions"].as_array()?.first().cloned()
        })
        .await
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
async fn added_agents_and_areas_hold_their_forms_in_their_own_targets() {
    let place = Place::new();
    let _up = place.up();
    let api =
        Api { base: format!("http://127.0.0.1:{}", place.port), client: reqwest::Client::new() };

    // Bookkeeper, added by hand in Work; its detail lists what the home lets it do.
    let preview = api
        .agent("new --name Bookkeeper --area work --asks ask-first --files read-write --json")
        .await;
    let code = preview["code"].as_str().expect("a confirming code").to_string();
    api.agent(&format!("new --name Bookkeeper --area work --asks ask-first --files read-write --confirm {code} --json"))
        .await;
    let detail = eventually("Bookkeeper's detail", || async {
        let response = api
            .client
            .get(format!("{}/api/surface/agents/bookkeeper", api.base))
            .send()
            .await
            .ok()?;
        response.status().is_success().then_some(())?;
        response.json::<Value>().await.ok()
    })
    .await;
    assert_eq!(
        detail["granted"],
        json!([{ "id": "genui", "text": "Show forms and tables in your threads — sending asks you first" }]),
        "{detail}"
    );

    // Bookkeeper draws in its own thread; the Home area draws in one of its own.
    api.command(json!({ "type": "newThread", "agent": "bookkeeper", "sessionId": "bk-1", "text": "Draw the note" }))
        .await;
    let drawn = api.drawn("bk-1").await;
    assert_eq!(drawn["surface"]["title"], "Bookkeeper's note");
    api.settled("bk-1", 1).await;
    let (status, spawned) = api
        .post(
            "/api/kernel/processes",
            json!({ "target": "adk-rust@home", "sessionId": "home-1", "input": { "text": "Draw the note" } }),
        )
        .await;
    assert_eq!(status, 200, "{spawned}");
    let drawn = api.drawn("home-1").await;
    assert_eq!(drawn["surface"]["title"], "Home note");
    api.settled("home-1", 1).await;

    // Sent from the web home: each is held as its own agent, in its own target.
    for (session, text) in [("bk-1", "Paid on the 3rd"), ("home-1", "Water the plants")] {
        let sent = api
            .command(json!({
                "type": "submitDrawn", "sessionId": session, "surfaceId": "note", "version": 1,
                "actionId": "save", "values": { "/content": text }
            }))
            .await;
        assert_eq!(sent["type"], "formSent", "{sent}");
    }
    let bookkeeper = api.decision("bk-1").await;
    let home = api.decision("home-1").await;
    assert_eq!(bookkeeper["agent"]["name"], "bookkeeper", "{bookkeeper}");
    assert!(
        bookkeeper["timeline"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("You pressed Send on a form Bookkeeper drew")
    );
    let targets = |session: &'static str| {
        let api = &api;
        async move {
            api.processes()
                .await
                .into_iter()
                .filter(|process| process["run"]["sessionId"] == session)
                .map(|process| process["run"]["target"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert!(targets("bk-1").await.iter().all(|target| target == "bookkeeper"));
    assert!(targets("home-1").await.iter().all(|target| target == "adk-rust@home"));
    for card in [&bookkeeper, &home] {
        let offered: Vec<&str> = card["allowed"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["answer"].as_str().unwrap())
            .collect();
        assert!(!offered.contains(&"alwaysAllow"), "{offered:?}");
    }
    assert!(
        !place.ws.path().join("bookkeeper.txt").exists()
            && !place.ws.path().join("home.txt").exists()
    );

    // Allow once runs each in its own target, once.
    for card in [&bookkeeper, &home] {
        api.command(json!({
            "type": "decide", "decisionId": card["id"], "version": card["version"],
            "answer": "allowOnce", "expanded": true
        }))
        .await;
    }
    for (file, text) in [("bookkeeper.txt", "Paid on the 3rd"), ("home.txt", "Water the plants")] {
        let path = place.ws.path().join(file);
        let written = eventually(&format!("{file} written"), || async {
            std::fs::read_to_string(&path).ok()
        })
        .await;
        assert_eq!(written, text);
    }
    for session in ["bk-1", "home-1"] {
        let forms = eventually(&format!("the form of {session} done"), || async {
            let forms = api.get(&format!("/api/surface/genui/sessions/{session}/forms")).await;
            (forms["forms"][0]["status"] == "done").then_some(forms)
        })
        .await;
        assert!(
            forms["forms"][0]["text"].as_str().unwrap().starts_with("Sent · done at "),
            "{forms}"
        );
        let thread = api.get(&format!("/api/surface/sessions/{session}/thread")).await;
        assert!(
            !thread.to_string().contains("was not run"),
            "nothing is answered 'not run': {thread}"
        );
    }
    assert!(targets("bk-1").await.iter().all(|target| target == "bookkeeper"));
    assert!(targets("home-1").await.iter().all(|target| target == "adk-rust@home"));
}
