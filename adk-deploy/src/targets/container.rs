//! The `container` target: an OCI image with Compose and Kubernetes manifests, for
//! self-hosting on any container runtime. It needs no cloud account.
//!
//! [`ContainerTarget::plan`] renders four files into `.adk-deploy/container/`:
//!
//! | File | Contents |
//! |------|----------|
//! | `Dockerfile` | Multi-stage build: the agent binary compiled on `rust:1.95-slim`, run as the distroless `nonroot` user (UID 65532) |
//! | `Dockerfile.dockerignore` | Keeps `target/`, `.git/`, `.env` files and `.adk-deploy/` out of the build context |
//! | `compose.yaml` | One service; secrets interpolated from the environment at `docker compose up`, published on `127.0.0.1` unless the ingress is public |
//! | `kubernetes.yaml` | `Deployment` with probes from `[health]`, `Service`, `HorizontalPodAutoscaler`, and a `PersistentVolumeClaim` when `dataDir` is set; secrets from a Kubernetes `Secret` |
//!
//! No generated file holds a secret value: Compose reads secrets from the shell or an
//! `--env-file`, and Kubernetes from the `Secret` named by `secretName`.
//!
//! # Example
//!
//! ```
//! use adk_deploy::DeploymentManifest;
//! use adk_deploy::targets::{DeployRequest, DeployTarget};
//! use adk_deploy::targets::container::ContainerTarget;
//!
//! let manifest: DeploymentManifest = toml::from_str(
//!     r#"
//!     [agent]
//!     name = "support"
//!     binary = "support"
//!
//!     [[services]]
//!     name = "sessions"
//!     kind = "in-memory"
//!
//!     [targets.container]
//!     namespace = "agents"
//!     "#,
//! )
//! .unwrap();
//! let target = ContainerTarget::from_manifest(".", &manifest).unwrap();
//! let plan = target.plan(&DeployRequest::new(manifest, "support:0.1.0")).unwrap();
//! let names: Vec<&str> = plan.resources.iter().map(|r| r.name.as_str()).collect();
//! assert_eq!(names, ["Dockerfile", "Dockerfile.dockerignore", "compose.yaml", "kubernetes.yaml"]);
//! ```

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::yaml::Yaml;
use super::{
    DeployPlan, DeployRequest, DeployTarget, DeploymentOutcome, DeploymentStatus, PlannedResource,
    RuntimeOptions, RuntimeSpec,
};
use crate::{DeployError, DeployResult, DeploymentManifest, DeploymentStrategyKind};

/// The builder image, kept in lockstep with `rust-toolchain.toml`, which pins Rust 1.95.0.
pub const DEFAULT_BUILDER_IMAGE: &str = "rust:1.95-slim";
/// The runtime image: glibc and CA certificates, no shell, running as `nonroot`.
pub const DEFAULT_RUNTIME_IMAGE: &str = "gcr.io/distroless/cc-debian12:nonroot";
const DEFAULT_OUT_DIR: &str = ".adk-deploy/container";
/// The `nonroot` user of the distroless runtime image.
const AGENT_UID: u32 = 65532;

/// Who receives traffic from the provider's network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Ingress {
    /// Only the private network: Compose publishes on `127.0.0.1`, Kubernetes uses a
    /// `ClusterIP` service.
    Internal,
    /// Beyond the private network: Compose publishes on every interface, Kubernetes uses a
    /// `LoadBalancer` service. Requires `[auth]`.
    Public,
}

/// What `deploy` does after writing the files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ApplyMode {
    /// Write the files only.
    #[default]
    None,
    /// `docker compose up --detach --build`.
    Compose,
    /// `docker build`, `docker push`, `kubectl apply`, then wait for the rollout.
    Kubernetes,
}

/// `[targets.container]` settings.
///
/// # Example
///
/// ```toml
/// [targets.container]
/// image = "ghcr.io/acme/support:1.0.0"
/// args = ["--log-format", "json"]
/// allowedHosts = ["support.example.com"]
/// ingress = "internal"
/// namespace = "agents"
/// secretName = "support-secrets"
/// cpu = "500m"
/// memory = "512Mi"
/// apply = "kubernetes"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct ContainerTargetConfig {
    /// Image reference to build and run (default `<agent name>:<version>`).
    pub image: Option<String>,
    /// Arguments passed to the agent binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Container port (default `8080`).
    pub port: Option<u16>,
    /// Public host names the agent answers, besides loopback names and IP addresses.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Ingress (default `internal`).
    pub ingress: Option<Ingress>,
    /// Host address Compose publishes the port on (default `127.0.0.1`, or `0.0.0.0` for
    /// public ingress).
    pub publish: Option<String>,
    /// Kubernetes namespace (default: the namespace `kubectl` is set to).
    pub namespace: Option<String>,
    /// Kubernetes `Secret` holding the `[[secrets]]` keys (default `<name>-secrets`).
    pub secret_name: Option<String>,
    /// CPU request (default `250m`).
    pub cpu: Option<String>,
    /// Memory request and limit (default `256Mi`).
    pub memory: Option<String>,
    /// Directory backed by a persistent volume, for a SQLite session store. Allows one replica.
    pub data_dir: Option<String>,
    /// Size of the persistent volume (default `1Gi`).
    pub data_size: Option<String>,
    /// Docker build context, relative to the manifest's directory (default `.`). Set it to the
    /// workspace root when the agent is a workspace member or has path dependencies.
    pub context: Option<String>,
    /// Output directory, relative to the manifest's directory (default `.adk-deploy/container`).
    pub out_dir: Option<String>,
    /// What `deploy` runs after writing the files (default `none`).
    #[serde(default)]
    pub apply: ApplyMode,
    /// Builder stage image (default [`DEFAULT_BUILDER_IMAGE`]).
    pub builder_image: Option<String>,
    /// Runtime stage image (default [`DEFAULT_RUNTIME_IMAGE`]).
    pub runtime_image: Option<String>,
}

