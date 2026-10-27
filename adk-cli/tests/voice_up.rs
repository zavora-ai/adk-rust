//! `adk-rust up --pack voice` (spec 3.3 §5.6): the served harness talks in the home's voice
//! view. A scripted realtime provider (`--voice-script`) plays the conversation through the
//! voice socket: the model's `bash` is held and prompted, a spoken "allow once" counts down,
//! the call runs once, and the card reads "You said yes by voice".
//!
//! The binary runs as a process with a scripted model on SQLite (`harness.db`). The
//! PostgreSQL and Redis variants are `#[ignore]`d and need `DATABASE_URL` or `REDIS_URL`; the
//! PostgreSQL one runs in a database of its own and in `scripts/test-postgres.sh`:
//!
//! ```sh
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//! REDIS_URL=redis://localhost:6379 cargo nextest run -p adk-cli \
//!   --features voice,scripted-model,postgres,redis --test voice_up --run-ignored only
//! ```

#![cfg(all(
    unix,
    feature = "scripted-model",
    feature = "voice",
    feature = "surface",
    feature = "persist"
))]

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use adk_realtime::ServerEvent;
use adk_realtime::scripted::{self, ScriptedReply, ScriptedStep};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const REPLY: &str = r#"[{"text": "Hello."}]"#;
/// What the model runs once the person allows it: a mark in the workspace per run.
const SEND: &str = "printf x >> sent.txt";
/// The binary frame tag of microphone audio on the voice socket.
const FRAME_AUDIO: u8 = 0x01;
/// How long a prompt shows before a spoken answer applies to it, with a margin.
const DWELL: Duration = Duration::from_millis(1_300);
/// How long the test waits for one update: the cancel window is five seconds.
const WAIT: Duration = Duration::from_secs(30);

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

fn speech_started() -> ServerEvent {
    ServerEvent::SpeechStarted { event_id: "speech".into(), audio_start_ms: 0 }
}

/// The person asks, the model runs `bash` and says the call waits; the person then says
/// "allow once".
fn script() -> Vec<ScriptedStep> {
    vec![
        ScriptedReply::new(
            [
                vec![speech_started(), scripted::input_transcript("send the invoice")],
                scripted::calls("call-1", "bash", json!({ "command": SEND })),
            ]
            .concat(),
        )
        .on_audio(),
        ScriptedReply::new(scripted::spoken("That needs your answer.")).on_tool_output(),
        ScriptedReply::new(vec![speech_started(), scripted::input_transcript("allow once")])
            .on_audio(),
    ]
}

/// `adk-rust up --pack voice` on `store` (default: `harness.db` in the kernel home); returns
/// once it listens.
fn up(home: &Path, ws: &Path, port: u16, store: Option<&str>) -> Up {
    let script_path = ws.join("voice-script.json");
    std::fs::write(&script_path, serde_json::to_string(&script()).unwrap()).unwrap();
    let kernel = home.join("kernel");
    let (kernel, dir, bind, script_path) = (
        kernel.display().to_string(),
        ws.display().to_string(),
        format!("127.0.0.1:{port}"),
        script_path.display().to_string(),
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
        .args(["--model-script", REPLY, "--mode", "ask", "--pack", "voice"])
        .args(["--voice-script", &script_path])
        .current_dir(ws)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(url) = store {
        command.args(["--store", url]);
    }
    common::isolate(&mut command, home);
    command
        .env_remove("TELEGRAM_BOT_TOKEN")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENAI_API_KEY");
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

/// The next update on `socket` whose `type` is `kind`.
async fn update(socket: &mut Socket, kind: &str) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
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

/// 20 ms of microphone silence: the scripted provider takes its next step on audio.
async fn speak(socket: &mut Socket) {
    let mut frame = vec![FRAME_AUDIO];
    frame.extend_from_slice(&[0; 640]);
    socket.send(Message::Binary(frame.into())).await.unwrap();
}

/// The marks the allowed command left.
fn sent(ws: &Path) -> usize {
    std::fs::read_to_string(ws.join("sent.txt")).map_or(0, |text| text.len())
}

/// The scenario's first step, by voice, on the store `store` names.
async fn a_spoken_allow_once_runs_the_call_once(store: Option<&str>) {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    let port = free_port();
    let _up = up(home.path(), ws.path(), port, store);
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // The home offers the scripted model, and a ticket opens the socket.
    let models: Value =
        client.get(format!("{base}/api/surface/voice")).send().await.unwrap().json().await.unwrap();
    assert_eq!(models["models"][0]["id"], "scripted", "{models}");
    let ticket: Value = client
        .post(format!("{base}/api/surface/voice/sessions"))
        .json(&json!({ "model": "scripted" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let socket_path = ticket["socket"].as_str().expect("a socket path");
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}{socket_path}"))
            .await
            .unwrap();
    let hello = json!({ "type": "hello", "ticket": ticket["ticket"] }).to_string();
    socket.send(Message::Text(hello.into())).await.unwrap();
    update(&mut socket, "ready").await;

    // The person asks; the model's `bash` is held and prompted.
    speak(&mut socket).await;
    let prompted = loop {
        let prompted = update(&mut socket, "prompted").await;
        if prompted["decisionId"].is_string() {
            break prompted;
        }
    };
    let id = prompted["decisionId"].as_str().unwrap().to_string();

    // "Allow once" counts down; nothing runs before the window ends.
    tokio::time::sleep(DWELL).await;
    speak(&mut socket).await;
    let heard = update(&mut socket, "heard").await;
    assert_eq!(heard["text"], "allow once", "{heard}");
    let pending = update(&mut socket, "pending").await;
    assert_eq!(pending["decisionId"], id.as_str(), "{pending}");
    assert_eq!(sent(ws.path()), 0);
    let outcome = update(&mut socket, "outcome").await;
    assert_eq!(outcome["decided"], true, "{outcome}");

    // The call runs once, and the card says how it was answered.
    let deadline = Instant::now() + WAIT;
    while sent(ws.path()) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(sent(ws.path()), 1);
    let card: Value = client
        .get(format!("{base}/api/surface/decisions/{id}?surface=web"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let said = card["decided"]["text"].as_str().unwrap_or_default();
    assert!(said.starts_with("You said yes by voice at "), "{card}");
    socket.close(None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spoken_allow_once_on_sqlite() {
    a_spoken_allow_once_runs_the_call_once(None).await;
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL: a PostgreSQL server"]
async fn a_spoken_allow_once_on_postgres() {
    let server = std::env::var("DATABASE_URL").expect("DATABASE_URL names a PostgreSQL server");
    let admin = sqlx::PgPool::connect(&server).await.unwrap();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // A name of digits and underscores only, so it needs no quoting.
    let database = format!("voice_up_{nanos}");
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
    a_spoken_allow_once_runs_the_call_once(Some(&url)).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)")))
        .execute(&admin)
        .await
        .unwrap();
}

#[cfg(feature = "redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs REDIS_URL: a Redis server"]
async fn a_spoken_allow_once_on_redis() {
    let url = std::env::var("REDIS_URL").expect("REDIS_URL names a Redis server");
    a_spoken_allow_once_runs_the_call_once(Some(&url)).await;
}
