//! Amazon ECS Express Mode deployment: client and deploy target.
//!
//! [`EcsExpressClient`] calls the four Express service operations of the Amazon ECS API —
//! `CreateExpressGatewayService`, `DescribeExpressGatewayService`,
//! `UpdateExpressGatewayService`, and `DeleteExpressGatewayService` — over the documented
//! JSON 1.1 protocol, signed with Signature Version 4. Credentials come from the AWS default
//! chain: environment, shared config and SSO profiles, web identity, and the ECS and EC2
//! instance metadata endpoints.
//!
//! [`EcsExpressTarget`] deploys a manifest through it. ECS Express Mode provisions the
//! Application Load Balancer, target group, security groups, TLS certificate, and autoscaling
//! policy, and serves the agent at `https://<name>.ecs.<region>.on.aws`.
//!
//! Building and pushing the image is out of scope; push it to Amazon ECR first:
//!
//! ```bash
//! docker build -f .adk-deploy/container/Dockerfile -t 123456789012.dkr.ecr.us-east-1.amazonaws.com/support:1.0.0 .
//! docker push 123456789012.dkr.ecr.us-east-1.amazonaws.com/support:1.0.0
//! ```
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_deploy::DeploymentManifest;
//! use adk_deploy::aws::EcsExpressTarget;
//! use adk_deploy::targets::{DeployRequest, DeployTarget};
//! use std::path::Path;
//!
//! # async fn deploy() -> adk_deploy::DeployResult<()> {
//! let manifest = DeploymentManifest::from_path(Path::new("adk-deploy.toml"))?;
//! let target = EcsExpressTarget::from_manifest(&manifest).await?;
//! let image = "123456789012.dkr.ecr.us-east-1.amazonaws.com/support:1.0.0";
//! let outcome = target.deploy(&DeployRequest::new(manifest, image)).await?;
//! println!("{} at {:?}", outcome.deployment_id, outcome.endpoint);
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use crate::targets::{
    DeployPlan, DeployRequest, DeployTarget, DeploymentOutcome, DeploymentStatus, PlannedResource,
    RuntimeOptions, RuntimeSpec,
};
use crate::{DeployError, DeployResult, DeploymentManifest};

const TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const CONTENT_TYPE: &str = "application/x-amz-json-1.1";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(900);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

// ── Wire types (camelCase per the Amazon ECS API reference) ──────────────

/// `CreateExpressGatewayService` request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CreateExpressGatewayServiceRequest {
    /// Cluster name or ARN (default: the `default` cluster).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    /// Service name, unique in the cluster.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Task execution role: pulls the image, writes logs, reads `secrets`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_role_arn: Option<String>,
    /// Infrastructure role ECS manages the load balancer and scaling with.
    pub infrastructure_role_arn: String,
    /// Role the agent's own AWS calls run as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_role_arn: Option<String>,
    /// Load balancer health check path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check_path: Option<String>,
    /// CPU units per task, such as `"512"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Memory per task in MiB, such as `"1024"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// `X86_64` or `ARM64`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_architecture: Option<String>,
    /// VPC subnets and security groups (default: the default VPC).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_configuration: Option<NetworkConfiguration>,
    /// The agent container.
    pub primary_container: ExpressGatewayContainer,
    /// Task count bounds and the scaling metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaling_target: Option<ScalingTarget>,
    /// Resource tags.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<Tag>,
}

/// `UpdateExpressGatewayService` request: a new revision rolled out without downtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct UpdateExpressGatewayServiceRequest {
    /// The service to update.
    pub service_arn: String,
    /// Task execution role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_role_arn: Option<String>,
    /// Role the agent's own AWS calls run as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_role_arn: Option<String>,
    /// Load balancer health check path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check_path: Option<String>,
    /// CPU units per task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Memory per task in MiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// `X86_64` or `ARM64`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_architecture: Option<String>,
    /// VPC subnets and security groups.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_configuration: Option<NetworkConfiguration>,
    /// The agent container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_container: Option<ExpressGatewayContainer>,
    /// Task count bounds and the scaling metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaling_target: Option<ScalingTarget>,
}

