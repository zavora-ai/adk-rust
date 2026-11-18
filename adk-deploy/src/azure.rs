//! Azure Container Apps deployment: client and deploy target.
//!
//! [`ContainerAppsClient`] creates or updates (`PUT`), reads (`GET`), and deletes (`DELETE`)
//! `Microsoft.App/containerApps` resources through the Azure Resource Manager REST API.
//! Tokens for `https://management.azure.com/.default` come from `DefaultAzureCredential`:
//! environment service principals, workload identity (GitHub Actions OIDC), managed identity,
//! and the Azure CLI. A pipeline can pass a token directly with
//! [`ContainerAppsClient::with_static_token`].
//!
//! [`ContainerAppsTarget`] deploys a manifest through it. Secrets are Key Vault references the
//! app reads with a user-assigned managed identity, so no secret value passes through the
//! deployment.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_deploy::DeploymentManifest;
//! use adk_deploy::azure::ContainerAppsTarget;
//! use adk_deploy::targets::{DeployRequest, DeployTarget};
//! use std::path::Path;
//!
//! # async fn deploy() -> adk_deploy::DeployResult<()> {
//! let manifest = DeploymentManifest::from_path(Path::new("adk-deploy.toml"))?;
//! let target = ContainerAppsTarget::from_manifest(&manifest)?;
//! let image = "acme.azurecr.io/support-desk:1.0.0";
//! let outcome = target.deploy(&DeployRequest::new(manifest, image)).await?;
//! println!("{} at {:?}", outcome.deployment_id, outcome.endpoint);
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use crate::targets::container::dns_label;
use crate::targets::{
    DeployPlan, DeployRequest, DeployTarget, DeploymentOutcome, DeploymentStatus, Ingress,
    PlannedResource, RuntimeOptions, RuntimeSpec,
};
use crate::{DeployError, DeployResult, DeploymentManifest};

/// The Container Apps API version the target uses unless `apiVersion` overrides it.
pub const DEFAULT_API_VERSION: &str = "2024-03-01";
const ARM_ENDPOINT: &str = "https://management.azure.com";
const ARM_SCOPE: &str = "https://management.azure.com/.default";
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(900);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
enum TokenSource {
    Credential(Arc<dyn TokenCredential>),
    Static(String),
}

/// Minimal Azure Resource Manager client for `Microsoft.App/containerApps`.
#[derive(Debug, Clone)]
pub struct ContainerAppsClient {
    http: reqwest::Client,
    endpoint: String,
    api_version: String,
    token: TokenSource,
}

impl ContainerAppsClient {
    /// Creates a client authenticating with `DefaultAzureCredential`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the credential chain or the HTTP client cannot be
    /// built.
    pub fn from_env() -> DeployResult<Self> {
        let credential = azure_identity::DefaultAzureCredential::new().map_err(|error| {
            DeployError::Client { message: format!("Azure credentials are unavailable: {error}") }
        })?;
        Self::build(TokenSource::Credential(credential))
    }

    /// Creates a client authenticating with `credential`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the HTTP client cannot be built.
    pub fn with_credential(credential: Arc<dyn TokenCredential>) -> DeployResult<Self> {
        Self::build(TokenSource::Credential(credential))
    }

    /// Creates a client that sends `token` as its bearer token, such as the output of
    /// `az account get-access-token --resource https://management.azure.com/`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the HTTP client cannot be built.
    pub fn with_static_token(token: impl Into<String>) -> DeployResult<Self> {
        Self::build(TokenSource::Static(token.into()))
    }

