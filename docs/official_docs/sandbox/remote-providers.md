# Remote Sandbox Providers

`adk-sandbox` runs agent commands in microVMs and containers that a provider hosts, behind the same
`SandboxClient` interface as the local and Docker clients. The harness shell, `SandboxTool`
and the `adk-code` executors take any client, so a remote provider is a configuration choice,
not a different code path.

| Provider | Feature | Isolation | API |
|----------|---------|-----------|-----|
| [E2B](https://e2b.dev) | `remote-e2b` | Firecracker microVM | REST control plane ([OpenAPI](https://github.com/e2b-dev/E2B/blob/main/spec/openapi.yml)) plus the in-VM `envd` daemon: files over HTTP ([envd.yaml](https://github.com/e2b-dev/E2B/blob/main/spec/envd/envd.yaml)), processes over the [Connect protocol](https://connectrpc.com/docs/protocol/) ([process.proto](https://github.com/e2b-dev/E2B/blob/main/spec/envd/process/process.proto)) |
| [Daytona](https://www.daytona.io) | `remote-daytona` | Container or Linux VM, per sandbox class | REST control plane ([OpenAPI](https://www.daytona.io/docs/openapi.json)) plus the toolbox API ([OpenAPI](https://www.daytona.io/docs/toolbox-openapi.json)) at `{toolboxProxyUrl}/{sandboxId}` |
| [Modal](https://modal.com/docs/guide/sandboxes) | `remote-modal` | gVisor container | gRPC control plane at `api.modal.com` plus each sandbox's task command router, over `tonic` ([`modal_proto`](https://github.com/modal-labs/modal-client/tree/main/modal_proto)) |
| `LocalUnixClient` | `workspace` | None: a process in a host directory | Local |
| `DockerClient` | `workspace-docker` | Container | Local Docker daemon |

## The interface

A provider implements `SandboxClient` for the environment lifecycle and `SandboxSession` for
work inside one environment. Every operation the remote providers need already exists on
these traits or has a default, so a client written before 3.0 keeps compiling.

| Operation | Method | Default |
|-----------|--------|---------|
| Create | `SandboxClient::provision(&Manifest)` | — |
| Attach (resumes a paused environment) | `SandboxClient::start(&SessionHandle)`, with the handle of an environment created elsewhere | — |
| Run a command with a timeout, environment and working directory | `SandboxSession::exec(CommandRequest)` | Exports the variables in front of the command and ends the call at the timeout |
| Read and write files | `SandboxSession::read_file`, `write_file` | — |
| Upload and download | `SandboxSession::upload(local, path)`, `download(path, local)` | Through `read_file` and `write_file` |
| Snapshot, and create from a snapshot | `SandboxClient::snapshot`, `resume` | — |
| Pause | `SandboxClient::pause` | `SandboxError::Unsupported` |
| Destroy | `SandboxClient::stop` | — |
| Describe | `SandboxClient::provider()`, `capabilities()` | `"custom"`, nothing claimed |

`ClientCapabilities` reports `snapshot`, `pause`, `remote` (environments run off this host),
`network_blocked` (the client's configuration keeps them off the network) and `max_lifetime`
(the provider destroys an environment on its own after this long). The harness reads `remote`
and `network_blocked` to decide how to govern commands.

## Quick start

```toml
[dependencies]
adk-sandbox = { version = "3.0.0", features = ["remote-e2b"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust,no_run
use std::time::Duration;

use adk_sandbox::workspace::{CommandRequest, Manifest, ManifestEntry, SandboxClient};
use adk_sandbox::{E2bClient, E2bConfig, SandboxError};

#[tokio::main]
async fn main() -> Result<(), SandboxError> {
    // Reads E2B_API_KEY.
    let config = E2bConfig::from_env()?
        .with_timeout(Duration::from_secs(600))
        .with_internet_access(false);
    let client = E2bClient::new(config)?;
    let handle = client
        .provision(&Manifest::new(vec![ManifestEntry::File {
            path: "main.py".into(),
            content: b"print(sum(range(10)))".to_vec(),
        }]))
        .await?;
    let session = client.start(&handle).await?;
    let output = session
        .exec(CommandRequest::new("python3 main.py").timeout(Duration::from_secs(30)))
        .await?;
    println!("{} (exit {})", output.stdout.trim(), output.exit_code);
    client.stop(&handle).await
}
```

Daytona is the same with `DaytonaClient::new(DaytonaConfig::from_env()?)` and feature
`remote-daytona`; Modal with `ModalClient::new(ModalConfig::from_env()?)` and feature
`remote-modal`.

## Configuration

### E2B

| `E2bConfig` | Default | Meaning |
|-------------|---------|---------|
| `new(api_key)`, `from_env()` | — | The key; `from_env` reads `E2B_API_KEY`, and `E2B_DOMAIN` when set |
| `with_template` | `base` | The template, or a snapshot id, new sandboxes start from |
| `with_timeout` | 15 minutes | The sandbox's lifetime; E2B kills it after this whatever this process does |
| `with_command_timeout` | 120 seconds | The timeout of a command that sets none |
| `with_internet_access` | `true` | `allow_internet_access`; `false` reports `network_blocked` |
| `with_env`, `with_metadata` | empty | Variables every sandbox starts with; metadata the E2B list API filters on |
| `with_user`, `with_workdir` | `user`, `/home/user` | The user commands run as, and the root relative paths resolve against |
| `with_api_url`, `with_domain`, `with_envd_url` | `https://api.e2b.app`, `e2b.app`, per sandbox | Self-hosted deployments and test servers |

| Operation | E2B request |
|-----------|-------------|
| `provision` | `POST /v2/sandboxes`, then the manifest through `envd` |
| `start` on another process's sandbox | `POST /v2/sandboxes/{id}/connect`, which resumes a paused one |
| `exec` | `envd` `POST /process.Process/Start` (Connect server stream); a timeout sends `SIGKILL` through `process.Process/SendSignal` |
| `read_file`, `write_file` | `envd` `GET /files`, `POST /files` (`application/octet-stream`) |
| `pause`, `snapshot`, `stop` | `POST /sandboxes/{id}/pause`, `POST /sandboxes/{id}/snapshots`, `DELETE /sandboxes/{id}` |
| `resume` | `POST /v2/sandboxes` with the snapshot id as the template |

### Daytona

| `DaytonaConfig` | Default | Meaning |
|-----------------|---------|---------|
| `new(api_key)`, `from_env()` | — | The key; `from_env` reads `DAYTONA_API_KEY`, and `DAYTONA_API_URL`, `DAYTONA_ORGANIZATION_ID` and `DAYTONA_TARGET` when set |
| `with_snapshot`, `with_target` | Daytona's default snapshot and region | The image and region of new sandboxes |
| `with_ttl` | 60 minutes | `ttlMinutes`: Daytona destroys the sandbox after this, whatever its state |
| `with_auto_stop` | 15 minutes | `autoStopInterval`; sandboxes are created with `autoDeleteInterval: 0`, so a stopped sandbox is deleted |
| `with_network_blocked` | `false` | `networkBlockAll`; `true` reports `network_blocked` |
| `with_command_timeout`, `with_start_timeout` | 120 seconds each | A command's default timeout; how long `provision` waits for `started` |
| `with_env`, `with_label`, `with_organization` | empty, empty, none | Variables, labels, and `X-Daytona-Organization-ID` |
| `with_workdir`, `with_api_url`, `with_toolbox_url` | `/home/daytona`, `https://app.daytona.io/api`, per sandbox | The workspace root; self-hosted deployments and test servers |

| Operation | Daytona request |
|-----------|-----------------|
| `provision` | `POST /sandbox`, polling `GET /sandbox/{id}` until `started`; a sandbox that fails to start is deleted |
| `start` on another process's sandbox | `GET /sandbox/{id}`, and `POST /sandbox/{id}/start` when it is not running |
| `exec` | toolbox `POST /process/execute` with `cwd`, `envs` and `timeout`; Daytona returns stdout and stderr combined, which arrive as `stdout` |
| `read_file`, `write_file` | toolbox `GET /files/download`, `POST /files/upload-v2` (multipart `file`) |
| `pause`, `snapshot`, `stop` | `POST /sandbox/{id}/pause` (sandbox classes that support it), `POST /sandbox/{id}/snapshot`, `DELETE /sandbox/{id}` |
| `resume` | `POST /sandbox` with the snapshot name |

### Modal

Modal's API is gRPC only; its SDKs speak it to `api.modal.com`. `ModalClient` speaks the same
protocol through `tonic`, from the subset of Modal's published protobuf definitions it needs,
vendored in `adk-sandbox/proto/modal/` (from
[`modal-labs/modal-client`](https://github.com/modal-labs/modal-client), Apache-2.0) with the
generated Rust code committed, so a build needs no `protoc`. Modal states that direct use of the
gRPC API carries no compatibility guarantee; the client follows the requests its current Go and
JavaScript SDKs send.

| `ModalConfig` | Default | Meaning |
|---------------|---------|---------|
| `new(token_id, token_secret)`, `from_env()` | — | A Modal token (`ak-…`, `as-…`); `from_env` reads `MODAL_TOKEN_ID` and `MODAL_TOKEN_SECRET`, and `MODAL_ENVIRONMENT` and `MODAL_SERVER_URL` when set |
| `with_app_name`, `with_environment` | `adk-sandbox`, the token's default | The app sandboxes belong to, created on first use, and its environment |
| `with_image`, `with_image_command` | `python:3.13-slim` plus `git` and `patch` | The registry image Modal builds once and caches; `with_image` replaces the default and its additions, `with_image_command` appends a Dockerfile command |
| `with_image_id` | none | An image Modal already built, such as a snapshot; nothing is built |
| `with_timeout` | 15 minutes | `timeout_secs`: Modal terminates the sandbox after this whatever this process does (at most 24 hours) |
| `with_idle_timeout` | none | `idle_timeout_secs`: Modal terminates an idle sandbox after this |
| `with_network_blocked` | `false` | `NetworkAccess::BLOCKED`; `true` reports `network_blocked` |
| `with_command_timeout`, `with_start_timeout` | 120 seconds, 300 seconds | A command's default timeout; how long `provision` waits for the image build and for scheduling |
| `with_cpu`, `with_memory_mib` | Modal's defaults | Resources each sandbox requests |
| `with_env`, `with_tag`, `with_name_prefix` | empty, empty, unnamed | Variables every command starts with, tags the sandbox list filters on, and names `{prefix}-{12 hex digits}` |
| `with_workdir`, `with_snapshot_ttl` | `/root`, 30 days | The workspace root; how long Modal keeps a filesystem snapshot |
| `with_server_url` | `https://api.modal.com` | A test server |

| Operation | Modal RPC |
|-----------|-----------|
| `provision` | `AppGetOrCreate` and `ImageGetOrCreate`, with `ImageJoinStreaming` while the image builds (once per client); `SandboxCreateV2`, whose answer names the task and its command router; then the manifest through the router |
| `start` on another process's sandbox | `SandboxGetTaskIdV2` and `SandboxGetCommandRouterAccess`; the V1 RPCs for a V1 sandbox id |
| `exec` | router `TaskExecStart` with `timeout_secs`, after which Modal kills the command, and the variables as an inline secret source; `TaskExecStdioRead` for stdout and stderr, `TaskExecWait` for the exit |
| `read_file`, `write_file` | `cat` through the router, the content over stdin and stdout |
| `snapshot`, `resume` | router `TaskSnapshotFilesystem`, an image; `resume` creates a sandbox from it |
| `stop` | `SandboxTerminateV2` |
| `pause` | Not supported: `SandboxError::Unsupported` |
| `ModalClient::running_sandboxes()` | `SandboxListV2` and `SandboxList`: the ids running in the app, for cleanup |

| Credential | Goes to |
|------------|---------|
| Token id and secret, and the auth token `AuthTokenGet` issues for them | The control plane only, as `x-modal-token-id`, `x-modal-token-secret` and `x-modal-auth-token` |
| The sandbox's router JWT | That sandbox's command router only, as `authorization: Bearer`; a router URL that is not `https` is refused |

The client identifies itself as Modal's SDKs do, with `x-modal-client-type` `CLIENT_TYPE_LIBMODAL`
and `x-modal-libmodal-version` `adk-sandbox/<version>`. Modal reports a command it killed at its
timeout as exit code 137 or a signal; `exec` returns that as `timed_out` with exit code `-1`.

## Bounds and cleanup

| Bound | Behaviour |
|-------|-----------|
| Lifetime | Every environment is created with a provider-side lifetime, so it ends even when the process that created it dies |
| `stop` | Destroys the environment; snapshots taken from it remain |
| Dropped client | Destroys every environment the client created and did not stop; environments it only attached to are left alone |
| Cancelled call | `SessionBackend` and the harness shell destroy the environment of a call dropped mid-command, since nothing else would end the command |
| Requests | Each HTTP or gRPC call has a deadline: a command's timeout plus 30 seconds (`COMMAND_GRACE`); 60 seconds (`REQUEST_TIMEOUT`) for the rest, up to five times that for an E2B snapshot and twice that for a Modal snapshot |
| Output | stdout and stderr are capped at 1 MiB each with a truncation notice; a file is capped at 100 MiB (`MAX_FILE_BYTES`) |
| Credentials | Keys go only to the configured origin, redirects are not followed, and keys never appear in `Debug` output or errors; Modal's router gets only its sandbox's JWT |

Provider errors are `SandboxError::Remote { provider, operation, status, message }`, which
converts to an `AdkError` with code `code.sandbox_remote`, the provider and the upstream
status, and a category from the status: 401 `Unauthorized`, 403 `Forbidden`, 404 `NotFound`,
408 and 504 `Timeout`, 409 `Conflict`, 429 `RateLimited`, other 4xx `InvalidInput`, 5xx and
transport failures `Unavailable`. Modal answers with gRPC status codes, which take their HTTP
equivalents (the `google.rpc.Code` mapping: `UNAUTHENTICATED` 401, `PERMISSION_DENIED` 403,
`NOT_FOUND` 404, `RESOURCE_EXHAUSTED` 429, `DEADLINE_EXCEEDED` 504, `UNAVAILABLE` 503), with the
code's name at the start of `message`.

## The harness shell

With `adk-harness` feature `remote-shell`, `ShellIsolation::Remote` runs `bash` in an
environment the client provides, one per (app, user, session), through the same governed path
as the container shell:

```rust,no_run
use std::sync::Arc;
use std::time::Duration;

use adk_harness::{HarnessAgent, HarnessMode, RemoteShellConfig, ShellIsolation};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_sandbox::workspace::{Manifest, ManifestEntry};
use adk_sandbox::{E2bClient, E2bConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(AnthropicClient::new(AnthropicConfig::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-sonnet-5-5",
    ))?);
    let e2b = Arc::new(E2bClient::new(E2bConfig::from_env()?.with_internet_access(false))?);
    let shell = RemoteShellConfig::new(e2b)
        .with_manifest(Manifest::new(vec![ManifestEntry::GitRepo {
            url: "https://github.com/zavora-ai/adk-rust".into(),
            branch: None,
            path: "repo".into(),
        }]))
        .with_idle_ttl(Duration::from_secs(600));
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .shell(ShellIsolation::Remote(shell))
        .mode(HarnessMode::Auto)
        .build()?;
    println!("{}", harness.info().isolation);
    Ok(())
}
```

> **Note:** a `GitRepo` entry clones inside the sandbox, which needs the network; with
> `with_internet_access(false)` seed the environment with `File` entries instead.

| Concern | Behaviour |
|---------|-----------|
| Status | `IsolationStatus::Remote { provider, off_host }`; `off_host` is the client's `capabilities().remote` |
| Approvals | An off-host environment is governed as the container shell is; a client on this host, such as `LocalUnixClient`, as the host shell |
| `auto_allow_sandboxed` | Refused at build unless the client is off-host and reports `network_blocked` |
| Files | The environment has its own filesystem: the host workspace is not mounted, and `read_file` does not see what `bash` writes |
| Kill switch | A freeze ends the command where it stands and destroys the environment within the 250 ms governance poll, and ends a provisioning still in progress |
| Run budget | The call's provisioning and command are capped at the wall time left in the run's `RunBudget` |
| Timeout, cancellation | Either destroys the environment; the next command provisions a new one |
| Audit | `harness.bash.begin` names the enforcer `remote`, the `provider` and the `environment` handle; `harness.bash.end` records `timedOut` and `killedByFreeze` |
| Rules | The declaration's `profileDigest` covers the provider, `network_blocked` and the manifest, so a standing rule never carries over to another configuration |
| Not available | Unsandboxed retry (off by default), hooks with `isolation = "inherit"`, a goal's `until` check, and shell credentials, as for the container shell |

## Agents and CodeAct

`SessionBackend` is a `SandboxBackend` over any `SandboxClient`: the first request provisions
an environment, every request runs in it, and the environment is destroyed by `close`, when
the backend drops, or when a request is cancelled. `SandboxTool` over it gives an `LlmAgent` or
a `CodeActAgent` code execution in that environment through the governed tool path:

```rust,no_run
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_sandbox::workspace::SessionBackend;
use adk_sandbox::{DaytonaClient, DaytonaConfig, SandboxTool};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(AnthropicClient::new(AnthropicConfig::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-haiku-5-5",
    ))?);
    let daytona = Arc::new(DaytonaClient::new(DaytonaConfig::from_env()?.with_network_blocked(true))?);
    let backend = Arc::new(SessionBackend::new(daytona));
    let agent = LlmAgentBuilder::new("analyst")
        .model(model)
        .instruction("Run code with sandbox_exec to answer.")
        .tool(Arc::new(SandboxTool::new(backend)))
        .build()?;
    println!("{}", agent.name());
    Ok(())
}
```

| `SandboxTool` inside a run | Behaviour |
|----------------------------|-----------|
| A frozen `GovernanceControl` | The call is refused before it starts |
| A freeze while code runs | The call ends with the freeze error; a `SessionBackend` destroys its environment |
| A `RunBudget` wall-time limit | The call's timeout is cut to the time left; a spent budget refuses the call |
| Languages | Python (`python3`), JavaScript (`node`) and shell commands by default; `with_languages` adds Rust (`rustc`) for an image that has it |

`RustExecutor` and `CodeTool` in `adk-code` take an `Arc<dyn SandboxBackend>`, so they run
remotely the same way.

## Testing

The contract tests replay each provider's documented request and response shapes against a
local server; each fixture cites the document it follows:

```bash
cargo nextest run -p adk-sandbox --features remote-e2b,remote-daytona,remote-modal \
  -E 'binary(remote_e2b_contract_tests) | binary(remote_daytona_contract_tests) | binary(remote_modal_contract_tests)'
```

Modal's contract tests run against a local gRPC server generated from the same vendored
protobuf definitions as the client but compiled apart from it (`adk-sandbox/tests/modal_mock/`),
so every call crosses the real wire encoding.

The live tests are `#[ignore]`d and need the provider's key in the environment or the
workspace `.env`:

| Provider | Variable | Command |
|----------|----------|---------|
| E2B | `E2B_API_KEY` | `cargo nextest run -p adk-sandbox --features remote-e2b --run-ignored only -E 'test(live_e2b_round_trip)'` |
| Daytona | `DAYTONA_API_KEY` | `cargo nextest run -p adk-sandbox --features remote-daytona --run-ignored only -E 'test(live_daytona_round_trip)'` |
| Modal | `MODAL_TOKEN_ID`, `MODAL_TOKEN_SECRET` | `cargo nextest run -p adk-sandbox --features remote-modal --run-ignored only -E 'test(live_modal_round_trip)'` |

### Regenerating the Modal protocol code

The vendored `.proto` files keep every upstream name and field number. After changing them,
regenerate the committed code (needs `protoc`):

```bash
cargo run --manifest-path adk-sandbox/proto/modal/codegen/Cargo.toml
```

It writes the client code to `adk-sandbox/src/workspace/remote/modal/proto/` and the contract
tests' mock server to `adk-sandbox/tests/modal_mock/`.

## Writing a provider

Implement `SandboxClient` and `SandboxSession` for the provider, override `exec` when the
provider runs commands with its own environment and deadline, report `provider()` and
`capabilities()` truthfully, create environments with a provider-side lifetime, and destroy
the ones the client created when it drops. `LocalUnixClient` is the reference for path
handling, the E2B and Daytona clients for the remote concerns above over HTTP, and the Modal
client for a gRPC API.
