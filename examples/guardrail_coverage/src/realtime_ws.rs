//! A scripted realtime WebSocket server on 127.0.0.1 speaking the OpenAI Realtime event
//! protocol.
//!
//! The real `OpenAIRealtimeModel` connects to it through its base URL. On the first
//! `response.create` the server calls one tool; once the tool's `function_call_output` and the
//! next `response.create` arrive, it answers with text and closes the connection. Every client
//! frame is recorded, so a scenario can read exactly what the model was sent for the tool call.

use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// The call the scripted model makes.
#[derive(Clone)]
pub struct ScriptedToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

/// A running scripted realtime server.
pub struct RealtimeServer {
    pub url: String,
    frames: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl RealtimeServer {
    pub async fn start(call: ScriptedToolCall) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/v1/realtime", listener.local_addr()?);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let recorded = frames.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let recorded = recorded.clone();
                let call = call.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, call, recorded).await;
                });
            }
        });
        Ok(Self { url, frames, task })
    }

    /// Every client frame received so far, in order.
    pub fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }

    /// The `output` of every `function_call_output` item the client sent.
    pub fn tool_outputs(&self) -> Vec<String> {
        self.frames()
            .iter()
            .filter(|frame| frame["type"] == "conversation.item.create")
            .filter(|frame| frame["item"]["type"] == "function_call_output")
            .filter_map(|frame| frame["item"]["output"].as_str().map(str::to_string))
            .collect()
    }
}

impl Drop for RealtimeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    stream: tokio::net::TcpStream,
    call: ScriptedToolCall,
    recorded: Arc<Mutex<Vec<Value>>>,
) -> anyhow::Result<()> {
    let mut ws = tokio_tungstenite::accept_async(stream).await?;
    let mut responses = 0;
    while let Some(message) = ws.next().await {
        let Message::Text(text) = message? else { continue };
        let frame: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        recorded.lock().unwrap().push(frame.clone());
        if frame["type"] != "response.create" {
            continue;
        }
        responses += 1;
        let response_id = format!("resp_{responses}");
        if responses == 1 {
            send(
                &mut ws,
                json!({
                    "type": "response.function_call_arguments.done",
                    "event_id": "evt_call",
                    "response_id": response_id,
                    "item_id": "item_call",
                    "output_index": 0,
                    "call_id": call.call_id,
                    "name": call.name,
                    "arguments": call.arguments,
                }),
            )
            .await?;
            send(&mut ws, response_done(&response_id)).await?;
        } else {
            send(
                &mut ws,
                json!({
                    "type": "response.output_text.done",
                    "event_id": "evt_text",
                    "response_id": response_id,
                    "item_id": "item_text",
                    "output_index": 0,
                    "content_index": 0,
                    "text": "All set.",
                }),
            )
            .await?;
            send(&mut ws, response_done(&response_id)).await?;
            ws.close(None).await?;
            break;
        }
    }
    Ok(())
}

fn response_done(response_id: &str) -> Value {
    json!({
        "type": "response.done",
        "event_id": format!("evt_done_{response_id}"),
        "response": {"id": response_id, "status": "completed"},
    })
}

async fn send<S>(ws: &mut S, frame: Value) -> anyhow::Result<()>
where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    ws.send(Message::Text(frame.to_string().into())).await?;
    Ok(())
}
