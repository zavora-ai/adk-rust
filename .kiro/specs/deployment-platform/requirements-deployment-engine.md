# Requirements Document

## Introduction

The ADK Deployment Platform is a production deployment system purpose-built for ADK-Rust agents. It provides a CLI-driven workflow and a control plane API that lets agent developers package, deploy, configure, scale, and monitor their agents in containerized environments. Think "Vercel for AI agents" — zero-to-production with a single command, while supporting the full breadth of ADK-Rust capabilities: multi-model LLM backends, session persistence (InMemory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Firestore), memory/RAG, real-time audio/video, graph workflows, A2A federation, guardrails, auth, telemetry, and plugins.

The platform is implemented as two new crates (`adk-deploy` for the client/CLI and `adk-deploy-server` for the control plane) plus extensions to the existing `adk-cli` for ergonomic developer experience.

## Delivery Strategy and v1 Scope

This specification is delivered in phases. The publishable `v1` target is a **self-hosted, single-cluster deployment platform** that composes existing ADK-Rust crates rather than re-implementing their responsibilities inside the deployment layer.

The `v1` implementation SHALL:

1. Treat `adk-deploy` as the canonical deployment contract for all supported ADK-Rust runtime capabilities.
2. Treat `adk-deploy-server` as a control plane that configures and orchestrates `adk-server`/`adk-runner` based workloads, rather than a synthetic dashboard-only service.
3. Integrate existing ADK-Rust crates for authentication, telemetry, evaluation, runtime, sessions, artifacts, guardrails, graph workflows, and Studio-originated deployments wherever those crates already define the platform behavior.
4. Reject unsupported capability combinations during manifest validation or deploy admission, rather than silently accepting them and presenting simulated control-plane data.
5. Use operator-provided bearer credentials and externally managed identity integration for self-hosted authentication in `v1`; fully managed identity flows and multi-cloud deployment modes are deferred to later phases.

Deferred from the publishable `v1` target:

- Fully managed cloud control plane
- Hybrid customer-compute deployment mode
- Built-in identity provider / hosted OAuth authority
- Simulated observability, billing, or fleet state presented as production data

## Glossary

- **Agent_Bundle**: A self-contained deployment artifact containing the compiled agent binary, a deployment manifest (`adk-deploy.toml`), and optional asset files (e.g., skill definitions, prompt templates, static data).
- **Deployment_Manifest**: A TOML configuration file (`adk-deploy.toml`) at the project root that declares the agent's runtime requirements, environment variables, service backends, scaling rules, and health check configuration.
- **Control_Plane**: The server-side component (`adk-deploy-server`) that receives deployment requests, manages agent lifecycle, orchestrates infrastructure, and exposes management APIs.
- **Agent_Instance**: A single running container or process executing an agent binary, serving HTTP traffic via `adk-server`.
- **Deployment**: A versioned release of an Agent_Bundle to the Control_Plane, identified by a unique deployment ID and associated with an environment.
- **Environment**: A named isolation boundary (e.g., `staging`, `production`) with its own configuration, secrets, and scaling rules.
- **Health_Probe**: An HTTP endpoint (`/api/health`) on each Agent_Instance used by the Control_Plane to determine instance readiness and liveness.
- **Secret_Store**: An encrypted key-value store managed by the Control_Plane for sensitive configuration (API keys, database credentials, OAuth tokens).
- **Scaling_Policy**: A set of rules defining how the Control_Plane adjusts the number of Agent_Instances based on load metrics.
- **Deployment_CLI**: The `adk deploy` subcommand group added to `adk-cli` for developer interaction with the Control_Plane.
- **Agent_Log**: Structured log output (JSON) from an Agent_Instance, collected and queryable via the Control_Plane.
- **Rollback**: The act of reverting a Deployment to a previously successful version.
- **Blue_Green_Deployment**: A deployment strategy where a new version runs alongside the old version, with traffic switched atomically after health verification.
- **Canary_Deployment**: A deployment strategy where a small percentage of traffic is routed to the new version before full rollout.
- **Service_Binding**: A declaration in the Deployment_Manifest that connects an agent to a backing service (database, cache, message queue) managed or referenced by the Control_Plane.
- **Studio_Deploy_Flow**: The end-to-end process of deploying an agent from ADK Studio's visual builder, which includes code generation from the `ProjectSchema`, `DeployManifest` creation, conversion to `adk-deploy.toml`, bundling, and push to the Control_Plane.

