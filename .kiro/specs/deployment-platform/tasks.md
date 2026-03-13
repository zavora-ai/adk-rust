# Deployment Platform Tasks

## Phase 1: Contract and Security Foundation

- [x] Re-scope the platform to a publishable `self-hosted v1` target and update crate descriptions/docs accordingly.
- [x] Expand `adk-deploy` manifest coverage to include auth, guardrails, telemetry, realtime, A2A, graph/HITL, plugins, skills, and richer service bindings.
- [x] Make manifest validation reject unsupported or unsafe capability combinations explicitly.
- [x] Replace self-issued control-plane login with operator-provided bearer credentials and secure local token handling in the CLI.
- [x] Bind control-plane request authentication to explicit workspace/user context instead of implicit global state.
- [x] Remove plaintext credential persistence from the default CLI flow.

## Phase 2: Real Deployment Runtime

- [x] Replace metadata-only deploy admission for bundle-backed CLI/Studio pushes with real bundle upload, checksum verification, and durable artifact storage.
- [ ] Assemble deployed runtimes by composing `adk-server` and `adk-runner` with manifest-driven session, memory, artifact, auth, and guardrail configuration.
- [ ] Implement health checks, rollout state transitions, rollback, and admission-time validation for realtime/A2A features.
- [ ] Provision or bind declared backing services for supported session, memory, artifact, and graph checkpoint backends.

## Phase 3: Observability, Evaluation, and HITL

- [ ] Replace synthetic traces/logs/metrics with real telemetry ingestion based on ADK runtime signals.
- [ ] Expose log streaming, metrics summaries, and trace inspection using real deployment/runtime data.
- [ ] Integrate `adk-eval` for platform evaluations and wire graph/HITL state from runtime events.
- [ ] Ensure the control plane reports source metadata consistently for CLI, Studio, and catalog/template deploys.

## Phase 4: Console and Publish Gates

- [ ] Rebuild the console around the real APIs with secure auth UX, automated tests, and no dependency on simulated control-plane data.
- [ ] Align Console information architecture with the deployment-platform mock and phase the screens by backend readiness.
- [ ] Add crate-level and end-to-end tests for `adk-deploy`, `adk-deploy-server`, and `adk-deploy-console`.
- [x] Pass `fmt`, `clippy -D warnings`, and targeted tests for all deploy crates.
- [x] Update README, docs, examples, and changelog for the publishable scope.
