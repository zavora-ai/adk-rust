# Vertex-Only Deployments

Guarantee that Gemini traffic reaches only Vertex AI (`{location}-aiplatform.googleapis.com`) and never the Gemini API Studio endpoint (`generativelanguage.googleapis.com`). Regulated workloads — HIPAA, data-residency, VPC Service Controls — require this: the Studio endpoint is not covered by a Google Cloud BAA, while Vertex AI is.

## Endpoint Decision

`GeminiModel::from_env` and `provider_from_env` consult two environment flags before any API key. A flag is truthy when its value is `1` or a case-insensitive `true`.

| Flag | Status | Notes |
|------|--------|-------|
| `GOOGLE_GENAI_USE_ENTERPRISE` | Current | Takes precedence when both are set |
| `GOOGLE_GENAI_USE_VERTEXAI` | Deprecated | Still honored for adk-python parity; no deprecation warning is emitted |

| Vertex flag | `gemini-vertex` feature | Result |
|-------------|-------------------------|--------|
| Truthy | Compiled | Vertex AI via Application Default Credentials (`GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`) |
| Truthy, project or location missing | Compiled | Error naming the missing variable — no Studio fallback |
| Truthy | Not compiled | `GeminiModel::from_env` errors; `provider_from_env` emits a `tracing` warning and falls through to API-key detection, which may select Studio |
| Not truthy | — | Gemini API (Studio) via `GOOGLE_API_KEY` or `GEMINI_API_KEY` |

> **Important:** the flags take precedence over every API key, including `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` in `provider_from_env`. A stray API key cannot divert a Vertex-pinned deployment to Studio.

## Setup

### 1. Enable the `gemini-vertex` feature

Compile the Vertex backend in — without it, the flag cannot be honored:

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["minimal", "gemini-vertex"] }
```

Or use the platform preset, which includes it:

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["minimal", "gemini-agent-platform"] }
```

### 2. Set the environment flags

```bash
export GOOGLE_GENAI_USE_ENTERPRISE=true
export GOOGLE_CLOUD_PROJECT=my-project
export GOOGLE_CLOUD_LOCATION=us-central1
```

Unset `GOOGLE_API_KEY` and `GEMINI_API_KEY` in the deployment environment for defense in depth — with the flag truthy they are never consulted, but removing them eliminates the residual path entirely.

### 3. Configure Application Default Credentials

The Vertex path authenticates with ADC, not an API key:

```bash
# Local development
gcloud auth application-default login

# Service accounts (CI, containers)
export GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json
```

On GKE and Cloud Run, Workload Identity provides ADC without a key file.

### 4. Construct the model from the environment

```rust
use adk_model::gemini::GeminiModel;

// The Vertex path builds Application Default Credentials, which requires a
// current Tokio runtime.
#[tokio::main]
async fn main() -> adk_core::Result<()> {
    // Vertex AI when a flag is truthy; errors instead of falling back to
    // Studio when the Vertex configuration is incomplete.
    let model = GeminiModel::from_env("gemini-3.7-flash")?;
    Ok(())
}
```

Or via provider auto-detection on the umbrella crate:

```rust
use adk_rust::provider_from_env;

#[tokio::main]
async fn main() -> adk_rust::Result<()> {
    let model = provider_from_env()?;
    Ok(())
}
```

## Verifying No Studio Traffic

**Build-time** — the flag errors instead of silently degrading:

- `GeminiModel::from_env` with a truthy flag and no `gemini-vertex` feature returns an error naming the feature.
- A truthy flag with `GOOGLE_CLOUD_PROJECT` or `GOOGLE_CLOUD_LOCATION` missing returns an error naming the missing variable.

**Runtime** — watch for the guard-rail warning. `provider_from_env` emits it whenever a Vertex flag is truthy but the `gemini-vertex` feature is not compiled and detection falls through to API keys:

```text
WARN vertex backend requested via environment but the gemini-vertex feature is not compiled; api-key detection may select the gemini studio endpoint (generativelanguage.googleapis.com)
```

A Vertex-only deployment must treat this warning as a deployment error: it means the binary was built without the `gemini-vertex` feature. Alert on it in log aggregation.

**Network** — for VPC Service Controls perimeters, block egress to `generativelanguage.googleapis.com`. With the flag truthy and the feature compiled, no code path constructs a Studio client, so the block never fires for ADK traffic.

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Error naming `GOOGLE_CLOUD_PROJECT` / `GOOGLE_CLOUD_LOCATION` | Flag truthy, Vertex target incomplete | Set both variables |
| Error naming the `gemini-vertex` feature | Flag truthy, feature not compiled | Add `gemini-vertex` (or `gemini-agent-platform`) to the `adk-rust` features |
| The guard-rail warning appears | `provider_from_env` fell back to API keys | Rebuild with `gemini-vertex`; remove Studio API keys from the environment |
| Authentication errors at request time | ADC not configured | Run `gcloud auth application-default login` or set `GOOGLE_APPLICATION_CREDENTIALS` |