## Requirements

### Requirement 1: ADK Studio Deployment Integration

**User Story:** As an agent developer using ADK Studio's visual builder, I want to deploy my visually designed agent directly to the deployment platform from the Studio UI, so that I can go from visual design to production without leaving the Studio.

#### Acceptance Criteria

1. WHEN the developer clicks the "Deploy" button in ADK Studio, THE Studio backend SHALL generate a `DeployManifest` from the `ProjectSchema` and convert it to an `adk-deploy.toml` Deployment_Manifest.
2. THE Studio backend SHALL invoke the code generation pipeline to produce a compilable Rust project from the visual workflow before bundling.
3. WHEN the Studio project declares action nodes, THE generated `adk-deploy.toml` SHALL include the inferred permissions and risk tier from the `DeployManifest`.
4. THE Studio backend SHALL map Studio `DeployManifest` fields (capabilities, guardrails, callback mode, tool confirmation policy) to corresponding Deployment_Manifest sections.
5. WHEN the Studio project declares tool configurations of type `mcp`, THE generated Deployment_Manifest SHALL include MCP server endpoints as Service_Bindings.
6. THE Studio deploy flow SHALL support selecting a target Environment (`staging`, `production`) from the UI before deployment.
7. WHEN the Studio project uses a provider that requires API keys (e.g., OpenAI, Anthropic, Gemini), THE Studio deploy flow SHALL verify that the required secret keys exist in the target Environment's Secret_Store before proceeding.
8. THE Control_Plane SHALL accept deployments originating from ADK Studio with `source.kind = "adk_studio"` and track the originating `project_id` for traceability.
9. WHEN a Studio-originated Deployment is active, THE Control_Plane SHALL expose a link back to the Studio project in the deployment status response.
10. THE Control_Plane SHALL normalize deployment source metadata in status responses using a stable source kind plus optional source identifiers, so the Console can render consistent source badges and deep links for Studio, CLI, and catalog/template deployments.

### Requirement 2: Deployment Manifest Definition (CLI)

**User Story:** As an agent developer, I want to declare my agent's deployment configuration in a single TOML file, so that I can version-control my infrastructure alongside my code.

#### Acceptance Criteria

1. THE Deployment_CLI SHALL parse a `adk-deploy.toml` file from the project root directory.
2. WHEN the `adk-deploy.toml` file is absent, THE Deployment_CLI SHALL return an error message indicating the file path and expected format.
3. THE Deployment_Manifest SHALL support declaring the agent binary name, Rust toolchain version, build profile, and target triple.
4. THE Deployment_Manifest SHALL support declaring environment variables as key-value pairs with optional secret references.
5. THE Deployment_Manifest SHALL support declaring Service_Bindings for session backends (InMemory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Firestore).
6. THE Deployment_Manifest SHALL support declaring Service_Bindings for memory backends (InMemory, SQLite, PostgreSQL/pgvector, Redis, MongoDB, Neo4j).
7. THE Deployment_Manifest SHALL support declaring Scaling_Policy parameters including minimum instances, maximum instances, and target metric thresholds.
8. WHEN the Deployment_Manifest contains invalid TOML syntax, THE Deployment_CLI SHALL return an error message with the line number and column of the syntax error.
9. WHEN the Deployment_Manifest references an unknown Service_Binding type, THE Deployment_CLI SHALL return an error listing valid binding types.

### Requirement 3: Agent Bundling and Packaging

