use axum::{
    Json,
    extract::Query,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone, Serialize)]
pub struct UiProtocolCapability {
    pub protocol: &'static str,
    pub versions: Vec<&'static str>,
    pub features: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiCapabilities {
    pub default_protocol: &'static str,
    pub protocols: Vec<UiProtocolCapability>,
    pub tool_envelope_version: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub mime_type: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResourceContent {
    pub uri: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiResourceListResponse {
    pub resources: Vec<UiResource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiResourceReadResponse {
    pub contents: Vec<UiResourceContent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterUiResourceRequest {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub mime_type: String,
    pub text: String,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadUiResourceQuery {
    pub uri: String,
}

#[derive(Debug, Clone)]
struct UiResourceEntry {
    resource: UiResource,
    content: UiResourceContent,
}

static UI_RESOURCE_REGISTRY: OnceLock<RwLock<HashMap<String, UiResourceEntry>>> = OnceLock::new();

fn resource_registry() -> &'static RwLock<HashMap<String, UiResourceEntry>> {
    UI_RESOURCE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn validate_ui_resource_uri(uri: &str) -> Result<(), (StatusCode, String)> {
    if !uri.starts_with("ui://") {
        return Err((
            StatusCode::BAD_REQUEST,
            "ui resource uri must start with 'ui://'".to_string(),
        ));
    }
    Ok(())
}

fn validate_ui_resource_mime(mime_type: &str) -> Result<(), (StatusCode, String)> {
    if mime_type != "text/html;profile=mcp-app" {
        return Err((
            StatusCode::BAD_REQUEST,
            "mimeType must be 'text/html;profile=mcp-app'".to_string(),
        ));
    }
    Ok(())
}

/// GET /api/ui/capabilities
pub async fn ui_capabilities() -> Json<UiCapabilities> {
    Json(UiCapabilities {
        default_protocol: "adk_ui",
        protocols: vec![
            UiProtocolCapability {
                protocol: "adk_ui",
                versions: vec!["1.0"],
                features: vec!["legacy_components", "theme", "events"],
            },
            UiProtocolCapability {
                protocol: "a2ui",
                versions: vec!["0.9"],
                features: vec!["jsonl", "createSurface", "updateComponents", "updateDataModel"],
            },
            UiProtocolCapability {
                protocol: "ag_ui",
                versions: vec!["0.1"],
                features: vec!["run_lifecycle", "custom_events", "event_stream"],
            },
            UiProtocolCapability {
                protocol: "mcp_apps",
                versions: vec!["sep-1865"],
                features: vec!["ui_resource_uri", "tool_meta", "html_resource"],
            },
        ],
        tool_envelope_version: "1.0",
    })
}

/// GET /api/ui/resources
pub async fn list_ui_resources() -> Json<UiResourceListResponse> {
    let resources = resource_registry()
        .read()
        .map(|registry| registry.values().map(|entry| entry.resource.clone()).collect())
        .unwrap_or_default();
    Json(UiResourceListResponse { resources })
}

/// GET /api/ui/resources/read?uri=ui://...
pub async fn read_ui_resource(
    Query(query): Query<ReadUiResourceQuery>,
) -> Result<Json<UiResourceReadResponse>, (StatusCode, String)> {
    validate_ui_resource_uri(&query.uri)?;
    let guard = resource_registry()
        .read()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "resource registry poisoned".to_string()))?;
    let Some(entry) = guard.get(&query.uri) else {
        return Err((StatusCode::NOT_FOUND, format!("resource not found: {}", query.uri)));
    };
    Ok(Json(UiResourceReadResponse { contents: vec![entry.content.clone()] }))
}

/// POST /api/ui/resources/register
pub async fn register_ui_resource(
    Json(req): Json<RegisterUiResourceRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    validate_ui_resource_uri(&req.uri)?;
    validate_ui_resource_mime(&req.mime_type)?;

    let entry = UiResourceEntry {
        resource: UiResource {
            uri: req.uri.clone(),
            name: req.name.clone(),
            description: req.description.clone(),
            mime_type: req.mime_type.clone(),
            meta: req.meta.clone(),
        },
        content: UiResourceContent {
            uri: req.uri.clone(),
            mime_type: req.mime_type,
            text: Some(req.text),
            blob: None,
            meta: req.meta,
        },
    };

    resource_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "resource registry poisoned".to_string()))?
        .insert(req.uri, entry);

    Ok(StatusCode::CREATED)
}
