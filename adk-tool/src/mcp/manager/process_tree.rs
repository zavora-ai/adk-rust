//! What holds a stdio server's process tree, so that ending the server ends every process it
//! started: a Job Object on Windows (spec 3.2 §10 W1), a session and process group of its own
//! on Unix (spec 3.2 §7.7, K-I5).

#[cfg(any(unix, windows))]
use std::future::Future;

use adk_core::AdkError;
#[cfg(any(unix, windows))]
use rmcp::RoleClient;
#[cfg(any(unix, windows))]
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
#[cfg(not(unix))]
use rmcp::transport::TokioChildProcess;
#[cfg(any(unix, windows))]
use rmcp::transport::Transport;

#[cfg(windows)]
use adk_sandbox::process_tree::JobObject;
#[cfg(unix)]
use adk_sandbox::process_tree::ProcessGroup;

/// The transport of a stdio server whose process tree ends with it.
#[cfg(windows)]
pub(super) type ServerTransport = JobTransport;
/// The transport of a stdio server whose process tree ends with it.
#[cfg(unix)]
pub(super) type ServerTransport = GroupTransport;
/// The transport of a stdio server; only its own process ends with it.
#[cfg(not(any(unix, windows)))]
pub(super) type ServerTransport = TokioChildProcess;

/// The error of server `id`, whose command `program` could not be spawned.
fn not_spawned(id: &str, program: &std::ffi::OsStr, error: std::io::Error) -> AdkError {
    let program = program.to_string_lossy();
    AdkError::tool(format!(
        "failed to spawn server '{id}': command '{program}' not found. Verify it is installed and on PATH: {error}"
    ))
}

/// Logs a tree whose processes had not all exited when the wait ended.
#[cfg(any(unix, windows))]
fn note_termination(result: std::io::Result<bool>, wait: std::time::Duration) {
    match result {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            wait = ?wait,
            "a process of the MCP server had not exited when its transport closed"
        ),
        Err(error) => {
            tracing::warn!(error = %error, "could not wait for the MCP server's processes to exit")
        }
    }
}

/// Starts server `id` from `command` with its standard input and output piped for the MCP
/// connection, held so that its whole process tree ends with the returned transport.
///
/// On Windows the server starts suspended and runs only once it is in a Job Object, so
/// nothing it starts is outside it (spec 3.2 §10 W1).
#[cfg(windows)]
pub(super) fn spawn(
    id: &str,
    mut command: tokio::process::Command,
) -> adk_core::Result<ServerTransport> {
    command.creation_flags(adk_sandbox::process_tree::CREATE_SUSPENDED);
    let program = command.as_std().get_program().to_os_string();
    // TokioChildProcess::new spawns the child; dropping the transport ends it.
    let transport =
        TokioChildProcess::new(command).map_err(|error| not_spawned(id, &program, error))?;
    JobTransport::contain(transport).map_err(|error| {
        AdkError::tool(format!(
            "MCP server '{id}' could not be placed in a Job Object, which stops the processes \
             it starts, so it was not started: {error}"
        ))
    })
}

/// Starts server `id` from `command` with its standard input and output piped for the MCP
/// connection, held so that its whole process tree ends with the returned transport.
///
/// On Unix the server leads a session and process group of its own, which every process it
/// starts joins (spec 3.2 §7.7, K-I5).
#[cfg(unix)]
pub(super) fn spawn(
    id: &str,
    mut command: tokio::process::Command,
) -> adk_core::Result<ServerTransport> {
    use std::process::Stdio;

    // The streams rmcp's own stdio transport sets: JSON-RPC over standard input and output,
    // standard error inherited.
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    ProcessGroup::new_session(&mut command);
    let mut child =
        command.spawn().map_err(|error| not_spawned(id, command.as_std().get_program(), error))?;
    let pipes = (child.stdout.take(), child.stdin.take());
    let group = ProcessGroup::contain(child).map_err(|error| {
        AdkError::tool(format!(
            "MCP server '{id}' could not be held in a process group of its own, which stops \
             the processes it starts, so it was not started: {error}"
        ))
    })?;
    // Dropping the group on failure ends the server.
    let (Some(stdout), Some(stdin)) = pipes else {
        return Err(AdkError::tool(format!(
            "MCP server '{id}' started without piped standard input and output"
        )));
    };
    Ok(GroupTransport {
        transport: rmcp::transport::async_rw::AsyncRwTransport::new_client(stdout, stdin),
        group,
    })
}

