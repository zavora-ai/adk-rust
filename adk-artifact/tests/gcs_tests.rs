//! Contract tests for the GCS artifact backend against a mock GCS JSON API,
//! plus an ignored live test against a real bucket.

#![cfg(feature = "gcs")]

use adk_artifact::{
    ArtifactService, DeleteRequest, GcsArtifactService, ListRequest, LoadRequest, SaveRequest,
    VersionsRequest,
};
use adk_core::Part;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct StoredObject {
    data: Vec<u8>,
    content_type: Option<String>,
    metadata: HashMap<String, String>,
}

type Store = Arc<Mutex<BTreeMap<String, StoredObject>>>;

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
    let store = store.lock().unwrap();
    let items: Vec<Value> = store
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(name, object)| object_resource(name, object))
        .collect();
    axum::Json(json!({ "items": items })).into_response()
}

async fn get_object(
    State(store): State<Store>,
    Path((_bucket, object_name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let store = store.lock().unwrap();
    let Some(object) = store.get(&object_name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if params.get("alt").is_some_and(|alt| alt == "media") {
        let content_type =
            object.content_type.clone().unwrap_or_else(|| "application/octet-stream".to_string());
        return ([(header::CONTENT_TYPE, content_type)], object.data.clone()).into_response();
    }
    axum::Json(object_resource(&object_name, object)).into_response()
}

async fn delete_object(
    State(store): State<Store>,
    Path((_bucket, object_name)): Path<(String, String)>,
) -> Response {
    let mut store = store.lock().unwrap();
    if store.remove(&object_name).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Parses the `multipart/related` upload body: a JSON object-resource part
/// followed by a media part.
fn parse_multipart_related(headers: &HeaderMap, body: &[u8]) -> (Value, Vec<u8>) {
    let content_type = headers[header::CONTENT_TYPE].to_str().unwrap();
    let boundary = content_type.split("boundary=").nth(1).unwrap().trim();
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(start) = find_subslice(rest, delimiter.as_bytes()) {
        rest = &rest[start + delimiter.len()..];
        if rest.starts_with(b"--") {
            break;
        }
        let Some(end) = find_subslice(rest, delimiter.as_bytes()) else {
            break;
        };
        let segment = &rest[..end];
        // Each part is `\r\nheaders\r\n\r\ncontent\r\n`.
        let content_start = find_subslice(segment, b"\r\n\r\n").unwrap() + 4;
        let content = &segment[content_start..segment.len() - 2];
        parts.push(content.to_vec());
    }
    assert_eq!(parts.len(), 2, "expected metadata + media parts");
    let resource: Value = serde_json::from_slice(&parts[0]).unwrap();
    (resource, parts[1].clone())
}

async fn upload_object(State(store): State<Store>, headers: HeaderMap, body: Bytes) -> Response {
    let (resource, data) = parse_multipart_related(&headers, &body);
    let name = resource["name"].as_str().unwrap().to_string();
    let content_type = resource["contentType"].as_str().map(str::to_string);
    let metadata: HashMap<String, String> = resource
        .get("metadata")
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect())
        .unwrap_or_default();
    let object = StoredObject { data, content_type, metadata };
    let response = object_resource(&name, &object);
    store.lock().unwrap().insert(name, object);
    axum::Json(response).into_response()
}

async fn spawn_mock_gcs() -> (String, Store) {
    let store: Store = Arc::new(Mutex::new(BTreeMap::new()));
    let app = axum::Router::new()
        .route("/storage/v1/b/{bucket}/o", get(list_objects))
        .route("/storage/v1/b/{bucket}/o/{object}", get(get_object))
        .route("/storage/v1/b/{bucket}/o/{object}", delete(delete_object))
        .route("/upload/storage/v1/b/{bucket}/o", post(upload_object))
        .with_state(store.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), store)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn mock_service(endpoint: &str) -> GcsArtifactService {
    GcsArtifactService::with_credentials(
        "test-bucket",
        api_key_credentials::Builder::new("test-key").build(),
    )
    .unwrap()
    .with_endpoint(endpoint)
}

fn save_request(app_name: &str, file_name: &str, part: Part) -> SaveRequest {
    SaveRequest {
        app_name: app_name.to_string(),
        user_id: "user123".to_string(),
        session_id: "session456".to_string(),
        file_name: file_name.to_string(),
        part,
        version: None,
    }
}

fn load_request(app_name: &str, file_name: &str, version: Option<i64>) -> LoadRequest {
    LoadRequest {
        app_name: app_name.to_string(),
        user_id: "user123".to_string(),
        session_id: "session456".to_string(),
        file_name: file_name.to_string(),
        version,
    }
}

fn inline_part(data: &[u8]) -> Part {
    Part::InlineData {
        mime_type: "image/png".to_string(),
        data: data.to_vec(),
        uri: None,
        annotations: None,
    }
}

#[tokio::test]
async fn test_mock_round_trip_save_load_list_versions_delete() {
    let (endpoint, store) = spawn_mock_gcs().await;
    let service = mock_service(&endpoint);

    // Two versions of a session-scoped artifact: auto-versioning starts at 0.
    let v0 = service.save(save_request("my-app", "chart.png", inline_part(b"v0"))).await.unwrap();
    assert_eq!(v0.version, 0);
    let v1 = service.save(save_request("my-app", "chart.png", inline_part(b"v1"))).await.unwrap();
    assert_eq!(v1.version, 1);

    // One user-namespaced artifact.
    let user_v0 =
        service.save(save_request("my-app", "user:profile.png", inline_part(b"me"))).await.unwrap();
    assert_eq!(user_v0.version, 0);

    // The mock recorded the exact adk-python blob names on the wire.
    {
        let store = store.lock().unwrap();
        let names: Vec<&String> = store.keys().collect();
        assert_eq!(
            names,
            vec![
                "my-app/user123/session456/chart.png/0",
                "my-app/user123/session456/chart.png/1",
                "my-app/user123/user/user:profile.png/0",
            ]
        );
    }

    // Load: latest, pinned version, and user namespace.
    let latest = service.load(load_request("my-app", "chart.png", None)).await.unwrap();
    assert_eq!(latest.part, inline_part(b"v1"));
    let pinned = service.load(load_request("my-app", "chart.png", Some(0))).await.unwrap();
    assert_eq!(pinned.part, inline_part(b"v0"));
    let user = service.load(load_request("my-app", "user:profile.png", None)).await.unwrap();
    assert_eq!(user.part, inline_part(b"me"));

    // List includes both session-scoped and user-namespaced filenames.
    let listed = service
        .list(ListRequest {
            app_name: "my-app".to_string(),
            user_id: "user123".to_string(),
            session_id: "session456".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(listed.file_names, vec!["chart.png".to_string(), "user:profile.png".to_string()]);

    // Versions come back ascending, matching adk-python.
    let versions = service
        .versions(VersionsRequest {
            app_name: "my-app".to_string(),
            user_id: "user123".to_string(),
            session_id: "session456".to_string(),
            file_name: "chart.png".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(versions.versions, vec![0, 1]);

    // Delete all versions removes every blob of the artifact and nothing else.
    service
        .delete(DeleteRequest {
            app_name: "my-app".to_string(),
            user_id: "user123".to_string(),
            session_id: "session456".to_string(),
            file_name: "chart.png".to_string(),
            version: None,
        })
        .await
        .unwrap();
    {
        let store = store.lock().unwrap();
        let names: Vec<&String> = store.keys().collect();
        assert_eq!(names, vec!["my-app/user123/user/user:profile.png/0"]);
    }
    let missing = service.load(load_request("my-app", "chart.png", None)).await;
    assert!(missing.is_err_and(|error| error.is_not_found()));
}

#[tokio::test]
async fn test_mock_text_and_file_data_metadata_keys() {
    let (endpoint, store) = spawn_mock_gcs().await;
    let service = mock_service(&endpoint);

    // Text artifacts are flagged adkIsText and reconstruct as Part::Text.
    let text_part = Part::Text { text: "hello artifact".to_string() };
    service.save(save_request("my-app", "notes.txt", text_part.clone())).await.unwrap();
    let loaded = service.load(load_request("my-app", "notes.txt", None)).await.unwrap();
    assert_eq!(loaded.part, text_part);

    // File-data artifacts store only metadata (adkFileUri/adkFileMimeType).
    let file_part = Part::FileData {
        mime_type: "video/mp4".to_string(),
        file_uri: "gs://other-bucket/video.mp4".to_string(),
        annotations: None,
    };
    service.save(save_request("my-app", "video.mp4", file_part.clone())).await.unwrap();
    let loaded = service.load(load_request("my-app", "video.mp4", None)).await.unwrap();
    assert_eq!(loaded.part, file_part);

    let store = store.lock().unwrap();
    let text_object = &store["my-app/user123/session456/notes.txt/0"];
    assert_eq!(text_object.metadata["adkIsText"], "true");
    assert_eq!(text_object.content_type.as_deref(), Some("text/plain"));
    let file_object = &store["my-app/user123/session456/video.mp4/0"];
    assert_eq!(file_object.metadata["adkFileUri"], "gs://other-bucket/video.mp4");
    assert_eq!(file_object.metadata["adkFileMimeType"], "video/mp4");
    assert!(file_object.data.is_empty());
}

// Regression for googleapis/python-aiplatform#6521: when deployed, the
// app_name is the engine ID. Save and load must build the same blob name
// from it, or artifacts silently vanish between the two operations.
#[tokio::test]
async fn test_engine_id_app_name_round_trips() {
    let (endpoint, store) = spawn_mock_gcs().await;
    let service = mock_service(&endpoint);
    let engine_id = "1234567890123456789";

    let part = inline_part(b"engine artifact");
    let saved = service.save(save_request(engine_id, "output.png", part.clone())).await.unwrap();
    assert_eq!(saved.version, 0);

    let loaded =
        service.load(load_request(engine_id, "output.png", Some(saved.version))).await.unwrap();
    assert_eq!(loaded.part, part);

    let store = store.lock().unwrap();
    assert!(store.contains_key("1234567890123456789/user123/session456/output.png/0"));
}

/// Live round-trip against a real bucket. Requires ADC plus
/// `GOOGLE_CLOUD_PROJECT` and `ADK_ARTIFACT_BUCKET`.
#[tokio::test]
#[ignore = "requires GOOGLE_CLOUD_PROJECT, ADK_ARTIFACT_BUCKET, and ADC"]
async fn test_live_gcs_round_trip() {
    let _project =
        std::env::var("GOOGLE_CLOUD_PROJECT").expect("set GOOGLE_CLOUD_PROJECT to run this test");
    let bucket =
        std::env::var("ADK_ARTIFACT_BUCKET").expect("set ADK_ARTIFACT_BUCKET to run this test");
    let service = GcsArtifactService::new_with_adc(bucket).unwrap();

    let file_name = format!("live-test-{}.bin", std::process::id());
    let part = inline_part(b"live round trip");
    let saved = service.save(save_request("adk-rust-live-test", &file_name, part.clone())).await;
    let saved = saved.unwrap();
    let loaded = service
        .load(load_request("adk-rust-live-test", &file_name, Some(saved.version)))
        .await
        .unwrap();
    assert_eq!(loaded.part, part);
    service
        .delete(DeleteRequest {
            app_name: "adk-rust-live-test".to_string(),
            user_id: "user123".to_string(),
            session_id: "session456".to_string(),
            file_name,
            version: None,
        })
        .await
        .unwrap();
}
