//! Provider-neutral deploy targets.
//!
//! A [`DeployTarget`] turns a [`DeploymentManifest`] and a container image into a running
//! deployment on one provider. Every target first derives the same [`RuntimeSpec`] from the
//! manifest with [`RuntimeSpec::from_manifest`], which is where the platform's guarantees are
//! enforced, so no target can deploy an agent that breaks them:
//!
//! | Guarantee | Enforced by |
//! |-----------|-------------|
//! | Secrets come from the provider's secret store | A literal `[env]` value under a secret-bearing name (`*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`) and a connection URL carrying a password are refused; secret references become provider secret references |
//! | Durable stores are configured explicitly | A session store binding is required; `managed` bindings are refused on targets that cannot provision them; SQLite is refused beyond one replica; `in-memory` is accepted only when declared, with a warning |
//! | The kill switch and runs stay authenticated | A target with public ingress refuses a manifest without `[auth]` in `bearer` or `oidc` mode, and [`verify_endpoint`] fails a deployment that answers unauthenticated requests |
//! | Health and readiness endpoints | `[health]` becomes the provider's liveness and readiness probes |
//!
//! The container reads its configuration from the environment variables in [`contract`].
//!
//! # Example
//!
//! ```
//! use adk_deploy::DeploymentManifest;
//! use adk_deploy::targets::{RuntimeOptions, RuntimeSpec};
//!
//! let manifest: DeploymentManifest = toml::from_str(
//!     r#"
//!     [agent]
//!     name = "support"
//!     binary = "support"
//!
//!     [[services]]
//!     name = "sessions"
//!     kind = "postgres"
//!     mode = "external"
//!     secretRef = "DATABASE_URL"
//!
//!     [[secrets]]
//!     key = "DATABASE_URL"
//!
//!     [[secrets]]
//!     key = "ANTHROPIC_API_KEY"
//!
//!     [env]
//!     ANTHROPIC_API_KEY = { secretRef = "ANTHROPIC_API_KEY" }
//!     "#,
//! )
//! .unwrap();
//! let runtime = RuntimeSpec::from_manifest(
//!     &manifest,
//!     "ghcr.io/acme/support:1.0.0",
//!     &RuntimeOptions::new("container"),
//! )
//! .unwrap();
//! assert_eq!(runtime.secret_env["ADK_SERVICE_SESSIONS_URL"], "DATABASE_URL");
//! assert_eq!(runtime.env["BIND_HOST"], "0.0.0.0");
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use crate::{
    AuthModeSpec, BindingMode, DeployError, DeployResult, DeploymentManifest, EnvVarSpec,
    HealthCheckConfig, ScalingPolicy, ServiceKind,
};

pub mod container;
mod yaml;

/// Environment variables every deploy target sets on the agent container.
///
/// The agent reads its listen address, public host names, store URLs, and auth settings from
/// these, so one binary runs unchanged on every target.
///
/// | Variable | Value |
/// |----------|-------|
/// | `PORT` | The container port, `8080` unless the target sets another |
/// | `BIND_HOST` | `0.0.0.0` — the provider's network fronts the container |
/// | `ADK_ALLOWED_HOSTS` | Comma-separated public host names for `SecurityConfig::allowed_hosts` |
/// | `ADK_AGENT_NAME`, `ADK_AGENT_VERSION` | `[agent] name` and `version` |
/// | `ADK_DEPLOY_TARGET` | The target name, such as `container` |
/// | `ADK_SERVICE_<NAME>_URL` | The connection URL of the `[[services]]` binding `<name>` |
/// | `ADK_AUTH_MODE` and `ADK_AUTH_*` | `[auth]`: mode, issuer, audience, JWKS URI, required scopes |
/// | `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME` | `[telemetry]` |
pub mod contract {
    /// Listen port.
    pub const PORT: &str = "PORT";
    /// Listen address.
    pub const BIND_HOST: &str = "BIND_HOST";
    /// Comma-separated public host names.
    pub const ALLOWED_HOSTS: &str = "ADK_ALLOWED_HOSTS";
    /// `[agent] name`.
    pub const AGENT_NAME: &str = "ADK_AGENT_NAME";
    /// `[agent] version`.
    pub const AGENT_VERSION: &str = "ADK_AGENT_VERSION";
    /// The deploy target name.
    pub const DEPLOY_TARGET: &str = "ADK_DEPLOY_TARGET";
    /// `[auth] mode`: `bearer` or `oidc`.
    pub const AUTH_MODE: &str = "ADK_AUTH_MODE";
    /// `[auth] issuer`.
    pub const AUTH_ISSUER: &str = "ADK_AUTH_ISSUER";
    /// `[auth] audience`.
    pub const AUTH_AUDIENCE: &str = "ADK_AUTH_AUDIENCE";
    /// `[auth] jwksUri`.
    pub const AUTH_JWKS_URI: &str = "ADK_AUTH_JWKS_URI";
    /// `[auth] requiredScopes`, comma-separated.
    pub const AUTH_REQUIRED_SCOPES: &str = "ADK_AUTH_REQUIRED_SCOPES";