/// Starts server `id` from `command` with its standard input and output piped for the MCP
/// connection; only the server's own process ends with the returned transport.
#[cfg(not(any(unix, windows)))]
pub(super) fn spawn(
    id: &str,
    command: tokio::process::Command,
) -> adk_core::Result<ServerTransport> {
    let program = command.as_std().get_program().to_os_string();
    TokioChildProcess::new(command).map_err(|error| not_spawned(id, &program, error))
}

/// A stdio server's transport together with the Job Object that holds its process tree.
///
/// The job lives exactly as long as the transport, so whatever ends the server's process —
/// a stop, a restart, a crash's cleanup, a dropped connection, or the manager's own process
/// exiting — also ends every process the server started.
#[cfg(windows)]
pub(super) struct JobTransport {
    transport: TokioChildProcess,
    job: JobObject,
}

#[cfg(windows)]
impl JobTransport {
    /// Places the suspended server `transport` started in a new Job Object, then lets it run.
    ///
    /// On failure the transport is dropped, which ends the still-suspended server.
    fn contain(transport: TokioChildProcess) -> std::io::Result<Self> {
        let pid = transport
            .id()
            .ok_or_else(|| std::io::Error::other("the server exited before it was started"))?;
        let job = JobObject::contain_suspended(pid)?;
        Ok(Self { transport, job })
    }
}

#[cfg(windows)]
impl Drop for JobTransport {
    fn drop(&mut self) {
        // The job ends the server and everything it started, and the drop returns once they
        // have exited, so no write one had in flight lands afterwards.
        note_termination(
            self.job.terminate_and_wait(JobObject::TERMINATION_WAIT),
            JobObject::TERMINATION_WAIT,
        );
    }
}

#[cfg(windows)]
impl Transport<RoleClient> for JobTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.transport.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.transport.receive()
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.transport.close()
    }
}

/// How long a server whose input has closed has to exit on its own before its group is
/// ended: the time rmcp's own stdio transport gives it.
#[cfg(unix)]
const EXIT_AFTER_INPUT_CLOSES: std::time::Duration = std::time::Duration::from_secs(3);

/// A stdio server's transport over the pipes of a server that leads a session and process
/// group of its own, which the transport holds.
///
/// The group lives exactly as long as the transport, so whatever ends the transport — a
/// stop, a restart, an update or a removal, a crash's cleanup, a dropped connection — also
/// ends every process the server started that is still in the group, and returns once each
/// has exited. Closing the transport first closes the server's input and gives it
/// [`EXIT_AFTER_INPUT_CLOSES`] to exit; the group is then sent `SIGTERM`, and `SIGKILL`
/// once the server has exited or the grace of [`ProcessGroup::terminate_and_wait`] ends.
#[cfg(unix)]
pub(super) struct GroupTransport {
    transport: rmcp::transport::async_rw::AsyncRwTransport<
        RoleClient,
        tokio::process::ChildStdout,
        tokio::process::ChildStdin,
    >,
    group: ProcessGroup,
}

#[cfg(unix)]
impl Drop for GroupTransport {
    fn drop(&mut self) {
        // Ends the server and everything it started that is still running, and returns once
        // they have exited; after `close` the group is gone and this only confirms it.
        note_termination(
            self.group.terminate_and_wait(ProcessGroup::TERMINATION_WAIT),
            ProcessGroup::TERMINATION_WAIT,
        );
    }
}

#[cfg(unix)]
impl Transport<RoleClient> for GroupTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.transport.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.transport.receive()
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let closed = self.transport.close().await;
        if let Err(error) = self.group.wait_for_leader(EXIT_AFTER_INPUT_CLOSES).await {
            tracing::warn!(error = %error, "could not wait for the MCP server to exit");
        }
        note_termination(
            self.group.terminate_and_wait_async(ProcessGroup::TERMINATION_WAIT).await,
            ProcessGroup::TERMINATION_WAIT,
        );
        closed
    }
}
