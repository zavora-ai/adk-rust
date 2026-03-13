use serde::{Deserialize, Serialize};

use crate::{DeploymentManifest, DeploymentStrategyKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub id: String,
    pub name: String,
    pub plan: String,
    pub region: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub name: String,
    pub agents: usize,
    pub status: String,
    pub region: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub name: String,
    pub environment: String,
    pub version: String,
    pub health: String,
    pub instances: usize,
    pub request_rate: String,
    pub latency_p95: String,
    pub deployed_at: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TraceSummary {
    pub id: String,
    pub status: String,
    pub model: String,
    pub duration: String,
    pub tokens: String,
    pub cost: String,
    pub step: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub instance: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HitlCheckpoint {
    pub id: String,
    pub agent: String,
    pub wait: String,
    pub checkpoint_type: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BillingSummary {
    pub label: String,
    pub value: String,
    pub sub: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AlertSummary {
    pub name: String,
    pub state: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardResponse {
    pub workspace: WorkspaceSummary,
    pub agents: Vec<AgentSummary>,
    pub traces: Vec<TraceSummary>,
    pub logs: Vec<LogEntry>,
    pub hitl: Vec<HitlCheckpoint>,
    pub environments: Vec<EnvironmentSummary>,
    pub usage: Vec<BillingSummary>,
    pub alerts: Vec<AlertSummary>,
    pub active_strategy: DeploymentStrategyKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricPoint {
    pub label: String,
    pub value: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveInstance {
    pub id: String,
    pub state: String,
    pub stats: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSummary {
    pub version: String,
    pub timestamp: String,
    pub status: String,
    pub strategy: DeploymentStrategyKind,
    pub triggered_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetail {
    pub name: String,
    pub environment: String,
    pub description: String,
    pub endpoint: String,
    pub strategy: DeploymentStrategyKind,
    pub scaling_policy: String,
    pub deployment_source: String,
    pub uptime: String,
    pub error_rate: String,
    pub metrics: Vec<MetricPoint>,
    pub instances: Vec<ActiveInstance>,
    pub deployments: Vec<DeploymentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSummary {
    pub request_rate: String,
    pub latency_p50: String,
    pub latency_p95: String,
    pub latency_p99: String,
    pub error_rate: String,
    pub active_connections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentRecord {
    pub id: String,
    pub workspace_id: String,
    pub environment: String,
    pub agent_name: String,
    pub version: String,
    pub status: DeploymentStatusValue,
    pub strategy: DeploymentStrategyKind,
    pub rollout_phase: String,
    pub endpoint_url: String,
    pub checksum_sha256: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<String>,
    pub created_at: String,
    pub manifest: DeploymentManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentStatusValue {
    Pending,
    Building,
    Deploying,
    Healthy,
    Degraded,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PushDeploymentRequest {
    pub workspace_id: Option<String>,
    pub environment: String,
    pub manifest: DeploymentManifest,
    pub bundle_path: String,
    pub checksum_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PushDeploymentResponse {
    pub deployment: DeploymentRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStatusResponse {
    pub deployment: DeploymentRecord,
    pub metrics: MetricsSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentHistoryResponse {
    pub items: Vec<DeploymentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretSetRequest {
    pub environment: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretListResponse {
    pub environment: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub token: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionResponse {
    pub user_id: String,
    pub workspace_id: String,
    pub workspace_name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub timestamp: String,
    pub action: String,
    pub resource: String,
    pub result: String,
}