/// Deploys to a container runtime: Docker Compose or Kubernetes.
pub struct ContainerTarget {
    project_dir: PathBuf,
    config: ContainerTargetConfig,
    runner: Arc<dyn CommandRunner>,
}

impl std::fmt::Debug for ContainerTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerTarget")
            .field("project_dir", &self.project_dir)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ContainerTarget {
    /// The target name.
    pub const NAME: &'static str = "container";

    /// Creates a target for the project whose `adk-deploy.toml` is in `project_dir`.
    pub fn new(project_dir: impl Into<PathBuf>, config: ContainerTargetConfig) -> Self {
        Self { project_dir: project_dir.into(), config, runner: Arc::new(ProcessRunner) }
    }

    /// Creates a target from the manifest's `[targets.container]` table.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::InvalidManifest`] when the table has an unknown key or a value of
    /// the wrong type.
    pub fn from_manifest(
        project_dir: impl Into<PathBuf>,
        manifest: &DeploymentManifest,
    ) -> DeployResult<Self> {
        Ok(Self::new(project_dir, manifest.target_config(Self::NAME)?))
    }

    /// The settings this target was created with.
    pub fn config(&self) -> &ContainerTargetConfig {
        &self.config
    }

    /// The image this target builds when none is given: `[targets.container] image`, or
    /// `<agent name>:<version>`.
    pub fn default_image(&self, manifest: &DeploymentManifest) -> String {
        self.config.image.clone().unwrap_or_else(|| {
            format!("{}:{}", dns_label(&manifest.agent.name, 63), manifest.agent.version)
        })
    }

    /// The directory the files are written to.
    pub fn out_dir(&self) -> PathBuf {
        self.project_dir.join(self.out_rel())
    }

    #[cfg(test)]
    fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }

    fn out_rel(&self) -> &str {
        self.config.out_dir.as_deref().unwrap_or(DEFAULT_OUT_DIR)
    }

    fn context_rel(&self) -> &str {
        self.config.context.as_deref().unwrap_or(".")
    }

    fn ingress(&self) -> Ingress {
        self.config.ingress.unwrap_or(Ingress::Internal)
    }

    fn runtime_options(&self) -> RuntimeOptions {
        RuntimeOptions::new(Self::NAME)
            .with_port(self.config.port.unwrap_or(super::DEFAULT_PORT))
            .with_args(self.config.args.clone())
            .with_allowed_hosts(self.config.allowed_hosts.clone())
            .with_public_ingress(self.ingress() == Ingress::Public)
    }

    fn secret_name(&self, runtime: &RuntimeSpec) -> String {
        self.config
            .secret_name
            .clone()
            .unwrap_or_else(|| format!("{}-secrets", dns_label(&runtime.name, 55)))
    }

    /// The project directory relative to the build context, with `/` separators and no
    /// trailing slash; empty when they are the same directory.
    fn project_in_context(&self) -> DeployResult<String> {
        let context_rel = self.context_rel();
        if normalized(Path::new(context_rel)).as_os_str().is_empty() {
            return Ok(String::new());
        }
        let project = self.project_dir.canonicalize()?;
        let context = self.project_dir.join(context_rel).canonicalize()?;
        let inner = project.strip_prefix(&context).map_err(|_| DeployError::InvalidManifest {
            message: format!(
                "[targets.container] context '{context_rel}' must be the manifest's directory or one of its parents"
            ),
        })?;
        Ok(slash_path(inner))
    }

    fn dockerfile(
        &self,
        manifest: &DeploymentManifest,
        runtime: &RuntimeSpec,
    ) -> DeployResult<String> {
        let project = self.project_in_context()?;
        let manifest_path = if project.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{project}/Cargo.toml")
        };
        let build = &manifest.build;
        let mut cargo = format!(
            "cargo build --locked --profile {} --manifest-path {manifest_path} --bin {}",
            build.profile, manifest.agent.binary
        );
        if !build.features.is_empty() {
            cargo.push_str(&format!(" --features {}", build.features.join(",")));
        }
        let mut binary_dir = String::from("/build/target/");
        if let Some(triple) = &build.target {
            cargo.push_str(&format!(" --target {triple}"));
            binary_dir.push_str(&format!("{triple}/"));
        }
        binary_dir.push_str(match build.profile.as_str() {
            "dev" | "test" => "debug",
            profile => profile,
        });

        let builder = self.config.builder_image.as_deref().unwrap_or(DEFAULT_BUILDER_IMAGE);
        let runtime_image = self.config.runtime_image.as_deref().unwrap_or(DEFAULT_RUNTIME_IMAGE);
        let mut out = format!(
            "# Generated by adk-deploy for {} {}. Regenerate with `cargo adk deploy --target container --plan`.\n\
             FROM {builder} AS builder\n",
            runtime.name, runtime.version
        );
        if !build.system_deps.is_empty() {
            out.push_str(&format!(
                "RUN apt-get update \\\n    && apt-get install -y --no-install-recommends {} \\\n    && rm -rf /var/lib/apt/lists/*\n",
                build.system_deps.join(" ")
            ));
        }
        out.push_str(&format!(
            "WORKDIR /build\nCOPY . .\n# One target directory whether or not the agent is a workspace member.\nENV CARGO_TARGET_DIR=/build/target\nRUN {cargo}\n"
        ));
        if self.config.data_dir.is_some() {
            // The runtime image has no shell, so the data directory is made here and copied
            // with its owner; a new volume mounted on it starts owned by the agent's user.
            out.push_str("RUN mkdir -p /adk-data\n");
        }
        out.push_str(&format!(
            "\nFROM {runtime_image}\nWORKDIR /app\nCOPY --from=builder {binary_dir}/{} /app/agent\n",
            manifest.agent.binary
        ));
        if let Some(data_dir) = &self.config.data_dir {
            out.push_str(&format!(
                "COPY --from=builder --chown={AGENT_UID}:{AGENT_UID} /adk-data {data_dir}\n"
            ));
        }
        for asset in &build.assets {
            let asset = relative_asset(asset)?;
            let source =
                if project.is_empty() { asset.clone() } else { format!("{project}/{asset}") };
            out.push_str(&format!("COPY --from=builder /build/{source} /app/{asset}\n"));
        }
        // A numeric user lets Kubernetes verify runAsNonRoot.
        out.push_str(&format!(
            "ENV PORT={port} BIND_HOST=0.0.0.0\nEXPOSE {port}\nUSER {AGENT_UID}:{AGENT_UID}\nENTRYPOINT [\"/app/agent\"]\n",
            port = runtime.port
        ));
        if !runtime.args.is_empty() {
            let args = serde_json::to_string(&runtime.args).map_err(|error| {
                DeployError::InvalidManifest {
                    message: format!("args are not serializable: {error}"),
                }
            })?;
            out.push_str(&format!("CMD {args}\n"));
        }
        Ok(out)
    }

    fn compose(&self, runtime: &RuntimeSpec) -> DeployResult<String> {
        let name = dns_label(&runtime.name, 63);
        let project = self.project_in_context()?;
        let out_rel = slash_path(&normalized(Path::new(self.out_rel())));
        let depth = out_rel.split('/').filter(|part| !part.is_empty()).count();
        let mut context = "../".repeat(depth);
        let context_rel = slash_path(&normalized(Path::new(self.context_rel())));
        context.push_str(&context_rel);
        let context = context.trim_end_matches('/').to_string();
        let dockerfile = if project.is_empty() {
            format!("{out_rel}/Dockerfile")
        } else {
            format!("{project}/{out_rel}/Dockerfile")
        };

        let mut environment = Yaml::map();
        for (key, value) in &runtime.env {
            // Compose interpolates `$` in literal values.
            environment = environment.with(key, value.replace('$', "$$"));
        }
        for (key, secret) in &runtime.secret_env {
            environment = environment.with(
                key,
                format!("${{{secret}:?{secret} must be set in the environment or an --env-file}}"),
            );
        }
        let publish = self.config.publish.clone().unwrap_or_else(|| {
            match self.ingress() {
                Ingress::Internal => "127.0.0.1",
                Ingress::Public => "0.0.0.0",
            }
            .to_string()
        });
        let mut service = Yaml::map()
            .with("image", &runtime.image)
            .with("build", Yaml::map().with("context", context).with("dockerfile", dockerfile));
        if !runtime.args.is_empty() {
            service = service.with("command", Yaml::seq(runtime.args.iter()));
        }
        service = service
            .with("environment", environment)
            .with("ports", Yaml::seq([format!("{publish}:{port}:{port}", port = runtime.port)]))
            .with("read_only", true)
            .with("tmpfs", Yaml::seq(["/tmp"]))
            .with("cap_drop", Yaml::seq(["ALL"]))
            .with("security_opt", Yaml::seq(["no-new-privileges:true"]))
            .with("restart", "unless-stopped");
        let mut document = Yaml::map();
        if let Some(data_dir) = &self.config.data_dir {
            service = service.with("volumes", Yaml::seq([format!("{name}-data:{data_dir}")]));
            document = document
                .with("name", &name)
                .with("services", Yaml::map().with("agent", service))
                .with("volumes", Yaml::map().with(&format!("{name}-data"), Yaml::map()));
        } else {
            document =
                document.with("name", &name).with("services", Yaml::map().with("agent", service));
        }
        Ok(format!(
            "# Generated by adk-deploy for {} {}. Secrets are read from the environment at\n\
             # `docker compose up`; this file holds none.\n{}",
            runtime.name,
            runtime.version,
            document.render()
        ))
    }

    fn kubernetes(&self, manifest: &DeploymentManifest, runtime: &RuntimeSpec) -> String {
        let name = dns_label(&runtime.name, 63);
        let secret_name = self.secret_name(runtime);
        let metadata = |kind_name: &str| {
            Yaml::map()
                .with("name", kind_name)
                .with_opt("namespace", self.config.namespace.as_deref())
                .with(
                    "labels",
                    Yaml::map()
                        .with("app.kubernetes.io/name", &name)
                        .with("app.kubernetes.io/version", &runtime.version)
                        .with("app.kubernetes.io/managed-by", "adk-deploy"),
                )
        };
        let selector = Yaml::map().with("app.kubernetes.io/name", &name);

        let mut env = Vec::new();
        for (key, value) in &runtime.env {
            // Kubernetes expands `$(VAR)` in values; `$$(` keeps it literal.
            env.push(Yaml::map().with("name", key).with("value", value.replace("$(", "$$(")));
        }
        for (key, secret) in &runtime.secret_env {
            env.push(Yaml::map().with("name", key).with(
                "valueFrom",
                Yaml::map().with(
                    "secretKeyRef",
                    Yaml::map().with("name", &secret_name).with("key", secret),
                ),
            ));
        }
        let probe = || {
            Yaml::map()
                .with(
                    "httpGet",
                    Yaml::map().with("path", &runtime.health.path).with("port", "http"),
                )
                .with("periodSeconds", runtime.health.interval_secs)
                .with("timeoutSeconds", runtime.health.timeout_secs)
                .with("failureThreshold", runtime.health.failure_threshold)
        };
        let cpu = self.config.cpu.as_deref().unwrap_or("250m");
        let memory = self.config.memory.as_deref().unwrap_or("256Mi");
        let mut mounts = vec![Yaml::map().with("name", "tmp").with("mountPath", "/tmp")];
        let mut volumes = vec![Yaml::map().with("name", "tmp").with("emptyDir", Yaml::map())];
        if let Some(data_dir) = &self.config.data_dir {
            mounts.push(Yaml::map().with("name", "data").with("mountPath", data_dir));
            volumes.push(Yaml::map().with("name", "data").with(
                "persistentVolumeClaim",
                Yaml::map().with("claimName", format!("{name}-data")),
            ));
        }
        let mut container = Yaml::map().with("name", "agent").with("image", &runtime.image);
        if !runtime.args.is_empty() {
            container = container.with("args", Yaml::seq(runtime.args.iter()));
        }
        container = container
            .with(
                "ports",
                Yaml::seq([Yaml::map().with("name", "http").with("containerPort", runtime.port)]),
            )
            .with("env", Yaml::Seq(env))
            .with("readinessProbe", probe())
            .with("livenessProbe", probe())
            .with(
                "resources",
                Yaml::map()
                    .with("requests", Yaml::map().with("cpu", cpu).with("memory", memory))
                    .with("limits", Yaml::map().with("memory", memory)),
            )
            .with(
                "securityContext",
                Yaml::map()
                    .with("allowPrivilegeEscalation", false)
                    .with("readOnlyRootFilesystem", true)
                    .with("capabilities", Yaml::map().with("drop", Yaml::seq(["ALL"]))),
            )
            .with("volumeMounts", Yaml::Seq(mounts));
        // A ReadWriteOnce volume attaches to one pod at a time, so its pod is replaced in place.
        let strategy = if self.config.data_dir.is_some() {
            Yaml::map().with("type", "Recreate")
        } else {
            Yaml::map().with("type", "RollingUpdate").with(
                "rollingUpdate",
                Yaml::map().with("maxUnavailable", 0u32).with("maxSurge", 1u32),
            )
        };
        let deployment = Yaml::map()
            .with("apiVersion", "apps/v1")
            .with("kind", "Deployment")
            .with("metadata", metadata(&name))
            .with(
                "spec",
                Yaml::map()
                    .with("replicas", runtime.scaling.min_instances)
                    .with("selector", Yaml::map().with("matchLabels", selector.clone()))
                    .with("strategy", strategy)
                    .with(
                        "template",
                        Yaml::map()
                            .with(
                                "metadata",
                                Yaml::map().with(
                                    "labels",
                                    Yaml::map()
                                        .with("app.kubernetes.io/name", &name)
                                        .with("app.kubernetes.io/version", &runtime.version),
                                ),
                            )
                            .with(
                                "spec",
                                Yaml::map()
                                    .with("automountServiceAccountToken", false)
                                    .with(
                                        "securityContext",
                                        Yaml::map()
                                            .with("runAsNonRoot", true)
                                            .with("runAsUser", AGENT_UID)
                                            .with("runAsGroup", AGENT_UID)
                                            // Volumes are made group-writable for the agent.
                                            .with("fsGroup", AGENT_UID)
                                            .with(
                                                "seccompProfile",
                                                Yaml::map().with("type", "RuntimeDefault"),
                                            ),
                                    )
                                    .with("containers", Yaml::seq([container]))
                                    .with("volumes", Yaml::Seq(volumes)),
                            ),
                    ),
            );
        let service = Yaml::map()
            .with("apiVersion", "v1")
            .with("kind", "Service")
            .with("metadata", metadata(&name))
            .with(
                "spec",
                Yaml::map()
                    .with(
                        "type",
                        match self.ingress() {
                            Ingress::Internal => "ClusterIP",
                            Ingress::Public => "LoadBalancer",
                        },
                    )
                    .with("selector", selector)
                    .with(
                        "ports",
                        Yaml::seq([Yaml::map()
                            .with("name", "http")
                            .with("port", 80u16)
                            .with("targetPort", "http")]),
                    ),
            );
        let mut documents = vec![deployment, service];
        if runtime.scaling.max_instances > runtime.scaling.min_instances {
            documents.push(
                Yaml::map()
                    .with("apiVersion", "autoscaling/v2")
                    .with("kind", "HorizontalPodAutoscaler")
                    .with("metadata", metadata(&name))
                    .with(
                        "spec",
                        Yaml::map()
                            .with(
                                "scaleTargetRef",
                                Yaml::map()
                                    .with("apiVersion", "apps/v1")
                                    .with("kind", "Deployment")
                                    .with("name", &name),
                            )
                            .with("minReplicas", runtime.scaling.min_instances.max(1))
                            .with("maxReplicas", runtime.scaling.max_instances)
                            .with(
                                "metrics",
                                Yaml::seq([Yaml::map().with("type", "Resource").with(
                                    "resource",
                                    Yaml::map().with("name", "cpu").with(
                                        "target",
                                        Yaml::map().with("type", "Utilization").with(
                                            "averageUtilization",
                                            runtime.scaling.target_cpu_percent.unwrap_or(70),
                                        ),
                                    ),
                                )]),
                            ),
                    ),
            );
        }
        if self.config.data_dir.is_some() {
            documents.push(
                Yaml::map()
                    .with("apiVersion", "v1")
                    .with("kind", "PersistentVolumeClaim")
                    .with("metadata", metadata(&format!("{name}-data")))
                    .with(
                        "spec",
                        Yaml::map().with("accessModes", Yaml::seq(["ReadWriteOnce"])).with(
                            "resources",
                            Yaml::map().with(
                                "requests",
                                Yaml::map().with(
                                    "storage",
                                    self.config.data_size.as_deref().unwrap_or("1Gi"),
                                ),
                            ),
                        ),
                    ),
            );
        }
        let keys: Vec<&str> = runtime.secret_keys().into_iter().collect();
        let mut out = format!(
            "# Generated by adk-deploy for {} {} from {}.\n",
            runtime.name, runtime.version, manifest.agent.binary
        );
        if keys.is_empty() {
            out.push_str("# The agent reads no secrets.\n");
        } else {
            out.push_str(&format!(
                "# Secrets come from the Secret '{secret_name}', keys: {}.\n\
                 # Create it from your secret store before applying, for example with External\n\
                 # Secrets or `kubectl create secret generic {secret_name} --from-env-file=<file>`.\n",
                keys.join(", ")
            ));
        }
        let rendered: Vec<String> = documents.iter().map(Yaml::render).collect();
        out.push_str(&rendered.join("---\n"));
        out
    }

    /// Writes a plan's files into [`out_dir`](Self::out_dir) and returns that directory.
    ///
    /// # Errors
    ///
    /// Returns [`DeployError::Io`] when the directory or a file cannot be written.
    pub async fn write_files(&self, plan: &DeployPlan) -> DeployResult<PathBuf> {
        let out_dir = self.out_dir();
        tokio::fs::create_dir_all(&out_dir).await?;
        for resource in &plan.resources {
            tokio::fs::write(out_dir.join(&resource.name), &resource.body).await?;
        }
        Ok(out_dir)
    }

    fn deployment_id(&self, runtime: &RuntimeSpec) -> String {
        let name = dns_label(&runtime.name, 63);
        match self.config.apply {
            ApplyMode::None => format!("files:{}", self.out_dir().display()),
            ApplyMode::Compose => format!("compose:{name}"),
            ApplyMode::Kubernetes => match &self.config.namespace {
                Some(namespace) => format!("kubernetes:{namespace}/{name}"),
                None => format!("kubernetes:{name}"),
            },
        }
    }
}

