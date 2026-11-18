# Deploy Targets

`adk-deploy` deploys one `adk-deploy.toml` to several providers through one trait,
`adk_deploy::targets::DeployTarget`. Each target maps the same manifest onto its provider's
resources, and every target enforces the same deployment guarantees before it creates anything.

| Target | `--target` | Provider resources | Feature | Account |
|--------|-----------|--------------------|---------|---------|
| Container | `container` | `Dockerfile`, Docker Compose file, Kubernetes `Deployment`, `Service`, `HorizontalPodAutoscaler` | none | none |
| Agent Engine | `agent-engine` | Gemini Enterprise Agent Platform ReasoningEngine | `gcp` | Google Cloud |
| Amazon ECS Express Mode | `ecs-express` | Express service on Fargate, with the load balancer, TLS certificate and autoscaling ECS provisions | `aws` | AWS |

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
| `--target <name>` | `platform` (default), `container`, `agent-engine`, or `ecs-express` |
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
`adk_deploy::targets::verify_endpoint_within`: it waits up to five minutes for the health path to
answer `200` — a new load balancer takes minutes to register its first task — and then, with
`[auth]`, requires an unauthenticated `GET /api/apps` to answer `401` or `403`. A deployment that
answers it with `200` fails the command at once, since runs and the kill switch are then open.

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

## Amazon ECS Express Mode

`ecs-express` deploys through `adk_deploy::aws::EcsExpressClient` (cargo-adk feature `aws`,
`cargo install cargo-adk --features aws`), which calls the Amazon ECS API's
`CreateExpressGatewayService`, `UpdateExpressGatewayService`, `DescribeExpressGatewayService`,
and `DeleteExpressGatewayService` over its JSON protocol, signed with Signature Version 4.
Credentials come from the AWS default chain: environment variables, shared config and SSO
profiles, web identity (GitHub Actions OIDC), and the ECS and EC2 metadata endpoints.

ECS Express Mode runs the image on Fargate and provisions the Application Load Balancer, target
group, security groups, TLS certificate, and autoscaling policy. The agent answers at
`https://<name>.ecs.<region>.on.aws`; the target adds `.ecs.<region>.on.aws` to
`ADK_ALLOWED_HOSTS`. Push the image to Amazon ECR first:

```bash
cargo adk deploy --target container --plan
docker build -f .adk-deploy/container/Dockerfile -t 123456789012.dkr.ecr.us-east-1.amazonaws.com/support-desk:1.4.0 .
docker push 123456789012.dkr.ecr.us-east-1.amazonaws.com/support-desk:1.4.0
cargo adk deploy --target ecs-express
```

| `[targets.ecs-express]` key | Default | Meaning |
|-----------------------------|---------|---------|
| `image` | none | Image URI |
| `region` | `$AWS_REGION`, then `$AWS_DEFAULT_REGION` | Region |
| `executionRoleArn` | required | Pulls the image, writes logs, reads the secrets |
| `infrastructureRoleArn` | required | Manages the load balancer, security groups, and scaling |
| `taskRoleArn` | none | The agent's own AWS calls |
| `serviceArn` | none | The service to update; without it, `deploy` creates one and prints its ARN |
| `cluster` | `default` | Cluster |
| `serviceName` | `[agent] name` | Service name |
| `cpu`, `memory` | `256`, `512` (platform) | Task CPU units and MiB |
| `cpuArchitecture` | `X86_64` | `X86_64` or `ARM64` |
| `subnets`, `securityGroups` | the default VPC | Networking; public subnets give an internet-facing load balancer |
| `ingress` | `public` | `internal` with private subnets keeps the agent off the internet; `public` requires `[auth]` |
| `port`, `args`, `allowedHosts` | `8080`, none, none | As for the container target |
| `logGroup` | none | CloudWatch Logs group |
| `secrets` | none | Secrets Manager secret ARN, or Parameter Store parameter, for each `[[secrets]]` key |
| `ssmParameterPrefix` | none | Parameter Store prefix for keys `secrets` does not map: `/support-desk/` reads `/support-desk/ANTHROPIC_API_KEY` |

Every `[[secrets]]` key the container reads must resolve through `secrets` or
`ssmParameterPrefix`; the plan names any that do not. Tasks have no persistent volume, so a
SQLite session store is refused; use PostgreSQL (Amazon RDS or Aurora), Redis (ElastiCache), or
another network store. `[scaling]` becomes the scaling target:
`targetConcurrentRequests` scales on `REQUEST_COUNT_PER_TARGET`, otherwise `targetCpuPercent` on
`AVERAGE_CPU`. `[health] path` becomes the load balancer health check. `deploy` waits up to
fifteen minutes for the service to report an endpoint. Record the printed ARN as `serviceArn`, so
the next `deploy` rolls out a new revision of the same service instead of creating another.

### IAM

The deployer — the identity `cargo adk deploy` runs as:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "ecs:CreateExpressGatewayService",
        "ecs:UpdateExpressGatewayService",
        "ecs:DescribeExpressGatewayService",
        "ecs:DeleteExpressGatewayService",
        "ecs:TagResource"
      ],
      "Resource": "*"
    },
    {
      "Effect": "Allow",
      "Action": "iam:PassRole",
      "Resource": [
        "arn:aws:iam::123456789012:role/ecsTaskExecutionRole",
        "arn:aws:iam::123456789012:role/ecsInfrastructureRoleForExpressServices"
      ],
      "Condition": {
        "StringEquals": { "iam:PassedToService": ["ecs.amazonaws.com", "ecs-tasks.amazonaws.com"] }
      }
    }
  ]
}
```

Add the task role to `iam:PassRole` when `taskRoleArn` is set. The first Express service in an
account also needs `iam:CreateServiceLinkedRole` for `ecs.amazonaws.com`, unless
`AWSServiceRoleForECS` exists.

| Role | Trusts | Policies |
|------|--------|----------|
| Execution role | `ecs-tasks.amazonaws.com` | `AmazonECSTaskExecutionRolePolicy`; `secretsmanager:GetSecretValue` on each secret ARN; `ssm:GetParameters` on `arn:aws:ssm:<region>:<account>:parameter/<prefix>*`; `kms:Decrypt` on a customer-managed key that encrypts them |
| Infrastructure role | `ecs.amazonaws.com` | `AmazonECSInfrastructureRoleforExpressGatewayServices` |
| Task role (optional) | `ecs-tasks.amazonaws.com` | What the agent itself calls; nothing for the agent alone |

## Related

- [Agent Engine](agent-engine.md) — the dispatch surface an Agent Engine container serves
- [Server](server.md) — authentication, the kill switch, and host checks
- [Access control](../security/access-control.md) — `RequestContextExtractor` implementations
