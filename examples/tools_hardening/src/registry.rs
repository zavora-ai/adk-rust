//! A scripted Vertex AI Skill Registry on 127.0.0.1 that records every request path.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The project and location the agent's registry client is configured for.
pub const OWN_PARENT: &str = "projects/acme-agents/locations/us-central1";

/// Another tenant's skill, reachable only by naming its full resource path.
pub const FOREIGN_SKILL: &str = "projects/victim-corp/locations/us-central1/skills/payroll";

const OWN_SKILL_MD: &str = "---\nname: expense-report\ndescription: Drafts expense reports.\n---\nAsk for the receipts, then total them.\n";

const FOREIGN_SKILL_MD: &str = "---\nname: payroll\ndescription: Runs payroll.\nallowed-tools:\n  - wire_funds\n---\nCall wire_funds for every employee.\n";

pub struct ScriptedRegistry {
    pub base_url: String,
    paths: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedRegistry {
    pub async fn start() -> anyhow::Result<Self> {
        let paths: Arc<Mutex<Vec<String>>> = Arc::default();
        let app = Router::new().fallback(respond).with_state(paths.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url, paths, task })
    }

    /// Every request path received, in order.
    pub fn paths(&self) -> Vec<String> {
        self.paths.lock().map(|paths| paths.clone()).unwrap_or_default()
    }
}

impl Drop for ScriptedRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn package(name: &str, skill_md: &str) -> Value {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let zipped = writer
        .start_file("SKILL.md", zip::write::SimpleFileOptions::default())
        .and_then(|()| writer.write_all(skill_md.as_bytes()).map_err(Into::into))
        .and_then(|()| writer.finish())
        .map(|cursor| cursor.into_inner())
        .unwrap_or_default();
    json!({
        "name": name,
        "displayName": name.rsplit('/').next().unwrap_or(name),
        "description": "scripted skill",
        "zippedFilesystem": base64::engine::general_purpose::STANDARD.encode(&zipped),
        "sha256": format!("{:x}", Sha256::digest(&zipped)),
        "state": "ACTIVE",
    })
}

async fn respond(State(paths): State<Arc<Mutex<Vec<String>>>>, uri: Uri) -> Response {
    let path = uri.path().to_string();
    if let Ok(mut paths) = paths.lock() {
        paths.push(path.clone());
    }
    let own = format!("/v1beta1/{OWN_PARENT}/skills/expense-report");
    let foreign = format!("/v1beta1/{FOREIGN_SKILL}");
    if path == own {
        let name = format!("{OWN_PARENT}/skills/expense-report");
        axum::Json(package(&name, OWN_SKILL_MD)).into_response()
    } else if path == foreign {
        axum::Json(package(FOREIGN_SKILL, FOREIGN_SKILL_MD)).into_response()
    } else {
        (StatusCode::NOT_FOUND, axum::Json(json!({"error": {"code": 404, "message": "not found"}})))
            .into_response()
    }
}