/// `ExpressGatewayContainer`: the container behind the load balancer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ExpressGatewayContainer {
    /// Image URI.
    pub image: String,
    /// Port the container listens on (platform default 80).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_port: Option<u16>,
    /// Arguments passed to the image's entrypoint.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub command: Vec<String>,
    /// Plain environment variables.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub environment: Vec<KeyValuePair>,
    /// Environment variables from Secrets Manager or Systems Manager Parameter Store.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub secrets: Vec<Secret>,
    /// CloudWatch Logs group and stream prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_logs_configuration: Option<AwsLogsConfiguration>,
}

/// A plain environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KeyValuePair {
    /// Variable name.
    pub name: String,
    /// Variable value.
    pub value: String,
}

/// An environment variable read from a secret at task start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Secret {
    /// Variable name.
    pub name: String,
    /// Secrets Manager secret ARN, or Parameter Store parameter ARN or name.
    pub value_from: String,
}

/// CloudWatch Logs settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AwsLogsConfiguration {
    /// Log group.
    pub log_group: String,
    /// Stream prefix.
    pub log_stream_prefix: String,
}

/// Subnets and security groups.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NetworkConfiguration {
    /// Security group IDs.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub security_groups: Vec<String>,
    /// Subnet IDs; public subnets give an internet-facing load balancer.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub subnets: Vec<String>,
}

/// `ExpressGatewayScalingTarget`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ScalingTarget {
    /// `AVERAGE_CPU`, `AVERAGE_MEMORY`, or `REQUEST_COUNT_PER_TARGET`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_scaling_metric: Option<String>,
    /// Target value of the metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_scaling_target_value: Option<u32>,
    /// Minimum running tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_task_count: Option<u32>,
    /// Maximum running tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_task_count: Option<u32>,
}

/// A resource tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tag {
    /// Tag key.
    pub key: String,
    /// Tag value.
    pub value: String,
}

/// An Express service as the API returns it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ExpressGatewayService {
    /// Service ARN.
    #[serde(default)]
    pub service_arn: String,
    /// Service name.
    #[serde(default)]
    pub service_name: String,
    /// Cluster ARN.
    #[serde(default)]
    pub cluster: Option<String>,
    /// `ACTIVE`, `DRAINING`, or `INACTIVE`, with a reason.
    #[serde(default)]
    pub status: Option<ServiceStatus>,
    /// The configurations serving traffic.
    #[serde(default)]
    pub active_configurations: Vec<ServiceConfiguration>,
    /// The configuration an update rolls out (update responses only).
    #[serde(default)]
    pub target_configuration: Option<ServiceConfiguration>,
    /// The deployment in progress.
    #[serde(default)]
    pub current_deployment: Option<String>,
}

impl ExpressGatewayService {
    /// The first ingress endpoint, as an `https://` URL.
    pub fn endpoint(&self) -> Option<String> {
        self.active_configurations
            .iter()
            .chain(self.target_configuration.iter())
            .flat_map(|configuration| configuration.ingress_paths.iter())
            .map(|path| path.endpoint.trim_end_matches('/'))
            .find(|endpoint| !endpoint.is_empty())
            .map(|endpoint| {
                if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                    endpoint.to_string()
                } else {
                    format!("https://{endpoint}")
                }
            })
    }
}

/// `ExpressGatewayServiceStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServiceStatus {
    /// `ACTIVE`, `DRAINING`, or `INACTIVE`.
    #[serde(default)]
    pub status_code: String,
    /// Why the service is in this state.
    #[serde(default)]
    pub status_reason: Option<String>,
}

/// One revision's configuration, with the endpoints it serves on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServiceConfiguration {
    /// Service revision ARN.
    #[serde(default)]
    pub service_revision_arn: Option<String>,
    /// Endpoints and their access type.
    #[serde(default)]
    pub ingress_paths: Vec<IngressPath>,
}

/// An endpoint the service answers on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IngressPath {
    /// `PUBLIC` or `PRIVATE`.
    #[serde(default)]
    pub access_type: String,
    /// Host name or URL.
    #[serde(default)]
    pub endpoint: String,
}

#[derive(Deserialize)]
struct ServiceEnvelope {
    service: ExpressGatewayService,
}

// ── Client ────────────────────────────────────────────────────────────────

/// Minimal Amazon ECS Express Mode client (create, describe, update, delete).
#[derive(Debug, Clone)]
pub struct EcsExpressClient {
    http: reqwest::Client,
    region: String,
    endpoint: String,
    credentials: SharedCredentialsProvider,
}

