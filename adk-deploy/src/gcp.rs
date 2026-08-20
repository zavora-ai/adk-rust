//! Google Cloud deployment client for Agent Engine (ReasoningEngine) BYOC.
//!
//! A minimal REST client with exactly four operations against the v1beta1
//! `reasoningEngines` surface: create, poll, get, delete. It deploys a
//! prebuilt container image — building and pushing the image is out of
//! scope; use Cloud Build:
//!
//! ```bash
//! gcloud builds submit --tag us-central1-docker.pkg.dev/PROJECT/REPO/agent:latest
//! ```
//!
//! ([`gcloud_build_submit_command`] renders this command for a given image
//! URI.)
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_deploy::gcp::{CreateReasoningEngineRequest, GcpDeployClient, GcpDeployConfig};
//!
//! # async fn deploy() -> adk_deploy::DeployResult<()> {
//! let client = GcpDeployClient::new_with_adc(GcpDeployConfig::new("my-project", "us-central1"))?;
//! let request = CreateReasoningEngineRequest::byoc(
//!     "my-agent",
//!     "us-central1-docker.pkg.dev/my-project/agents/my-agent:latest",
//! );
//! let operation = client.create_reasoning_engine(&request).await?;
//! let engine = client.wait_for_operation(operation).await?;
//! # let _ = engine;
//! # Ok(())
//! # }
//! ```
//!
//! # asyncQuery
//!
//! `reasoningEngines:asyncQuery` (durable query jobs) is not declared in
//! [`default_class_methods`]: the capability cannot be added to an engine
//! post-create, and adk-python's `AdkApp` does not register it either —
//! strict parity excludes it (Agent Engine plan, verification task V12).

use crate::error::{DeployError, DeployResult};
use adk_core::{AdkError, ErrorComponent};
use adk_gcp::{
    GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller, VertexResourceName,
    is_scoped_resource_name,
};
use google_cloud_auth::credentials::Credentials;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tracing::info;

const API_VERSION: &str = "v1beta1";
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
// Engine creation provisions serving infrastructure and routinely takes
// minutes, so the deploy deadline is far above adk-gcp's 120 s default
// while keeping the shared backoff shape (100 ms initial, capped).
const OPERATION_POLL_TIMEOUT: Duration = Duration::from_secs(900);
const OPERATION_POLL_MAX_DELAY: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Renders the Cloud Build command that builds and pushes the agent image.
///
/// # Example
///
/// ```rust
/// use adk_deploy::gcp::gcloud_build_submit_command;
///
/// let command = gcloud_build_submit_command("us-central1-docker.pkg.dev/p/agents/a:latest");
/// assert_eq!(
///     command,
///     "gcloud builds submit --tag us-central1-docker.pkg.dev/p/agents/a:latest",
/// );
/// ```
pub fn gcloud_build_submit_command(image_uri: &str) -> String {
    format!("gcloud builds submit --tag {image_uri}")
}

/// Configuration for [`GcpDeployClient`].
#[derive(Debug, Clone)]
pub struct GcpDeployConfig {
    project_id: String,
    location: String,
    endpoint: Option<String>,
}

impl GcpDeployConfig {
    /// Creates a config for the given project and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self { project_id: project_id.into(), location: location.into(), endpoint: None }
    }

    /// Sets a custom API origin (loopback HTTP allowed for tests; anything
    /// else must be HTTPS).
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}-aiplatform.googleapis.com", self.location))
    }
}

// ── Wire DTOs (camelCase per the v1beta1 REST reference) ─────────────────

/// The `ReasoningEngine` resource sent to `reasoningEngines.create`.
///
/// Field placement follows the current REST surface: the image lives in
/// `spec.containerSpec`, while env vars, scaling, and resource limits live
/// in `spec.deploymentSpec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReasoningEngineRequest {
    /// Required display name of the engine.
    pub display_name: String,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Engine configuration.
    pub spec: ReasoningEngineSpec,
    /// Customer-managed encryption key (CMEK) securing the engine and all
    /// sub-resources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_spec: Option<EncryptionSpec>,
    /// Resource labels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,
}