    /// The variable carrying the connection URL of the service binding `name`:
    /// `ADK_SERVICE_<NAME>_URL`, upper-cased with every other character as `_`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_deploy::targets::contract::service_url_var;
    ///
    /// assert_eq!(service_url_var("graph-checkpoints"), "ADK_SERVICE_GRAPH_CHECKPOINTS_URL");
    /// ```
    pub fn service_url_var(name: &str) -> String {
        let name: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
            .collect();
        format!("ADK_SERVICE_{name}_URL")
    }

    /// The connection URL of the service binding `name`, read from the environment.
    pub fn service_url(name: &str) -> Option<String> {
        std::env::var(service_url_var(name)).ok().filter(|value| !value.is_empty())
    }

    /// A comma-separated contract value as a list, without blanks.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_deploy::targets::contract::parse_list;
    ///
    /// assert_eq!(
    ///     parse_list(" agents.example.com,.on.aws ,"),
    ///     vec!["agents.example.com".to_string(), ".on.aws".to_string()],
    /// );
    /// ```
    pub fn parse_list(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The public host names in `ADK_ALLOWED_HOSTS`, for `SecurityConfig::with_allowed_hosts`;
    /// empty when it is unset.
    pub fn allowed_hosts() -> Vec<String> {
        std::env::var(ALLOWED_HOSTS).map(|value| parse_list(&value)).unwrap_or_default()
    }

    /// The scopes in `ADK_AUTH_REQUIRED_SCOPES`; empty when it is unset.
    pub fn required_scopes() -> Vec<String> {
        std::env::var(AUTH_REQUIRED_SCOPES).map(|value| parse_list(&value)).unwrap_or_default()
    }
}

/// A provider a deployment manifest deploys to.
///
/// Implementations derive a [`RuntimeSpec`] with [`RuntimeSpec::from_manifest`] and map it onto
/// the provider's resources, so every target keeps the guarantees listed in the
/// [module documentation](self).
///
/// # Example
///
/// ```no_run
/// use adk_deploy::DeploymentManifest;
/// use adk_deploy::targets::{DeployRequest, DeployTarget};
/// use adk_deploy::targets::container::ContainerTarget;
/// use std::path::Path;
///
/// # async fn run() -> adk_deploy::DeployResult<()> {
/// let manifest = DeploymentManifest::from_path(Path::new("adk-deploy.toml"))?;
/// let target = ContainerTarget::from_manifest(".", &manifest)?;
/// let request = DeployRequest::new(manifest, "ghcr.io/acme/support:1.0.0");
/// let plan = target.plan(&request)?;
/// for resource in &plan.resources {
///     println!("{} {}", resource.kind, resource.name);
/// }
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait DeployTarget: Send + Sync {
    /// The name `cargo adk deploy --target` selects this target by.
    fn name(&self) -> &'static str;

    /// Renders what [`deploy`](Self::deploy) would create, without contacting the provider.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when the manifest breaks a guarantee or the
    /// target's `[targets.<name>]` table is incomplete.
    fn plan(&self, request: &DeployRequest) -> DeployResult<DeployPlan>;

    /// Creates or updates the deployment and waits until the provider accepts it.
    ///
    /// # Errors
    ///
    /// Returns the planning errors of [`plan`](Self::plan), and [`DeployError::Client`] when
    /// the provider refuses the deployment.
    async fn deploy(&self, request: &DeployRequest) -> DeployResult<DeploymentOutcome>;

    /// Reports the current state of a deployment by the id [`deploy`](Self::deploy) returned.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the provider cannot be reached.
    async fn status(&self, deployment_id: &str) -> DeployResult<DeploymentStatus>;

    /// Removes a deployment by the id [`deploy`](Self::deploy) returned.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Client`] when the provider refuses the removal.
    async fn destroy(&self, deployment_id: &str) -> DeployResult<()>;
}

/// What to deploy: the manifest and the container image built from it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeployRequest {
    /// The deployment manifest.
    pub manifest: DeploymentManifest,
    /// The container image reference, such as `ghcr.io/acme/support:1.0.0`.
    pub image: String,
}

