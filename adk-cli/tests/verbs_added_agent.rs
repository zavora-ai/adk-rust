//! Screen actions on `adk-rust up` (spec 3.3 §2 item 7, §12 "One verb, three origins"): an
//! agent a person added asks to snooze something in Coming up, its request waits through a
//! `kill -9` and a restart on SQLite (`harness.db`) and on PostgreSQL (`--store`), Allow once on
//! the phone runs it once, and the terminal attached to the kernel draws its card and snoozes
//! in up's own process.
//!
//! ```sh
//! cargo nextest run -p adk-cli --features verb-ui,scripted-model --test verbs_added_agent
//! ```
//!
//! The PostgreSQL leg runs by hand (owner question 31):
//!
//! ```sh
//! DATABASE_URL=postgres://… cargo nextest run -p adk-cli \
//!   --features verb-ui,scripted-model,postgres --test verbs_added_agent --run-ignored only
//! ```
//!
//! With `voice`, "One verb, three origins" runs on `up` too (spec 3.3 §12, Experimental): the
//! person snoozes News digest by typing `/snooze` in the terminal, by clicking in the home and
//! by voice, and the kernel's record of the three, kept on SQLite (and on PostgreSQL), differs
//! only in where and how each was given; with `audit-sql` the comparison is of the signed
//! records `adk-rust audit export` reads back.
//!
//! ```sh
//! cargo nextest run -p adk-cli --features verb-ui,voice,scripted-model,audit-sql \
//!   --test verbs_added_agent -E 'test(/three_origins/)'
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
    /// `--store`: every store in one shared backend; `None` keeps them in `harness.db`.
    store: Option<String>,
}

impl Place {
    fn new(store: Option<String>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let kernel = home.path().join("kernel");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        Self { home, ws, kernel, port, store }
    }

    /// `adk-rust up` in Ask first with `script`; returns once it listens.
    fn up(&self, script: &str) -> Up {
        self.up_with(script, &[])
    }

    /// [`up`](Self::up) with `extra` flags.
    fn up_with(&self, script: &str, extra: &[&str]) -> Up {
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
            .args(extra)
            .current_dir(self.ws.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(store) = &self.store {
            command.args(["--store", store]);
        }
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
    snooze_on(None).await;
}

/// The scenario with every store on PostgreSQL, in a database of its own (spec 3.3 §12).
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL, a PostgreSQL server; run by hand"]
async fn an_added_agents_snooze_waits_through_a_restart_on_postgres() {
    let server = std::env::var("DATABASE_URL").expect("DATABASE_URL names a PostgreSQL server");
    let admin = sqlx::PgPool::connect(&server).await.unwrap();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // A name of digits and underscores only, so it needs no quoting.
    let database = format!("verbs_added_agent_{nanos}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    snooze_on(Some(with_database(&server, &database))).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)")))
        .execute(&admin)
        .await
        .unwrap();
}

/// `server` with its database path replaced by `database`, its query kept.
#[cfg(feature = "postgres")]
fn with_database(server: &str, database: &str) -> String {
    let (base, query) = server.split_once('?').unwrap_or((server, ""));
    let host = base.find("://").map_or(0, |at| at + 3);
    let root = base[host..].find('/').map_or(base, |at| &base[..host + at]);
    match query {
        "" => format!("{root}/{database}"),
        query => format!("{root}/{database}?{query}"),
    }
}

async fn snooze_on(store: Option<String>) {
    let place = Place::new(store);
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
        json!([{
            "id": "verb-ui",
            "text": "Ask to change things on your screens — always asks you first",
            "modes": ["askFirst", "goAhead"],
        }])
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

/// "One verb, three origins" on `adk-rust up` over SQLite (spec 3.3 §12, Experimental).
#[cfg(feature = "voice")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_verb_three_origins_on_up() {
    three_origins::on(None).await;
}

/// The same on PostgreSQL, in a database of its own.
#[cfg(all(feature = "voice", feature = "postgres"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL, a PostgreSQL server; run by hand"]
async fn one_verb_three_origins_on_up_on_postgres() {
    let server = std::env::var("DATABASE_URL").expect("DATABASE_URL names a PostgreSQL server");
    let admin = sqlx::PgPool::connect(&server).await.unwrap();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let database = format!("three_origins_{nanos}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    three_origins::on(Some(with_database(&server, &database))).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)")))
        .execute(&admin)
        .await
        .unwrap();
}

#[cfg(feature = "voice")]
mod three_origins {
    use std::time::Duration;

    use adk_realtime::ServerEvent;
    use adk_realtime::scripted::{self, ScriptedReply};
    use chrono::{DateTime, Timelike, Utc};
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::Message;

    use super::{Api, Place, eventually};

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// What the person says: an action, not a decision.
    const SAID: &str = "Snooze news digest";