**User Story:** As an agent developer, I want to package my agent into a deployable bundle with a single command, so that I can prepare it for deployment without manual steps.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy build`, THE Deployment_CLI SHALL compile the agent binary in release mode for the target triple specified in the Deployment_Manifest.
2. WHEN the developer runs `adk deploy build`, THE Deployment_CLI SHALL produce an Agent_Bundle containing the compiled binary, the Deployment_Manifest, and any declared asset files.
3. THE Agent_Bundle SHALL include a SHA-256 checksum file for integrity verification.
4. WHEN compilation fails, THE Deployment_CLI SHALL display the compiler error output and return a non-zero exit code.
5. THE Deployment_CLI SHALL support cross-compilation targets including `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`.
6. WHEN the Deployment_Manifest declares feature flags, THE Deployment_CLI SHALL pass the feature flags to the Cargo build command.

### Requirement 4: Deployment Lifecycle

**User Story:** As an agent developer, I want to deploy my agent to an environment with a single command, so that I can go from code to production quickly.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy push`, THE Deployment_CLI SHALL upload the Agent_Bundle to the Control_Plane and return a Deployment ID.
2. WHEN the developer runs `adk deploy push --env production`, THE Deployment_CLI SHALL target the specified Environment.
3. THE Control_Plane SHALL assign a monotonically increasing version number to each Deployment within an Environment.
4. WHEN the upload completes, THE Control_Plane SHALL validate the Agent_Bundle checksum against the declared SHA-256 hash.
5. IF the checksum validation fails, THEN THE Control_Plane SHALL reject the Deployment and return an error with the expected and actual checksums.
6. WHEN a Deployment is accepted, THE Control_Plane SHALL provision Agent_Instances according to the Scaling_Policy minimum instance count.
7. THE Control_Plane SHALL expose each deployed agent at a stable URL in the format `https://{agent-name}.{environment}.{platform-domain}`.

### Requirement 5: Health Checking and Readiness

**User Story:** As a platform operator, I want the platform to continuously verify agent health, so that unhealthy instances are replaced automatically.

#### Acceptance Criteria

1. THE Control_Plane SHALL send HTTP GET requests to the Health_Probe endpoint of each Agent_Instance at a configurable interval (default: 10 seconds).
2. WHEN an Agent_Instance returns a non-200 status code from the Health_Probe for three consecutive checks, THE Control_Plane SHALL mark the instance as unhealthy.
3. WHEN an Agent_Instance is marked unhealthy, THE Control_Plane SHALL terminate the instance and provision a replacement.
4. WHEN a new Agent_Instance starts, THE Control_Plane SHALL wait for a successful Health_Probe response before routing traffic to the instance.
5. THE Deployment_Manifest SHALL support configuring the health check interval, timeout, and failure threshold.
6. IF an Agent_Instance does not respond to the Health_Probe within the configured timeout (default: 5 seconds), THEN THE Control_Plane SHALL count the check as a failure.

### Requirement 6: Scaling and Autoscaling

**User Story:** As an agent developer, I want my agent to scale automatically based on load, so that it handles traffic spikes without manual intervention.

#### Acceptance Criteria

1. THE Control_Plane SHALL maintain at least the minimum instance count declared in the Scaling_Policy at all times.
2. THE Control_Plane SHALL maintain at most the maximum instance count declared in the Scaling_Policy at all times.
3. WHEN the average request latency across Agent_Instances exceeds the target latency threshold for 60 seconds, THE Control_Plane SHALL add one Agent_Instance.
4. WHEN the average CPU utilization across Agent_Instances falls below 20% for 300 seconds and the current instance count exceeds the minimum, THE Control_Plane SHALL remove one Agent_Instance.
5. THE Deployment_Manifest SHALL support declaring scaling metrics including request latency, CPU utilization, and concurrent request count.
6. WHEN scaling up, THE Control_Plane SHALL wait for the new Agent_Instance to pass the Health_Probe before routing traffic to the instance.
7. WHEN scaling down, THE Control_Plane SHALL drain active requests from the target Agent_Instance before terminating the instance, with a maximum drain timeout of 30 seconds.

### Requirement 7: Environment and Secret Management

