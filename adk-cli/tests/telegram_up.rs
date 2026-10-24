//! `adk-rust up` with `TELEGRAM_BOT_TOKEN` (spec 3.3 §5.3): the bot starts with the kernel,
//! a stranger gets a pairing code, `adk-rust pair accept` pairs the account, the kernel's
//! routes list it for the home's "Where you can say yes from", a held write reaches Telegram
//! headed in plain words, and Allow once lets it run. Without the token nothing of it starts
//! and nothing mentions Telegram.
//!
//! The binary runs as a process with a scripted model, against a fake Telegram Bot API on
//! loopback (`TELEGRAM_API_BASE`).

#![cfg(all(unix, feature = "scripted-model", feature = "persist", feature = "channels"))]

mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, State};
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

const TOKEN: &str = "777000:AAFakeBotForUp_tests";
const ANA: i64 = 4217;
const STRANGER: i64 = 666;
const WRITE_THEN_REPLY: &str = r#"[
    {"call": {"name": "write_file", "args": {"path": "invoice.txt", "content": "INV-1"}}},
    {"text": "The invoice is written."}
]"#;

/// A fake Bot API: keeps every call, serves queued updates to `getUpdates`.
#[derive(Clone, Default)]
struct Fake {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    updates: Arc<Mutex<Vec<Value>>>,
    next: Arc<Mutex<i64>>,
}

impl Fake {
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn texts(&self, chat: i64) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|(method, body)| method == "sendMessage" && body["chat_id"] == chat)
            .filter_map(|(_, body)| body["text"].as_str().map(str::to_string))
            .collect()
    }

    fn deliver(&self, update: Value) {
        let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        *next += 1;
        let mut update = update;
        update["update_id"] = json!(*next);
        self.updates.lock().unwrap_or_else(PoisonError::into_inner).push(update);
    }

    fn say(&self, user: i64, text: &str) {
        let id = *self.next.lock().unwrap_or_else(PoisonError::into_inner) + 1000;
        self.deliver(json!({ "message": {
            "message_id": id,
            "from": { "id": user, "is_bot": false, "first_name": "Ana" },
            "chat": { "id": user, "type": "private" },
            "date": 0,
            "text": text,
        }}));
    }

    /// Presses `label` on the last card sent to `chat`.
    fn press(&self, chat: i64, label: &str) -> bool {
        let card = self.calls().into_iter().rev().find(|(method, body)| {
            method == "sendMessage"
                && body["chat_id"] == chat
                && body["text"].as_str().is_some_and(|text| text.starts_with("Waiting for you"))
        });
        let Some((_, card)) = card else { return false };
        let data = card["reply_markup"]["inline_keyboard"][0]
            .as_array()
            .into_iter()
            .flatten()
            .find(|button| button["text"] == label)
            .map(|button| button["callback_data"].clone());
        let Some(data) = data else { return false };
        self.deliver(json!({ "callback_query": {
            "id": "cb1",
            "from": { "id": chat, "is_bot": false, "first_name": "Ana" },
            "message": { "message_id": 1, "chat": { "id": chat, "type": "private" }, "date": 0 },
            "data": data,
        }}));
        true
    }
}