impl DeployRequest {
    /// Creates a request for `manifest` and `image`.
    pub fn new(manifest: DeploymentManifest, image: impl Into<String>) -> Self {
        Self { manifest, image: image.into() }
    }
}

/// What a target would create, for review before [`DeployTarget::deploy`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeployPlan {
    /// The target name.
    pub target: String,
    /// The runtime the manifest describes, after the guarantee checks.
    pub runtime: RuntimeSpec,
    /// The files or provider requests the target creates, in creation order.
    pub resources: Vec<PlannedResource>,
}

impl DeployPlan {
    /// Creates a plan for `runtime` on `target`.
    pub fn new(target: impl Into<String>, runtime: RuntimeSpec) -> Self {
        Self { target: target.into(), runtime, resources: Vec::new() }
    }

    /// Adds a resource to the plan.
    #[must_use]
    pub fn with_resource(mut self, resource: PlannedResource) -> Self {
        self.resources.push(resource);
        self
    }
}

/// One file or provider request in a [`DeployPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PlannedResource {
    /// What the resource is, such as `Dockerfile` or `ecs:CreateExpressGatewayService`.
    pub kind: String,
    /// The file path relative to the output directory, or the provider resource name.
    pub name: String,
    /// The file contents or request body. Secret values never appear here — only references.
    pub body: String,
}

impl PlannedResource {
    /// Creates a planned resource.
    pub fn new(kind: impl Into<String>, name: impl Into<String>, body: impl Into<String>) -> Self {
        Self { kind: kind.into(), name: name.into(), body: body.into() }
    }
}

/// The result of [`DeployTarget::deploy`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeploymentOutcome {
    /// The target name.
    pub target: String,
    /// The id [`DeployTarget::status`] and [`DeployTarget::destroy`] take.
    pub deployment_id: String,
    /// The URL the agent answers on, when the provider assigns one.
    pub endpoint: Option<String>,
    /// The state the deployment reached.
    pub status: DeploymentStatus,
}

impl DeploymentOutcome {
    /// Creates an outcome.
    pub fn new(
        target: impl Into<String>,
        deployment_id: impl Into<String>,
        endpoint: Option<String>,
        status: DeploymentStatus,
    ) -> Self {
        Self { target: target.into(), deployment_id: deployment_id.into(), endpoint, status }
    }
}

/// The state of a deployment on its provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
#[non_exhaustive]
pub enum DeploymentStatus {
    /// The provider is still creating or updating the deployment.
    Pending {
        /// The provider's description of the step in progress.
        detail: String,
    },
    /// The deployment serves traffic.
    Ready,
    /// The provider gave up on the deployment.
    Failed {
        /// The provider's reason.
        reason: String,
    },
    /// The provider has no such deployment.
    NotFound,
}

