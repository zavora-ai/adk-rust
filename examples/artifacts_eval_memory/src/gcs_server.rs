//! A scripted Google Cloud Storage JSON API on 127.0.0.1.
//!
//! It serves the five calls `GcsArtifactService` makes — list, get, download, multipart
//! upload, delete — and honours the `ifGenerationMatch=0` upload precondition the way GCS
//! does: an upload that names an existing object fails with HTTP 412. Listing waits a few
//! milliseconds, as a real bucket does, so concurrent saves overlap.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_artifact::GcsArtifactService;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};

#[derive(Clone)]
struct StoredObject {
    data: Vec<u8>,
    content_type: Option<String>,
    metadata: HashMap<String, String>,
}

type Store = Arc<Mutex<BTreeMap<String, StoredObject>>>;

/// A running scripted bucket.
pub struct ScriptedGcs {
    endpoint: String,
}

impl ScriptedGcs {
    pub async fn start() -> anyhow::Result<Self> {
        let store: Store = Arc::new(Mutex::new(BTreeMap::new()));
        let app = axum::Router::new()
            .route("/storage/v1/b/{bucket}/o", get(list_objects))
            .route("/storage/v1/b/{bucket}/o/{object}", get(get_object))
            .route("/storage/v1/b/{bucket}/o/{object}", delete(delete_object))
            .route("/upload/storage/v1/b/{bucket}/o", post(upload_object))
            .with_state(store);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { endpoint })
    }

    /// A real `GcsArtifactService` pointed at this bucket.
    pub fn service(&self) -> anyhow::Result<GcsArtifactService> {
        Ok(GcsArtifactService::with_credentials(
            "validation-bucket",
            api_key_credentials::Builder::new("scripted-key").build(),
        )?
        .with_endpoint(&self.endpoint))
    }
}

fn object_resource(name: &str, object: &StoredObject) -> Value {
    let mut resource = json!({ "name": name });
    if let Some(content_type) = &object.content_type {
        resource["contentType"] = json!(content_type);
    }
    if !object.metadata.is_empty() {
        resource["metadata"] = json!(object.metadata);
    }
    resource
}

async fn list_objects(
    State(store): State<Store>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let prefix = params.get("prefix").cloned().unwrap_or_default();
    let items: Vec<Value> = store
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(name, object)| object_resource(name, object))
        .collect();
    tokio::time::sleep(Duration::from_millis(15)).await;
    axum::Json(json!({ "items": items })).into_response()
}

async fn get_object(
    State(store): State<Store>,
    Path((_bucket, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let store = store.lock().unwrap();
    let Some(object) = store.get(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if params.get("alt").is_some_and(|alt| alt == "media") {
        let content_type =
            object.content_type.clone().unwrap_or_else(|| "application/octet-stream".to_string());
        return ([(header::CONTENT_TYPE, content_type)], object.data.clone()).into_response();
    }
    axum::Json(object_resource(&name, object)).into_response()
}

async fn delete_object(
    State(store): State<Store>,
    Path((_bucket, name)): Path<(String, String)>,
) -> Response {
    if store.lock().unwrap().remove(&name).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn upload_object(
    State(store): State<Store>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((resource, data)) = parse_multipart_related(&headers, &body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(name) = resource["name"].as_str().map(str::to_string) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let object = StoredObject {
        data,
        content_type: resource["contentType"].as_str().map(str::to_string),
        metadata: resource
            .get("metadata")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let mut store = store.lock().unwrap();
    // GCS: `ifGenerationMatch=0` means "only if no live object has this name".
    if params.get("ifGenerationMatch").is_some_and(|value| value == "0")
        && store.contains_key(&name)
    {
        return (
            StatusCode::PRECONDITION_FAILED,
            axum::Json(json!({"error": {"code": 412, "message": "conditionNotMet"}})),
        )
            .into_response();
    }
    let response = object_resource(&name, &object);
    store.insert(name, object);
    axum::Json(response).into_response()
}

/// Splits a `multipart/related` upload into its JSON resource and media parts.
fn parse_multipart_related(headers: &HeaderMap, body: &[u8]) -> Option<(Value, Vec<u8>)> {
    let content_type = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    let boundary = content_type.split("boundary=").nth(1)?.trim();
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(start) = find(rest, delimiter.as_bytes()) {
        rest = &rest[start + delimiter.len()..];
        if rest.starts_with(b"--") {
            break;
        }
        let end = find(rest, delimiter.as_bytes())?;
        let segment = &rest[..end];
        let content_start = find(segment, b"\r\n\r\n")? + 4;
        parts.push(segment[content_start..segment.len() - 2].to_vec());
    }
    if parts.len() != 2 {
        return None;
    }
    let resource = serde_json::from_slice(&parts[0]).ok()?;
    Some((resource, parts.pop()?))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}
