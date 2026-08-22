//! Vertex AI Agent Engine sandbox client (`sandboxEnvironments`, v1beta1).
//!
//! A managed code-execution sandbox lives under a reasoning engine at
//! `projects/*/locations/*/reasoningEngines/*/sandboxEnvironments/*`. This
//! module provides:
//!
//! - **[`VertexSandboxClient`]** — the REST surface: create (LRO), get,
//!   list, delete (LRO), and the synchronous `:execute` method, plus
//!   [`execute_code`](VertexSandboxClient::execute_code) implementing the
//!   chunk conventions shared with adk-python and the Vertex AI SDK.
//! - **[`SandboxCodeExecutor`]** — adk-python
//!   `AgentEngineSandboxCodeExecutor` parity: per-session lazy sandbox
//!   creation with recreate-on-not-running semantics.
//! - **[`VertexSandboxTool`]** — an [`adk_core::Tool`] exposing sandbox
//!   code execution to LLM agents, keyed by the calling session.
//!
//! Transport, credential caching, LRO polling, and scope validation come
//! from [`adk_gcp`].
//!
//! # Chunk conventions
//!
//! `:execute` exchanges opaque `Chunk` values. The code-execution
//! conventions over them (adk-python/SDK parity):
//!
//! | Chunk | `mimeType` | `data` (base64 of) | `metadata.attributes` |
//! |-------|------------|--------------------|-----------------------|
//! | Code input | `application/json` | `{"code": "<source>"}` | — |
//! | Input file | the file's MIME type | raw bytes | `file_name` = base64(filename) |
//! | Console output | `application/json` | `{"msg_out": ..., "msg_err": ...}` | no `file_name` |
//! | Output file | may be absent | raw bytes | `file_name` = base64(filename) |
//!
//! Files are limited to 100 MB per request and per response. The request
//! side is enforced before sending; the response side is bounded by the
//! client's response-size limit (140 MiB, covering base64 overhead).
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_code::vertex_sandbox::{
//!     CreateSandboxRequest, VertexSandboxClient, VertexSandboxConfig,
//! };
//!
//! # async fn run() -> adk_core::Result<()> {
//! let config = VertexSandboxConfig::new("my-project", "us-central1");
//! let client = VertexSandboxClient::new_with_adc(config)?;
//!
//! let sandbox = client
//!     .create_sandbox("4242", CreateSandboxRequest::new("my-sandbox"))
//!     .await?;
//! let name = sandbox.name.expect("created sandbox has a name");
//!
//! let result = client.execute_code(&name, "print('hello')", &[]).await?;
//! assert_eq!(result.stdout, "hello\n");
//!
//! client.delete_sandbox(&name).await?;
//! # Ok(())
//! # }
//! ```

mod client;
mod executor;
mod tool;
mod types;

pub use client::VertexSandboxClient;
pub use executor::SandboxCodeExecutor;
pub use tool::VertexSandboxTool;
pub use types::{
    Chunk, ChunkMetadata, CodeExecutionEnvironment, CodeLanguage, ComputerUseEnvironment,
    CreateSandboxRequest, InputFile, MachineConfig, OutputFile, SandboxEnvironment,
    SandboxEnvironmentSpec, SandboxExecutionResult, SandboxState, decode_output_chunks,
    encode_code_chunk, encode_file_chunk,
};

use adk_core::{ErrorComponent, Result};
use adk_gcp::{GcpErrorCodes, GcpErrorContext};
use std::time::Duration;

/// Machine-readable error codes stamped on every vertex sandbox error.
const CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "code.vertex_sandbox.invalid_input",
    unauthorized: "code.vertex_sandbox.unauthorized",
    forbidden: "code.vertex_sandbox.forbidden",
    not_found: "code.vertex_sandbox.not_found",
    rate_limited: "code.vertex_sandbox.rate_limited",
    timeout: "code.vertex_sandbox.timeout",
    unavailable: "code.vertex_sandbox.unavailable",
    credentials_unavailable: "code.vertex_sandbox.credentials_unavailable",
    invalid_response: "code.vertex_sandbox.invalid_response",
    invalid_request: "code.vertex_sandbox.invalid_request",
    upstream_error: "code.vertex_sandbox.upstream_error",
    operation_failed: "code.vertex_sandbox.operation_failed",
};