async fn api(
    State(fake): State<Fake>,
    UrlPath((bot, method)): UrlPath<(String, String)>,
    body: Option<Json<Value>>,
) -> Json<Value> {
    if bot != format!("bot{TOKEN}") {
        return Json(json!({ "ok": false, "error_code": 401, "description": "Unauthorized" }));
    }
    let body = body.map(|Json(body)| body).unwrap_or(Value::Null);
    if method != "getUpdates" {
        fake.calls.lock().unwrap_or_else(PoisonError::into_inner).push((method.clone(), body));
        let result = match method.as_str() {
            "sendMessage" => {
                json!({ "message_id": 1, "chat": { "id": 1, "type": "private" }, "date": 0 })
            }
            _ => json!(true),
        };
        return Json(json!({ "ok": true, "result": result }));
    }
    let offset = body["offset"].as_i64().unwrap_or(0);
    let started = Instant::now();
    loop {
        let ready: Vec<Value> = {
            let mut updates = fake.updates.lock().unwrap_or_else(PoisonError::into_inner);
            updates.retain(|update| update["update_id"].as_i64().unwrap_or(0) >= offset);
            updates.clone()
        };
        if !ready.is_empty() || started.elapsed() > Duration::from_millis(500) {
            return Json(json!({ "ok": true, "result": ready }));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn fake_api() -> (Fake, SocketAddr) {
    let fake = Fake::default();
    let app = Router::new().route("/{bot}/{method}", post(api)).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, addr)
}

/// `adk-rust up` at `kernel` on `port`, with `env`; returns once it listens.
fn up(home: &Path, kernel: &Path, ws: &Path, port: u16, env: &[(&str, &str)]) -> Child {
    let (kernel, dir, bind) =
        (kernel.display().to_string(), ws.display().to_string(), format!("127.0.0.1:{port}"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["up", "--home", &kernel, "--bind", &bind, "--dir", &dir])
        .args(["--model-script", WRITE_THEN_REPLY, "--mode", "ask"])
        .current_dir(ws)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::isolate(&mut command, home);
    command.env_remove("TELEGRAM_BOT_TOKEN").env_remove("TELEGRAM_API_BASE");
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("adk-rust starts");
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if child.try_wait().unwrap().is_some() || started.elapsed() > common::TIMEOUT {
            let _ = child.kill();
            panic!("up did not listen:\n{}", common::text(&child.wait_with_output().unwrap()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child
}

/// Waits up to a minute for `probe`.
async fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(started.elapsed() < common::TIMEOUT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
async fn up_starts_the_telegram_bot_and_a_paired_account_allows_a_write_once() {
    let (fake, addr) = fake_api().await;
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let base = format!("http://{addr}");
    let child = up(
        home.path(),
        &kernel,
        ws.path(),
        port,
        &[("TELEGRAM_BOT_TOKEN", TOKEN), ("TELEGRAM_API_BASE", &base)],
    );
    let kernel_arg = kernel.display().to_string();

    // The bot lists "Stop all"; a stranger gets a code and starts nothing.
    eventually("the bot's commands", || {
        fake.calls().iter().any(|(method, _)| method == "setMyCommands").then_some(())
    })
    .await;
    fake.say(STRANGER, "hello");
    eventually("the stranger's code", || {
        fake.texts(STRANGER).into_iter().find(|text| text.contains("pair accept"))
    })
    .await;

    // Ana pairs from the kernel's machine.
    fake.say(ANA, "/start");
    let message = eventually("Ana's code", || {
        fake.texts(ANA).into_iter().find(|text| text.contains("pair accept"))
    })
    .await;
    let code: String = message
        .split("adk-rust pair accept ")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let accepted =
        common::run(&["pair", "--home", &kernel_arg, "accept", &code], home.path(), ws.path());
    assert!(accepted.status.success(), "{}", common::text(&accepted));
    let listed = common::run(&["pair", "--home", &kernel_arg, "list"], home.path(), ws.path());
    assert!(common::text(&listed).contains("(telegram)"), "{}", common::text(&listed));
    // The home's "Where you can say yes from" reads the kernel's chat accounts.
    let accounts: Value = reqwest::get(format!("http://127.0.0.1:{port}/api/kernel/channels"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accounts["accounts"][0]["channel"], "telegram", "{accounts}");
    assert_eq!(accounts["accounts"][0]["state"], "active", "{accounts}");

    // A held write reaches the thread headed in plain words, without its arguments.
    fake.say(ANA, "Write the October invoice");
    let card = eventually("the decision card", || {
        fake.texts(ANA).into_iter().find(|text| text.starts_with("Waiting for you"))
    })
    .await;
    assert!(card.contains("<b>Write file</b>"), "{card}");
    assert!(!card.contains("requires confirmation") && !card.contains("invoice.txt"), "{card}");
    assert!(card.contains(&format!("<a href=\"http://127.0.0.1:{port}/ui\">")), "{card}");
    assert!(!ws.path().join("invoice.txt").exists(), "held, not written");

    // Allow once: the write runs once and the reply comes back.
    assert!(fake.press(ANA, "Allow once"));
    eventually("the reply", || {
        fake.texts(ANA).into_iter().find(|text| text == "The invoice is written.")
    })
    .await;
    assert_eq!(std::fs::read_to_string(ws.path().join("invoice.txt")).unwrap(), "INV-1");
    for (method, body) in fake.calls() {
        assert!(!body.to_string().contains("INV-1"), "an argument reached Telegram in {method}");
    }

    let mut child = child;
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    assert!(common::text(&output).contains("chat  Telegram"), "{}", common::text(&output));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_token_up_starts_nothing_of_telegram() {
    let (fake, addr) = fake_api().await;
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let kernel = home.path().join("kernel");
    let port = free_port();
    let base = format!("http://{addr}");
    let mut child = up(
        home.path(),
        &kernel,
        ws.path(),
        port,
        &[("TELEGRAM_API_BASE", &base), ("RUST_LOG", "debug")],
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = child.kill();
    let output = common::text(&child.wait_with_output().unwrap());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert!(!output.to_lowercase().contains("telegram"), "{output}");
}
