//! Sandbox session trait for workspace operations.
//!
//! The [`SandboxSession`] trait provides a live session handle for
//! workspace operations including command execution, file I/O,
//! directory listing, and patch application.
//!
//! All file paths are relative to the workspace root directory.
//! Attempts to access paths outside the workspace return `PathTraversal` errors.

use std::path::Path;
use std::time::Instant;

use async_trait::async_trait;

use super::types::{CommandRequest, DirEntry, ExecOutput};
use crate::error::SandboxError;

/// A live sandbox session providing workspace operations.
///
/// All file paths are relative to the workspace root directory.
/// Attempts to access paths outside the workspace return `PathTraversal` errors.
///
/// # Requirements
///
/// - Implementations must be `Send + Sync` to support use across async
///   task boundaries.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::SandboxSession;
///
/// async fn use_session(session: &dyn SandboxSession) {
///     let output = session.exec_command("ls -la", None).await.unwrap();
///     println!("stdout: {}", output.stdout);
///
///     session.write_file("hello.txt", b"Hello, world!").await.unwrap();
///     let content = session.read_file("hello.txt").await.unwrap();
///     assert_eq!(content, b"Hello, world!");
/// }
/// ```
#[async_trait]
pub trait SandboxSession: Send + Sync {
    /// Executes a shell command in the workspace.
    ///
    /// Returns stdout, stderr, and exit code. Does not propagate
    /// non-zero exit codes as errors — the caller inspects the
    /// `exit_code` field to determine success.
    ///
    /// # Arguments
    ///
    /// * `command` - The shell command to execute.
    /// * `working_dir` - Optional working directory relative to the
    ///   workspace root. If `None`, uses the workspace root.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::PathTraversal` if `working_dir` escapes
    /// the workspace root.
    async fn exec_command(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError>;

    /// Reads a file from the workspace.
    ///
    /// # Arguments
    ///
    /// * `path` - File path relative to the workspace root.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::PathTraversal` if the path escapes the
    /// workspace root.
    /// Returns `SandboxError::ExecutionFailed` if the file does not exist.
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError>;

    /// Writes content to a file in the workspace, creating parent
    /// directories as needed.
    ///
    /// # Arguments
    ///
    /// * `path` - File path relative to the workspace root.
    /// * `content` - The bytes to write to the file.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::PathTraversal` if the path escapes the
    /// workspace root.
    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), SandboxError>;

    /// Lists entries in a directory within the workspace.
    ///
    /// # Arguments
    ///
    /// * `path` - Directory path relative to the workspace root.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::PathTraversal` if the path escapes the
    /// workspace root.
    /// Returns `SandboxError::ExecutionFailed` if the directory does not exist.
    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError>;

    /// Applies a unified diff patch to the workspace.
    ///
    /// # Arguments
    ///
    /// * `patch` - A unified diff string to apply.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::ExecutionFailed` if the patch cannot be applied.
    async fn apply_patch(&self, patch: &str) -> Result<(), SandboxError>;

    /// Executes `request`: its command in its working directory, with its environment, ended
    /// after its timeout.
    ///
    /// The default exports `request.env` in front of the command and runs it through
    /// [`exec_command`](Self::exec_command); on timeout it drops that call, which ends the
    /// command for a session whose processes die with the call, and returns an output with
    /// `timed_out` set. A session whose provider runs commands with their own environment
    /// and deadline overrides it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_sandbox::workspace::{CommandRequest, SandboxSession};
    /// use std::time::Duration;
    ///
    /// async fn run(session: &dyn SandboxSession) {
    ///     let request = CommandRequest::new("echo $GREETING")
    ///         .env("GREETING", "hello")
    ///         .timeout(Duration::from_secs(5));
    ///     let output = session.exec(request).await.unwrap();
    ///     assert_eq!(output.stdout, "hello\n");
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::InvalidRequest` when an environment variable name is not a
    /// shell identifier, and whatever [`exec_command`](Self::exec_command) returns.
    async fn exec(&self, request: CommandRequest) -> Result<ExecOutput, SandboxError> {
        let mut command = String::new();
        for (key, value) in &request.env {
            validate_env_name(key)?;
            command.push_str(&format!("export {key}={}; ", shell_quote(value)));
        }
        command.push_str(&request.command);
        let run = self.exec_command(&command, request.working_dir.as_deref());
        let Some(timeout) = request.timeout else {
            return run.await;
        };
        let started = Instant::now();
        match tokio::time::timeout(timeout, run).await {
            Ok(output) => output,
            Err(_) => Ok(ExecOutput::new("", "", -1, started.elapsed(), true)),
        }
    }

    /// Copies the host file `local` to `path` in the workspace.
    ///
    /// The default reads `local` and calls [`write_file`](Self::write_file).
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::ExecutionFailed` when `local` cannot be read, and whatever
    /// `write_file` returns.
    async fn upload(&self, local: &Path, path: &str) -> Result<(), SandboxError> {
        let content = tokio::fs::read(local).await.map_err(|error| {
            SandboxError::ExecutionFailed(format!("cannot read {}: {error}", local.display()))
        })?;
        self.write_file(path, &content).await
    }

    /// Copies `path` in the workspace to the host file `local`, replacing it.
    ///
    /// The default calls [`read_file`](Self::read_file) and writes the bytes to `local`.
    ///
    /// # Errors
    ///
    /// Returns whatever `read_file` returns, and `SandboxError::ExecutionFailed` when `local`
    /// cannot be written.
    async fn download(&self, path: &str, local: &Path) -> Result<(), SandboxError> {
        let content = self.read_file(path).await?;
        tokio::fs::write(local, content).await.map_err(|error| {
            SandboxError::ExecutionFailed(format!("cannot write {}: {error}", local.display()))
        })
    }
}

/// Refuses an environment variable name that is not a shell identifier, so a name cannot
/// smuggle shell text into a command.
pub(crate) fn validate_env_name(name: &str) -> Result<(), SandboxError> {
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(SandboxError::InvalidRequest(format!(
            "environment variable name {name:?} is not a shell identifier; use letters, digits \
             and underscores, not starting with a digit"
        )))
    }
}

/// Quotes `value` for a POSIX shell.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