/// The error identity every vertex sandbox failure carries.
pub(crate) fn errors() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Code, CODES, "vertex sandbox")
}

/// Maximum raw file bytes accepted per `:execute` request (100 MB).
///
/// The service enforces the same limit on responses; the client's
/// response-size bound is set above the base64-encoded equivalent.
pub const MAX_REQUEST_FILE_BYTES: usize = 100 * 1024 * 1024;

/// Response-size bound: 100 MB of files survives base64 encoding (4/3
/// overhead ≈ 133 MiB) plus JSON framing within 140 MiB.
const MAX_RESPONSE_BYTES: usize = 140 * 1024 * 1024;

/// Whole-request timeout: long-running code plus 100 MB uploads need more
/// headroom than the adk-gcp default.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Sandbox provisioning routinely outlives the default 120 s LRO deadline.
const CREATE_POLL_TIMEOUT: Duration = Duration::from_secs(300);

/// Display name [`SandboxCodeExecutor`] uses for lazily created sandboxes
/// (adk-python parity).
pub const DEFAULT_SANDBOX_DISPLAY_NAME: &str = "default_sandbox";

/// TTL [`SandboxCodeExecutor`] sends for lazily created sandboxes: one year,
/// matching adk-python.
///
/// > **Note:** the service documents no hard TTL maximum, but sandboxes may
/// > lose state after roughly 14 days of disuse. Every `:execute` call
/// > resets the TTL server-side.
pub const DEFAULT_SANDBOX_TTL: &str = "31536000s";

/// Environment variable holding the GCP project (set inside deployed engines).
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
/// Environment variable holding the GCP location.
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";

/// Configuration for [`VertexSandboxClient`].
///
/// Mirrors `VertexAiMemoryConfig`: project, location, optional endpoint
/// override, and a [`from_env`](Self::from_env) constructor reading the
/// platform's container environment.
///
/// # Example
///
/// ```rust
/// use adk_code::vertex_sandbox::VertexSandboxConfig;
///
/// let config = VertexSandboxConfig::new("my-project", "us-central1");
/// ```
#[derive(Debug, Clone)]
pub struct VertexSandboxConfig {
    pub(crate) project_id: String,
    pub(crate) location: String,
    pub(crate) endpoint: Option<String>,
}

impl VertexSandboxConfig {
    /// Creates a config for the given project and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self { project_id: project_id.into(), location: location.into(), endpoint: None }
    }

    /// Builds a config from the environment variables the platform sets
    /// inside deployed containers: `GOOGLE_CLOUD_PROJECT` and
    /// `GOOGLE_CLOUD_LOCATION`. Values are trimmed; blank counts as missing.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error naming every missing or blank variable.
    pub fn from_env() -> Result<Self> {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let project_id = read(ENV_GOOGLE_CLOUD_PROJECT);
        let location = read(ENV_GOOGLE_CLOUD_LOCATION);

        match (project_id, location) {
            (Some(project_id), Some(location)) => Ok(Self::new(project_id, location)),
            (project_id, location) => {
                let missing = [
                    (ENV_GOOGLE_CLOUD_PROJECT, project_id.is_none()),
                    (ENV_GOOGLE_CLOUD_LOCATION, location.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, is_missing)| is_missing.then_some(key))
                .collect::<Vec<_>>()
                .join(", ");
                Err(errors().invalid_input(format!(
                    "missing or blank environment variable(s): {missing}. The Agent Engine platform sets these inside deployed containers; set them explicitly elsewhere, or construct the config with VertexSandboxConfig::new",
                )))
            }
        }
    }

    /// Sets a custom API origin.
    ///
    /// The origin receives Google authorization headers plus executed code
    /// and files. Use only a trusted HTTPS origin, or loopback HTTP for
    /// local tests. Userinfo, paths, queries, and fragments are rejected
    /// before transport.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub(crate) fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}-aiplatform.googleapis.com", self.location))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_defaults_to_regional_origin() {
        let config = VertexSandboxConfig::new("p", "europe-west1");
        assert_eq!(config.endpoint(), "https://europe-west1-aiplatform.googleapis.com");
        let overridden = config.with_endpoint("http://127.0.0.1:8080");
        assert_eq!(overridden.endpoint(), "http://127.0.0.1:8080");
    }
}
