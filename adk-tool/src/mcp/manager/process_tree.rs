//! The Job Object that holds a stdio server's process tree on Windows (spec 3.2 §10 W1).

use std::future::Future;

use adk_sandbox::process_tree::JobObject;
use rmcp::RoleClient;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{TokioChildProcess, Transport};

/// A stdio server's transport together with the Job Object that holds its process tree.
///
/// The job lives exactly as long as the transport, so whatever ends the server's process —
/// a stop, a restart, a crash's cleanup, a dropped connection, or the manager's own process
/// exiting — also ends every process the server started.
pub(super) struct JobTransport {
    // Dropped first: the transport ends the server, then closing the job ends what is left.
    transport: TokioChildProcess,
    _job: JobObject,
}

impl JobTransport {
    /// Places the suspended server `transport` started in a new Job Object, then lets it run.
    ///
    /// On failure the transport is dropped, which ends the still-suspended server.
    pub(super) fn contain(transport: TokioChildProcess) -> std::io::Result<Self> {
        let pid = transport
            .id()
            .ok_or_else(|| std::io::Error::other("the server exited before it was started"))?;
        let job = JobObject::contain_suspended(pid)?;
        Ok(Self { transport, _job: job })
    }
}

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