impl CreateReasoningEngineRequest {
    /// Builds a BYOC create request with the WP1 class-method contract, the
    /// `google-adk` framework declaration, and the given container image.
    pub fn byoc(display_name: impl Into<String>, image_uri: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            description: None,
            spec: ReasoningEngineSpec {
                container_spec: Some(ContainerSpec { image_uri: image_uri.into(), port: None }),
                deployment_spec: None,
                class_methods: default_class_methods(),
                agent_framework: Some("google-adk".to_string()),
                service_account: None,
            },
            encryption_spec: None,
            labels: None,
        }
    }

    /// Sets the service account the engine runs as.
    #[must_use]
    pub fn with_service_account(mut self, service_account: impl Into<String>) -> Self {
        self.spec.service_account = Some(service_account.into());
        self
    }

    /// Secures the engine with a customer-managed encryption key.
    #[must_use]
    pub fn with_kms_key(mut self, kms_key_name: impl Into<String>) -> Self {
        self.encryption_spec = Some(EncryptionSpec { kms_key_name: kms_key_name.into() });
        self
    }

    /// Sets the deployment spec (env vars, scaling, resource limits).
    #[must_use]
    pub fn with_deployment_spec(mut self, deployment_spec: DeploymentSpec) -> Self {
        self.spec.deployment_spec = Some(deployment_spec);
        self
    }
}

/// `spec` of a ReasoningEngine (BYOC subset).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEngineSpec {
    /// The container image to run (the BYOC `deployment_source`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_spec: Option<ContainerSpec>,
    /// Runtime deployment configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_spec: Option<DeploymentSpec>,
    /// Class-method declarations the host dispatches on. Defaults to the
    /// WP1 contract via [`default_class_methods`].
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub class_methods: Vec<Value>,
    /// The agent framework declaration (`"google-adk"` for adk-rust — the
    /// runtime contract is the ADK one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_framework: Option<String>,
    /// Service account the engine artifact runs as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
}

/// `spec.containerSpec` — the container image and port.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSpec {
    /// Artifact Registry image URI.
    pub image_uri: String,
    /// Port the container listens on; the platform defaults to 8080.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,
}

/// `spec.deploymentSpec` — env vars, scaling, and resource limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSpec {
    /// Plain environment variables.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub env: Vec<EnvVar>,
    /// Environment variables sourced from Secret Manager.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub secret_env: Vec<SecretEnvVar>,
    /// PSC interface configuration (private networking). Passed through
    /// opaquely — compose it per the REST reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub psc_interface_config: Option<Value>,
    /// Container resource limits; only `cpu` and `memory` keys are
    /// supported (platform default `{"cpu": "4", "memory": "4Gi"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<BTreeMap<String, String>>,
    /// Minimum running instances (platform default 1, range 0–75).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_instances: Option<i32>,
    /// Maximum instances (platform default 100, range 1–1000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_instances: Option<i32>,
    /// Requests handled concurrently per container (platform default 9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_concurrency: Option<i32>,
}

/// A plain environment variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    /// Variable name.
    pub name: String,
    /// Variable value.
    pub value: String,
}

/// An environment variable sourced from Cloud Secret Manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretEnvVar {
    /// Variable name.
    pub name: String,
    /// The secret providing the value.
    pub secret_ref: SecretRef,
}

/// Reference to a Secret Manager secret version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    /// Secret name.
    pub secret: String,
    /// Secret version (`"latest"`, an integer, or a version alias).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Customer-managed encryption key spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionSpec {
    /// Full Cloud KMS key resource name.
    pub kms_key_name: String,
}

/// A `ReasoningEngine` resource as returned by get/create.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEngine {
    /// Full resource name
    /// (`projects/{p}/locations/{l}/reasoningEngines/{id}`).
    pub name: String,
    /// Display name.
    #[serde(default)]
    pub display_name: String,
    /// Creation timestamp (RFC 3339).
    #[serde(default)]
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    #[serde(default)]
    pub update_time: Option<String>,
}

/// A long-running operation returned by create/delete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// Operation resource name.
    pub name: String,
    /// Whether the operation has finished.
    #[serde(default)]
    pub done: bool,
    /// Terminal error, when the operation failed.
    #[serde(default)]
    pub error: Option<OperationError>,
    /// Terminal response, when the operation succeeded.
    #[serde(default)]
    pub response: Option<Value>,
}

/// The `google.rpc.Status` carried by a failed operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationError {
    /// Canonical gRPC status code.
    #[serde(default)]
    pub code: i64,
    /// Human-readable message.
    #[serde(default)]
    pub message: String,
}

/// The WP1 class-method contract as `classMethods` declarations, matching
/// what a container built on `adk_server::agent_engine` actually serves.
///
/// Entry shape `{"name": ..., "api_mode": ...}` follows the BYOC codelab's
/// Terraform. `register_operations` is declared too — the dispatch surface
/// serves it and hosts may call it for discovery.
pub fn default_class_methods() -> Vec<Value> {
    const METHODS: [(&str, &str); 14] = [
        ("create_session", ""),
        ("get_session", ""),
        ("list_sessions", ""),
        ("delete_session", ""),
        ("register_operations", ""),
        ("async_create_session", "async"),
        ("async_get_session", "async"),
        ("async_list_sessions", "async"),
        ("async_delete_session", "async"),
        ("async_add_session_to_memory", "async"),
        ("async_search_memory", "async"),
        ("stream_query", "stream"),
        ("async_stream_query", "async_stream"),
        ("streaming_agent_run_with_events", "async_stream"),
    ];
    METHODS.iter().map(|(name, api_mode)| json!({ "name": name, "api_mode": api_mode })).collect()
}

