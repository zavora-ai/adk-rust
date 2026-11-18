- **Deploy targets** (`adk-deploy`, `cargo-adk`): `targets::DeployTarget` deploys one
  `adk-deploy.toml` and a container image to a provider, and `cargo adk deploy --target <name>`
  plans, deploys, reports and removes deployments. The `container` target renders a non-root
  distroless `Dockerfile`, a Docker Compose file and Kubernetes manifests with probes, autoscaling
  and secrets from a Kubernetes `Secret`; `gcp::AgentEngineTarget` deploys to Agent Engine
  (cargo-adk feature `gcp`). Every target enforces the deployment guarantees first: no literal
  secrets, an explicit session store, `[auth]` before public ingress, and probes from `[health]`.
  `targets::verify_endpoint` checks a deployed agent's health and that it refuses unauthenticated
  calls. The `deploy_targets` example runs on the deploy contract.
