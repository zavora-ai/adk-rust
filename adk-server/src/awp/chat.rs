//! `GET /awp/v2/chat`: the on-site assistant over a WebSocket (spec 3.5 §7.9, §7.10).
//!
//! Each socket is one thread with the site's public agent. The first frame,
//! `{"type": "ready", "thread": ..., "subject": ...}`, carries the visitor's token; sending it
//! back as `?subject=` on the next socket continues as the same visitor. The visitor sends text
//! frames — plain text or `{"text": "..."}` — and receives
//! `{"type": "reply", "thread": ..., "text": ...}` for each answer, and for each `reply_in_thread` the agent sends. A frame over 4 KiB, more
//! than 20 messages a minute, or ten minutes of silence closes the socket.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use adk_auth::VisitorTokens;

use super::control::EdgeControl;
use super::public::{LiveThreads, PublicPartition};
use super::tiers::EdgeCaller;

/// The chat socket's path.
pub const CHAT_PATH: &str = "/awp/v2/chat";

const MAX_FRAME: usize = 4 * 1024;
const MAX_PER_MINUTE: usize = 20;
const IDLE: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub(crate) struct ChatState {
    pub(crate) public: Arc<PublicPartition>,
    pub(crate) live: LiveThreads,
    pub(crate) control: Arc<EdgeControl>,
    pub(crate) tokens: VisitorTokens,
}

pub(crate) async fn chat(
    State(state): State<ChatState>,
    caller: Option<Extension<EdgeCaller>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let caller =
        caller.map_or_else(|| EdgeCaller::public("ip:unknown"), |Extension(caller)| caller);
    upgrade.max_message_size(MAX_FRAME).on_upgrade(move |socket| serve(state, caller, socket))
}

fn frame(kind: &str, thread: &str, text: &str) -> Message {
    Message::Text(json!({ "type": kind, "thread": thread, "text": text }).to_string().into())
}

async fn serve(state: ChatState, caller: EdgeCaller, socket: WebSocket) {
    let thread = format!("chat-{}", uuid::Uuid::new_v4().simple());
    let (mut sink, mut stream) = socket.split();
    let (sender, mut replies) = mpsc::channel::<String>(16);
    state.live.open(&thread, sender);
    let mut sent: Vec<Instant> = Vec::new();
    // A browser cannot read the upgrade's headers, so the visitor's token rides the first
    // frame; the page sends it back as `?subject=` to continue as the same visitor.
    let mut ready = json!({ "type": "ready", "thread": thread, "text": "" });
    if let Some(visitor) = &caller.visitor {
        ready["subject"] = json!(state.tokens.issue(visitor));
    }
    let _ = sink.send(Message::Text(ready.to_string().into())).await;
    loop {
        let next = tokio::select! {
            reply = replies.recv() => {
                if let Some(reply) = reply
                    && sink.send(frame("reply", &thread, &reply)).await.is_err()
                {
                    break;
                }
                continue;
            }
            message = tokio::time::timeout(IDLE, stream.next()) => message,
        };
        let text = match next {
            Ok(Some(Ok(Message::Text(text)))) => text.to_string(),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(_)) | Some(Err(_)) | None) | Err(_) => break,
        };
        let text = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value.get("text").and_then(|text| text.as_str()).map(str::to_owned))
            .unwrap_or(text);
        let now = Instant::now();
        sent.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        if sent.len() >= MAX_PER_MINUTE {
            let _ = sink.send(frame("error", &thread, "too many messages; wait a minute")).await;
            continue;
        }
        sent.push(now);
        if state.control.is_read_only() {
            let _ = sink.send(frame("error", &thread, "the site is read-only")).await;
            continue;
        }
        let governance = state.control.governance().cloned();
        let answer =
            state.public.converse(&caller, "chat", &thread, &thread, &text, &[], governance).await;
        // Replies the agent sent with `reply_in_thread` during the turn come first.
        while let Ok(reply) = replies.try_recv() {
            let _ = sink.send(frame("reply", &thread, &reply)).await;
        }
        let sent_ok = match answer {
            Ok(reply) if !reply.is_empty() => sink.send(frame("reply", &thread, &reply)).await,
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "awp chat turn failed");
                sink.send(frame("error", &thread, "the assistant could not answer")).await
            }
        };
        if sent_ok.is_err() {
            break;
        }
    }
    state.live.close(&thread);
}
