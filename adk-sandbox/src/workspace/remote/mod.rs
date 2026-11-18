//! Remote sandbox providers behind [`SandboxClient`](super::SandboxClient).
//!
//! Each provider runs workspaces in its own cloud and talks to it over its documented API, so
//! the harness shell, [`SessionBackend`](super::SessionBackend) and everything else written
//! against `SandboxClient` run commands there unchanged.
//!
//! | Provider | Feature | Isolation | API |
//! |----------|---------|-----------|-----|
//! | `E2bClient` | `remote-e2b` | Firecracker microVM | REST control plane plus the in-VM `envd` daemon (Connect protocol over HTTP) |
//! | `DaytonaClient` | `remote-daytona` | Container or Linux VM, per sandbox class | REST control plane plus the toolbox API |
//! | `ModalClient` | `remote-modal` | gVisor container | gRPC control plane plus each sandbox's task command router, over `tonic` |
//!
//! # Bounds
//!
//! | Bound | How |
//! |-------|-----|
//! | Lifetime | Every environment is created with a provider-side lifetime (`E2bConfig::with_timeout`, `DaytonaConfig::with_ttl`), so the provider destroys it even when this process dies |
//! | Release | `stop` destroys the environment; dropping the client destroys every environment it created and did not stop |
//! | Requests | Each HTTP or gRPC call has a deadline: the command's timeout plus a grace period for a command, [`REQUEST_TIMEOUT`] otherwise |
//! | Output | stdout and stderr are capped at 1 MiB each, with a truncation notice; a file read is capped at [`MAX_FILE_BYTES`] |
//! | Credentials | API keys are sent only to the configured API origin and never appear in `Debug` output or errors |

use std::fmt;
use std::future::Future;
use std::time::Duration;

#[cfg(any(feature = "remote-e2b", feature = "remote-daytona"))]
use futures::StreamExt;

use super::manifest::{Manifest, ManifestEntry};
use super::path_safety::{patch_target_paths, validate_relative_path};
use super::session::{SandboxSession, shell_quote};
use super::types::{CommandRequest, DirEntry, EntryType};
use crate::error::SandboxError;

#[cfg(feature = "remote-daytona")]
mod daytona;
#[cfg(feature = "remote-e2b")]
mod e2b;
#[cfg(feature = "remote-modal")]
mod modal;

#[cfg(feature = "remote-daytona")]
pub use daytona::{DaytonaClient, DaytonaConfig, DaytonaSession};
#[cfg(feature = "remote-e2b")]
pub use e2b::{E2bClient, E2bConfig, E2bSession};
#[cfg(feature = "remote-modal")]
pub use modal::{ModalClient, ModalConfig, ModalSession};

/// Deadline of a provider request that runs no command: create, delete, file transfer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a command request may take beyond the command's own timeout: connection setup,
/// queueing and the response's way back.
pub const COMMAND_GRACE: Duration = Duration::from_secs(30);

/// Largest file `read_file` or `download` accepts from a provider (100 MiB).
pub const MAX_FILE_BYTES: usize = 100 * 1024 * 1024;

/// Bytes kept from each of a command's stdout and stderr (1 MiB), as `LocalUnixSession` keeps.
pub(crate) const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Bytes of a provider's error body kept in a [`SandboxError::Remote`] message.
pub(crate) const ERROR_EXCERPT_BYTES: usize = 512;

/// An API key or token, kept out of `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(any(feature = "remote-e2b", feature = "remote-daytona"))]
/// The HTTP client every provider uses: rustls, a connect timeout, no redirects to other
/// origins (a redirect could carry the API key elsewhere), and per-request deadlines set by
/// each call.
pub(crate) fn http_client(provider: &str) -> Result<reqwest::Client, SandboxError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("adk-sandbox/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| SandboxError::Remote {
            provider: provider.to_string(),
            operation: "build HTTP client".to_string(),
            status: None,
            message: error.to_string(),
        })
}

#[cfg(any(feature = "remote-e2b", feature = "remote-daytona"))]
/// Reads a required configuration value from the environment.
pub(crate) fn env_var(key: &str, provider: &str) -> Result<String, SandboxError> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            SandboxError::BackendUnavailable(format!(
                "{key} is not set; set it to your {provider} API key or build the config with \
                 its constructor"
            ))
        })
}