#[async_trait]
impl DeployTarget for ContainerTarget {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn plan(&self, request: &DeployRequest) -> DeployResult<DeployPlan> {
        let manifest = &request.manifest;
        let mut runtime =
            RuntimeSpec::from_manifest(manifest, &request.image, &self.runtime_options())?;
        if self.config.data_dir.is_some() && runtime.scaling.max_instances > 1 {
            return Err(DeployError::InvalidManifest {
                message: "[targets.container] dataDir is a ReadWriteOnce volume one replica \
                          mounts at a time: set [scaling] maxInstances = 1"
                    .to_string(),
            });
        }
        if manifest.strategy.kind != DeploymentStrategyKind::Rolling {
            runtime.warnings.push(
                "the container target rolls out with a Kubernetes RollingUpdate; canary and \
                 blue-green need a progressive-delivery controller"
                    .to_string(),
            );
        }
        let dockerfile = self.dockerfile(manifest, &runtime)?;
        let compose = self.compose(&runtime)?;
        let kubernetes = self.kubernetes(manifest, &runtime);
        Ok(DeployPlan::new(Self::NAME, runtime)
            .with_resource(PlannedResource::new("Dockerfile", "Dockerfile", dockerfile))
            .with_resource(PlannedResource::new(
                "dockerignore",
                "Dockerfile.dockerignore",
                DOCKERIGNORE,
            ))
            .with_resource(PlannedResource::new("compose", "compose.yaml", compose))
            .with_resource(PlannedResource::new("kubernetes", "kubernetes.yaml", kubernetes)))
    }

