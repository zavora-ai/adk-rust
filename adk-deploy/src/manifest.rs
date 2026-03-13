use std::{collections::HashMap, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{DeployError, DeployResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentManifest {
    pub agent: AgentConfig,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub scaling: ScalingPolicy,
    #[serde(default)]
    pub health: HealthCheckConfig,
    #[serde(default)]
    pub strategy: DeploymentStrategyConfig,
    #[serde(default)]
    pub services: Vec<ServiceBinding>,
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    #[serde(default)]
    pub env: HashMap<String, EnvVarSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AgentAuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrails: Option<GuardrailConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime: Option<RealtimeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a2a: Option<A2aConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,
}

impl DeploymentManifest {
    /// Load a deployment manifest from disk and validate it.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_deploy::DeploymentManifest;
    /// use std::path::Path;
    ///
    /// let manifest = DeploymentManifest::from_path(Path::new("adk-deploy.toml")).unwrap();
    /// assert!(!manifest.agent.binary.is_empty());
    /// ```
    pub fn from_path(path: &Path) -> DeployResult<Self> {
        if !path.exists() {
            return Err(DeployError::ManifestNotFound { path: path.to_path_buf() });
        }
        let raw = fs::read_to_string(path)?;
        let manifest = toml::from_str::<DeploymentManifest>(&raw)
            .map_err(|error| DeployError::ManifestParse { message: error.to_string() })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Serialize the manifest to TOML.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_deploy::{AgentConfig, DeploymentManifest};
    ///
    /// let manifest = DeploymentManifest {
    ///     agent: AgentConfig::new("demo", "demo"),
    ///     ..DeploymentManifest::default()
    /// };
    /// let toml = manifest.to_toml_string().unwrap();
    /// assert!(toml.contains("[agent]"));
    /// ```
    pub fn to_toml_string(&self) -> DeployResult<String> {
        self.validate()?;
        toml::to_string_pretty(self)
            .map_err(|error| DeployError::ManifestParse { message: error.to_string() })
    }

    /// Validate manifest semantics before build or push.
    pub fn validate(&self) -> DeployResult<()> {
        use std::collections::BTreeSet;

        if self.agent.name.trim().is_empty() {
            return Err(DeployError::InvalidManifest {
                message: "agent.name must not be empty".to_string(),
            });
        }
        if self.agent.binary.trim().is_empty() {
            return Err(DeployError::InvalidManifest {
                message: "agent.binary must not be empty".to_string(),
            });
        }
        if self.scaling.min_instances > self.scaling.max_instances {
            return Err(DeployError::InvalidManifest {
                message:
                    "scaling.min_instances must be less than or equal to scaling.max_instances"
                        .to_string(),
            });
        }
        if self.strategy.kind == DeploymentStrategyKind::Canary {
            let traffic = self.strategy.traffic_percent.unwrap_or(10);
            if traffic == 0 || traffic > 100 {
                return Err(DeployError::InvalidManifest {
                    message:
                        "strategy.traffic_percent must be between 1 and 100 for canary deployments"
                            .to_string(),
                });
            }
        }
        let mut binding_names = BTreeSet::new();
        for binding in &self.services {
            if !binding_names.insert(binding.name.clone()) {
                return Err(DeployError::InvalidManifest {
                    message: format!("service binding names must be unique: '{}'", binding.name),
                });
            }
            if binding.mode == BindingMode::External
                && binding.connection_url.is_none()
                && binding.secret_ref.is_none()
            {
                return Err(DeployError::InvalidManifest {
                    message: format!(
                        "external service binding '{}' requires connection_url or secret_ref",
                        binding.name
                    ),
                });
            }
        }
        let declared_secrets: BTreeSet<&str> =
            self.secrets.iter().map(|secret| secret.key.as_str()).collect();
        for (key, value) in &self.env {
            if let EnvVarSpec::SecretRef { secret_ref } = value
                && !declared_secrets.contains(secret_ref.as_str())
            {
                return Err(DeployError::InvalidManifest {
                    message: format!("env '{key}' references undeclared secret '{secret_ref}'"),
                });
            }
        }
        if let Some(auth) = &self.auth {
            auth.validate()?;
        }
        if let Some(guardrails) = &self.guardrails {
            guardrails.validate()?;
        }
        if let Some(realtime) = &self.realtime {
            realtime.validate()?;
        }
        if let Some(graph) = &self.graph {
            graph.validate(&self.services)?;
        }
        let mut plugin_names = BTreeSet::new();
        for plugin in &self.plugins {
            if plugin.name.trim().is_empty() {
                return Err(DeployError::InvalidManifest {
                    message: "plugin.name must not be empty".to_string(),
                });
            }
            if !plugin_names.insert(plugin.name.clone()) {
                return Err(DeployError::InvalidManifest {
                    message: format!("plugin names must be unique: '{}'", plugin.name),
                });
            }
        }
        if let Some(skills) = &self.skills
            && skills.directory.trim().is_empty()
        {
            return Err(DeployError::InvalidManifest {
                message: "skills.directory must not be empty".to_string(),
            });
        }
        Ok(())
    }
}

impl Default for DeploymentManifest {
    fn default() -> Self {
        Self {
            agent: AgentConfig::new("example-agent", "example-agent"),
            build: BuildConfig::default(),
            scaling: ScalingPolicy::default(),
            health: HealthCheckConfig::default(),
            strategy: DeploymentStrategyConfig::default(),
            services: Vec::new(),
            secrets: Vec::new(),
            env: HashMap::new(),
            telemetry: None,
            auth: None,
            guardrails: None,
            realtime: None,
            a2a: None,
            graph: None,
            plugins: Vec::new(),
            skills: None,
            source: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub name: String,
    pub binary: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
}

impl AgentConfig {
    pub fn new(name: impl Into<String>, binary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            binary: binary.into(),
            version: default_version(),
            description: None,
            toolchain: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildConfig {
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub system_deps: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            profile: default_profile(),
            target: None,
            features: Vec::new(),
            system_deps: Vec::new(),
            assets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScalingPolicy {
    #[serde(default = "default_min_instances")]
    pub min_instances: u32,
    #[serde(default = "default_max_instances")]
    pub max_instances: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_cpu_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_concurrent_requests: Option<u32>,
}

impl Default for ScalingPolicy {
    fn default() -> Self {
        Self {
            min_instances: default_min_instances(),
            max_instances: default_max_instances(),
            target_latency_ms: Some(500),
            target_cpu_percent: Some(70),
            target_concurrent_requests: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckConfig {
    #[serde(default = "default_health_path")]
    pub path: String,
    #[serde(default = "default_health_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_health_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            path: default_health_path(),
            interval_secs: default_health_interval(),
            timeout_secs: default_health_timeout(),
            failure_threshold: default_failure_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStrategyConfig {
    #[serde(rename = "type")]
    pub kind: DeploymentStrategyKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_percent: Option<u8>,
}

impl Default for DeploymentStrategyConfig {
    fn default() -> Self {
        Self { kind: DeploymentStrategyKind::Rolling, traffic_percent: None }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentStrategyKind {
    Rolling,
    BlueGreen,
    Canary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceBinding {
    pub name: String,
    pub kind: ServiceKind,
    #[serde(default)]
    pub mode: BindingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceKind {
    InMemory,
    Postgres,
    Redis,
    Sqlite,
    MongoDb,
    Neo4j,
    Firestore,
    Pgvector,
    RedisMemory,
    MongoMemory,
    Neo4jMemory,
    ArtifactStorage,
    McpServer,
    CheckpointPostgres,
    CheckpointRedis,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BindingMode {
    #[default]
    Managed,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub key: String,
    #[serde(default = "default_required")]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum EnvVarSpec {
    Plain(String),
    SecretRef { secret_ref: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resource_attributes: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthConfig {
    pub mode: AuthModeSpec,
    #[serde(default)]
    pub required_scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
}

impl AgentAuthConfig {
    fn validate(&self) -> DeployResult<()> {
        if self.mode == AuthModeSpec::Disabled && !self.required_scopes.is_empty() {
            return Err(DeployError::InvalidManifest {
                message: "auth.required_scopes requires auth.mode != disabled".to_string(),
            });
        }
        if self.mode == AuthModeSpec::Oidc
            && (self.issuer.is_none() || self.audience.is_none() || self.jwks_uri.is_none())
        {
            return Err(DeployError::InvalidManifest {
                message: "auth.mode = oidc requires auth.issuer, auth.audience, and auth.jwks_uri"
                    .to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthModeSpec {
    Disabled,
    Bearer,
    Oidc,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardrailConfig {
    #[serde(default)]
    pub pii_redaction: bool,
    #[serde(default)]
    pub content_filters: Vec<String>,
}

impl GuardrailConfig {
    fn validate(&self) -> DeployResult<()> {
        if !self.pii_redaction && self.content_filters.is_empty() {
            return Err(DeployError::InvalidManifest {
                message:
                    "guardrails must enable pii_redaction or declare at least one content_filter"
                        .to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeConfig {
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub sticky_sessions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_timeout_secs: Option<u64>,
}

impl RealtimeConfig {
    fn validate(&self) -> DeployResult<()> {
        const ALLOWED: &[&str] = &["openai", "gemini", "vertex-live", "livekit", "openai-webrtc"];
        for feature in &self.features {
            if !ALLOWED.iter().any(|candidate| candidate == feature) {
                return Err(DeployError::InvalidManifest {
                    message: format!(
                        "unsupported realtime feature '{feature}'. valid values: {}",
                        ALLOWED.join(", ")
                    ),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct A2aConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GraphConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_binding: Option<String>,
    #[serde(default)]
    pub hitl_enabled: bool,
}

impl GraphConfig {
    fn validate(&self, services: &[ServiceBinding]) -> DeployResult<()> {
        if let Some(binding_name) = &self.checkpoint_binding {
            let binding = services
                .iter()
                .find(|binding| binding.name == *binding_name)
                .ok_or_else(|| DeployError::InvalidManifest {
                    message: format!(
                        "graph.checkpoint_binding references unknown service binding '{binding_name}'"
                    ),
                })?;
            if !matches!(
                binding.kind,
                ServiceKind::CheckpointPostgres | ServiceKind::CheckpointRedis
            ) {
                return Err(DeployError::InvalidManifest {
                    message: format!(
                        "graph.checkpoint_binding '{}' must reference checkpoint-postgres or checkpoint-redis",
                        binding_name
                    ),
                });
            }
        } else if self.hitl_enabled {
            return Err(DeployError::InvalidManifest {
                message:
                    "graph.hitl_enabled requires graph.checkpoint_binding for resumable workflows"
                        .to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillConfig {
    pub directory: String,
    #[serde(default)]
    pub hot_reload: bool,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_profile() -> String {
    "release".to_string()
}

fn default_min_instances() -> u32 {
    1
}

fn default_max_instances() -> u32 {
    10
}

fn default_health_path() -> String {
    "/api/health".to_string()
}

fn default_health_interval() -> u64 {
    10
}

fn default_health_timeout() -> u64 {
    5
}

fn default_failure_threshold() -> u32 {
    3
}

fn default_required() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{
        AgentAuthConfig, AuthModeSpec, DeploymentManifest, EnvVarSpec, GraphConfig, RealtimeConfig,
        ServiceBinding, ServiceKind,
    };

    #[test]
    fn rejects_undeclared_secret_refs_in_env() {
        let mut manifest = DeploymentManifest::default();
        manifest.env.insert(
            "OPENAI_API_KEY".to_string(),
            EnvVarSpec::SecretRef { secret_ref: "missing".to_string() },
        );

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("undeclared secret"));
    }

    #[test]
    fn rejects_invalid_realtime_feature() {
        let manifest = DeploymentManifest {
            realtime: Some(RealtimeConfig {
                features: vec!["unsupported".to_string()],
                sticky_sessions: true,
                drain_timeout_secs: Some(30),
            }),
            ..Default::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("unsupported realtime feature"));
    }

    #[test]
    fn requires_graph_checkpoint_binding_for_hitl() {
        let manifest = DeploymentManifest {
            graph: Some(GraphConfig { checkpoint_binding: None, hitl_enabled: true }),
            ..Default::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("graph.hitl_enabled"));
    }

    #[test]
    fn requires_oidc_fields_when_auth_mode_is_oidc() {
        let manifest = DeploymentManifest {
            auth: Some(AgentAuthConfig {
                mode: AuthModeSpec::Oidc,
                required_scopes: vec!["deploy:read".to_string()],
                issuer: None,
                audience: Some("adk-cli".to_string()),
                jwks_uri: None,
            }),
            ..Default::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("auth.mode = oidc"));
    }

    #[test]
    fn accepts_supported_graph_checkpoint_binding() {
        let mut manifest = DeploymentManifest::default();
        manifest.services.push(ServiceBinding {
            name: "graph-checkpoint".to_string(),
            kind: ServiceKind::CheckpointPostgres,
            mode: super::BindingMode::Managed,
            connection_url: None,
            secret_ref: None,
        });
        manifest.graph = Some(GraphConfig {
            checkpoint_binding: Some("graph-checkpoint".to_string()),
            hitl_enabled: true,
        });

        manifest.validate().unwrap();
    }
}