**User Story:** As an agent developer, I want to manage environment-specific configuration and secrets securely, so that API keys and credentials are not stored in source code.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy secret set KEY VALUE --env production`, THE Deployment_CLI SHALL store the secret in the Secret_Store for the specified Environment.
2. THE Secret_Store SHALL encrypt all secret values at rest using AES-256-GCM.
3. WHEN an Agent_Instance starts, THE Control_Plane SHALL inject secrets declared in the Deployment_Manifest as environment variables into the instance process.
4. THE Deployment_CLI SHALL support listing secret keys (without values) for an Environment via `adk deploy secret list --env production`.
5. WHEN the developer runs `adk deploy secret delete KEY --env production`, THE Deployment_CLI SHALL remove the secret from the Secret_Store.
6. THE Control_Plane SHALL support multiple Environments per agent, each with independent secret and configuration namespaces.
7. IF a Deployment_Manifest references a secret key that does not exist in the Secret_Store, THEN THE Control_Plane SHALL reject the Deployment and list the missing secret keys.

### Requirement 8: Deployment Strategies

**User Story:** As an agent developer, I want to choose how new versions are rolled out, so that I can minimize risk during deployments.

#### Acceptance Criteria

1. THE Deployment_Manifest SHALL support declaring a deployment strategy of `rolling`, `blue-green`, or `canary`.
2. WHEN the strategy is `rolling`, THE Control_Plane SHALL replace Agent_Instances one at a time, waiting for each new instance to pass the Health_Probe before replacing the next.
3. WHEN the strategy is `blue-green`, THE Control_Plane SHALL provision a complete set of new Agent_Instances, verify health of all new instances, and then atomically switch traffic from old to new instances.
4. WHEN the strategy is `canary`, THE Control_Plane SHALL route a configurable percentage of traffic (default: 10%) to the new version and the remainder to the current version.
5. WHILE a Canary_Deployment is active, THE Control_Plane SHALL monitor error rates on the canary instances.
6. IF the canary error rate exceeds 5% during a Canary_Deployment, THEN THE Control_Plane SHALL automatically roll back the canary and route all traffic to the previous version.
7. WHEN the developer runs `adk deploy promote`, THE Control_Plane SHALL promote the canary to receive 100% of traffic and decommission the previous version.
8. THE Control_Plane SHALL expose rollout status using a single canonical deployment strategy enum plus strategy-specific phase fields, so that the Console can render either rolling, blue-green, or canary state without mixing the terminology for multiple strategies on one deployment.

### Requirement 9: Rollback

**User Story:** As an agent developer, I want to roll back to a previous version instantly, so that I can recover from bad deployments.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy rollback --env production`, THE Control_Plane SHALL revert to the most recent successful Deployment version.
2. WHEN the developer runs `adk deploy rollback --env production --version N`, THE Control_Plane SHALL revert to the specified Deployment version N.
3. THE Control_Plane SHALL retain the five most recent Deployment versions per Environment for rollback purposes.
4. WHEN a rollback is initiated, THE Control_Plane SHALL apply the same deployment strategy (rolling, blue-green) as the original Deployment.
5. IF the specified rollback version does not exist, THEN THE Control_Plane SHALL return an error listing available versions.

### Requirement 10: Logging and Log Streaming

**User Story:** As an agent developer, I want to view real-time logs from my deployed agents, so that I can debug issues in production.

#### Acceptance Criteria

1. THE Control_Plane SHALL collect Agent_Logs from all Agent_Instances in structured JSON format.
2. WHEN the developer runs `adk deploy logs --env production`, THE Deployment_CLI SHALL stream Agent_Logs in real-time via SSE.
3. THE Deployment_CLI SHALL support filtering logs by severity level (`error`, `warn`, `info`, `debug`) via the `--level` flag.
4. THE Deployment_CLI SHALL support filtering logs by Agent_Instance ID via the `--instance` flag.
5. THE Control_Plane SHALL retain Agent_Logs for a configurable duration (default: 7 days).
6. WHEN the developer runs `adk deploy logs --env production --since 1h`, THE Deployment_CLI SHALL display historical logs from the specified time window.

### Requirement 11: Metrics and Observability

**User Story:** As an agent developer, I want to see metrics about my agent's performance, so that I can understand usage patterns and optimize.

#### Acceptance Criteria

1. THE Control_Plane SHALL collect request count, request latency (p50, p95, p99), error rate, and active connection count from each Agent_Instance.
2. THE Control_Plane SHALL collect LLM-specific metrics including token usage (prompt tokens, completion tokens), model invocation count, and model latency per provider.
3. THE Control_Plane SHALL collect session metrics including active session count, session creation rate, and session storage utilization.
4. WHEN the developer runs `adk deploy metrics --env production`, THE Deployment_CLI SHALL display a summary of current metrics.
5. THE Control_Plane SHALL expose a Prometheus-compatible `/metrics` endpoint for integration with external monitoring systems.
6. THE Control_Plane SHALL forward OpenTelemetry spans from Agent_Instances to a configurable OTLP collector endpoint.