    async fn deploy(&self, request: &DeployRequest) -> DeployResult<DeploymentOutcome> {
        let plan = self.plan(request)?;
        let out_dir = self.write_files(&plan).await?;
        let runtime = &plan.runtime;
        let id = self.deployment_id(runtime);
        let name = dns_label(&runtime.name, 63);
        let out = out_dir.display().to_string();
        match self.config.apply {
            ApplyMode::None => Ok(DeploymentOutcome::new(
                Self::NAME,
                id,
                None,
                DeploymentStatus::Pending {
                    detail: format!(
                        "files written to {out}; run `docker compose -f {out}/compose.yaml up --detach --build` \
                         or build and push {} and run `kubectl apply -f {out}/kubernetes.yaml`",
                        runtime.image
                    ),
                },
            )),
            ApplyMode::Compose => {
                self.runner
                    .run(
                        "docker",
                        &args([
                            "compose",
                            "-f",
                            &format!("{out}/compose.yaml"),
                            "up",
                            "--detach",
                            "--build",
                        ]),
                        &self.project_dir,
                    )
                    .await?;
                let publish =
                    self.config.publish.clone().unwrap_or_else(|| "127.0.0.1".to_string());
                let host = if publish == "0.0.0.0" { "127.0.0.1".to_string() } else { publish };
                let endpoint = format!("http://{host}:{}", runtime.port);
                let status = self.status(&id).await?;
                Ok(DeploymentOutcome::new(Self::NAME, id, Some(endpoint), status))
            }
            ApplyMode::Kubernetes => {
                let context = self.project_dir.join(self.context_rel());
                let context = context.display().to_string();
                let dockerfile = format!("{out}/Dockerfile");
                self.runner
                    .run(
                        "docker",
                        &args(["build", "-f", &dockerfile, "-t", &runtime.image, &context]),
                        &self.project_dir,
                    )
                    .await?;
                self.runner
                    .run("docker", &args(["push", &runtime.image]), &self.project_dir)
                    .await?;
                self.runner
                    .run(
                        "kubectl",
                        &args(["apply", "-f", &format!("{out}/kubernetes.yaml")]),
                        &self.project_dir,
                    )
                    .await?;
                let mut rollout =
                    args(["rollout", "status", &format!("deployment/{name}"), "--timeout=300s"]);
                if let Some(namespace) = &self.config.namespace {
                    rollout.extend(args(["--namespace", namespace]));
                }
                self.runner.run("kubectl", &rollout, &self.project_dir).await?;
                let status = self.status(&id).await?;
                Ok(DeploymentOutcome::new(Self::NAME, id, None, status))
            }
        }
    }