// ── Client ────────────────────────────────────────────────────────────────

// The machine-readable codes the shared adk-gcp transport stamps on this
// client's failures, following the workspace `<component>.<backend>.<failure>`
// convention.
const GCP_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "deploy.gcp.invalid_input",
    unauthorized: "deploy.gcp.unauthorized",
    forbidden: "deploy.gcp.forbidden",
    not_found: "deploy.gcp.not_found",
    rate_limited: "deploy.gcp.rate_limited",
    timeout: "deploy.gcp.timeout",
    unavailable: "deploy.gcp.unavailable",
    credentials_unavailable: "deploy.gcp.credentials_unavailable",
    invalid_response: "deploy.gcp.invalid_response",
    invalid_request: "deploy.gcp.invalid_request",
    upstream_error: "deploy.gcp.upstream_error",
    operation_failed: "deploy.gcp.operation_failed",
};

/// Minimal Agent Engine deployment client (create, poll, get, delete).
pub struct GcpDeployClient {
    gcp: GcpHttpClient,
    project_id: String,
    location: String,
}

impl GcpDeployClient {
    /// Creates a client using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not
    /// a valid secure origin, or the HTTP client cannot be built.
    pub fn new_with_adc(config: GcpDeployConfig) -> DeployResult<Self> {
        Self::build(config, None)
    }

    /// Creates a client with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or
    /// the redirect-disabled HTTP client cannot be built.
    pub fn with_credentials(
        config: GcpDeployConfig,
        credentials: Credentials,
    ) -> DeployResult<Self> {
        Self::build(config, Some(credentials))
    }

    fn build(config: GcpDeployConfig, credentials: Option<Credentials>) -> DeployResult<Self> {
        let errors = GcpErrorContext::new(ErrorComponent::Deploy, GCP_ERROR_CODES, "gcp deploy");
        let mut builder = GcpHttpClient::builder(errors, config.endpoint())
            .api_version(API_VERSION)
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .max_response_bytes(MAX_RESPONSE_BYTES);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        let gcp = builder.build().map_err(map_gcp_error)?;
        Ok(Self { gcp, project_id: config.project_id, location: config.location })
    }

    /// Creates a reasoning engine, returning the pending operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or the response is not an
    /// operation.
    pub async fn create_reasoning_engine(
        &self,
        request: &CreateReasoningEngineRequest,
    ) -> DeployResult<Operation> {
        info!(engine.display_name = %request.display_name, "creating reasoning engine");
        let http_request = self
            .gcp
            .request(Method::POST, &format!("{}/reasoningEngines", self.parent()))
            .await
            .map_err(map_gcp_error)?
            .json(request);
        let value = self.gcp.send_value(http_request).await.map_err(map_gcp_error)?;
        parse_operation(value)
    }

    /// Fetches the current state of a long-running operation (single poll).
    ///
    /// # Errors
    ///
    /// Returns an error when the name is outside this client's project and
    /// location or the request fails.
    pub async fn poll_operation(&self, operation_name: &str) -> DeployResult<Operation> {
        self.validate_operation_name(operation_name)?;
        let request = self.gcp.request(Method::GET, operation_name).await.map_err(map_gcp_error)?;
        let value = self.gcp.send_value(request).await.map_err(map_gcp_error)?;
        parse_operation(value)
    }

    /// Polls an operation to completion with backoff and returns its
    /// terminal response, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the operation fails, does not finish within
    /// the 15-minute deadline, or a poll changes operation identity.
    pub async fn wait_for_operation(&self, operation: Operation) -> DeployResult<Option<Value>> {
        let initial = serde_json::to_value(&operation).map_err(|error| DeployError::Client {
            message: format!("failed to serialize reasoning engine operation: {error}"),
        })?;
        LroPoller::new()
            .with_poll_timeout(OPERATION_POLL_TIMEOUT)
            .with_max_delay(OPERATION_POLL_MAX_DELAY)
            .wait_for_operation(
                &self.gcp,
                initial,
                "reasoning engine",
                false,
                &self.project_id,
                &self.location,
            )
            .await
            .map_err(map_gcp_error)
    }