impl EcsExpressClient {
    /// Creates a client for `region` with credentials from the AWS default chain.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when no credentials provider is configured or the HTTP
    /// client cannot be built.
    pub async fn from_env(region: impl Into<String>) -> DeployResult<Self> {
        let region = region.into();
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.clone()))
            .load()
            .await;
        let credentials = config.credentials_provider().ok_or_else(|| DeployError::Client {
            message: "no AWS credentials provider is configured; set AWS_PROFILE, the \
                      AWS_ACCESS_KEY_ID pair, or run where an IAM role is attached"
                .to_string(),
        })?;
        Self::with_credentials(region, credentials)
    }

    /// Creates a client for `region` with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the HTTP client cannot be built.
    pub fn with_credentials(
        region: impl Into<String>,
        credentials: SharedCredentialsProvider,
    ) -> DeployResult<Self> {
        let region = region.into();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let endpoint = format!("https://ecs.{region}.amazonaws.com/");
        Ok(Self { http, region, endpoint, credentials })
    }

    /// Sends requests to `endpoint` instead of the regional ECS endpoint: loopback HTTP for
    /// tests, HTTPS otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when `endpoint` is neither HTTPS nor loopback HTTP.
    pub fn with_endpoint(mut self, endpoint: &str) -> DeployResult<Self> {
        let url = url::Url::parse(endpoint).map_err(|error| DeployError::Client {
            message: format!("ECS endpoint '{endpoint}' is not a URL: {error}"),
        })?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err(DeployError::Client {
                message: format!("ECS endpoint '{endpoint}' must be HTTPS (or loopback HTTP)"),
            });
        }
        self.endpoint = format!("{}/", endpoint.trim_end_matches('/'));
        Ok(self)
    }

    /// The region requests are signed for.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Creates an Express service.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] with the API's error type and message when ECS refuses
    /// the request.
    pub async fn create_express_gateway_service(
        &self,
        request: &CreateExpressGatewayServiceRequest,
    ) -> DeployResult<ExpressGatewayService> {
        info!(service.name = ?request.service_name, "creating ecs express service");
        self.service_call("CreateExpressGatewayService", request).await
    }

    /// Rolls out a new revision of an Express service.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when ECS refuses the request.
    pub async fn update_express_gateway_service(
        &self,
        request: &UpdateExpressGatewayServiceRequest,
    ) -> DeployResult<ExpressGatewayService> {
        info!(service.arn = %request.service_arn, "updating ecs express service");
        self.service_call("UpdateExpressGatewayService", request).await
    }

    /// Describes an Express service, or returns `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] for any failure but a missing service.
    pub async fn describe_express_gateway_service(
        &self,
        service_arn: &str,
    ) -> DeployResult<Option<ExpressGatewayService>> {
        let body = serde_json::json!({ "serviceArn": service_arn });
        match self.call("DescribeExpressGatewayService", &body).await {
            Ok(value) => parse_service(value).map(Some),
            Err(CallError::Api { kind, .. }) if is_not_found(&kind) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Deletes an Express service and the infrastructure ECS created for it.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when ECS refuses the request.
    pub async fn delete_express_gateway_service(
        &self,
        service_arn: &str,
    ) -> DeployResult<ExpressGatewayService> {
        info!(service.arn = %service_arn, "deleting ecs express service");
        self.service_call(
            "DeleteExpressGatewayService",
            &serde_json::json!({ "serviceArn": service_arn }),
        )
        .await
    }

    async fn service_call<T: Serialize>(
        &self,
        operation: &str,
        body: &T,
    ) -> DeployResult<ExpressGatewayService> {
        let value = self.call(operation, body).await?;
        parse_service(value)
    }

    async fn call<T: Serialize>(&self, operation: &str, body: &T) -> Result<Value, CallError> {
        let body = serde_json::to_vec(body).map_err(|error| {
            CallError::Transport(format!("failed to encode {operation}: {error}"))
        })?;
        let target = format!("{TARGET_PREFIX}.{operation}");
        let credentials = self.credentials.provide_credentials().await.map_err(|error| {
            CallError::Transport(format!("AWS credentials are unavailable: {error}"))
        })?;
        let identity = credentials.into();
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("ecs")
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|error| CallError::Transport(format!("failed to sign {operation}: {error}")))?
            .into();
        let headers = [("content-type", CONTENT_TYPE), ("x-amz-target", target.as_str())];
        let signable = SignableRequest::new(
            "POST",
            self.endpoint.as_str(),
            headers.iter().copied(),
            SignableBody::Bytes(&body),
        )
        .map_err(|error| CallError::Transport(format!("failed to sign {operation}: {error}")))?;
        let (instructions, _signature) = sign(signable, &params)
            .map_err(|error| CallError::Transport(format!("failed to sign {operation}: {error}")))?
            .into_parts();

        let mut request = self.http.post(&self.endpoint).body(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        for (name, value) in instructions.headers() {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CallError::Transport(format!("{operation} failed: {error}")))?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            CallError::Transport(format!("{operation} response failed: {error}"))
        })?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(CallError::Transport(format!(
                "{operation} response exceeds {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|error| {
                CallError::Transport(format!(
                    "{operation} returned invalid JSON ({status}): {error}"
                ))
            })?
        };
        if status.is_success() {
            return Ok(value);
        }
        // awsJson errors name their shape in `__type`, sometimes namespaced with `#`.
        let kind = value
            .get("__type")
            .and_then(Value::as_str)
            .map(|kind| kind.rsplit('#').next().unwrap_or(kind).to_string())
            .unwrap_or_else(|| format!("HTTP {status}"));
        let message = value
            .get("message")
            .or_else(|| value.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Err(CallError::Api { operation: operation.to_string(), kind, message })
    }
}