/// Target-specific inputs to [`RuntimeSpec::from_manifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuntimeOptions {
    /// The target name, exposed as `ADK_DEPLOY_TARGET`.
    pub target: String,
    /// The container port.
    pub port: u16,
    /// Arguments passed to the agent binary.
    pub args: Vec<String>,
    /// Public host names the agent answers, exposed as `ADK_ALLOWED_HOSTS`.
    pub allowed_hosts: Vec<String>,
    /// Whether the provider exposes the agent beyond a private network.
    pub public: bool,
    /// Whether the provider authenticates every caller before the request reaches the
    /// container, as Agent Engine does with Google Cloud IAM.
    pub provider_authenticates: bool,
    /// Whether the provider supplies the stores `managed` bindings describe, as Agent Engine
    /// supplies Vertex AI Sessions and Memory Bank.
    pub managed_stores: bool,
}

impl RuntimeOptions {
    /// Options for `target` with port `8080`, no arguments, and private ingress.
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            port: DEFAULT_PORT,
            args: Vec::new(),
            allowed_hosts: Vec::new(),
            public: false,
            provider_authenticates: false,
            managed_stores: false,
        }
    }

    /// Sets the container port.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Sets the arguments passed to the agent binary.
    #[must_use]
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Sets the public host names the agent answers.
    #[must_use]
    pub fn with_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.allowed_hosts = hosts;
        self
    }

    /// Marks the ingress public, which requires `[auth]`.
    #[must_use]
    pub fn with_public_ingress(mut self, public: bool) -> Self {
        self.public = public;
        self
    }

    /// Records that the provider authenticates every caller before the container.
    #[must_use]
    pub fn with_provider_authentication(mut self, authenticates: bool) -> Self {
        self.provider_authenticates = authenticates;
        self
    }

    /// Records that the provider supplies the stores `managed` bindings describe.
    #[must_use]
    pub fn with_managed_stores(mut self, managed: bool) -> Self {
        self.managed_stores = managed;
        self
    }
}

/// The default container port.
pub const DEFAULT_PORT: u16 = 8080;

/// The provider-neutral description of the agent container, derived from a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeSpec {
    /// `[agent] name`.
    pub name: String,
    /// `[agent] version`.
    pub version: String,
    /// The container image reference.
    pub image: String,
    /// The container port.
    pub port: u16,
    /// Arguments passed to the agent binary.
    pub args: Vec<String>,
    /// Environment variables with literal values: the [`contract`] variables and the plain
    /// `[env]` entries.
    pub env: BTreeMap<String, String>,
    /// Environment variables read from the provider's secret store: variable name to the
    /// declared `[[secrets]]` key.
    pub secret_env: BTreeMap<String, String>,
    /// Liveness and readiness probe settings.
    pub health: HealthCheckConfig,
    /// Replica bounds and autoscaling targets.
    pub scaling: ScalingPolicy,
    /// Whether the ingress is public.
    pub public: bool,
    /// Whether callers must authenticate: `[auth]` is in `bearer` or `oidc` mode.
    pub auth_required: bool,
    /// Concerns the checks accepted but the operator should read.
    pub warnings: Vec<String>,
}

