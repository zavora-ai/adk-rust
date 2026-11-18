# Deploy Targets

`adk-deploy` deploys one `adk-deploy.toml` to several providers through one trait,
`adk_deploy::targets::DeployTarget`. Each target maps the same manifest onto its provider's
resources, and every target enforces the same deployment guarantees before it creates anything.

| Target | `--target` | Provider resources | Feature | Account |
|--------|-----------|--------------------|---------|---------|
| Container | `container` | `Dockerfile`, Docker Compose file, Kubernetes `Deployment`, `Service`, `HorizontalPodAutoscaler` | none | none |
| Agent Engine | `agent-engine` | Gemini Enterprise Agent Platform ReasoningEngine | `gcp` | Google Cloud |

```bash
cargo adk deploy --target container --plan                  # write and print, deploy nothing
cargo adk deploy --target container                         # deploy
cargo adk deploy --target container --status compose:support-desk
cargo adk deploy --target container --destroy compose:support-desk
```

`cargo adk deploy` without `--target` keeps pushing a bundle to the ADK platform control plane.

## Guarantees

`RuntimeSpec::from_manifest` derives the provider-neutral runtime from the manifest. A manifest
that breaks a guarantee is refused with the change to make, before any provider is contacted.

| Guarantee | Enforcement |
|-----------|-------------|
| Secrets come from the provider's secret store | An `[env]` literal under a secret-bearing name (`*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*_SECRET_KEY`, `*_PASSWORD`, `*_PRIVATE_KEY`) is refused, and so is a `connectionUrl` carrying a password. Each `{ secretRef = "KEY" }` becomes a reference into the provider's secret store; no generated file or request body holds a secret value |
| Durable stores are configured explicitly | A session store binding is required (`postgres`, `redis`, `mongo-db`, `neo4j`, `firestore`, `sqlite`, or `in-memory`). A `managed` binding is refused on a target that provisions no stores. SQLite is refused beyond one replica. `in-memory` is accepted only when declared, with a warning |
| Runs and the kill switch stay authenticated | A target with public ingress refuses a manifest without `[auth]` in `bearer` or `oidc` mode. After deploying, `cargo adk deploy` checks the endpoint: `GET /api/apps` without credentials must answer `401` or `403` |
| Health and readiness | `[health]` becomes the provider's liveness and readiness probes |
| One configuration per variable | An `[env]` entry that sets a contract variable such as `PORT` is refused |

Concerns the checks accept, such as an in-memory session store or a declared secret nothing
references, print as warnings in the plan.

## The deploy contract

Every target sets the same environment variables on the agent container, so one binary runs
unchanged on every target. `adk_deploy::targets::contract` names them.

| Variable | Value |
|----------|-------|
| `PORT` | The container port, `8080` unless the target sets another |
| `BIND_HOST` | `0.0.0.0` — the provider's network fronts the container |
| `ADK_ALLOWED_HOSTS` | Comma-separated public host names, for `SecurityConfig::allowed_hosts` |
| `ADK_AGENT_NAME`, `ADK_AGENT_VERSION` | `[agent] name` and `version` |
| `ADK_DEPLOY_TARGET` | The target name |
| `ADK_SERVICE_<NAME>_URL` | The connection URL of the `[[services]]` binding `<name>`, from its `secretRef` or `connectionUrl` |
| `ADK_AUTH_MODE` | `[auth] mode`: `bearer` or `oidc` |
| `ADK_AUTH_ISSUER`, `ADK_AUTH_AUDIENCE`, `ADK_AUTH_JWKS_URI`, `ADK_AUTH_REQUIRED_SCOPES` | The rest of `[auth]` |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | `[telemetry]` |
| Each `[env]` entry | Its literal value, or the referenced secret |

The agent reads them at start. This server takes its session store from the `sessions` binding,
answers its public host names, and authenticates callers when `[auth]` asks for it:

```rust
use std::sync::Arc;

use adk_core::{GovernanceControl, SingleAgentLoader};
use adk_deploy::targets::contract;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{SessionService, SqliteSessionService};

async fn app(agent: Arc<dyn adk_core::Agent>) -> anyhow::Result<axum::Router> {
    let url = contract::service_url("sessions")
        .ok_or_else(|| anyhow::anyhow!("the sessions binding is not set"))?;
    let sessions = SqliteSessionService::new(&url).await?;
    sessions.migrate().await?;
    let sessions: Arc<dyn SessionService> = Arc::new(sessions);

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_governance(GovernanceControl::new())
        .with_allowed_hosts(contract::allowed_hosts());
    // With [auth], add .with_request_context(extractor) that grants the admin scope to operators.
    Ok(ServerBuilder::new(config).enable_governance_endpoints().build())
}
```

