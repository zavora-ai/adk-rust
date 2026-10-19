//! Deployment manifest, bundling, and control-plane client for ADK-Rust.
//!
//! - [`DeploymentManifest`] parses and validates `adk-deploy.toml`.
//! - [`BundleBuilder`] builds the agent binary and packages it with its assets into a
//!   `.tar.gz` with a SHA-256 checksum.
//! - [`DeployClient`] pushes, inspects, rolls back, and promotes deployments on a control
//!   plane, and manages its secrets.
//! - `gcp::GcpDeployClient` (feature `gcp`) creates, polls, gets, and deletes Gemini
//!   Enterprise Agent Platform ReasoningEngines for BYOC containers.
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
#[cfg(feature = "gcp")]
pub mod gcp;
mod manifest;
mod models;

pub use bundle::{BundleArtifact, BundleBuilder};
pub use client::DeployClient;
pub use config::DeployClientConfig;
pub use error::{DeployError, DeployResult};
pub use manifest::{
    A2aConfig, AgentAuthConfig, AgentConfig, AuthModeSpec, BindingMode, BuildConfig,
    DeploymentManifest, DeploymentStrategyConfig, DeploymentStrategyKind, EnvVarSpec, GraphConfig,
    GuardrailConfig, HealthCheckConfig, InteractionConfig, ManualInteractionConfig, PluginRef,
    RealtimeConfig, SecretRef, ServiceBinding, ServiceKind, SkillConfig, SourceInfo,
    TelemetryConfig, TriggerInteractionConfig, TriggerKind,
};
pub use models::{
    ActiveInstance, AgentDetail, AgentSummary, AlertSummary, AuditEvent, AuthSessionResponse,
    BillingSummary, DashboardResponse, DeploymentActionState, DeploymentActions,
    DeploymentHistoryResponse, DeploymentRecord, DeploymentStatusResponse, DeploymentStatusValue,
    DeploymentSummary, EnvironmentSummary, HitlCheckpoint, LogEntry, LoginRequest, LoginResponse,
    MetricPoint, MetricsSummary, PushDeploymentRequest, PushDeploymentResponse, SecretListResponse,
    SecretSetRequest, TraceAdkIdentity, TraceExecutionIdentity, TraceInvocation, TraceSession,
    TraceSpan, TraceSummary, TraceTokenUsage, WorkspaceSummary,
};