impl RuntimeSpec {
    /// Derives the runtime from `manifest`, enforcing the deployment guarantees.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when the manifest fails validation or breaks a
    /// guarantee: a literal secret, no session store, a `managed` binding, SQLite beyond one
    /// replica, public ingress without `[auth]`, or a contract variable set in `[env]`.
    pub fn from_manifest(
        manifest: &DeploymentManifest,
        image: &str,
        options: &RuntimeOptions,
    ) -> DeployResult<Self> {
        manifest.validate()?;
        let target = options.target.as_str();
        if image.trim().is_empty() {
            return Err(invalid(format!(
                "target '{target}' needs a container image; pass --image or set image in [targets.{target}]"
            )));
        }
        if !manifest.health.path.starts_with('/') {
            return Err(invalid(format!(
                "health.path '{}' must start with '/'",
                manifest.health.path
            )));
        }

        let declared: BTreeSet<&str> =
            manifest.secrets.iter().map(|secret| secret.key.as_str()).collect();
        let mut warnings = Vec::new();
        let mut env = BTreeMap::new();
        let mut secret_env = BTreeMap::new();

        env.insert(contract::PORT.to_string(), options.port.to_string());
        env.insert(contract::BIND_HOST.to_string(), "0.0.0.0".to_string());
        env.insert(contract::AGENT_NAME.to_string(), manifest.agent.name.clone());
        env.insert(contract::AGENT_VERSION.to_string(), manifest.agent.version.clone());
        env.insert(contract::DEPLOY_TARGET.to_string(), target.to_string());
        if !options.allowed_hosts.is_empty() {
            env.insert(contract::ALLOWED_HOSTS.to_string(), options.allowed_hosts.join(","));
        }

        let auth_required = match &manifest.auth {
            Some(auth) => match auth.mode {
                AuthModeSpec::Disabled => false,
                AuthModeSpec::Bearer | AuthModeSpec::Oidc => {
                    let mode = if auth.mode == AuthModeSpec::Bearer { "bearer" } else { "oidc" };
                    env.insert(contract::AUTH_MODE.to_string(), mode.to_string());
                    for (key, value) in [
                        (contract::AUTH_ISSUER, &auth.issuer),
                        (contract::AUTH_AUDIENCE, &auth.audience),
                        (contract::AUTH_JWKS_URI, &auth.jwks_uri),
                    ] {
                        if let Some(value) = value {
                            env.insert(key.to_string(), value.clone());
                        }
                    }
                    if !auth.required_scopes.is_empty() {
                        env.insert(
                            contract::AUTH_REQUIRED_SCOPES.to_string(),
                            auth.required_scopes.join(","),
                        );
                    }
                    true
                }
            },
            None => false,
        };
        if options.public && !auth_required && !options.provider_authenticates {
            return Err(invalid(format!(
                "target '{target}' exposes the agent publicly, so callers must authenticate: set \
                 [auth] mode = \"bearer\" or \"oidc\", or make the ingress internal in [targets.{target}]"
            )));
        }
        if !auth_required && !options.provider_authenticates {
            warnings.push(
                "no [auth]: the agent must authenticate callers itself, or stay on a private network"
                    .to_string(),
            );
        }

        if let Some(telemetry) = &manifest.telemetry {
            if let Some(endpoint) = &telemetry.otlp_endpoint {
                env.insert("OTEL_EXPORTER_OTLP_ENDPOINT".to_string(), endpoint.clone());
            }
            if let Some(service) = &telemetry.service_name {
                env.insert("OTEL_SERVICE_NAME".to_string(), service.clone());
            }
            if !telemetry.resource_attributes.is_empty() {
                let attributes: BTreeMap<_, _> = telemetry.resource_attributes.iter().collect();
                let attributes: Vec<String> =
                    attributes.into_iter().map(|(key, value)| format!("{key}={value}")).collect();
                env.insert("OTEL_RESOURCE_ATTRIBUTES".to_string(), attributes.join(","));
            }
        }

        let mut session_store = None;
        for binding in &manifest.services {
            // An in-memory store lives in the process, so no provider provisions it.
            let in_process = binding.kind == ServiceKind::InMemory;
            if binding.mode == BindingMode::Managed && (options.managed_stores || in_process) {
                if is_session_store(binding.kind) && session_store.is_none() {
                    session_store = Some(binding);
                }
                continue;
            }
            if binding.mode == BindingMode::Managed {
                return Err(invalid(format!(
                    "service binding '{}' is managed, and target '{target}' provisions no stores: \
                     set mode = \"external\" with secretRef naming a [[secrets]] key that holds \
                     the connection URL",
                    binding.name
                )));
            }
            let var = contract::service_url_var(&binding.name);
            match (&binding.secret_ref, &binding.connection_url) {
                (Some(secret), _) => {
                    if !declared.contains(secret.as_str()) {
                        return Err(invalid(format!(
                            "service binding '{}' references undeclared secret '{secret}'; add \
                             [[secrets]] key = \"{secret}\"",
                            binding.name
                        )));
                    }
                    secret_env.insert(var, secret.clone());
                }
                (None, Some(url)) => {
                    if url::Url::parse(url).is_ok_and(|url| url.password().is_some()) {
                        return Err(invalid(format!(
                            "service binding '{}' has a password in connectionUrl; move the URL \
                             into a [[secrets]] key and reference it with secretRef",
                            binding.name
                        )));
                    }
                    env.insert(var, url.clone());
                }
                (None, None) => {}
            }
            if is_session_store(binding.kind) && session_store.is_none() {
                session_store = Some(binding);
            }
        }
        match session_store.map(|binding| (binding.kind, binding.name.as_str())) {
            None => {
                return Err(invalid(format!(
                    "declare the session store target '{target}' runs on: a [[services]] binding \
                     of kind postgres, redis, mongo-db, neo4j, firestore or sqlite with mode = \
                     \"external\", or kind = \"in-memory\" to accept losing sessions on restart"
                )));
            }
            Some((ServiceKind::InMemory, name)) => warnings.push(format!(
                "service binding '{name}' keeps sessions in memory: they are lost on every \
                 restart and are not shared between replicas"
            )),
            Some((ServiceKind::Sqlite, name)) if manifest.scaling.max_instances > 1 => {
                return Err(invalid(format!(
                    "service binding '{name}' is SQLite, which each replica would hold its own \
                     copy of: set [scaling] maxInstances = 1, or use postgres or redis"
                )));
            }
            Some(_) => {}
        }

        for (key, value) in &manifest.env {
            if env.contains_key(key) {
                return Err(invalid(format!(
                    "env '{key}' is set by target '{target}' from the manifest; remove it from [env]"
                )));
            }
            match value {
                EnvVarSpec::Plain(value) => {
                    if looks_secret(key) {
                        return Err(invalid(format!(
                            "env '{key}' holds a literal value under a secret-bearing name; declare \
                             [[secrets]] key = \"{key}\" and set {key} = {{ secretRef = \"{key}\" }}"
                        )));
                    }
                    env.insert(key.clone(), value.clone());
                }
                EnvVarSpec::SecretRef { secret_ref } => {
                    secret_env.insert(key.clone(), secret_ref.clone());
                }
            }
        }

        let referenced: BTreeSet<&str> = secret_env.values().map(String::as_str).collect();
        for secret in &manifest.secrets {
            if secret.required && !referenced.contains(secret.key.as_str()) {
                warnings.push(format!(
                    "secret '{}' is declared but no [env] entry or service binding references it, \
                     so the container does not receive it",
                    secret.key
                ));
            }
        }

        Ok(Self {
            name: manifest.agent.name.clone(),
            version: manifest.agent.version.clone(),
            image: image.to_string(),
            port: options.port,
            args: options.args.clone(),
            env,
            secret_env,
            health: manifest.health.clone(),
            scaling: manifest.scaling.clone(),
            public: options.public,
            auth_required,
            warnings,
        })
    }