    async fn status(&self, deployment_id: &str) -> DeployResult<DeploymentStatus> {
        if let Some(project) = deployment_id.strip_prefix("compose:") {
            let output = self
                .runner
                .run(
                    "docker",
                    &args(["compose", "-p", project, "ps", "--all", "--format", "json"]),
                    &self.project_dir,
                )
                .await?;
            return Ok(compose_status(&output));
        }
        if let Some(target) = deployment_id.strip_prefix("kubernetes:") {
            let (namespace, name) = match target.split_once('/') {
                Some((namespace, name)) => (Some(namespace), name),
                None => (None, target),
            };
            let mut get =
                args(["get", "deployment", name, "--ignore-not-found", "--output", "json"]);
            if let Some(namespace) = namespace {
                get.extend(args(["--namespace", namespace]));
            }
            let output = self.runner.run("kubectl", &get, &self.project_dir).await?;
            return Ok(kubernetes_status(&output));
        }
        if deployment_id.starts_with("files:") {
            return Ok(DeploymentStatus::NotFound);
        }
        Err(DeployError::Client {
            message: format!(
                "deployment id '{deployment_id}' is not a container deployment; expected compose:<project> or kubernetes:[<namespace>/]<name>"
            ),
        })
    }

    async fn destroy(&self, deployment_id: &str) -> DeployResult<()> {
        if let Some(project) = deployment_id.strip_prefix("compose:") {
            // Named volumes survive, so a SQLite store outlives the containers.
            self.runner
                .run("docker", &args(["compose", "-p", project, "down"]), &self.project_dir)
                .await?;
            return Ok(());
        }
        if let Some(target) = deployment_id.strip_prefix("kubernetes:") {
            let (namespace, name) = match target.split_once('/') {
                Some((namespace, name)) => (Some(namespace), name),
                None => (None, target),
            };
            // The PersistentVolumeClaim is kept: it holds the session store.
            let mut delete = args([
                "delete",
                "deployment,service,horizontalpodautoscaler",
                "--selector",
                &format!("app.kubernetes.io/name={name}"),
                "--ignore-not-found",
            ]);
            if let Some(namespace) = namespace {
                delete.extend(args(["--namespace", namespace]));
            }
            self.runner.run("kubectl", &delete, &self.project_dir).await?;
            return Ok(());
        }
        Err(DeployError::Client {
            message: format!(
                "deployment id '{deployment_id}' is not a running container deployment; expected compose:<project> or kubernetes:[<namespace>/]<name>"
            ),
        })
    }
}