    fn build(token: TokenSource) -> DeployResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            endpoint: ARM_ENDPOINT.to_string(),
            api_version: DEFAULT_API_VERSION.to_string(),
            token,
        })
    }

    /// Sends requests to `endpoint` instead of Azure Resource Manager: loopback HTTP for tests,
    /// HTTPS otherwise (sovereign clouds).
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when `endpoint` is neither HTTPS nor loopback HTTP.
    pub fn with_endpoint(mut self, endpoint: &str) -> DeployResult<Self> {
        let url = url::Url::parse(endpoint).map_err(|error| DeployError::Client {
            message: format!("ARM endpoint '{endpoint}' is not a URL: {error}"),
        })?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err(DeployError::Client {
                message: format!("ARM endpoint '{endpoint}' must be HTTPS (or loopback HTTP)"),
            });
        }
        self.endpoint = endpoint.trim_end_matches('/').to_string();
        Ok(self)
    }

    /// Sets the `api-version` query parameter.
    #[must_use]
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// Creates or replaces the container app `resource_id` with `body`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] with the ARM error code and message when the request is
    /// refused.
    pub async fn put_container_app(&self, resource_id: &str, body: &Value) -> DeployResult<Value> {
        info!(resource.id = %resource_id, "creating or updating container app");
        let (_, value) = self.send(Method::PUT, resource_id, Some(body)).await?;
        Ok(value)
    }

    /// Reads the container app `resource_id`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] for any failure but a missing app.
    pub async fn get_container_app(&self, resource_id: &str) -> DeployResult<Option<Value>> {
        match self.send(Method::GET, resource_id, None).await {
            Ok((_, value)) => Ok(Some(value)),
            Err(ArmError::NotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Deletes the container app `resource_id`; a missing app is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the request is refused.
    pub async fn delete_container_app(&self, resource_id: &str) -> DeployResult<()> {
        info!(resource.id = %resource_id, "deleting container app");
        match self.send(Method::DELETE, resource_id, None).await {
            Ok(_) | Err(ArmError::NotFound) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn send(
        &self,
        method: Method,
        resource_id: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), ArmError> {
        let token = match &self.token {
            TokenSource::Static(token) => token.clone(),
            TokenSource::Credential(credential) => credential
                .get_token(&[ARM_SCOPE])
                .await
                .map_err(|error| {
                    ArmError::Transport(format!("Azure token request failed: {error}"))
                })?
                .token
                .secret()
                .to_string(),
        };
        let url = format!("{}{resource_id}?api-version={}", self.endpoint, self.api_version);
        let mut request = self.http.request(method.clone(), &url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|error| {
            ArmError::Transport(format!("{method} {resource_id} failed: {error}"))
        })?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(|error| {
            ArmError::Transport(format!("{method} {resource_id} response failed: {error}"))
        })?;
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        if (200..300).contains(&status) {
            return Ok((status, value));
        }
        if status == 404 {
            return Err(ArmError::NotFound);
        }
        let code =
            value.pointer("/error/code").and_then(Value::as_str).unwrap_or("Unknown").to_string();
        let message =
            value.pointer("/error/message").and_then(Value::as_str).unwrap_or_default().to_string();
        Err(ArmError::Api { method: method.to_string(), status, code, message })
    }
}

enum ArmError {
    NotFound,
    Api { method: String, status: u16, code: String, message: String },
    Transport(String),
}

impl From<ArmError> for DeployError {
    fn from(error: ArmError) -> Self {
        let message = match error {
            ArmError::NotFound => "the container app does not exist".to_string(),
            ArmError::Api { method, status, code, message } => {
                format!("Azure Resource Manager {method} failed ({status}): {code}: {message}")
            }
            ArmError::Transport(message) => message,
        };
        DeployError::Client { message }
    }
}

/// `[targets.azure-container-apps]` settings.
///
/// # Example
///
/// ```toml
/// [targets.azure-container-apps]
/// image = "acme.azurecr.io/support-desk:1.0.0"
/// resourceGroup = "agents"
/// location = "eastus"
/// environmentId = "/subscriptions/0000/resourceGroups/agents/providers/Microsoft.App/managedEnvironments/agents"
/// identity = "/subscriptions/0000/resourceGroups/agents/providers/Microsoft.ManagedIdentity/userAssignedIdentities/support-desk"
/// keyVaultUrl = "https://acme-agents.vault.azure.net"
/// registryServer = "acme.azurecr.io"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct ContainerAppsTargetConfig {
    /// Image reference.
    pub image: Option<String>,
    /// Subscription ID (default `$AZURE_SUBSCRIPTION_ID`).
    pub subscription_id: Option<String>,
    /// Resource group (required).
    pub resource_group: Option<String>,
    /// Azure region, such as `eastus` (required).
    pub location: Option<String>,
    /// Resource ID of the Container Apps environment (required).
    pub environment_id: Option<String>,
    /// Container app name (default: `[agent] name`, at most 32 characters).
    pub name: Option<String>,
    /// Resource ID of the user-assigned managed identity that reads Key Vault and pulls the
    /// image.
    pub identity: Option<String>,
    /// Key Vault URL, such as `https://acme-agents.vault.azure.net`.
    pub key_vault_url: Option<String>,
    /// Key Vault secret name (or full secret URL) for each `[[secrets]]` key; default: the key
    /// lower-cased with `_` as `-`.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    /// Registry the identity pulls from, such as `acme.azurecr.io`.
    pub registry_server: Option<String>,
    /// CPU cores per replica, such as `0.5`.
    pub cpu: Option<f64>,
    /// Memory per replica, such as `"1Gi"`.
    pub memory: Option<String>,
    /// Container port (default `8080`).
    pub port: Option<u16>,
    /// Arguments passed to the agent binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Public host names besides the `.azurecontainerapps.io` default domain.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// `public` (default: external ingress) or `internal` (environment-internal ingress).
    pub ingress: Option<Ingress>,
    /// Container Apps API version (default [`DEFAULT_API_VERSION`]).
    pub api_version: Option<String>,
    /// Azure Resource Manager endpoint override, for sovereign clouds and tests.
    pub endpoint: Option<String>,
}

/// Deploys to Azure Container Apps through [`ContainerAppsClient`].
pub struct ContainerAppsTarget {
    client: ContainerAppsClient,
    config: ContainerAppsTargetConfig,
    ready_timeout: Duration,
    poll_interval: Duration,
}

impl ContainerAppsTarget {
    /// The target name.
    pub const NAME: &'static str = "azure-container-apps";

    /// Creates a target from `[targets.azure-container-apps]`, authenticating with
    /// `$AZURE_ACCESS_TOKEN` when it is set and `DefaultAzureCredential` otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when the table is invalid, and the errors of
    /// [`ContainerAppsClient::from_env`].
    pub fn from_manifest(manifest: &DeploymentManifest) -> DeployResult<Self> {
        let config: ContainerAppsTargetConfig = manifest.target_config(Self::NAME)?;
        let mut client = match std::env::var("AZURE_ACCESS_TOKEN") {
            Ok(token) if !token.is_empty() => ContainerAppsClient::with_static_token(token)?,
            _ => ContainerAppsClient::from_env()?,
        };
        if let Some(endpoint) = &config.endpoint {
            client = client.with_endpoint(endpoint)?;
        }
        if let Some(api_version) = &config.api_version {
            client = client.with_api_version(api_version);
        }
        Ok(Self::with_client(client, config))
    }

    /// Creates a target over an existing client.
    pub fn with_client(client: ContainerAppsClient, config: ContainerAppsTargetConfig) -> Self {
        Self {
            client,
            config,
            ready_timeout: DEFAULT_READY_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// Sets how long `deploy` waits for the app to provision, and how often it checks.
    #[must_use]
    pub fn with_polling(mut self, timeout: Duration, interval: Duration) -> Self {
        self.ready_timeout = timeout;
        self.poll_interval = interval;
        self
    }

    fn required<'a>(value: &'a Option<String>, key: &str) -> DeployResult<&'a str> {
        value.as_deref().ok_or_else(|| DeployError::InvalidManifest {
            message: format!(
                "target 'azure-container-apps' needs {key} in [targets.azure-container-apps]"
            ),
        })
    }

    /// The ARM resource ID of the container app `manifest` deploys.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when the subscription or resource group is
    /// unknown.
    pub fn resource_id(&self, manifest: &DeploymentManifest) -> DeployResult<String> {
        let subscription = self
            .config
            .subscription_id
            .clone()
            .or_else(|| std::env::var("AZURE_SUBSCRIPTION_ID").ok())
            .ok_or_else(|| DeployError::InvalidManifest {
                message: "target 'azure-container-apps' needs subscriptionId in \
                          [targets.azure-container-apps] or $AZURE_SUBSCRIPTION_ID"
                    .to_string(),
            })?;
        let group = Self::required(&self.config.resource_group, "resourceGroup")?;
        Ok(format!(
            "/subscriptions/{subscription}/resourceGroups/{group}/providers/Microsoft.App/containerApps/{}",
            self.app_name(manifest)
        ))
    }

    fn app_name(&self, manifest: &DeploymentManifest) -> String {
        let name = self.config.name.clone().unwrap_or_else(|| dns_label(&manifest.agent.name, 32));
        // Names start with a letter.
        if name.starts_with(|c: char| c.is_ascii_lowercase()) {
            name
        } else {
            format!("a{}", &name[..name.len().min(31)])
        }
    }

    fn ingress(&self) -> Ingress {
        self.config.ingress.unwrap_or(Ingress::Public)
    }

    fn runtime(&self, request: &DeployRequest) -> DeployResult<RuntimeSpec> {
        if request
            .manifest
            .services
            .iter()
            .any(|binding| binding.kind == crate::ServiceKind::Sqlite)
        {
            return Err(DeployError::InvalidManifest {
                message: "Container Apps replicas have no persistent block volume for SQLite: use \
                          postgres, redis, mongo-db, neo4j or firestore"
                    .to_string(),
            });
        }
        let mut hosts = self.config.allowed_hosts.clone();
        hosts.push(".azurecontainerapps.io".to_string());
        RuntimeSpec::from_manifest(
            &request.manifest,
            &request.image,
            &RuntimeOptions::new(Self::NAME)
                .with_port(self.config.port.unwrap_or(crate::targets::DEFAULT_PORT))
                .with_args(self.config.args.clone())
                .with_allowed_hosts(hosts)
                .with_public_ingress(self.ingress() == Ingress::Public),
        )
    }

    /// The Container Apps secret name for a `[[secrets]]` key: lower-case alphanumerics and `-`.
    fn app_secret_name(key: &str) -> String {
        dns_label(key, 253)
    }

    /// The `PUT` body for `runtime`.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when a required setting is missing, secrets are
    /// read without a Key Vault and identity, or `[health]` is outside the probe limits.
    pub fn app_body(&self, runtime: &RuntimeSpec) -> DeployResult<Value> {
        let location = Self::required(&self.config.location, "location")?;
        let environment = Self::required(&self.config.environment_id, "environmentId")?;
        let health = &runtime.health;
        if !(1..=240).contains(&health.interval_secs)
            || !(1..=240).contains(&health.timeout_secs)
            || !(1..=10).contains(&health.failure_threshold)
        {
            return Err(DeployError::InvalidManifest {
                message: "Container Apps probes take intervalSecs and timeoutSecs from 1 to 240 \
                          and failureThreshold from 1 to 10: adjust [health]"
                    .to_string(),
            });
        }

        let keys: BTreeSet<&str> = runtime.secret_keys();
        let mut secrets = Vec::new();
        if !keys.is_empty() {
            let identity = Self::required(
                &self.config.identity,
                "identity (a user-assigned managed identity that reads the Key Vault)",
            )?;
            for key in &keys {
                let reference = self
                    .config
                    .secrets
                    .get(*key)
                    .cloned()
                    .unwrap_or_else(|| key.to_ascii_lowercase().replace('_', "-"));
                let url = if reference.starts_with("https://") {
                    reference
                } else {
                    let vault = Self::required(&self.config.key_vault_url, "keyVaultUrl")?;
                    format!("{}/secrets/{reference}", vault.trim_end_matches('/'))
                };
                secrets.push(json!({ "name": Self::app_secret_name(key), "keyVaultUrl": url, "identity": identity }));
            }
        }

        let mut env: Vec<Value> = runtime
            .env
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        env.extend(
            runtime.secret_env.iter().map(
                |(name, key)| json!({ "name": name, "secretRef": Self::app_secret_name(key) }),
            ),
        );
        let probe = |kind: &str| {
            json!({
                "type": kind,
                "httpGet": { "path": health.path, "port": runtime.port },
                "periodSeconds": health.interval_secs,
                "timeoutSeconds": health.timeout_secs,
                "failureThreshold": health.failure_threshold,
            })
        };
        let mut container = json!({
            "name": "agent",
            "image": runtime.image,
            "env": env,
            "probes": [probe("Liveness"), probe("Readiness")],
        });
        if !runtime.args.is_empty() {
            container["args"] = json!(runtime.args);
        }
        if let (Some(cpu), Some(memory)) = (self.config.cpu, &self.config.memory) {
            container["resources"] = json!({ "cpu": cpu, "memory": memory });
        }
        let mut scale = json!({
            "minReplicas": runtime.scaling.min_instances,
            "maxReplicas": runtime.scaling.max_instances,
        });
        if let Some(requests) = runtime.scaling.target_concurrent_requests {
            scale["rules"] = json!([{
                "name": "http-concurrency",
                "http": { "metadata": { "concurrentRequests": requests.to_string() } },
            }]);
        }
        let mut configuration = json!({
            "activeRevisionsMode": "Single",
            "ingress": {
                "external": self.ingress() == Ingress::Public,
                "targetPort": runtime.port,
                "transport": "auto",
                "allowInsecure": false,
            },
        });
        if !secrets.is_empty() {
            configuration["secrets"] = Value::Array(secrets);
        }
        if let Some(server) = &self.config.registry_server {
            let identity =
                Self::required(&self.config.identity, "identity (to pull from registryServer)")?;
            configuration["registries"] = json!([{ "server": server, "identity": identity }]);
        }
        let mut body = json!({
            "location": location,
            "tags": {
                "adk-agent": runtime.name,
                "adk-version": runtime.version,
                "managed-by": "adk-deploy",
            },
            "properties": {
                "environmentId": environment,
                "configuration": configuration,
                "template": { "containers": [container], "scale": scale },
            },
        });
        if let Some(identity) = &self.config.identity {
            let mut identities = serde_json::Map::new();
            identities.insert(identity.clone(), json!({}));
            body["identity"] =
                json!({ "type": "UserAssigned", "userAssignedIdentities": identities });
        }
        Ok(body)
    }

    async fn wait_until_provisioned(
        &self,
        resource_id: &str,
    ) -> DeployResult<(DeploymentStatus, Option<String>)> {
        let deadline = Instant::now() + self.ready_timeout;
        loop {
            let app = self.client.get_container_app(resource_id).await?;
            let status = app.as_ref().map_or(DeploymentStatus::NotFound, app_status);
            match &status {
                DeploymentStatus::Pending { detail } if Instant::now() >= deadline => {
                    return Err(DeployError::Client {
                        message: format!(
                            "container app {resource_id} did not provision within {}s: {detail}",
                            self.ready_timeout.as_secs()
                        ),
                    });
                }
                DeploymentStatus::Pending { .. } => tokio::time::sleep(self.poll_interval).await,
                DeploymentStatus::Ready
                | DeploymentStatus::Failed { .. }
                | DeploymentStatus::NotFound => {
                    return Ok((status, app.as_ref().and_then(app_endpoint)));
                }
            }
        }
    }
}

/// `https://` plus the app's ingress FQDN, when it has one.
fn app_endpoint(app: &Value) -> Option<String> {
    ["/properties/configuration/ingress/fqdn", "/properties/latestRevisionFqdn"]
        .iter()
        .filter_map(|pointer| app.pointer(pointer).and_then(Value::as_str))
        .find(|fqdn| !fqdn.is_empty())
        .map(|fqdn| format!("https://{fqdn}"))
}

fn app_status(app: &Value) -> DeploymentStatus {
    let state =
        app.pointer("/properties/provisioningState").and_then(Value::as_str).unwrap_or_default();
    let running =
        app.pointer("/properties/runningStatus").and_then(Value::as_str).unwrap_or_default();
    match state {
        "Succeeded" if running.is_empty() || running == "Running" => DeploymentStatus::Ready,
        "Succeeded" => DeploymentStatus::Pending { detail: format!("provisioned, {running}") },
        "Failed" | "Canceled" => DeploymentStatus::Failed {
            reason: format!("provisioning {}", state.to_ascii_lowercase()),
        },
        other => DeploymentStatus::Pending { detail: format!("provisioning {other}") },
    }
}

#[async_trait]
impl DeployTarget for ContainerAppsTarget {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn plan(&self, request: &DeployRequest) -> DeployResult<DeployPlan> {
        let runtime = self.runtime(request)?;
        let body = serde_json::to_string_pretty(&self.app_body(&runtime)?).map_err(|error| {
            DeployError::Client {
                message: format!("failed to serialize the container app: {error}"),
            }
        })?;
        Ok(DeployPlan::new(Self::NAME, runtime).with_resource(PlannedResource::new(
            "arm:PUT Microsoft.App/containerApps",
            self.resource_id(&request.manifest)?,
            body,
        )))
    }

    async fn deploy(&self, request: &DeployRequest) -> DeployResult<DeploymentOutcome> {
        let runtime = self.runtime(request)?;
        let resource_id = self.resource_id(&request.manifest)?;
        self.client.put_container_app(&resource_id, &self.app_body(&runtime)?).await?;
        let (status, endpoint) = self.wait_until_provisioned(&resource_id).await?;
        Ok(DeploymentOutcome::new(Self::NAME, resource_id, endpoint, status))
    }

    async fn status(&self, deployment_id: &str) -> DeployResult<DeploymentStatus> {
        Ok(self
            .client
            .get_container_app(deployment_id)
            .await?
            .as_ref()
            .map_or(DeploymentStatus::NotFound, app_status))
    }

    async fn destroy(&self, deployment_id: &str) -> DeployResult<()> {
        self.client.delete_container_app(deployment_id).await
    }
}