    /// Fetches a reasoning engine by numeric ID or full resource name.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or the payload is not a
    /// reasoning engine.
    pub async fn get_reasoning_engine(&self, engine: &str) -> DeployResult<ReasoningEngine> {
        let name = self.engine_name(engine)?;
        let request = self.gcp.request(Method::GET, &name).await.map_err(map_gcp_error)?;
        let value = self.gcp.send_value(request).await.map_err(map_gcp_error)?;
        serde_json::from_value(value).map_err(|error| DeployError::Client {
            message: format!("failed to parse reasoning engine payload: {error}"),
        })
    }

    /// Deletes a reasoning engine, returning the pending operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or the response is not an
    /// operation.
    pub async fn delete_reasoning_engine(&self, engine: &str) -> DeployResult<Operation> {
        let name = self.engine_name(engine)?;
        info!(engine.name = %name, "deleting reasoning engine");
        let request = self.gcp.request(Method::DELETE, &name).await.map_err(map_gcp_error)?;
        let value = self.gcp.send_value(request).await.map_err(map_gcp_error)?;
        parse_operation(value)
    }

    fn parent(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }

    fn engine_name(&self, engine: &str) -> DeployResult<String> {
        if let Some(name) = VertexResourceName::parse(engine) {
            if name.project_id() == self.project_id
                && name.location() == self.location
                && is_numeric_engine_id(name.engine_id())
            {
                return Ok(engine.to_string());
            }
        } else if is_numeric_engine_id(engine) {
            return Ok(
                VertexResourceName::new(&self.project_id, &self.location, engine).to_string()
            );
        }
        Err(DeployError::Client {
            message: format!(
                "reasoning engine '{engine}' is invalid. Provide a numeric ID or the exact resource name '{}/reasoningEngines/<numeric-id>'",
                self.parent(),
            ),
        })
    }

    fn validate_operation_name(&self, name: &str) -> DeployResult<()> {
        if is_scoped_resource_name(name, &self.project_id, &self.location) {
            return Ok(());
        }
        Err(DeployError::Client {
            message: format!("operation name '{name}' does not belong to {}", self.parent()),
        })
    }
}

// Reasoning-engine IDs are validated as all-ASCII digits (leading zeros
// accepted), matching the pre-adk-gcp behavior; adk-gcp's canonical-ID check
// is stricter and would reject zero-padded IDs.
fn is_numeric_engine_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

// The public error surface of this crate is [`DeployError`], not
// [`AdkError`]: failures from the shared adk-gcp transport keep their
// message but cross the boundary as the same `Client` variant the previous
// in-crate transport produced.
fn map_gcp_error(error: AdkError) -> DeployError {
    DeployError::Client { message: error.message }
}

fn parse_operation(value: Value) -> DeployResult<Operation> {
    let operation: Operation =
        serde_json::from_value(value).map_err(|error| DeployError::Client {
            message: format!("failed to parse reasoning engine operation: {error}"),
        })?;
    if operation.name.trim().is_empty() {
        return Err(DeployError::Client {
            message: "reasoning engine response did not contain an operation name".to_string(),
        });
    }
    Ok(operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_methods_cover_the_wp1_contract_exactly_once() {
        let methods = default_class_methods();
        assert_eq!(methods.len(), 14);
        let names: Vec<&str> =
            methods.iter().map(|method| method["name"].as_str().unwrap()).collect();
        for name in [
            "create_session",
            "async_stream_query",
            "streaming_agent_run_with_events",
            "register_operations",
            "async_search_memory",
        ] {
            assert_eq!(names.iter().filter(|n| **n == name).count(), 1, "{name}");
        }
        // asyncQuery is deliberately absent (V12: cannot be added
        // post-create; AdkApp parity excludes it).
        assert!(!names.contains(&"async_query"));
    }

    // tokio::test: the google-cloud-auth credentials builder requires an
    // ambient async runtime even for construction.
    #[tokio::test]
    async fn engine_names_resolve_ids_and_reject_foreign_names() {
        let config = GcpDeployConfig::new("p", "l").with_endpoint("http://127.0.0.1:1");
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build();
        let client = GcpDeployClient::with_credentials(config, credentials).unwrap();
        assert_eq!(
            client.engine_name("123").unwrap(),
            "projects/p/locations/l/reasoningEngines/123",
        );
        assert_eq!(
            client.engine_name("projects/p/locations/l/reasoningEngines/123").unwrap(),
            "projects/p/locations/l/reasoningEngines/123",
        );
        assert!(client.engine_name("projects/other/locations/l/reasoningEngines/1").is_err());
        assert!(client.engine_name("my-agent").is_err());
    }
}