    /// The declared secret keys the container reads, each once, in name order.
    pub fn secret_keys(&self) -> BTreeSet<&str> {
        self.secret_env.values().map(String::as_str).collect()
    }
}

fn invalid(message: String) -> DeployError {
    DeployError::InvalidManifest { message }
}

fn is_session_store(kind: ServiceKind) -> bool {
    match kind {
        ServiceKind::InMemory
        | ServiceKind::Postgres
        | ServiceKind::Redis
        | ServiceKind::Sqlite
        | ServiceKind::MongoDb
        | ServiceKind::Neo4j
        | ServiceKind::Firestore => true,
        ServiceKind::Pgvector
        | ServiceKind::RedisMemory
        | ServiceKind::MongoMemory
        | ServiceKind::Neo4jMemory
        | ServiceKind::ArtifactStorage
        | ServiceKind::McpServer
        | ServiceKind::CheckpointPostgres
        | ServiceKind::CheckpointRedis => false,
    }
}

// Names whose literal values would put a credential in a provider's plain configuration.
fn looks_secret(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    const SUFFIXES: [&str; 6] =
        ["API_KEY", "SECRET", "SECRET_KEY", "TOKEN", "PASSWORD", "PRIVATE_KEY"];
    SUFFIXES.iter().any(|suffix| key == *suffix || key.ends_with(&format!("_{suffix}")))
}

