use adk_ui::interop::mcp_apps::{McpUiPermissions, McpUiResourceCsp};
use adk_ui::{
    McpAppsRenderOptions, TOOL_ENVELOPE_VERSION, UI_DEFAULT_PROTOCOL, UI_PROTOCOL_CAPABILITIES,
    UiProtocolDeprecationSpec, validate_mcp_apps_render_options,
};
use axum::{Json, extract::Query, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize)]
pub struct UiProtocolCapability {
    pub protocol: &'static str,
    pub versions: Vec<&'static str>,
    pub features: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation: Option<UiProtocolDeprecation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProtocolDeprecation {
    pub stage: &'static str,
    pub announced_on: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sunset_target_on: Option<&'static str>,
    pub replacement_protocols: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

fn map_deprecation(
    spec: Option<&'static UiProtocolDeprecationSpec>,
) -> Option<UiProtocolDeprecation> {
    let spec = spec?;
    Some(UiProtocolDeprecation {
        stage: spec.stage,
        announced_on: spec.announced_on,
        sunset_target_on: spec.sunset_target_on,
        replacement_protocols: spec.replacement_protocols.to_vec(),
        note: spec.note,
    })
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

fn parse_ui_meta_options(
    meta: &Option<Value>,
) -> Result<McpAppsRenderOptions, (StatusCode, String)> {
    let Some(meta_value) = meta else {
        return Ok(McpAppsRenderOptions::default());
    };
    let meta_object = meta_value
        .as_object()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "_meta must be a JSON object".to_string()))?;
    let Some(ui_value) = meta_object.get("ui") else {
        return Ok(McpAppsRenderOptions::default());
    };
    let ui_object = ui_value
        .as_object()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "_meta.ui must be a JSON object".to_string()))?;

    let domain = ui_object
        .get("domain")
        .map(|domain_value| {
            domain_value.as_str().ok_or_else(|| {
                (StatusCode::BAD_REQUEST, "_meta.ui.domain must be a string".to_string())
            })
        })
        .transpose()?
        .map(ToString::to_string);

    let prefers_border = ui_object
        .get("prefersBorder")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                (StatusCode::BAD_REQUEST, "_meta.ui.prefersBorder must be a boolean".to_string())
            })
        })
        .transpose()?;

    let csp = ui_object
        .get("csp")
        .map(|value| {
            serde_json::from_value::<McpUiResourceCsp>(value.clone()).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("_meta.ui.csp must be an object with domain arrays: {}", error),
                )
            })
        })
        .transpose()?;

    let permissions = ui_object
        .get("permissions")
        .map(|value| {
            serde_json::from_value::<McpUiPermissions>(value.clone()).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("_meta.ui.permissions must be an object: {}", error),
                )
            })
        })
        .transpose()?;

    Ok(McpAppsRenderOptions { domain, prefers_border, csp, permissions, ..Default::default() })
}

fn validate_ui_meta(meta: &Option<Value>) -> Result<McpAppsRenderOptions, (StatusCode, String)> {
    let options = parse_ui_meta_options(meta)?;
    validate_mcp_apps_render_options(&options).map_err(|error| {
        (StatusCode::BAD_REQUEST, format!("Invalid _meta.ui options for mcp_apps: {}", error))
    })?;
    Ok(options)
}

/// GET /api/ui/capabilities
pub async fn ui_capabilities() -> Json<UiCapabilities> {
    Json(UiCapabilities {
        default_protocol: UI_DEFAULT_PROTOCOL,
        protocols: UI_PROTOCOL_CAPABILITIES
            .iter()
            .map(|spec| UiProtocolCapability {
                protocol: spec.protocol,
                versions: spec.versions.to_vec(),
                features: spec.features.to_vec(),
                deprecation: map_deprecation(spec.deprecation),
            })
            .collect(),
        tool_envelope_version: TOOL_ENVELOPE_VERSION,
    })
}

/// GET /api/ui/resources
pub async fn list_ui_resources() -> Json<UiResourceListResponse> {
    let resources: Vec<UiResource> = resource_registry()
        .read()
        .map(|registry| registry.values().map(|entry| entry.resource.clone()).collect())
        .unwrap_or_default();
    info!(resource_count = resources.len(), "ui resource list requested");
    Json(UiResourceListResponse { resources })
}

/// GET /api/ui/resources/read?uri=ui://...
pub async fn read_ui_resource(
    Query(query): Query<ReadUiResourceQuery>,
) -> Result<Json<UiResourceReadResponse>, (StatusCode, String)> {
    validate_ui_resource_uri(&query.uri)?;
    let guard = resource_registry().read().map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "resource registry poisoned".to_string())
    })?;
    let Some(entry) = guard.get(&query.uri) else {
        warn!(uri = %query.uri, "ui resource read failed: not found");
        return Err((StatusCode::NOT_FOUND, format!("resource not found: {}", query.uri)));
    };
    info!(uri = %query.uri, "ui resource read");
    Ok(Json(UiResourceReadResponse { contents: vec![entry.content.clone()] }))
}

/// POST /api/ui/resources/register
pub async fn register_ui_resource(
    Json(req): Json<RegisterUiResourceRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    validate_ui_resource_uri(&req.uri)?;
    validate_ui_resource_mime(&req.mime_type)?;
    let ui_meta_options = validate_ui_meta(&req.meta)?;

    let uri = req.uri.clone();
    let name = req.name.clone();
    let mime_type = req.mime_type.clone();
    let meta = req.meta.clone();
    let domain = ui_meta_options.domain.unwrap_or_else(|| "<none>".to_string());

    let entry = UiResourceEntry {
        resource: UiResource {
            uri: uri.clone(),
            name: name.clone(),
            description: req.description.clone(),
            mime_type: mime_type.clone(),
            meta: meta.clone(),
        },
        content: UiResourceContent {
            uri: uri.clone(),
            mime_type: mime_type.clone(),
            text: Some(req.text),
            blob: None,
            meta,
        },
    };

    resource_registry()
        .write()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "resource registry poisoned".to_string()))?
        .insert(uri.clone(), entry);
    info!(
        uri = %uri,
        name = %name,
        mime_type = %mime_type,
        ui_domain = %domain,
        "ui resource registered"
    );

    Ok(StatusCode::CREATED)
}