### Requirement 12: Service Binding Provisioning

**User Story:** As an agent developer, I want the platform to provision backing services declared in my manifest, so that I do not need to manage database infrastructure manually.

#### Acceptance Criteria

1. WHEN the Deployment_Manifest declares a PostgreSQL Service_Binding, THE Control_Plane SHALL provision a PostgreSQL instance and inject the connection URL as an environment variable.
2. WHEN the Deployment_Manifest declares a Redis Service_Binding, THE Control_Plane SHALL provision a Redis instance and inject the connection URL as an environment variable.
3. THE Control_Plane SHALL support `managed` and `external` binding modes for each Service_Binding type.
4. WHEN the binding mode is `external`, THE Control_Plane SHALL use the connection URL provided in the Deployment_Manifest or Secret_Store without provisioning infrastructure.
5. WHEN the binding mode is `managed`, THE Control_Plane SHALL handle backup, failover, and version upgrades for the provisioned service.
6. THE Deployment_Manifest SHALL support declaring Service_Bindings for artifact storage backends.

### Requirement 13: A2A Federation Support

**User Story:** As an agent developer, I want to deploy agents that communicate with each other via A2A protocol, so that I can build multi-agent systems in production.

#### Acceptance Criteria

1. WHEN the Deployment_Manifest declares A2A federation enabled, THE Control_Plane SHALL expose the agent's A2A endpoints (`/.well-known/agent.json`, `/a2a`, `/a2a/stream`).
2. THE Control_Plane SHALL maintain a service registry of all deployed agents with A2A enabled within the platform.
3. WHEN an agent declares A2A dependencies on other agents, THE Control_Plane SHALL inject the dependent agents' A2A endpoint URLs as environment variables.
4. THE Control_Plane SHALL support A2A discovery via the `/.well-known/agent.json` endpoint for each deployed agent.

### Requirement 14: Real-Time and WebSocket Support

**User Story:** As an agent developer, I want to deploy agents that use real-time audio/video streaming, so that voice-enabled agents work in production.

#### Acceptance Criteria

1. WHEN the Deployment_Manifest declares real-time capabilities, THE Control_Plane SHALL configure the Agent_Instance with WebSocket upgrade support.
2. THE Control_Plane SHALL configure load balancer sticky sessions for WebSocket connections to ensure connection affinity.
3. WHEN scaling down, THE Control_Plane SHALL not terminate Agent_Instances with active WebSocket connections until the connections close or the drain timeout expires.
4. THE Deployment_Manifest SHALL support declaring required real-time features (`openai`, `gemini`, `vertex-live`, `livekit`, `openai-webrtc`).

### Requirement 15: CLI Authentication and Multi-Tenancy

**User Story:** As an agent developer, I want to authenticate with the deployment platform and manage my agents independently from other users, so that my deployments are secure and isolated.

#### Acceptance Criteria

1. FOR the publishable self-hosted `v1`, WHEN the developer runs `adk deploy login`, THE Deployment_CLI SHALL accept an operator-provided bearer token, validate it with the Control_Plane, and store it using the local OS credential store instead of plaintext config persistence.
2. THE Deployment_CLI SHALL include the bearer token in all requests to the Control_Plane API.
3. THE Control_Plane SHALL bind each authenticated request to an explicit workspace/user context such that each authenticated principal can only access their authorized agents, Deployments, Environments, and secrets.
4. FOR later managed-platform phases, THE Deployment_CLI MAY initiate an OAuth2/OIDC authorization code flow with refresh-token support instead of direct token entry.
5. IF locally stored credentials are missing or invalid, THEN THE Deployment_CLI SHALL prompt the developer to re-authenticate via `adk deploy login`.
6. WHEN the developer runs `adk deploy logout`, THE Deployment_CLI SHALL delete the locally stored credentials.

### Requirement 16: Deployment Status and History