The [`deploy_targets` example](../../../examples/deploy_targets/README.md) is a complete agent on
the contract: SQLite sessions on a volume, bearer authentication with the `admin` scope for the
operator, and the kill switch endpoints.

## Target settings

Each target reads its own `[targets.<name>]` table. A misspelled key is refused. `image` in a
target table is the image `cargo adk deploy` uses when `--image` is absent.

```toml
[targets.container]
image = "ghcr.io/acme/support-desk:1.4.0"
allowedHosts = ["support.example.com"]
namespace = "agents"

[targets.agent-engine]
image = "us-central1-docker.pkg.dev/acme/agents/support-desk:1.4.0"
project = "acme"
location = "us-central1"
```

## `cargo adk deploy --target`

| Flag | Effect |
|------|--------|
| `--target <name>` | `platform` (default), `container`, or `agent-engine` |
| `--image <ref>` | The image to deploy; default `[targets.<name>] image`, then `<agent name>:<version>` for `container` |
| `--manifest <path>` | The manifest (default `adk-deploy.toml`); the target resolves paths from its directory |
| `--plan` | Writes the files or provider request bodies and prints the plan; deploys nothing |
| `--status <id>` | Prints the deployment's state: `pending`, `ready`, `failed`, or `notFound` |
| `--destroy <id>` | Removes the deployment |
| `--no-verify` | Skips the endpoint check after deploying |

`--plan` writes the container target's files to its output directory, and every other target's
provider requests to `.adk-deploy/<target>/`. The plan names the secrets the provider's secret
store must hold, the ingress, and whether callers authenticate.

After `deploy`, when the target reports an endpoint, `cargo adk deploy` calls
`adk_deploy::targets::verify_endpoint`: the health path must answer `200`, and with `[auth]`, an
unauthenticated `GET /api/apps` must answer `401` or `403`. A deployment that answers it with
`200` fails the command, since runs and the kill switch are then open.

## Container

The container target needs no cloud account. `--plan` writes four files into
`.adk-deploy/container/`:

| File | Contents |
|------|----------|
| `Dockerfile` | Multi-stage build: `cargo build --locked` on `rust:1.95-slim`, run as UID 65532 (`nonroot`) on `gcr.io/distroless/cc-debian12:nonroot` with `[build] assets` copied in, and `dataDir` created owned by that user |
| `Dockerfile.dockerignore` | Keeps `target/`, `.git/`, `.env` files, and `.adk-deploy/` out of the build context |
| `compose.yaml` | One service with a read-only root filesystem, every capability dropped, and secrets interpolated from the environment at `docker compose up` |
| `kubernetes.yaml` | `Deployment` with probes from `[health]`, a pod running as UID 65532 with a read-only root filesystem and `fsGroup` for volumes, and secrets from a Kubernetes `Secret`; a `Service`; a `HorizontalPodAutoscaler` when `maxInstances` exceeds `minInstances`; a `PersistentVolumeClaim` when `dataDir` is set |

| `[targets.container]` key | Default | Meaning |
|---------------------------|---------|---------|
| `image` | `<agent name>:<version>` | Image to build and run |
| `args` | none | Arguments passed to the agent binary |
| `port` | `8080` | Container port |
| `allowedHosts` | none | Public host names, exposed as `ADK_ALLOWED_HOSTS` |
| `ingress` | `internal` | `internal`: Compose publishes on `127.0.0.1`, Kubernetes uses a `ClusterIP` service. `public`: every interface and a `LoadBalancer` service; requires `[auth]` |
| `publish` | per `ingress` | Host address Compose publishes the port on |
| `namespace` | `kubectl`'s current namespace | Kubernetes namespace |
| `secretName` | `<name>-secrets` | Kubernetes `Secret` holding the `[[secrets]]` keys |
| `cpu`, `memory` | `250m`, `256Mi` | Kubernetes requests; `memory` is also the limit |
| `dataDir` | none | Directory on a persistent volume, for a SQLite session store, which requires it; one replica, `Recreate` rollouts |
| `dataSize` | `1Gi` | Size of the persistent volume |
| `context` | `.` | Docker build context relative to the manifest; the workspace root when the agent is a workspace member or has path dependencies |
| `outDir` | `.adk-deploy/container` | Where the files are written |
| `apply` | `none` | What `deploy` runs after writing the files: `none`, `compose`, or `kubernetes` |
| `builderImage`, `runtimeImage` | see above | Build and runtime stage images |