enum CallError {
    Api { operation: String, kind: String, message: String },
    Transport(String),
}

impl From<CallError> for DeployError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Api { operation, kind, message } => DeployError::Client {
                message: format!("ECS {operation} failed: {kind}: {message}"),
            },
            CallError::Transport(message) => DeployError::Client { message },
        }
    }
}

fn is_not_found(kind: &str) -> bool {
    matches!(kind, "ServiceNotFoundException" | "ResourceNotFoundException")
}

fn parse_service(value: Value) -> DeployResult<ExpressGatewayService> {
    serde_json::from_value::<ServiceEnvelope>(value).map(|envelope| envelope.service).map_err(
        |error| DeployError::Client {
            message: format!("ECS returned an unexpected service payload: {error}"),
        },
    )
}

// ── Deploy target ─────────────────────────────────────────────────────────

/// `[targets.ecs-express]` settings.
///
/// # Example
///
/// ```toml
/// [targets.ecs-express]
/// image = "123456789012.dkr.ecr.us-east-1.amazonaws.com/support:1.0.0"
/// region = "us-east-1"
/// executionRoleArn = "arn:aws:iam::123456789012:role/ecsTaskExecutionRole"
/// infrastructureRoleArn = "arn:aws:iam::123456789012:role/ecsInfrastructureRoleForExpressServices"
/// cpu = "512"
/// memory = "1024"
///
/// [targets.ecs-express.secrets]
/// ANTHROPIC_API_KEY = "arn:aws:secretsmanager:us-east-1:123456789012:secret:support/anthropic-AbCdEf"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct EcsExpressTargetConfig {
    /// Amazon ECR (or other registry) image URI.
    pub image: Option<String>,
    /// AWS region (default `$AWS_REGION`, then `$AWS_DEFAULT_REGION`).
    pub region: Option<String>,
    /// Cluster (default `default`).
    pub cluster: Option<String>,
    /// Service name (default: `[agent] name` as a DNS label).
    pub service_name: Option<String>,
    /// ARN of the service to update; without it, `deploy` creates the service.
    pub service_arn: Option<String>,
    /// Task execution role ARN (required).
    pub execution_role_arn: Option<String>,
    /// Infrastructure role ARN (required).
    pub infrastructure_role_arn: Option<String>,
    /// Task role ARN, for the agent's own AWS calls.
    pub task_role_arn: Option<String>,
    /// CPU units per task, such as `"512"`.
    pub cpu: Option<String>,
    /// Memory per task in MiB, such as `"1024"`.
    pub memory: Option<String>,
    /// `X86_64` (default) or `ARM64`.
    pub cpu_architecture: Option<String>,
    /// Subnet IDs (default: the default VPC's).
    #[serde(default)]
    pub subnets: Vec<String>,
    /// Security group IDs.
    #[serde(default)]
    pub security_groups: Vec<String>,
    /// Container port (default `8080`).
    pub port: Option<u16>,
    /// Arguments passed to the agent binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Public host names besides the `.ecs.<region>.on.aws` endpoint.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// `public` (default) or `internal`; internal needs private `subnets`.
    pub ingress: Option<crate::targets::Ingress>,
    /// CloudWatch Logs group.
    pub log_group: Option<String>,
    /// Secrets Manager secret ARN, or Parameter Store parameter, for each `[[secrets]]` key.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    /// Parameter Store path prefix for keys `secrets` does not map, such as `/support/`:
    /// `ANTHROPIC_API_KEY` reads the parameter `/support/ANTHROPIC_API_KEY`.
    pub ssm_parameter_prefix: Option<String>,
    /// Regional API endpoint override, for tests.
    pub endpoint: Option<String>,
}

