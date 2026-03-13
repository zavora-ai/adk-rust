//! Deployment manifest, bundling, and control-plane client for ADK-Rust.
//!
//! # Example
//!
//! ```no_run
//! use adk_deploy::DeploymentManifest;
//! use std::path::Path;
//!
//! let manifest = DeploymentManifest::from_path(Path::new("adk-deploy.toml")).unwrap();
//! assert_eq!(manifest.agent.name, "my-agent");
//! ```

mod bundle;
mod client;
mod config;
mod error;
mod manifest;
mod models;

pub use bundle::{BundleArtifact, BundleBuilder};
pub use client::DeployClient;
pub use config::DeployClientConfig;
pub use error::{DeployError, DeployResult};
pub use manifest::{
    A2aConfig, AgentAuthConfig, AgentConfig, AuthModeSpec, BindingMode, BuildConfig,
    DeploymentManifest, DeploymentStrategyConfig, DeploymentStrategyKind, EnvVarSpec, GraphConfig,
    GuardrailConfig, HealthCheckConfig, PluginRef, RealtimeConfig, SecretRef, ServiceBinding,
    ServiceKind, SkillConfig, SourceInfo, TelemetryConfig,
};
pub use models::{
    ActiveInstance, AgentDetail, AgentSummary, AlertSummary, AuditEvent, AuthSessionResponse,
    BillingSummary, DashboardResponse, DeploymentHistoryResponse, DeploymentRecord,
    DeploymentStatusResponse, DeploymentStatusValue, DeploymentSummary, EnvironmentSummary,
    HitlCheckpoint, LogEntry, LoginRequest, LoginResponse, MetricPoint, MetricsSummary,
    PushDeploymentRequest, PushDeploymentResponse, SecretListResponse, SecretSetRequest,
    TraceSummary, WorkspaceSummary,
};
