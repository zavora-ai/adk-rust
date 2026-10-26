//! The tray against a stub kernel on a loopback socket (spec 3.3 §5.5): the status it reads,
//! a `401` as sign-in, Stop all's request, and a re-read on every kernel event.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_desktop_core::{Badge, DesktopError, Reach, ServerTarget, StatusWatcher, stop_all};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// One request the stub saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Value,
}

/// What the stub answers.
enum Answer {
    Json(u16, Value),
    /// An event stream: one frame per message on the channel, held open until it closes.
    Events(mpsc::UnboundedReceiver<()>),
}

type Route = Arc<dyn Fn(&Seen) -> Answer + Send + Sync>;

/// A kernel stub: every request is recorded and answered by `route`.
async fn stub(route: Route) -> (ServerTarget, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else { return };
            let (route, log) = (Arc::clone(&route), Arc::clone(&log));
            tokio::spawn(async move { serve(socket, route, log).await });
        }
    });
    let target =
        ServerTarget::resolve(Some(&format!("http://127.0.0.1:{port}")), None, None).unwrap();
    (target, seen)
}

async fn serve(mut socket: TcpStream, route: Route, log: Arc<Mutex<Vec<Seen>>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        let Ok(read) = socket.read(&mut chunk).await else { return };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|four| four == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (
        request.next().unwrap_or_default().to_owned(),
        request.next().unwrap_or_default().to_owned(),
    );
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
        })
    };
    let length: usize = header("content-length").and_then(|value| value.parse().ok()).unwrap_or(0);
    while buffer.len() < head_end + length {
        let Ok(read) = socket.read(&mut chunk).await else { return };
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = serde_json::from_slice(&buffer[head_end..]).unwrap_or(Value::Null);
    let seen = Seen { method, path, authorization: header("authorization"), body };
    log.lock().unwrap().push(seen.clone());
    match route(&seen) {
        Answer::Json(status, value) => {
            let text = value.to_string();
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        }
        Answer::Events(mut frames) => {
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let mut id = 0;
            while frames.recv().await.is_some() {
                id += 1;
                let frame = format!("id: {id}\nevent: process\ndata: {{}}\n\n");
                if socket.write_all(frame.as_bytes()).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn status(decisions: u32) -> Value {
    json!({
        "decisions": decisions,
        "stopped": false,
        "spend": { "spentMicroUsd": 0, "limitMicroUsd": 5_000_000, "text": "$0.00 of $5.00 today" },
        "quiet": { "active": false, "start": "22:00", "end": "07:00", "text": "Quiet from 22:00" },
        "someNewField": true,
    })
}

#[tokio::test]
async fn a_kernel_s_status_line_is_read_for_the_tray() {
    let (target, seen) = stub(Arc::new(|request: &Seen| match request.path.as_str() {
        "/api/kernel/health" => Answer::Json(200, json!({ "state": "running", "frozen": false })),
        _ => Answer::Json(200, status(2)),
    }))
    .await;
    let reach = StatusWatcher::new(target).unwrap().read().await;
    assert_eq!(reach.badge(), Badge::Count(2));
    let paths: Vec<String> =
        seen.lock().unwrap().iter().map(|request| request.path.clone()).collect();
    assert_eq!(paths, ["/api/kernel/health", "/api/surface/status?surface=tray"]);
}

#[tokio::test]
async fn a_401_means_sign_in() {
    let (target, _) = stub(Arc::new(|_: &Seen| {
        Answer::Json(401, json!({ "error": { "message": "needs a token" } }))
    }))
    .await;
    assert_eq!(StatusWatcher::new(target.clone()).unwrap().read().await, Reach::SignIn);
    let refused = stop_all(&target).await.unwrap_err();
    assert!(matches!(refused, DesktopError::SignIn { .. }), "{refused}");
    assert!(refused.to_string().contains("--token"), "{refused}");

    // The status route alone asking for the token is sign-in too.
    let (target, _) = stub(Arc::new(|request: &Seen| match request.path.as_str() {
        "/api/kernel/health" => Answer::Json(200, json!({})),
        _ => Answer::Json(401, json!({})),
    }))
    .await;
    assert_eq!(StatusWatcher::new(target).unwrap().read().await, Reach::SignIn);
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let target = ServerTarget::resolve(Some(&format!("127.0.0.1:{port}")), None, None).unwrap();
    assert_eq!(StatusWatcher::new(target.clone()).unwrap().read().await, Reach::Unreachable);
    let error = stop_all(&target).await.unwrap_err();
    assert!(matches!(error, DesktopError::Unreachable { .. }), "{error}");
    assert!(error.to_string().contains("adk-rust up"), "{error}");
}

#[tokio::test]
async fn stop_all_sends_the_tray_s_command_with_the_token() {
    let (target, seen) = stub(Arc::new(|_: &Seen| {
        Answer::Json(200, json!({ "type": "stopped", "text": "All agents stopped" }))
    }))
    .await;
    let target =
        ServerTarget::resolve(Some(target.origin().as_str()), Some("t0ken"), None).unwrap();
    stop_all(&target).await.unwrap();
    let request = seen.lock().unwrap()[0].clone();
    assert_eq!((request.method.as_str(), request.path.as_str()), ("POST", "/api/surface/commands"));
    assert_eq!(request.authorization.as_deref(), Some("Bearer t0ken"));
    assert_eq!(
        request.body,
        json!({ "surface": "tray", "origin": { "kind": "pointer" }, "command": { "type": "stopAll" } })
    );
}

#[tokio::test]
async fn a_refusal_carries_the_kernel_s_problem() {
    let (target, _) = stub(Arc::new(|_: &Seen| {
        Answer::Json(403, json!({ "error": { "code": "surface.operator_only", "message": "stopping every agent needs the operator" } }))
    }))
    .await;
    match stop_all(&target).await.unwrap_err() {
        DesktopError::Refused { status, code, message } => {
            assert_eq!((status, code.as_deref()), (403, Some("surface.operator_only")));
            assert_eq!(message, "stopping every agent needs the operator");
        }
        other => panic!("{other}"),
    }
}

#[tokio::test]
async fn every_kernel_event_reads_the_status_again() {
    let decisions = Arc::new(Mutex::new(2_u32));
    let (frames_tx, frames_rx) = mpsc::unbounded_channel();
    let frames_rx = Arc::new(Mutex::new(Some(frames_rx)));
    let count = Arc::clone(&decisions);
    let (target, _) = stub(Arc::new(move |request: &Seen| match request.path.as_str() {
        "/api/kernel/health" => Answer::Json(200, json!({})),
        "/api/kernel/events" => match frames_rx.lock().unwrap().take() {
            Some(frames) => Answer::Events(frames),
            None => Answer::Json(503, json!({})),
        },
        _ => Answer::Json(200, status(*count.lock().unwrap())),
    }))
    .await;
    // Reads every minute at most: only an event makes the second read.
    let watcher = StatusWatcher::new(target).unwrap().every(Duration::from_secs(60));
    let (seen_tx, mut seen_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move {
        watcher.run(move |reach| seen_tx.send(reach.badge()).unwrap()).await
    });

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), seen_rx.recv()).await.unwrap(),
        Some(Badge::Count(2))
    );
    *decisions.lock().unwrap() = 1;
    // The stream may still be connecting: send frames until the read follows.
    let mut next = None;
    for _ in 0..50 {
        frames_tx.send(()).unwrap();
        if let Ok(badge) = tokio::time::timeout(Duration::from_millis(100), seen_rx.recv()).await {
            next = badge;
            break;
        }
    }
    assert_eq!(next, Some(Badge::Count(1)));
    running.abort();
}

#[tokio::test]
async fn a_status_read_the_kernel_fails_is_not_the_kernel_being_away() {
    for answer in [
        Answer::Json(500, json!({ "error": { "message": "store failed" } })),
        Answer::Json(200, json!("not a status")),
    ] {
        let answer = Arc::new(Mutex::new(Some(answer)));
        let (target, _) = stub(Arc::new(move |request: &Seen| match request.path.as_str() {
            "/api/kernel/health" => Answer::Json(200, json!({})),
            _ => answer.lock().unwrap().take().unwrap_or(Answer::Json(500, json!({}))),
        }))
        .await;
        assert_eq!(StatusWatcher::new(target).unwrap().read().await, Reach::Unread);
    }
}

#[tokio::test]
async fn one_failed_status_read_keeps_the_last_status() {
    // 2 waiting, then a 500, then 1 waiting.
    let answers = Arc::new(Mutex::new(vec![Some(2_u32), None]));
    let (frames_tx, frames_rx) = mpsc::unbounded_channel();
    let frames_rx = Arc::new(Mutex::new(Some(frames_rx)));
    let next = Arc::clone(&answers);
    let (target, seen) = stub(Arc::new(move |request: &Seen| match request.path.as_str() {
        "/api/kernel/health" => Answer::Json(200, json!({})),
        "/api/kernel/events" => match frames_rx.lock().unwrap().take() {
            Some(frames) => Answer::Events(frames),
            None => Answer::Json(503, json!({})),
        },
        _ => {
            let mut answers = next.lock().unwrap();
            let answer = if answers.len() > 1 { answers.remove(0) } else { answers[0] };
            match answer {
                Some(decisions) => Answer::Json(200, status(decisions)),
                None => Answer::Json(500, json!({ "error": { "message": "store failed" } })),
            }
        }
    }))
    .await;
    let watcher = StatusWatcher::new(target).unwrap().every(Duration::from_secs(60));
    let (seen_tx, mut seen_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move {
        watcher.run(move |reach| seen_tx.send(reach.clone()).unwrap()).await
    });
    let first = tokio::time::timeout(Duration::from_secs(5), seen_rx.recv()).await.unwrap();
    assert_eq!(first.map(|reach| reach.badge()), Some(Badge::Count(2)));

    // Events until the 500 has been read: it calls nothing.
    let reads = |seen: &Arc<Mutex<Vec<Seen>>>| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|request| request.path.starts_with("/api/surface/status"))
            .count()
    };
    for _ in 0..50 {
        if reads(&seen) >= 2 {
            break;
        }
        frames_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(reads(&seen) >= 2, "the failed read never happened");
    *answers.lock().unwrap() = vec![Some(1)];
    let mut after = None;
    for _ in 0..50 {
        frames_tx.send(()).unwrap();
        if let Ok(reach) = tokio::time::timeout(Duration::from_millis(100), seen_rx.recv()).await {
            after = reach;
            break;
        }
    }
    assert_eq!(after.map(|reach| reach.badge()), Some(Badge::Count(1)));
    running.abort();
}