/// Deploys to Amazon ECS Express Mode through [`EcsExpressClient`].
pub struct EcsExpressTarget {
    client: EcsExpressClient,
    config: EcsExpressTargetConfig,
    ready_timeout: Duration,
    poll_interval: Duration,
}

impl EcsExpressTarget {
    /// The target name.
    pub const NAME: &'static str = "ecs-express";

    /// Creates a target from `[targets.ecs-express]` with credentials from the AWS default
    /// chain.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when the table is invalid or no region is known,
    /// and the errors of [`EcsExpressClient::from_env`].
    pub async fn from_manifest(manifest: &DeploymentManifest) -> DeployResult<Self> {
        let config: EcsExpressTargetConfig = manifest.target_config(Self::NAME)?;
        let region = Self::region(&config)?;
        let mut client = EcsExpressClient::from_env(region).await?;
        if let Some(endpoint) = &config.endpoint {
            client = client.with_endpoint(endpoint)?;
        }
        Ok(Self::with_client(client, config))
    }

    /// Creates a target over an existing client.
    pub fn with_client(client: EcsExpressClient, config: EcsExpressTargetConfig) -> Self {
        Self {
            client,
            config,
            ready_timeout: DEFAULT_READY_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// Sets how long `deploy` waits for the service to serve, and how often it checks.
    #[must_use]
    pub fn with_polling(mut self, timeout: Duration, interval: Duration) -> Self {
        self.ready_timeout = timeout;
        self.poll_interval = interval;
        self
    }

    fn region(config: &EcsExpressTargetConfig) -> DeployResult<String> {
        config
            .region
            .clone()
            .or_else(|| std::env::var("AWS_REGION").ok())
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
            .ok_or_else(|| DeployError::InvalidManifest {
                message:
                    "target 'ecs-express' needs region in [targets.ecs-express] or $AWS_REGION"
                        .to_string(),
            })
    }

    fn runtime(&self, request: &DeployRequest) -> DeployResult<RuntimeSpec> {
        if request
            .manifest
            .services
            .iter()
            .any(|binding| binding.kind == crate::ServiceKind::Sqlite)
        {
            return Err(DeployError::InvalidManifest {
                message: "ECS Express Mode tasks have no persistent volume, so a SQLite store \
                          loses its sessions when a task is replaced: use postgres, redis, \
                          mongo-db, neo4j or firestore"
                    .to_string(),
            });
        }
        let mut hosts = self.config.allowed_hosts.clone();
        hosts.push(format!(".ecs.{}.on.aws", self.client.region()));
        let public = self.config.ingress != Some(crate::targets::Ingress::Internal);
        RuntimeSpec::from_manifest(
            &request.manifest,
            &request.image,
            &RuntimeOptions::new(Self::NAME)
                .with_port(self.config.port.unwrap_or(crate::targets::DEFAULT_PORT))
                .with_args(self.config.args.clone())
                .with_allowed_hosts(hosts)
                .with_public_ingress(public),
        )
    }

    fn required(value: &Option<String>, key: &str) -> DeployResult<String> {
        value.clone().ok_or_else(|| DeployError::InvalidManifest {
            message: format!("target 'ecs-express' needs {key} in [targets.ecs-express]"),
        })
    }

    fn container(&self, runtime: &RuntimeSpec) -> DeployResult<ExpressGatewayContainer> {
        let mut secrets = Vec::new();
        let mut unmapped = std::collections::BTreeSet::new();
        for (name, key) in &runtime.secret_env {
            let value_from = match (self.config.secrets.get(key), &self.config.ssm_parameter_prefix)
            {
                (Some(reference), _) => reference.clone(),
                (None, Some(prefix)) => format!("{prefix}{key}"),
                (None, None) => {
                    unmapped.insert(key.as_str());
                    continue;
                }
            };
            secrets.push(Secret { name: name.clone(), value_from });
        }
        if !unmapped.is_empty() {
            let unmapped: Vec<&str> = unmapped.into_iter().collect();
            return Err(DeployError::InvalidManifest {
                message: format!(
                    "target 'ecs-express' needs a Secrets Manager ARN or Parameter Store parameter \
                     for {}: add them to [targets.ecs-express.secrets], or set ssmParameterPrefix",
                    unmapped.join(", ")
                ),
            });
        }
        Ok(ExpressGatewayContainer {
            image: runtime.image.clone(),
            container_port: Some(runtime.port),
            command: runtime.args.clone(),
            environment: runtime
                .env
                .iter()
                .map(|(name, value)| KeyValuePair { name: name.clone(), value: value.clone() })
                .collect(),
            secrets,
            aws_logs_configuration: self.config.log_group.as_ref().map(|group| {
                AwsLogsConfiguration {
                    log_group: group.clone(),
                    log_stream_prefix: crate::targets::container::dns_label(&runtime.name, 63),
                }
            }),
        })
    }

    fn scaling(runtime: &RuntimeSpec) -> ScalingTarget {
        let (metric, value) = match runtime.scaling.target_concurrent_requests {
            Some(requests) => ("REQUEST_COUNT_PER_TARGET", requests),
            None => ("AVERAGE_CPU", u32::from(runtime.scaling.target_cpu_percent.unwrap_or(60))),
        };
        ScalingTarget {
            auto_scaling_metric: Some(metric.to_string()),
            auto_scaling_target_value: Some(value),
            min_task_count: Some(runtime.scaling.min_instances),
            max_task_count: Some(runtime.scaling.max_instances),
        }
    }

    fn network(&self) -> Option<NetworkConfiguration> {
        (!self.config.subnets.is_empty() || !self.config.security_groups.is_empty()).then(|| {
            NetworkConfiguration {
                security_groups: self.config.security_groups.clone(),
                subnets: self.config.subnets.clone(),
            }
        })
    }

    /// The `CreateExpressGatewayService` body for `runtime`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when a role ARN or a secret mapping is missing.
    pub fn create_request(
        &self,
        runtime: &RuntimeSpec,
    ) -> DeployResult<CreateExpressGatewayServiceRequest> {
        Ok(CreateExpressGatewayServiceRequest {
            cluster: self.config.cluster.clone(),
            service_name: Some(
                self.config
                    .service_name
                    .clone()
                    .unwrap_or_else(|| crate::targets::container::dns_label(&runtime.name, 255)),
            ),
            execution_role_arn: Some(Self::required(
                &self.config.execution_role_arn,
                "executionRoleArn",
            )?),
            infrastructure_role_arn: Self::required(
                &self.config.infrastructure_role_arn,
                "infrastructureRoleArn",
            )?,
            task_role_arn: self.config.task_role_arn.clone(),
            health_check_path: Some(runtime.health.path.clone()),
            cpu: self.config.cpu.clone(),
            memory: self.config.memory.clone(),
            cpu_architecture: self.config.cpu_architecture.clone(),
            network_configuration: self.network(),
            primary_container: self.container(runtime)?,
            scaling_target: Some(Self::scaling(runtime)),
            tags: vec![
                Tag { key: "adk:agent".to_string(), value: runtime.name.clone() },
                Tag { key: "adk:version".to_string(), value: runtime.version.clone() },
                Tag { key: "managed-by".to_string(), value: "adk-deploy".to_string() },
            ],
        })
    }

    /// The `UpdateExpressGatewayService` body for `runtime` on the service `service_arn`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when a role ARN or a secret mapping is missing.
    pub fn update_request(
        &self,
        service_arn: &str,
        runtime: &RuntimeSpec,
    ) -> DeployResult<UpdateExpressGatewayServiceRequest> {
        let create = self.create_request(runtime)?;
        Ok(UpdateExpressGatewayServiceRequest {
            service_arn: service_arn.to_string(),
            execution_role_arn: create.execution_role_arn,
            task_role_arn: create.task_role_arn,
            health_check_path: create.health_check_path,
            cpu: create.cpu,
            memory: create.memory,
            cpu_architecture: create.cpu_architecture,
            network_configuration: create.network_configuration,
            primary_container: Some(create.primary_container),
            scaling_target: create.scaling_target,
        })
    }

    async fn wait_until_serving(
        &self,
        service_arn: &str,
    ) -> DeployResult<(DeploymentStatus, Option<String>)> {
        let deadline = Instant::now() + self.ready_timeout;
        loop {
            let service = self.client.describe_express_gateway_service(service_arn).await?;
            let status = service.as_ref().map_or(DeploymentStatus::NotFound, service_status);
            let endpoint = service.as_ref().and_then(ExpressGatewayService::endpoint);
            match &status {
                DeploymentStatus::Ready
                | DeploymentStatus::Failed { .. }
                | DeploymentStatus::NotFound => {
                    return Ok((status, endpoint));
                }
                DeploymentStatus::Pending { detail } if Instant::now() >= deadline => {
                    return Err(DeployError::Client {
                        message: format!(
                            "ECS service {service_arn} did not serve within {}s: {detail}",
                            self.ready_timeout.as_secs()
                        ),
                    });
                }
                DeploymentStatus::Pending { .. } => tokio::time::sleep(self.poll_interval).await,
            }
        }
    }
}

/// Maps an Express service onto the target-neutral status: `ACTIVE` with an endpoint serves.
fn service_status(service: &ExpressGatewayService) -> DeploymentStatus {
    let (code, reason) = match &service.status {
        Some(status) => {
            (status.status_code.as_str(), status.status_reason.clone().unwrap_or_default())
        }
        None => ("", String::new()),
    };
    match code {
        "ACTIVE" if service.endpoint().is_some() => DeploymentStatus::Ready,
        "ACTIVE" => DeploymentStatus::Pending {
            detail: "waiting for the load balancer endpoint".to_string(),
        },
        "DRAINING" => DeploymentStatus::Pending { detail: format!("draining: {reason}") },
        "INACTIVE" => DeploymentStatus::NotFound,
        other => DeploymentStatus::Pending { detail: format!("status {other}: {reason}") },
    }
}

#[async_trait]
impl DeployTarget for EcsExpressTarget {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn plan(&self, request: &DeployRequest) -> DeployResult<DeployPlan> {
        let runtime = self.runtime(request)?;
        let (kind, name, body) = match &self.config.service_arn {
            Some(arn) => (
                "ecs:UpdateExpressGatewayService",
                arn.clone(),
                serde_json::to_string_pretty(&self.update_request(arn, &runtime)?),
            ),
            None => {
                let create = self.create_request(&runtime)?;
                (
                    "ecs:CreateExpressGatewayService",
                    create.service_name.clone().unwrap_or_default(),
                    serde_json::to_string_pretty(&create),
                )
            }
        };
        let body = body.map_err(|error| DeployError::Client {
            message: format!("failed to serialize the ECS request: {error}"),
        })?;
        Ok(DeployPlan::new(Self::NAME, runtime)
            .with_resource(PlannedResource::new(kind, name, body)))
    }

    async fn deploy(&self, request: &DeployRequest) -> DeployResult<DeploymentOutcome> {
        let runtime = self.runtime(request)?;
        let service = match &self.config.service_arn {
            Some(arn) => {
                self.client
                    .update_express_gateway_service(&self.update_request(arn, &runtime)?)
                    .await?
            }
            None => {
                self.client.create_express_gateway_service(&self.create_request(&runtime)?).await?
            }
        };
        let arn = service.service_arn.clone();
        if arn.is_empty() {
            return Err(DeployError::Client {
                message: "ECS returned a service without an ARN".to_string(),
            });
        }
        let (status, endpoint) = self.wait_until_serving(&arn).await?;
        Ok(DeploymentOutcome::new(Self::NAME, arn, endpoint, status))
    }

    async fn status(&self, deployment_id: &str) -> DeployResult<DeploymentStatus> {
        Ok(self
            .client
            .describe_express_gateway_service(deployment_id)
            .await?
            .as_ref()
            .map_or(DeploymentStatus::NotFound, service_status))
    }

    async fn destroy(&self, deployment_id: &str) -> DeployResult<()> {
        self.client.delete_express_gateway_service(deployment_id).await?;
        Ok(())
    }
}