#[cfg(any(feature = "remote-e2b", feature = "remote-daytona"))]
/// Sends `request` and returns the response when its status is a success.
///
/// # Errors
///
/// A transport failure is `SandboxError::Remote` without a status; a non-success status is
/// `SandboxError::Remote` with the status and an excerpt of the body.
pub(crate) async fn send(
    request: reqwest::RequestBuilder,
    provider: &str,
    operation: &str,
) -> Result<reqwest::Response, SandboxError> {
    let response = request.send().await.map_err(|error| SandboxError::Remote {
        provider: provider.to_string(),
        operation: operation.to_string(),
        status: None,
        message: if error.is_timeout() {
            "the request timed out".to_string()
        } else {
            // `without_url` keeps query strings, which may name files, out of the message.
            error.without_url().to_string()
        },
    })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = read_bounded(response, ERROR_EXCERPT_BYTES, provider, operation, true)
        .await
        .unwrap_or_default();
    Err(SandboxError::Remote {
        provider: provider.to_string(),
        operation: operation.to_string(),
        status: Some(status.as_u16()),
        message: String::from_utf8_lossy(&body).trim().to_string(),
    })
}

#[cfg(any(feature = "remote-e2b", feature = "remote-daytona"))]
/// Reads at most `max` bytes of `response`. With `truncate`, a longer body is cut;
/// otherwise it is an error.
pub(crate) async fn read_bounded(
    response: reqwest::Response,
    max: usize,
    provider: &str,
    operation: &str,
    truncate: bool,
) -> Result<Vec<u8>, SandboxError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| SandboxError::Remote {
            provider: provider.to_string(),
            operation: operation.to_string(),
            status: None,
            message: error.without_url().to_string(),
        })?;
        if body.len() + chunk.len() > max {
            if truncate {
                body.extend_from_slice(&chunk[..max - body.len()]);
                return Ok(body);
            }
            return Err(SandboxError::Remote {
                provider: provider.to_string(),
                operation: operation.to_string(),
                status: None,
                message: format!("the response exceeds the {max}-byte limit"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(any(feature = "remote-e2b", feature = "remote-daytona"))]
/// A response body decoded as JSON.
pub(crate) async fn json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    provider: &str,
    operation: &str,
) -> Result<T, SandboxError> {
    let body = read_bounded(response, MAX_FILE_BYTES, provider, operation, false).await?;
    serde_json::from_slice(&body).map_err(|error| SandboxError::Remote {
        provider: provider.to_string(),
        operation: operation.to_string(),
        status: None,
        message: format!("unexpected response body: {error}"),
    })
}

/// `path`, relative to the workspace root `root`, as the absolute path a provider API takes.
///
/// # Errors
///
/// Returns `SandboxError::PathTraversal` for an absolute path or one that escapes `root`.
pub(crate) fn absolute(root: &str, path: &str) -> Result<String, SandboxError> {
    validate_relative_path(path)?;
    let relative = path.trim_start_matches("./");
    if relative.is_empty() || relative == "." {
        return Ok(root.to_string());
    }
    Ok(format!("{}/{relative}", root.trim_end_matches('/')))
}

/// Keeps at most [`MAX_OUTPUT_BYTES`] of `bytes`, noting a cut as `LocalUnixSession` does.
pub(crate) fn capped_text(mut bytes: Vec<u8>, discarded: bool) -> String {
    let cut = discarded || bytes.len() > MAX_OUTPUT_BYTES;
    bytes.truncate(MAX_OUTPUT_BYTES);
    crate::child_io::note_truncation(crate::child_io::truncate_utf8(bytes, MAX_OUTPUT_BYTES), cut)
}

/// Lists a directory through a command, for providers whose file API has no listing.
pub(crate) async fn list_dir_via_exec(
    session: &dyn SandboxSession,
    path: &str,
) -> Result<Vec<DirEntry>, SandboxError> {
    validate_relative_path(path)?;
    // `-p` marks directories with a trailing slash; `-A` drops `.` and `..`.
    let output =
        session.exec(CommandRequest::new(format!("ls -1Ap -- {}", shell_quote(path)))).await?;
    if output.exit_code != 0 {
        return Err(SandboxError::ExecutionFailed(format!(
            "directory not found: {path}: {}",
            output.stderr.trim()
        )));
    }
    Ok(output
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| match line.strip_suffix('/') {
            Some(name) => DirEntry::new(name, EntryType::Directory),
            None => DirEntry::new(line, EntryType::File),
        })
        .collect())
}

/// Applies a unified diff through `patch -p0`, or `git apply -p0` where `patch` is missing,
/// after checking every path it names stays inside the workspace.
pub(crate) async fn apply_patch_via_exec(
    session: &dyn SandboxSession,
    patch: &str,
) -> Result<(), SandboxError> {
    for name in patch_target_paths(patch) {
        validate_relative_path(name)?;
    }
    let file = format!(".adk-exec/patch-{}.diff", uuid::Uuid::new_v4().simple());
    session.write_file(&file, patch.as_bytes()).await?;
    let quoted = shell_quote(&file);
    let output = session
        .exec(CommandRequest::new(format!(
            "if command -v patch >/dev/null 2>&1; then patch -p0 --no-backup-if-mismatch -i \
             {quoted}; else git apply -p0 {quoted}; fi; adk_status=$?; rm -f {quoted}; exit \
             $adk_status"
        )))
        .await?;
    if output.exit_code != 0 {
        return Err(SandboxError::ExecutionFailed(format!(
            "patch failed: {}{}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }
    Ok(())
}

/// Creates the manifest's entries in a fresh environment.
pub(crate) async fn populate(
    session: &dyn SandboxSession,
    manifest: &Manifest,
) -> Result<(), SandboxError> {
    for entry in &manifest.entries {
        let (path, result) = match entry {
            ManifestEntry::File { path, content } => {
                validate_relative_path(path)?;
                (path, session.write_file(path, content).await)
            }
            ManifestEntry::Directory { path } => {
                validate_relative_path(path)?;
                let output = session
                    .exec(CommandRequest::new(format!("mkdir -p -- {}", shell_quote(path))))
                    .await;
                (path, check_exit(output, "mkdir"))
            }
            ManifestEntry::GitRepo { url, branch, path } => {
                validate_relative_path(path)?;
                let branch = branch
                    .as_deref()
                    .map(|branch| format!("--branch {} ", shell_quote(branch)))
                    .unwrap_or_default();
                let output = session
                    .exec(CommandRequest::new(format!(
                        "git clone --depth 1 {branch}-- {} {}",
                        shell_quote(url),
                        shell_quote(path)
                    )))
                    .await;
                (path, check_exit(output, "git clone"))
            }
        };
        result.map_err(|error| SandboxError::ProvisionFailed {
            resource: path.clone(),
            reason: error.to_string(),
            suggestion: "Check the manifest entry; a git repository needs network access and \
                         git in the provider's image."
                .to_string(),
        })?;
    }
    Ok(())
}

fn check_exit(
    output: Result<super::types::ExecOutput, SandboxError>,
    what: &str,
) -> Result<(), SandboxError> {
    let output = output?;
    if output.exit_code == 0 {
        Ok(())
    } else {
        Err(SandboxError::ExecutionFailed(format!(
            "{what} exited with {}: {}",
            output.exit_code,
            output.stderr.trim()
        )))
    }
}

/// Destroys each of `ids` on the current Tokio runtime without waiting, from a client's
/// `Drop`. Outside a runtime the provider's lifetime bound ends them, which the warning says.
pub(crate) fn destroy_in_background<F, Fut>(provider: &'static str, ids: Vec<String>, destroy: F)
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<(), SandboxError>> + Send + 'static,
{
    if ids.is_empty() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            sandbox.provider = provider,
            sandbox.count = ids.len(),
            "no async runtime to destroy sandbox environments; they live until the provider's \
             lifetime bound ends them"
        );
        return;
    };
    for id in ids {
        let destroying = destroy(id.clone());
        runtime.spawn(async move {
            match tokio::time::timeout(super::release::RELEASE_TIMEOUT, destroying).await {
                Ok(Ok(())) => tracing::info!(
                    sandbox.provider = provider,
                    sandbox.id = %id,
                    "destroyed a sandbox environment its client dropped"
                ),
                Ok(Err(error)) => tracing::warn!(
                    sandbox.provider = provider,
                    sandbox.id = %id,
                    error = %error,
                    "could not destroy a sandbox environment its client dropped"
                ),
                Err(_) => tracing::warn!(
                    sandbox.provider = provider,
                    sandbox.id = %id,
                    "destroying a sandbox environment its client dropped timed out"
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_joins_inside_the_root_and_refuses_escapes() {
        assert_eq!(absolute("/home/user", "src/main.rs").unwrap(), "/home/user/src/main.rs");
        assert_eq!(absolute("/home/user/", "./a.txt").unwrap(), "/home/user/a.txt");
        assert_eq!(absolute("/home/user", ".").unwrap(), "/home/user");
        assert!(matches!(
            absolute("/home/user", "../etc"),
            Err(SandboxError::PathTraversal { .. })
        ));
        assert!(matches!(
            absolute("/home/user", "/etc/passwd"),
            Err(SandboxError::PathTraversal { .. })
        ));
    }

    #[test]
    fn secrets_never_print() {
        assert_eq!(format!("{:?}", Secret::new("e2b_live_key")), "<redacted>");
    }

    #[test]
    fn capped_text_cuts_and_notes() {
        let text = capped_text(vec![b'a'; MAX_OUTPUT_BYTES + 10], false);
        assert!(text.ends_with("(truncated: output exceeded the configured limit)"));
        assert_eq!(capped_text(b"ok".to_vec(), false), "ok");
    }
}