const DOCKERIGNORE: &str = "# Generated by adk-deploy: build output, VCS state, local secrets and generated files stay\n\
# out of the build context.\n\
target/\n\
**/target/\n\
.git/\n\
**/.env\n\
**/.env.*\n\
!**/.env.example\n\
**/.adk-deploy/\n";

fn compose_status(output: &str) -> DeploymentStatus {
    // `docker compose ps --format json` prints one object per line (or one array on older
    // Compose releases).
    let mut containers = Vec::new();
    for line in output.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(serde_json::Value::Array(items)) => containers.extend(items),
            Ok(value) => containers.push(value),
            Err(_) => {}
        }
    }
    if containers.is_empty() {
        return DeploymentStatus::NotFound;
    }
    let field = |container: &serde_json::Value, key: &str| {
        container.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_string()
    };
    for container in &containers {
        let state = field(container, "State");
        if state == "exited" || state == "dead" {
            return DeploymentStatus::Failed {
                reason: format!(
                    "container {} is {state}: {}",
                    field(container, "Name"),
                    field(container, "Status")
                ),
            };
        }
        if state != "running" {
            return DeploymentStatus::Pending {
                detail: format!("container {} is {state}", field(container, "Name")),
            };
        }
    }
    DeploymentStatus::Ready
}