### Docker Compose

Secrets come from the environment of `docker compose up` or an `--env-file`; the file holds none:

```bash
cargo adk deploy --target container --plan
docker compose --env-file .env -f .adk-deploy/container/compose.yaml up --detach --build
```

With `apply = "compose"`, `cargo adk deploy --target container` runs `docker compose up --detach
--build`, reports `compose:<name>` as the deployment id, and checks `http://127.0.0.1:<port>`.

### Kubernetes

Create the `Secret` from your secret store first — with the External Secrets Operator, or:

```bash
kubectl create secret generic support-desk-secrets --namespace agents \
  --from-literal=ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
  --from-literal=ADK_API_TOKEN="$ADK_API_TOKEN" \
  --from-literal=DATABASE_URL="$DATABASE_URL"
```

With `apply = "kubernetes"`, `cargo adk deploy --target container` runs `docker build`,
`docker push`, `kubectl apply`, and `kubectl rollout status`, and reports
`kubernetes:<namespace>/<name>`. `--destroy` deletes the `Deployment`, `Service`, and
`HorizontalPodAutoscaler` and keeps the `PersistentVolumeClaim`, which holds the session store.

The identity `kubectl` uses needs these permissions in the namespace:

| API group | Resources | Verbs |
|-----------|-----------|-------|
| `apps` | `deployments` | `get`, `list`, `watch`, `create`, `patch`, `delete` |
| `""` (core) | `services`, `persistentvolumeclaims` | `get`, `create`, `patch`, `delete` |
| `autoscaling` | `horizontalpodautoscalers` | `get`, `create`, `patch`, `delete` |
| `apps` | `replicasets` | `get`, `list`, `watch` (for `kubectl rollout status`) |

The `Secret` is created by whoever manages secrets; the deploy identity never reads it. The pod
runs with `automountServiceAccountToken: false`, so the agent holds no Kubernetes credentials.

## Agent Engine

`agent-engine` deploys through `adk_deploy::gcp::GcpDeployClient` (cargo-adk feature `gcp`,
`cargo install cargo-adk --features gcp`). It authenticates with Application Default
Credentials. The container serves the [Agent Engine dispatch surface](agent-engine.md).

| `[targets.agent-engine]` key | Default | Meaning |
|------------------------------|---------|---------|
| `image` | none | Artifact Registry image URI |
| `project`, `location` | `$GOOGLE_CLOUD_PROJECT`, `$GOOGLE_CLOUD_LOCATION` | Where the engine is created |
| `serviceAccount` | the Reasoning Engine service agent | Identity the engine runs as |
| `kmsKey` | none | Customer-managed encryption key |
| `displayName` | `[agent] name` | Engine display name |
| `cpu`, `memory` | platform defaults | Resource limits |
| `secrets` | the key lower-cased with `_` as `-` | Secret Manager secret for each `[[secrets]]` key |

Agent Engine authenticates every caller with Google Cloud IAM, so `[auth]` is not required, and
it supplies Vertex AI Sessions and Memory Bank, so `managed` bindings are accepted. `PORT` is the
platform's to assign. Each `deploy` creates an engine; `--destroy <engine>` deletes one.

| Who | Role | Why |
|-----|------|-----|
| The deployer | `roles/aiplatform.user` | `reasoningEngines.create`, `get`, and `delete`, and the operations they return |
| The deployer | `roles/iam.serviceAccountUser` on `serviceAccount` | Runs the engine as that account |
| The engine's identity | `roles/secretmanager.secretAccessor` on each referenced secret | Reads `secretEnv` at start |
| The engine's identity | `roles/artifactregistry.reader` on the image repository, when it is not already readable | Pulls the image |
| The engine's identity | `roles/aiplatform.user`, when it is a custom service account | Reaches Vertex AI Sessions and Memory Bank |

## Related

- [Agent Engine](agent-engine.md) — the dispatch surface an Agent Engine container serves
- [Server](server.md) — authentication, the kill switch, and host checks
- [Access control](../security/access-control.md) — `RequestContextExtractor` implementations