/// What [`verify_endpoint`] observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EndpointReport {
    /// The status of `GET <health path>`.
    pub health_status: u16,
    /// The status of an unauthenticated `GET /api/apps`.
    pub unauthenticated_status: u16,
    /// Checks that could not be decided, such as an agent without the REST API.
    pub warnings: Vec<String>,
}

/// Checks a deployed agent from outside: the health path answers `200`, and when the runtime
/// requires authentication, an unauthenticated request to the REST API is refused.
///
/// `GET /api/apps` sits behind the same authentication as runs and the kill switch, so an agent
/// that answers it without credentials runs in open mode.
///
/// # Errors
///
/// Returns [`DeployError::Client`] when the endpoint is unreachable, the health path does not
/// answer `200`, or the API answers an unauthenticated caller when authentication is required.
///
/// # Example
///
/// ```no_run
/// use adk_deploy::targets::{RuntimeSpec, verify_endpoint};
///
/// # async fn check(runtime: &RuntimeSpec) -> adk_deploy::DeployResult<()> {
/// let report = verify_endpoint("https://support.example.com", runtime).await?;
/// assert_eq!(report.health_status, 200);
/// # Ok(())
/// # }
/// ```
pub async fn verify_endpoint(
    endpoint: &str,
    runtime: &RuntimeSpec,
) -> DeployResult<EndpointReport> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let base = endpoint.trim_end_matches('/');
    let health = client.get(format!("{base}{}", runtime.health.path)).send().await?;
    let health_status = health.status().as_u16();
    if health_status != 200 {
        return Err(DeployError::Client {
            message: format!(
                "{base}{} answered {health_status}, not 200; the deployment is not healthy",
                runtime.health.path
            ),
        });
    }
    let unauthenticated = client.get(format!("{base}/api/apps")).send().await?;
    let unauthenticated_status = unauthenticated.status().as_u16();
    let mut warnings = Vec::new();
    match unauthenticated_status {
        401 | 403 => {}
        200..=299 if runtime.auth_required => {
            return Err(DeployError::Client {
                message: format!(
                    "{base}/api/apps answered {unauthenticated_status} without credentials, so \
                     runs and the kill switch are open; configure \
                     ServerConfig::with_request_context with the [auth] settings"
                ),
            });
        }
        200..=299 => warnings.push(format!(
            "{base}/api/apps answers without credentials; keep this deployment on a private network"
        )),
        status => warnings.push(format!(
            "{base}/api/apps answered {status}; authentication could not be checked"
        )),
    }
    Ok(EndpointReport { health_status, unauthenticated_status, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_names_are_recognised() {
        for key in
            ["ANTHROPIC_API_KEY", "ADK_SERVE_TOKEN", "DB_PASSWORD", "TOKEN", "signing_secret"]
        {
            assert!(looks_secret(key), "{key}");
        }
        for key in ["LOG_LEVEL", "TOKENIZER", "MONKEY", "RUST_LOG"] {
            assert!(!looks_secret(key), "{key}");
        }
    }
}