fn kubernetes_status(output: &str) -> DeploymentStatus {
    let Ok(deployment) = serde_json::from_str::<serde_json::Value>(output.trim()) else {
        return DeploymentStatus::NotFound;
    };
    let count = |pointer: &str| {
        deployment.pointer(pointer).and_then(serde_json::Value::as_u64).unwrap_or(0)
    };
    let wanted = count("/spec/replicas");
    let ready = count("/status/readyReplicas");
    if ready >= wanted.max(1) {
        DeploymentStatus::Ready
    } else {
        DeploymentStatus::Pending { detail: format!("{ready} of {wanted} replicas ready") }
    }
}

fn args<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

/// `name` as an RFC 1123 label: lowercase alphanumerics and `-`, at most `max` characters.
pub(crate) fn dns_label(name: &str, max: usize) -> String {
    let mut label = String::new();
    for c in name.chars() {
        let c = if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' };
        if !(c == '-' && (label.is_empty() || label.ends_with('-'))) {
            label.push(c);
        }
    }
    label.truncate(max);
    let label = label.trim_end_matches('-').to_string();
    if label.is_empty() { "agent".to_string() } else { label }
}

fn normalized(path: &Path) -> PathBuf {
    path.components().filter(|component| !matches!(component, Component::CurDir)).collect()
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn relative_asset(asset: &str) -> DeployResult<String> {
    let path = Path::new(asset);
    let safe = path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    let asset = slash_path(&normalized(path));
    if !safe || asset.is_empty() {
        return Err(DeployError::InvalidManifest {
            message: format!(
                "build asset '{}' must be a path inside the project directory",
                path.display()
            ),
        });
    }
    Ok(asset)
}

/// Runs the container tooling; replaced in tests to record the commands.
#[async_trait]
pub(crate) trait CommandRunner: Send + Sync {
    /// Runs `program` with `args` in `dir` and returns its standard output.
    async fn run(&self, program: &str, args: &[String], dir: &Path) -> DeployResult<String>;
}

struct ProcessRunner;

#[async_trait]
impl CommandRunner for ProcessRunner {
    async fn run(&self, program: &str, args: &[String], dir: &Path) -> DeployResult<String> {
        tracing::info!(command.program = program, command.args = ?args, "running container tooling");
        let output = tokio::process::Command::new(program)
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .map_err(|error| DeployError::Client {
                message: format!("could not run {program}: {error}. Install it and put it on PATH"),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DeployError::Client {
                message: format!("`{program} {}` failed: {stderr}", args.join(" ")),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        replies: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CommandRunner for Recorder {
        async fn run(&self, program: &str, args: &[String], _dir: &Path) -> DeployResult<String> {
            self.calls.lock().await.push(format!("{program} {}", args.join(" ")));
            let mut replies = self.replies.lock().await;
            Ok(if replies.is_empty() { String::new() } else { replies.remove(0) })
        }
    }

    fn manifest(apply: &str) -> DeploymentManifest {
        toml::from_str(&format!(
            r#"
            [agent]
            name = "support_agent"
            binary = "support"

            [[services]]
            name = "sessions"
            kind = "in-memory"

            [targets.container]
            namespace = "agents"
            apply = "{apply}"
            "#
        ))
        .unwrap()
    }

    #[test]
    fn labels_are_rfc_1123() {
        assert_eq!(dns_label("Support_Agent v2", 63), "support-agent-v2");
        assert_eq!(dns_label("__", 63), "agent");
        assert_eq!(dns_label("abcdef", 3), "abc");
        assert_eq!(dns_label("ab-cdef", 3), "ab");
    }

    #[test]
    fn assets_outside_the_project_are_refused() {
        assert_eq!(relative_asset("./prompts/").unwrap(), "prompts");
        assert!(relative_asset("../secrets").is_err());
        assert!(relative_asset("/etc/passwd").is_err());
    }

    #[test]
    fn compose_and_kubernetes_status_are_parsed() {
        assert_eq!(compose_status(""), DeploymentStatus::NotFound);
        assert_eq!(
            compose_status(
                r#"{"Name":"support-agent-1","State":"running","Status":"Up 3 seconds"}"#
            ),
            DeploymentStatus::Ready
        );
        assert_eq!(
            compose_status(r#"[{"Name":"a","State":"exited","Status":"Exited (1)"}]"#),
            DeploymentStatus::Failed { reason: "container a is exited: Exited (1)".to_string() }
        );
        assert_eq!(kubernetes_status(""), DeploymentStatus::NotFound);
        assert_eq!(
            kubernetes_status(r#"{"spec":{"replicas":2},"status":{"readyReplicas":1}}"#),
            DeploymentStatus::Pending { detail: "1 of 2 replicas ready".to_string() }
        );
        assert_eq!(
            kubernetes_status(r#"{"spec":{"replicas":2},"status":{"readyReplicas":2}}"#),
            DeploymentStatus::Ready
        );
    }

    #[tokio::test]
    async fn kubernetes_apply_builds_pushes_applies_and_waits() {
        let project = tempfile::tempdir().unwrap();
        let manifest = manifest("kubernetes");
        let recorder = Arc::new(Recorder::default());
        recorder.replies.lock().await.extend(
            ["", "", "", "", r#"{"spec":{"replicas":1},"status":{"readyReplicas":1}}"#]
                .map(String::from),
        );
        let target = ContainerTarget::from_manifest(project.path(), &manifest)
            .unwrap()
            .with_runner(recorder.clone());
        let outcome = target
            .deploy(&DeployRequest::new(manifest, "registry.example.com/support:0.1.0"))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            DeploymentOutcome::new(
                "container",
                "kubernetes:agents/support-agent",
                None,
                DeploymentStatus::Ready
            )
        );
        let out = target.out_dir().display().to_string();
        let context = project.path().join(".").display().to_string();
        assert_eq!(
            *recorder.calls.lock().await,
            vec![
                format!("docker build -f {out}/Dockerfile -t registry.example.com/support:0.1.0 {context}"),
                "docker push registry.example.com/support:0.1.0".to_string(),
                format!("kubectl apply -f {out}/kubernetes.yaml"),
                "kubectl rollout status deployment/support-agent --timeout=300s --namespace agents".to_string(),
                "kubectl get deployment support-agent --ignore-not-found --output json --namespace agents".to_string(),
            ]
        );
        for file in ["Dockerfile", "Dockerfile.dockerignore", "compose.yaml", "kubernetes.yaml"] {
            assert!(target.out_dir().join(file).is_file(), "{file}");
        }

        target.destroy("kubernetes:agents/support-agent").await.unwrap();
        assert_eq!(
            recorder.calls.lock().await.last().unwrap(),
            "kubectl delete deployment,service,horizontalpodautoscaler --selector app.kubernetes.io/name=support-agent --ignore-not-found --namespace agents"
        );
    }

    #[tokio::test]
    async fn compose_apply_starts_the_project_and_reports_its_endpoint() {
        let project = tempfile::tempdir().unwrap();
        let manifest = manifest("compose");
        let recorder = Arc::new(Recorder::default());
        recorder.replies.lock().await.extend(
            ["", r#"{"Name":"support-agent-agent-1","State":"running","Status":"Up 1 second"}"#]
                .map(String::from),
        );
        let target = ContainerTarget::from_manifest(project.path(), &manifest)
            .unwrap()
            .with_runner(recorder.clone());
        let outcome = target.deploy(&DeployRequest::new(manifest, "support:0.1.0")).await.unwrap();
        assert_eq!(
            outcome,
            DeploymentOutcome::new(
                "container",
                "compose:support-agent",
                Some("http://127.0.0.1:8080".to_string()),
                DeploymentStatus::Ready
            )
        );
        let out = target.out_dir().display().to_string();
        assert_eq!(
            *recorder.calls.lock().await,
            vec![
                format!("docker compose -f {out}/compose.yaml up --detach --build"),
                "docker compose -p support-agent ps --all --format json".to_string(),
            ]
        );
    }
}
