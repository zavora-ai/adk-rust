use crate::ServerConfig;
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

/// Runtime metadata used by the built-in agent interface.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetails {
    /// Stable agent name.
    pub name: String,
    /// Human-readable purpose.
    pub description: String,
    /// Broad presentation category.
    pub kind: &'static str,
    /// Primary request/response or realtime interaction pattern.
    pub interaction_mode: adk_core::AgentInteractionMode,
    /// Runtime execution capabilities.
    pub capabilities: adk_core::AgentCapabilities,
    /// Services configured on the server executing this agent.
    pub services: RuntimeServices,
    /// Immediate child agents for legacy composites and workflows.
    pub children: Vec<AgentChild>,
    /// Exact portable topology when the root provides one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<adk_core::AgentTopology>,
}

/// Shared runtime services visible to the built-in interface.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeServices {
    /// Whether an in-process telemetry exporter is configured.
    ///
    /// Kept for wire compatibility; use [`Self::telemetry_status`] to
    /// distinguish a ready exporter from one proven to be collecting.
    pub telemetry: bool,
    /// Current in-process telemetry collector state.
    pub telemetry_status: TelemetryStatus,
    /// An artifact service is available to tools and agents.
    pub artifacts: bool,
    /// A cross-session memory service is available.
    pub memory: bool,
}

/// Observable state of the in-process session telemetry collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TelemetryStatus {
    /// No in-process exporter is attached to this server.
    Disabled,
    /// An exporter is attached but has not retained a supported runtime span.
    Configured,
    /// The exporter has retained at least one supported runtime span.
    Collecting,
}

/// One immediate child in a legacy agent hierarchy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentChild {
    /// Stable child name.
    pub name: String,
    /// Human-readable child purpose.
    pub description: String,
    /// Runtime execution capabilities.
    pub capabilities: adk_core::AgentCapabilities,
}

#[derive(Clone)]
pub struct AppsController {
    config: ServerConfig,
}

impl AppsController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

/// Response format for /api/apps - simple list of agent names
pub async fn list_apps(
    State(controller): State<AppsController>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let apps = controller.config.agent_loader.list_agents();
    Ok(Json(apps))
}

/// Query params for /api/list-apps (adk-go compatible)
#[derive(Debug, Deserialize)]
pub struct ListAppsQuery {
    #[serde(default)]
    pub relative_path: Option<String>,
}

/// App info returned by /api/list-apps (adk-go compatible format)
#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub description: String,
}

/// Response format for /api/list-apps (adk-go compatible)
/// Returns just the agent names as strings - the frontend expects this format
pub async fn list_apps_compat(
    State(controller): State<AppsController>,
    Query(_query): Query<ListAppsQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let apps = controller.config.agent_loader.list_agents();
    Ok(Json(apps))
}

/// Return runtime metadata for one executable agent root.
pub async fn get_agent_details(
    State(controller): State<AppsController>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<AgentDetails>, StatusCode> {
    let agent = controller
        .config
        .agent_loader
        .load_agent(&name)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let topology = agent.topology();
    let interaction_mode = agent.interaction_mode();
    let children = agent
        .sub_agents()
        .iter()
        .map(|child| AgentChild {
            name: child.name().to_string(),
            description: child.description().to_string(),
            capabilities: child.capabilities(),
        })
        .collect::<Vec<_>>();
    let kind = if interaction_mode == adk_core::AgentInteractionMode::Realtime {
        "realtime"
    } else if topology.as_ref().is_some_and(|topology| {
        topology
            .relationships
            .iter()
            .any(|relationship| relationship.kind == adk_core::AgentRelationshipKind::Flow)
    }) {
        "workflow"
    } else if topology.is_some() {
        "team"
    } else if children.is_empty() {
        "agent"
    } else {
        "composite"
    };

    let telemetry_status = match controller.config.span_exporter.as_ref() {
        Some(exporter) if exporter.is_collecting() => TelemetryStatus::Collecting,
        Some(_) => TelemetryStatus::Configured,
        None => TelemetryStatus::Disabled,
    };

    Ok(Json(AgentDetails {
        name: agent.name().to_string(),
        description: agent.description().to_string(),
        kind,
        interaction_mode,
        capabilities: agent.capabilities(),
        services: RuntimeServices {
            telemetry: controller.config.span_exporter.is_some(),
            telemetry_status,
            artifacts: controller.config.artifact_service.is_some(),
            memory: controller.config.memory_service.is_some(),
        },
        children,
        topology,
    }))
}
