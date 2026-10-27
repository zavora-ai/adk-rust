//! Spec 3.3 §12 phase 5 "Speak, glance, stop" as one run with work in flight, on `adk-rust up`
//! serving the phone (`--home-url`, an operator token) with the voice pack, a scripted model,
//! a scripted realtime provider and a press driver whose `publish_post` is `publish_public`.
//!
//! | Step | What happens | What holds |
//! |---|---|---|
//! | 1 Speak | The person asks by voice; the model's `bash` waits; they say "allow once" | Nothing runs inside the cancel window; then the call runs exactly once |
//! | 2 High risk | The model asks to `publish_post`; the person says "allow once" | Voice decides nothing ("needs a screen"); the post waits |
//! | 3 Glance | The tray follows the kernel's events, reading its status after each | The waiting post shows on the tray on the event that follows it, with no read on a timer |
//! | 4 On screen | The web home approves the post with "Confirm it's you" (a passkey) | It runs exactly once, on the voice conversation |
//! | 5 Stop | With a turn running a step every 0.3 s, a voice call open and a call waiting, the tray sends Stop all | Within about a second the turn takes no more steps and is parked, the voice call ends with "All agents are stopped", and the waiting call, approved afterwards, does not run; the tray shows it stopped on the next event |
//!
//! The tray is the HTTP client the desktop tray is (`adk-desktop-core`'s `StatusWatcher` and
//! `stop_all`): `GET /api/kernel/events`, `GET /api/surface/status?surface=tray` after each
//! frame, and `POST /api/surface/commands` with surface `tray`. The binary runs as a process on
//! SQLite (`harness.db`); the PostgreSQL leg is `#[ignore]`d and runs in
//! `scripts/test-postgres.sh`:
//!
//! ```sh
//! cargo nextest run -p adk-cli --features voice,mobile,scripted-model --test speak_glance_stop
//! DATABASE_URL=postgres://… cargo nextest run -p adk-cli --features voice,mobile,scripted-model,postgres \
//!   --test speak_glance_stop --run-ignored only
//! ```

#![cfg(all(
    unix,
    feature = "scripted-model",
    feature = "voice",
    feature = "mobile",
    feature = "persist"
))]

#[path = "../../adk-surface/tests/mobile_support/authenticator.rs"]
mod authenticator;
mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use adk_realtime::ServerEvent;
use adk_realtime::scripted::{self, ScriptedReply, ScriptedStep};
use authenticator::Authenticator;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// The operator's token: `--home-url` asks every caller to sign in.
const TOKEN: &str = "speak-glance-stop-operator";
/// The binary frame tag of microphone audio on the voice socket.
const FRAME_AUDIO: u8 = 0x01;
/// How long a prompt shows before a spoken answer applies to it, with a margin.
const DWELL: Duration = Duration::from_millis(1_300);
/// How long the test waits for one update.
const WAIT: Duration = Duration::from_secs(30);
/// "About a second" (spec 3.3 §12 phase 5), with room for a loaded machine.
const ABOUT_A_SECOND: Duration = Duration::from_millis(1_500);
/// How many steps the running turn could take: far more than the run lasts.
const STEPS: usize = 60;

