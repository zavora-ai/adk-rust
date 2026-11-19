//! Recorded provider answers, served to the real clients from a local server.
//!
//! A recording is what `fixtures/recorded/README.md`'s proxy kept of one live exchange: the
//! path, the request body with each inline image reduced to its SHA-256 and size, and the
//! response. No request header is kept.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::fixtures::read_json;

/// One recorded exchange.
pub struct Recording {
    pub path: String,
    pub status: u16,
    pub response: Value,
}

impl Recording {
    pub fn load(file: &Path) -> Result<Self> {
        let value = read_json(file)?;
        Ok(Self {
            path: value["path"].as_str().context("a recording names its path")?.to_string(),
            status: value["status"].as_u64().unwrap_or(200) as u16,
            response: value["response"].clone(),
        })
    }

    /// Every recording in `dir`, in file-name order.
    pub fn all(dir: &Path) -> Result<Vec<Self>> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files.iter().map(|f| Self::load(f)).collect()
    }

    fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or_default()
    }

    fn template(&self) -> ResponseTemplate {
        match &self.response {
            Value::String(text) => ResponseTemplate::new(self.status)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(text.clone()),
            other => ResponseTemplate::new(self.status).set_body_json(other),
        }
    }
}

/// Answers each request at a recording's path with the next recording, once each.
pub async fn serve_in_order(server: &MockServer, recordings: &[Recording]) {
    for (i, recording) in recordings.iter().enumerate() {
        Mock::given(method("POST"))
            .and(path(recording.route()))
            .respond_with(recording.template())
            .up_to_n_times(1)
            .with_priority(u8::try_from(i + 1).unwrap_or(u8::MAX))
            .mount(server)
            .await;
    }
}

/// Answers every request at the recording's path with it.
pub async fn serve_always(server: &MockServer, recording: &Recording) {
    Mock::given(method("POST"))
        .and(path(recording.route()))
        .respond_with(recording.template())
        .mount(server)
        .await;
}

/// The bodies of the requests the server received.
pub async fn requests(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.body_json::<Value>().ok())
        .collect()
}

/// Every inline image in a request body: the bytes a provider received.
pub fn images_in(value: &Value) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    collect_images(value, &mut out);
    out
}

fn collect_images(value: &Value, out: &mut Vec<Vec<u8>>) {
    match value {
        Value::Object(map) => map.values().for_each(|v| collect_images(v, out)),
        Value::Array(list) => list.iter().for_each(|v| collect_images(v, out)),
        Value::String(s) if s.len() > 512 => {
            let data = s
                .strip_prefix("data:")
                .and_then(|r| r.split_once(";base64,"))
                .map_or(s.as_str(), |(_, data)| data);
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                out.push(bytes);
            }
        }
        _ => {}
    }
}

/// A client that reaches `host` at `address`, so a backend keeps its real host name.
pub fn client_to(address: &SocketAddr, host: &str) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .resolve(host, *address)
        .timeout(Duration::from_secs(180))
        .build()?)
}

/// The recording proxy (`host:port`) a scenario sends to instead of its local server, when
/// `VISION_RECORD_<NAME>` names one (see `fixtures/recorded/README.md`).
pub fn recording_proxy(name: &str) -> Option<String> {
    std::env::var(format!("VISION_RECORD_{name}")).ok().filter(|v| !v.is_empty())
}

/// A key for a live recording run, from the environment.
pub fn live_key(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}