**User Story:** As an agent developer, I want to see the status and history of my deployments, so that I can track what is running and what changed.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy status --env production`, THE Deployment_CLI SHALL display the current Deployment version, instance count, health status of each instance, and the deployment timestamp.
2. WHEN the developer runs `adk deploy history --env production`, THE Deployment_CLI SHALL display the five most recent Deployments with version number, timestamp, status, and deployment strategy used.
3. THE Control_Plane SHALL track Deployment status transitions: `pending`, `building`, `deploying`, `healthy`, `degraded`, `failed`, `rolled-back`.
4. WHEN a Deployment transitions to `failed`, THE Control_Plane SHALL automatically initiate a rollback to the previous healthy version.
5. THE deployment status response SHALL include deployment source metadata, rollout strategy, rollout phase, and any strategy-specific fields required by the Console to render the deployment pipeline and agent detail views.

### Requirement 17: Guardrail and Auth Configuration

**User Story:** As an agent developer, I want to configure guardrails and authentication for my deployed agents via the manifest, so that production agents enforce safety and access controls.

#### Acceptance Criteria

1. THE Deployment_Manifest SHALL support declaring guardrail configurations including content filtering rules and PII redaction settings.
2. THE Deployment_Manifest SHALL support declaring authentication requirements including API key validation, JWT verification, and OAuth2 scopes.
3. WHEN guardrails are declared, THE Control_Plane SHALL configure the Agent_Instance with the specified guardrail pipeline.
4. WHEN authentication is declared, THE Control_Plane SHALL configure the Agent_Instance with the specified auth middleware.

### Requirement 18: Manifest Validation and Dry Run

**User Story:** As an agent developer, I want to validate my deployment manifest before deploying, so that I catch configuration errors early.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy validate`, THE Deployment_CLI SHALL parse and validate the Deployment_Manifest against the schema without contacting the Control_Plane.
2. THE Deployment_CLI SHALL validate that all referenced secret keys exist in the Secret_Store when the `--remote` flag is provided.
3. WHEN the developer runs `adk deploy dry-run --env production`, THE Deployment_CLI SHALL simulate the deployment and display the planned actions without executing changes.
4. THE Deployment_CLI SHALL validate that declared Service_Binding types are compatible with the agent's declared feature flags.
5. IF validation detects errors, THEN THE Deployment_CLI SHALL display all errors (not just the first) with actionable remediation suggestions.

### Requirement 18: Container Image Generation

**User Story:** As an agent developer, I want the platform to generate optimized container images for my agent, so that deployments are reproducible and portable.

#### Acceptance Criteria

1. WHEN the developer runs `adk deploy build --container`, THE Deployment_CLI SHALL generate a multi-stage Dockerfile optimized for the agent binary.
2. THE generated container image SHALL use a minimal base image (distroless or Alpine-based) to reduce attack surface.
3. THE generated container image SHALL include only the compiled binary, declared assets, and required shared libraries.
4. THE Deployment_CLI SHALL tag the container image with the Deployment version and a `latest` tag for the Environment.
5. WHEN the Deployment_Manifest declares system dependencies (e.g., `cmake` for `openai-webrtc`), THE generated Dockerfile SHALL include the required build dependencies in the builder stage only.

### Requirement 19: Plugin and Skill Deployment

**User Story:** As an agent developer, I want my agent's plugins and skills to be deployed alongside the agent, so that the full agent capability set is available in production.

#### Acceptance Criteria

1. THE Agent_Bundle SHALL include skill definition files from the `.skills/` directory when present.
2. THE Agent_Bundle SHALL include plugin configuration files declared in the Deployment_Manifest.
3. WHEN the agent declares MCP tool server dependencies, THE Deployment_Manifest SHALL support declaring MCP server endpoints as Service_Bindings.
4. THE Control_Plane SHALL validate that declared skill files exist in the Agent_Bundle before accepting the Deployment.

### Requirement 20: Graph Workflow and HITL Support

**User Story:** As an agent developer, I want to deploy graph-based workflow agents with human-in-the-loop checkpoints, so that complex multi-step agents work correctly in production.

#### Acceptance Criteria

1. WHEN the Deployment_Manifest declares graph workflow capabilities, THE Control_Plane SHALL configure the Agent_Instance with persistent checkpoint storage.
2. THE Control_Plane SHALL expose a webhook endpoint for HITL approval callbacks at `/api/hitl/{deployment_id}/{checkpoint_id}`.
3. WHEN a graph workflow agent reaches a HITL checkpoint, THE Agent_Instance SHALL emit a webhook notification to a configurable callback URL.
4. THE Deployment_Manifest SHALL support declaring checkpoint storage backend (PostgreSQL or Redis) as a Service_Binding.