/// The press driver: a stdio MCP server. `publish_post` (`publish_public`) appends a mark per
/// post to `$PRESS_OUT`; `check_feed` (`read`, so it runs without asking) takes 0.3 s and
/// appends a mark to `$PRESS_TICKS`, so the run counts the steps a turn takes.
const PRESS: &str = r#"import json, os, sys, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method, ident = message.get("method"), message.get("id")
    if ident is None:
        continue
    if method == "initialize":
        result = {"protocolVersion": message["params"]["protocolVersion"],
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "press", "version": "1.0.0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "publish_post",
                             "description": "Publishes a post on the person's public blog. It cannot be unpublished.",
                             "inputSchema": {"type": "object",
                                             "properties": {"text": {"type": "string"}},
                                             "required": ["text"]}},
                            {"name": "check_feed",
                             "description": "Reads the press feed once.",
                             "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call" and message["params"]["name"] == "check_feed":
        time.sleep(0.3)
        with open(os.environ["PRESS_TICKS"], "a") as out:
            out.write("x")
        result = {"content": [{"type": "text", "text": "Nothing new."}], "isError": False}
    elif method == "tools/call":
        with open(os.environ["PRESS_OUT"], "a") as out:
            out.write("x")
        result = {"content": [{"type": "text", "text": "Published."}], "isError": False}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": ident, "result": result}), flush=True)
"#;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

fn speech_started() -> ServerEvent {
    ServerEvent::SpeechStarted { event_id: "speech".into(), audio_start_ms: 0 }
}

fn says(text: &str) -> Vec<ServerEvent> {
    vec![speech_started(), scripted::input_transcript(text)]
}

/// The voice conversation: an everyday call allowed once by voice, then a high-risk one voice
/// cannot allow.
fn voice_script() -> Vec<ScriptedStep> {
    vec![
        ScriptedReply::new(
            [
                says("send the invoice"),
                scripted::calls("call-1", "bash", json!({ "command": "printf x >> sent.txt" })),
            ]
            .concat(),
        )
        .on_audio(),
        ScriptedReply::new(scripted::spoken("That needs your answer.")).on_tool_output(),
        ScriptedReply::new(says("allow once")).on_audio(),
        ScriptedReply::new(scripted::spoken("Sent.")).on_tool_output(),
        ScriptedReply::new(
            [
                says("publish the release notes"),
                scripted::calls("call-2", "publish_post", json!({ "text": "Release 3.0 is out." })),
            ]
            .concat(),
        )
        .on_audio(),
        ScriptedReply::new(scripted::spoken("That needs your screen.")).on_tool_output(),
        ScriptedReply::new(says("allow once")).on_audio(),
        ScriptedReply::new(scripted::spoken("Published.")).on_tool_output(),
    ]
}

/// The text model, one step a call: the waiting call's turn, then the running turn's steps.
fn model_script() -> Value {
    let mut steps = vec![json!({ "call": { "name": "write_file", "args": {
        "path": "held.txt", "content": "Dear client, INV-7 is overdue."
    } } })];
    for _ in 0..STEPS {
        steps.push(json!({ "call": { "name": "check_feed", "args": {} } }));
    }
    steps.push(json!({ "text": "Done." }));
    Value::Array(steps)
}

struct Place {
    home: tempfile::TempDir,
    ws: tempfile::TempDir,
    port: u16,
}

impl Place {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        Self { home, ws, port }
    }

    fn kernel(&self) -> String {
        self.home.path().join("kernel").display().to_string()
    }

    /// `adk-rust up` serving the phone and the voice pack on `store`; returns once it listens.
    fn up(&self, store: Option<&str>) -> Up {
        let ws = self.ws.path();
        let script = ws.join("model-script.json");
        std::fs::write(&script, model_script().to_string()).unwrap();
        let voice = ws.join("voice-script.json");
        std::fs::write(&voice, serde_json::to_string(&voice_script()).unwrap()).unwrap();
        let (kernel, dir, bind, home_url) = (
            self.kernel(),
            ws.display().to_string(),
            format!("127.0.0.1:{}", self.port),
            format!("http://localhost:{}", self.port),
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
        command
            .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
            .args(["--model-script", &script.display().to_string(), "--mode", "ask"])
            .args(["--no-memory", "--no-consolidation", "--pack", "voice"])
            .args(["--voice-script", &voice.display().to_string()])
            .args(["--home-url", &home_url, "--auth-token", TOKEN])
            .current_dir(ws)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(store) = store {
            command.args(["--store", store]);
        }
        common::isolate(&mut command, self.home.path());
        command
            .env_remove("TELEGRAM_BOT_TOKEN")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("OPENAI_API_KEY");
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

    /// Mounts the press driver, as `adk-rust drivers mount --yes` does.
    fn mount_press(&self) {
        let ws = self.ws.path();
        std::fs::write(ws.join("press.py"), PRESS).unwrap();
        let quote = |path: &Path| format!("{:?}", path.display().to_string());
        let manifest = format!(
            "id = \"press\"\ncommand = \"python3\"\nargs = [\"-u\", {}]\nprotocol = \"initialize\"\n\
             sandbox = \"unsandboxed\"\nenv = {{ PRESS_OUT = {}, PRESS_TICKS = {} }}\n[tools]\n\
             publish_post = \"publish_public\"\ncheck_feed = \"read\"\n",
            quote(&ws.join("press.py")),
            quote(&ws.join("published.txt")),
            quote(&ws.join("ticks.txt")),
        );
        let path = self.home.path().join("press.toml");
        std::fs::write(&path, manifest).unwrap();
        let kernel = self.kernel();
        let mounted = common::run(
            &["drivers", "--home", &kernel, "mount", &path.display().to_string(), "--yes"],
            self.home.path(),
            ws,
        );
        assert!(mounted.status.success(), "{}", common::text(&mounted));
    }

    /// How many marks `file` holds: how many times the call that writes it ran.
    fn runs(&self, file: &str) -> usize {
        std::fs::read(self.ws.path().join(file)).map_or(0, |bytes| bytes.len())
    }
}

/// The operator's client: the web home and the tray, signed in with the token.
#[derive(Clone)]
struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    async fn get(&self, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "GET {path}: {status} {body}");
        body
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// `command` from the web home.
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

    async fn decisions(&self) -> Vec<Value> {
        self.get("/api/surface/decisions?surface=web").await["decisions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    async fn processes(&self) -> Vec<Value> {
        self.get("/api/kernel/processes").await["processes"].as_array().cloned().unwrap_or_default()
    }

    /// The run of the latest process of `session`.
    async fn run_of(&self, session: &str) -> Option<Value> {
        self.processes()
            .await
            .into_iter()
            .filter(|process| process["run"]["sessionId"] == session)
            .map(|process| process["run"].clone())
            .max_by_key(|run| run["updatedAt"].as_str().unwrap_or_default().to_string())
    }

    /// "Confirm it's you" in the web home: the person's passkey, then the answer it confirms
    /// (spec 3.3 §7.2, §8.2).
    async fn confirm(&self, passkey: &mut Authenticator, card: &Value) -> Value {
        let id = card["id"].as_str().unwrap();
        let (status, options) = self
            .post(
                &format!("/api/surface/decisions/{id}/confirm/options"),
                json!({ "version": card["version"], "answer": "allowOnce", "expanded": true }),
            )
            .await;
        assert_eq!(status, 200, "{options}");
        let credential = passkey.assert(&options["publicKey"]);
        let (status, outcome) = self
            .post(
                &format!("/api/surface/decisions/{id}/confirm"),
                json!({ "challengeId": options["challengeId"], "credential": credential }),
            )
            .await;
        assert_eq!(status, 200, "{outcome}");
        outcome
    }
}

/// What the tray read after one kernel event: when, the event, and its status.
#[derive(Debug, Clone)]
struct Glance {
    at: Instant,
    event: Value,
    status: Value,
}

/// The tray as the desktop tray reads the kernel: its status after every frame of the event
/// stream and at no other time, so whatever it shows came with an event.
#[derive(Clone)]
struct Tray {
    glances: Arc<Mutex<Vec<Glance>>>,
    task: Arc<tokio::task::JoinHandle<()>>,
}

impl Drop for Tray {
    fn drop(&mut self) {
        if Arc::strong_count(&self.task) == 1 {
            self.task.abort();
        }
    }
}

impl Tray {
    async fn follow(api: &Api) -> Self {
        let glances: Arc<Mutex<Vec<Glance>>> = Arc::default();
        let response = api
            .client
            .get(format!("{}/api/kernel/events", api.base))
            .bearer_auth(TOKEN)
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let (seen, api) = (Arc::clone(&glances), api.clone());
        let task = tokio::spawn(async move {
            let mut body = response.bytes_stream();
            let mut tail = String::new();
            while let Some(Ok(chunk)) = body.next().await {
                tail.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = tail.find("\n\n") {
                    let frame: String = tail.drain(..end + 2).collect();
                    let data: String = frame
                        .lines()
                        .filter_map(|line| line.strip_prefix("data:"))
                        .map(str::trim)
                        .collect();
                    if data.is_empty() {
                        continue;
                    }
                    let event = serde_json::from_str(&data).unwrap_or(Value::String(data));
                    let status = api.get("/api/surface/status?surface=tray").await;
                    seen.lock().unwrap_or_else(PoisonError::into_inner).push(Glance {
                        at: Instant::now(),
                        event,
                        status,
                    });
                }
            }
        });
        Self { glances, task: Arc::new(task) }
    }

    /// The first glance since `since` that `shows` accepts, waiting up to `limit`.
    async fn shows(
        &self,
        since: Instant,
        limit: Duration,
        shows: impl Fn(&Value) -> bool,
    ) -> Option<Glance> {
        let deadline = Instant::now() + limit;
        loop {
            let found = self
                .glances
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|glance| glance.at >= since && shows(&glance.status))
                .cloned();
            if found.is_some() || Instant::now() >= deadline {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// The next update on `socket` whose `type` is `kind`.
async fn update(socket: &mut Socket, kind: &str) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut seen = Vec::new();
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {kind}; saw {seen:?}"))
            .unwrap_or_else(|| panic!("the socket ended before {kind}; saw {seen:?}"))
            .unwrap();
        if let Message::Text(text) = message {
            let update: Value = serde_json::from_str(text.as_str()).unwrap();
            if update["type"] == kind {
                return update;
            }
            seen.push(update);
        }
    }
}

/// 20 ms of microphone silence: the scripted provider takes its next step on audio.
async fn speak(socket: &mut Socket) {
    let mut frame = vec![FRAME_AUDIO];
    frame.extend_from_slice(&[0; 640]);
    socket.send(Message::Binary(frame.into())).await.unwrap();
}

async fn eventually<T, F, Fut>(what: &str, limit: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(started.elapsed() <= limit, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn speak_glance_stop(store: Option<&str>) {
    let place = Place::new();
    let _up = place.up(store);
    let api =
        Api { base: format!("http://127.0.0.1:{}", place.port), client: reqwest::Client::new() };
    eventually("the kernel's routes", WAIT, || async {
        let response = api
            .client
            .get(format!("{}/api/kernel/health", api.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .ok()?;
        response.status().is_success().then_some(())
    })
    .await;
    place.mount_press();
    // The person's passkey on this computer, for "Confirm it's you" in the web home.
    let mut passkey = Authenticator::new(&format!("http://localhost:{}", place.port), "localhost");
    let (status, options) = api.post("/api/surface/phone/passkey/options", json!({})).await;
    assert_eq!(status, 200, "{options}");
    let credential = passkey.register(&options["publicKey"]);
    let (status, body) = api
        .post(
            "/api/surface/phone/passkey",
            json!({ "registrationId": options["registrationId"], "credential": credential }),
        )
        .await;
    assert!(status == 200 || status == 204, "{status} {body}");
    let tray = Tray::follow(&api).await;

    // 1. Speak: the model's everyday `bash` waits; "allow once" runs it once, after the
    // cancel window.
    let (status, ticket) =
        api.post("/api/surface/voice/sessions", json!({ "model": "scripted" })).await;
    assert_eq!(status, 200, "{ticket}");
    let path = ticket["socket"].as_str().expect("a socket path");
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}{path}", place.port))
            .await
            .unwrap();
    let hello = json!({ "type": "hello", "ticket": ticket["ticket"] }).to_string();
    socket.send(Message::Text(hello.into())).await.unwrap();
    update(&mut socket, "ready").await;
    speak(&mut socket).await;
    let prompted = loop {
        let prompted = update(&mut socket, "prompted").await;
        if prompted["decisionId"].is_string() {
            break prompted;
        }
    };
    let everyday = prompted["decisionId"].as_str().unwrap().to_string();
    tokio::time::sleep(DWELL).await;
    speak(&mut socket).await;
    assert_eq!(update(&mut socket, "heard").await["text"], "allow once");
    let pending = update(&mut socket, "pending").await;
    assert_eq!(pending["decisionId"], everyday.as_str(), "{pending}");
    assert_eq!(place.runs("sent.txt"), 0, "nothing runs inside the cancel window");
    let outcome = update(&mut socket, "outcome").await;
    assert_eq!(outcome["decided"], true, "{outcome}");
    eventually("the allowed call", WAIT, || async { (place.runs("sent.txt") > 0).then_some(()) })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(place.runs("sent.txt"), 1, "the everyday call ran exactly once");

    // 2. High risk: voice cannot allow `publish_post`; the tray shows it waiting on the
    // event that follows it (3. Glance).
    let decided_at = Instant::now();
    speak(&mut socket).await;
    let screen = loop {
        let screen = update(&mut socket, "needsScreen").await;
        if screen["decisionIds"].as_array().is_some_and(|ids| !ids.is_empty()) {
            break screen;
        }
    };
    let release = screen["decisionIds"][0].as_str().unwrap().to_string();
    let glance = tray
        .shows(decided_at, Duration::from_secs(3), |status| status["decisions"] == 1)
        .await
        .unwrap_or_else(|| {
            let glances = tray.glances.lock().unwrap_or_else(PoisonError::into_inner).clone();
            panic!("the tray never showed the post waiting on a kernel event: {glances:?}")
        });
    // The event that announced it: the inbox's held call, on the kernel's bus.
    assert_eq!(glance.event["event"]["type"], "held", "{glance:?}");
    assert_eq!(glance.event["event"]["request"]["toolName"], "publish_post", "{glance:?}");
    assert!(glance.status["stopped"] == false, "{glance:?}");
    tokio::time::sleep(DWELL).await;
    speak(&mut socket).await;
    assert_eq!(update(&mut socket, "heard").await["text"], "allow once");
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(place.runs("published.txt"), 0, "voice never allows a high-risk call");
    let card = api.get(&format!("/api/surface/decisions/{release}?surface=web")).await;
    assert_eq!(card["riskText"], "High risk", "{card}");
    assert!(card["decided"].is_null(), "voice decided nothing: {card}");
    let labels: Vec<&str> = card["allowed"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|option| option["label"].as_str())
        .collect();
    assert!(labels.contains(&"Confirm it's you and approve"), "{labels:?}");

    // 4. On screen: "Confirm it's you" in the web home runs it once, on the conversation.
    let outcome = api.confirm(&mut passkey, &card).await;
    assert_eq!(outcome["type"], "answered", "{outcome}");
    eventually("the post", WAIT, || async { (place.runs("published.txt") > 0).then_some(()) })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(place.runs("published.txt"), 1, "the post ran exactly once");
    let card = api.get(&format!("/api/surface/decisions/{release}?surface=web")).await;
    assert!(
        card["decided"]["text"].as_str().is_some_and(|text| text.starts_with("You said yes")),
        "{card}"
    );

    // 5. Stop, with work in flight: a call waiting in one thread, a turn running in another,
    // and the voice call open.
    api.command(
        json!({ "type": "send", "sessionId": "hold-1", "text": "Remind the client about INV-7." }),
    )
    .await;
    let held = eventually("the waiting call", WAIT, || async {
        api.decisions().await.into_iter().find(|card| card["sessionId"] == "hold-1")
    })
    .await;
    api.command(json!({ "type": "send", "sessionId": "work-1", "text": "Tidy the build folder." }))
        .await;
    let started = Instant::now();
    while place.runs("ticks.txt") < 3 {
        if started.elapsed() > WAIT {
            panic!(
                "the running turn took no steps: {:?} {:?}",
                api.run_of("work-1").await,
                api.decisions().await
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        api.run_of("work-1").await.is_some_and(|run| run["state"] == "running"),
        "the turn runs"
    );

    // Stop all from the tray: `adk-desktop-core::stop_all`'s request.
    let stopped_at = Instant::now();
    let (status, stopped) = api
        .post(
            "/api/surface/commands",
            json!({ "surface": "tray", "origin": { "kind": "pointer" }, "command": { "type": "stopAll" } }),
        )
        .await;
    assert_eq!(status, 200, "{stopped}");
    let ticks_at_stop = place.runs("ticks.txt");
    // The voice call ends at once.
    let error = update(&mut socket, "error").await;
    assert_eq!(error["code"], "surface.stopped", "{error}");
    update(&mut socket, "ended").await;
    let voice_ended = stopped_at.elapsed();
    // The tray shows everything stopped on the next event.
    let glance = tray
        .shows(stopped_at, Duration::from_secs(3), |status| status["stopped"] == true)
        .await
        .unwrap_or_else(|| panic!("the tray never showed Stop all on a kernel event"));
    let tray_after = glance.at.duration_since(stopped_at);
    // The running turn takes at most the step it was in, then parks.
    let parked = eventually("the running turn parked", Duration::from_secs(5), || async {
        api.run_of("work-1")
            .await
            .filter(|run| run["state"] != "running")
            .map(|_| stopped_at.elapsed())
    })
    .await;
    let ticks_after = place.runs("ticks.txt");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(place.runs("ticks.txt"), ticks_after, "the turn took no step after it parked");
    assert!(
        ticks_after <= ticks_at_stop + 1,
        "at most the step in flight finished: {ticks_at_stop} → {ticks_after}"
    );
    // The waiting call, allowed after the stop, does not run.
    let answered = api
        .command(json!({
            "type": "decide",
            "decisionId": held["id"],
            "version": held["version"],
            "answer": "allowOnce",
            "expanded": true
        }))
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !place.ws.path().join("held.txt").exists(),
        "the waiting call ran after Stop all: {answered}"
    );
    assert!(
        api.run_of("hold-1")
            .await
            .is_some_and(|run| run["state"] != "running" && run["state"] != "completed"),
        "the waiting call's turn stays stopped"
    );
    assert_eq!(place.runs("published.txt"), 1);
    assert_eq!(place.runs("sent.txt"), 1);
    println!(
        "after Stop all: the voice call ended in {voice_ended:?}, the tray showed it in \
         {tray_after:?}, the running turn parked in {parked:?} ({ticks_at_stop} → {ticks_after} steps)"
    );
    for (what, took) in
        [("the voice call", voice_ended), ("the tray", tray_after), ("the running turn", parked)]
    {
        assert!(took <= ABOUT_A_SECOND, "{what} took {took:?} to stop");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn speak_glance_stop_on_sqlite() {
    speak_glance_stop(None).await;
}

/// The same on PostgreSQL, in a database of its own.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL, a PostgreSQL server; run by hand"]
async fn speak_glance_stop_on_postgres() {
    let server = std::env::var("DATABASE_URL").expect("DATABASE_URL names a PostgreSQL server");
    let admin = sqlx::PgPool::connect(&server).await.unwrap();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // A name of digits and underscores only, so it needs no quoting.
    let database = format!("speak_glance_stop_{nanos}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    let (base, query) = server.split_once('?').unwrap_or((&server, ""));
    let host = base.find("://").map_or(0, |at| at + 3);
    let root = base[host..].find('/').map_or(base, |at| &base[..host + at]);
    let url = match query {
        "" => format!("{root}/{database}"),
        query => format!("{root}/{database}?{query}"),
    };
    speak_glance_stop(Some(&url)).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)")))
        .execute(&admin)
        .await
        .unwrap();
}