    /// The next update on `socket` whose `type` is `kind`.
    async fn update(socket: &mut Socket, kind: &str) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let message = tokio::time::timeout_at(deadline, socket.next())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {kind}"))
                .unwrap_or_else(|| panic!("the socket ended before {kind}"))
                .unwrap();
            if let Message::Text(text) = message {
                let update: Value = serde_json::from_str(text.as_str()).unwrap();
                if update["type"] == kind {
                    return update;
                }
            }
        }
    }

    /// Moves `id` back to `at`, as the person would before giving the same request again.
    async fn move_back(api: &Api, id: &str, at: DateTime<Utc>) {
        let (status, body) = api
            .post(
                &format!("/api/kernel/agenda/{id}/schedule"),
                json!({ "wake": { "anyOf": [{ "at": at.to_rfc3339() }] } }),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        eventually("News digest back in place", || async {
            (api.wake_of("News digest").await == Some(at)).then_some(())
        })
        .await;
    }

    pub(super) async fn on(store: Option<String>) {
        let place = Place::new(store);
        let api = place.api();
        let at = (Utc::now() + chrono::Duration::hours(8))
            .with_minute(0)
            .and_then(|at| at.with_second(0))
            .and_then(|at| at.with_nanosecond(0))
            .unwrap();
        let to = at + chrono::Duration::hours(1);
        let voice = json!([ScriptedReply::new(vec![
            ServerEvent::SpeechStarted { event_id: "speech".into(), audio_start_ms: 0 },
            scripted::input_transcript(SAID),
        ])
        .on_audio()]);
        let voice_script = place.ws.path().join("voice-script.json");
        std::fs::write(&voice_script, voice.to_string()).unwrap();
        let voice_script = voice_script.display().to_string();
        let _up = place.up_with(
            &json!([
                { "call": { "name": "propose_followup", "args": {
                    "title": "News digest",
                    "rationale": "The evening edition lands at 18:30",
                    "when": { "at": at.to_rfc3339() }
                } } },
                { "text": "It waits for you." },
                { "text": "Done." }
            ])
            .to_string(),
            &["--pack", "voice", "--voice-script", &voice_script],
        );

        // News digest in Coming up, at a whole hour eight hours from now.
        api.command_from(
            "web",
            "pointer",
            json!({ "type": "send", "sessionId": "ask-1", "text": "Set up my evening." }),
        )
        .await;
        let card =
            eventually("the suggestion", || async { api.decisions().await.into_iter().next() })
                .await;
        api.command_from(
            "web",
            "pointer",
            json!({ "type": "decide", "decisionId": card["id"], "version": card["version"], "answer": "allowOnce", "expanded": true }),
        )
        .await;
        eventually("News digest scheduled", || async {
            (api.wake_of("News digest").await == Some(at)).then_some(())
        })
        .await;
        let item =
            api.agenda("scheduled").await.into_iter().find(|item| item["title"] == "News digest");
        let id = item.unwrap()["id"].as_str().unwrap().to_string();

        // 1. Text: `/snooze News digest` in the terminal attached to the kernel.
        let (status, ran) = api
            .post(
                "/api/kernel/commands/run",
                json!({ "name": "snooze", "args": "News digest", "sessionId": "terminal" }),
            )
            .await;
        assert_eq!(status, 200, "{ran}");
        eventually("the typed snooze", || async {
            (api.wake_of("News digest").await == Some(to)).then_some(())
        })
        .await;
        move_back(&api, &id, at).await;

        // 2. Click: Snooze → "1 hour later" in the home, which sends the choice's `to` exactly
        // as `GET /api/surface/verbs` lists it: the same instant written another way, "+00:00"
        // for "Z", has another `argsDigest`.
        let listed = api.get("/api/surface/verbs?surface=web").await;
        let item = listed["comingUp"].as_array().unwrap().iter().find(|item| item["id"] == id);
        let choice = item.unwrap()["choices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|choice| choice["tomorrow"] == false)
            .cloned()
            .expect("News digest may move an hour later");
        let moves_to = choice["to"].as_str().and_then(|at| at.parse::<DateTime<Utc>>().ok());
        assert_eq!(moves_to, Some(to), "{choice}");
        let (status, done) = api
            .post(
                "/api/surface/verbs/call",
                json!({
                    "origin": { "kind": "pointer" },
                    "intent": {
                        "verb": "snooze",
                        "objects": [{ "kind": "comingUp", "id": id }],
                        "args": { "to": choice["to"] },
                    },
                }),
            )
            .await;
        assert_eq!(status, 200, "{done}");
        eventually("the clicked snooze", || async {
            (api.wake_of("News digest").await == Some(to)).then_some(())
        })
        .await;
        move_back(&api, &id, at).await;

        // 3. Voice: "Snooze news digest", after the cancel window.
        let ticket =
            api.post("/api/surface/voice/sessions", json!({ "model": "scripted" })).await.1;
        let path = ticket["socket"].as_str().expect("a socket path");
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}{path}", place.port))
                .await
                .unwrap();
        let hello = json!({ "type": "hello", "ticket": ticket["ticket"] }).to_string();
        socket.send(Message::Text(hello.into())).await.unwrap();
        update(&mut socket, "ready").await;
        let mut frame = vec![0x01];
        frame.extend_from_slice(&[0; 640]);
        socket.send(Message::Binary(frame.into())).await.unwrap();
        let pending = update(&mut socket, "pendingAction").await;
        assert!(
            pending["heading"].as_str().unwrap_or_default().contains("News digest"),
            "{pending}"
        );
        let outcome = update(&mut socket, "actionOutcome").await;
        assert_eq!(outcome["done"], true, "{outcome}");
        assert_eq!(api.wake_of("News digest").await, Some(to));
        socket.close(None).await.unwrap();

        // The record of the three differs only in where and how each was given.
        let lines = api.verb_lines().await;
        assert_eq!(lines.len(), 3, "{lines:?}");
        let hows: Vec<(Value, Value)> =
            lines.iter().rev().map(|line| (line["surface"].clone(), line["how"].clone())).collect();
        assert_eq!(
            hows,
            [
                (json!("terminal"), json!("keyboard")),
                (json!("web"), json!("pointer")),
                (json!("voice"), json!("voice")),
            ]
        );
        let same = |line: &Value| {
            let mut line = line.clone();
            for key in ["seq", "at", "surface", "how", "channel"] {
                line.as_object_mut().unwrap().remove(key);
            }
            line
        };
        assert_eq!(same(&lines[0]), same(&lines[1]), "{lines:?}");
        assert_eq!(same(&lines[1]), same(&lines[2]), "{lines:?}");
        assert_eq!(lines[0]["tool"], "snooze");
        #[cfg(feature = "audit-sql")]
        signed_records_differ_only_in_how(&place).await;
    }

    /// The signed `surface.verb` and `verb_snooze` records, exported as `adk-rust audit export`
    /// writes them: equal but for where and how; the voice record carries the transcript's
    /// digest and never the words.
    #[cfg(feature = "audit-sql")]
    async fn signed_records_differ_only_in_how(place: &Place) {
        use sha2::{Digest, Sha256};

        let id = std::fs::read_to_string(place.kernel.join("kernel-id")).unwrap();
        let database = match &place.store {
            Some(store) => store.clone(),
            None => format!("sqlite://{}", place.kernel.join("harness.db").display()),
        };
        let out = place.home.path().join("record.jsonl");
        let log = format!("kernel-{}", id.trim());
        let export = super::common::run(
            &[
                "audit",
                "export",
                "--database-url",
                &database,
                "--log-id",
                &log,
                "--out",
                &out.display().to_string(),
            ],
            place.home.path(),
            place.ws.path(),
        );
        assert!(export.status.success(), "{}", super::common::text(&export));
        let exported = std::fs::read_to_string(&out).unwrap();
        let records: Vec<Value> = exported
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|line| line["type"] == "entry")
            .filter_map(|entry| serde_json::from_str(entry["record"].as_str()?).ok())
            .collect();
        let without = |record: &Value, keys: &[&str], actor_keys: &[&str]| {
            let mut record = record.clone();
            for key in ["seq", "ts", "prevHash"] {
                record.as_object_mut().unwrap().remove(key);
            }
            if let Some(ext) = record["ext"].as_object_mut() {
                for key in keys {
                    ext.remove(*key);
                }
            }
            if let Some(actor) = record["actor"].as_object_mut() {
                for key in actor_keys {
                    actor.remove(*key);
                }
            }
            record
        };
        let verbs: Vec<&Value> =
            records.iter().filter(|record| record["action"] == "surface.verb").collect();
        assert_eq!(verbs.len(), 3, "{verbs:?}");
        let digest = format!("{:x}", Sha256::digest(SAID.as_bytes()));
        assert_eq!(verbs[2]["ext"]["transcriptDigest"], json!(digest), "{:?}", verbs[2]);
        assert!(!exported.to_lowercase().contains(&SAID.to_lowercase()), "never the words");
        let how = ["surface", "origin", "device", "presence", "transcriptDigest", "functionCallId"];
        let same: Vec<Value> = verbs.iter().map(|record| without(record, &how, &[])).collect();
        assert_eq!(same[0], same[1], "{verbs:?}");
        assert_eq!(same[1], same[2], "{verbs:?}");
        let calls: Vec<Value> = records
            .iter()
            .filter(|record| {
                record["ext"]["tool"] == "verb_snooze" || record["action"] == "verb_snooze"
            })
            .map(|record| without(record, &["functionCallId", "functionCall"], &["invocationId"]))
            .collect();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(calls[0], calls[1], "{calls:?}");
        assert_eq!(calls[1], calls[2], "{calls:?}");
        assert_eq!(calls[0]["actor"]["agent"], "urn:adk:agent:adk-rust/surface.verbs", "{calls:?}");
    }
}
