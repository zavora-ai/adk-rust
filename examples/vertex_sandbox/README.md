# Vertex AI Agent Engine Sandbox Example

Managed code execution against the Agent Engine `sandboxEnvironments`
surface (v1beta1): create a sandbox under a reasoning engine, run Python
against an input file, print the captured stdout, and delete the sandbox.

## Prerequisites

1. A Google Cloud project with the Vertex AI API enabled
2. A provisioned Agent Engine (reasoning engine)
3. Application Default Credentials:

   ```bash
   gcloud auth application-default login
   ```

## Configuration

Copy `.env.example` to `.env` and fill in:

| Variable | Description |
|----------|-------------|
| `GOOGLE_CLOUD_PROJECT` | GCP project ID |
| `GOOGLE_CLOUD_LOCATION` | Region of the engine (e.g. `us-central1`) |
| `GOOGLE_CLOUD_AGENT_ENGINE_ID` | Numeric reasoning-engine ID |

## Run

```bash
cargo run --manifest-path examples/vertex_sandbox/Cargo.toml
```

## What it demonstrates

- `VertexSandboxClient::create_sandbox` — waits the create LRO, re-fetches
  the sandbox
- `VertexSandboxClient::execute_code` — the code-execution chunk
  conventions (JSON code chunk, `file_name`-attributed input files,
  `msg_out`/`msg_err` console output), synchronous `:execute`
- `VertexSandboxClient::delete_sandbox` — waits the delete LRO

For per-session managed sandboxes inside an agent, see
`SandboxCodeExecutor` and `VertexSandboxTool` in
[`adk-code`](../../adk-code/README.md).
