//! A scripted Gemini `generateContent` server on 127.0.0.1.
//!
//! The real `GeminiModel` is pointed at it through `GeminiModel::new_with_base_url`, so the
//! scenario sees the exact function declarations the Gemini client puts on the wire.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

struct Shared {
    requests: Mutex<Vec<Value>>,
}

/// A running scripted Gemini server that answers every request with one text part.
pub struct GeminiUpstream {
    pub base_url: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl GeminiUpstream {
    pub async fn start() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared { requests: Mutex::new(Vec::new()) });
        let app = Router::new().fallback(generate).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1beta/", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url, shared, task })
    }

    /// Every request body received so far, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.shared.requests.lock().unwrap().clone()
    }
}

impl Drop for GeminiUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn generate(
    State(shared): State<Arc<Shared>>,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    shared.requests.lock().unwrap().push(request);
    let reply = json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "Noted."}]},
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 2, "totalTokenCount": 12},
        "modelVersion": "scripted-gemini"
    });
    if uri.path().contains("streamGenerateContent") {
        ([(header::CONTENT_TYPE, "text/event-stream")], format!("data: {reply}\n\n"))
            .into_response()
    } else {
        axum::Json(reply).into_response()
    }
}

/// The `parameters` schema of the function declaration `name` in a Gemini request, if any.
pub fn declared_parameters(request: &Value, name: &str) -> Option<Value> {
    request.get("tools")?.as_array()?.iter().find_map(|tool| {
        tool.get("functionDeclarations")?.as_array()?.iter().find_map(|decl| {
            (decl.get("name")?.as_str()? == name).then(|| decl.get("parameters").cloned())?
        })
    })
}
