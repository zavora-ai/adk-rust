//! Host callbacks exposed by an ACP client to an external agent.
//!
//! ACP agents may ask their client (usually an editor or CLI) to read or
//! write files and to manage terminal processes. ADK-Rust keeps these
//! capabilities opt-in.

use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    ClientCapabilities, CreateTerminalRequest, CreateTerminalResponse, FileSystemCapabilities,
    KillTerminalRequest, KillTerminalResponse, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, TerminalOutputRequest, TerminalOutputResponse,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{Agent, ConnectionTo, Dispatch, Error, HandleDispatchFrom, Handled};
use async_trait::async_trait;

/// File operations that an ACP client chooses to expose to an agent.
///
/// Implementations should validate every path against the session workspace
/// and any explicitly approved additional directories.
#[async_trait]
pub trait AcpFileSystem: std::fmt::Debug + Send + Sync {
    /// Whether ACP file reads are supported.
    fn supports_read(&self) -> bool;

    /// Whether ACP file writes are supported.
    fn supports_write(&self) -> bool;

    /// Read a text file or a requested line range.
    async fn read_text_file(
        &self,
        request: ReadTextFileRequest,
    ) -> Result<ReadTextFileResponse, Error>;

    /// Replace a text file with the supplied content.
    async fn write_text_file(
        &self,
        request: WriteTextFileRequest,
    ) -> Result<WriteTextFileResponse, Error>;
}

/// Terminal lifecycle operations that an ACP client exposes to an agent.
///
/// ACP advertises terminal support as one capability, so implementations must
/// provide the complete create/output/wait/kill/release lifecycle.
#[async_trait]
pub trait AcpTerminal: std::fmt::Debug + Send + Sync {
    /// Start a terminal command.
    async fn create_terminal(
        &self,
        request: CreateTerminalRequest,
    ) -> Result<CreateTerminalResponse, Error>;

    /// Read output accumulated by a terminal.
    async fn terminal_output(
        &self,
        request: TerminalOutputRequest,
    ) -> Result<TerminalOutputResponse, Error>;

    /// Release the client's resources for a terminal.
    async fn release_terminal(
        &self,
        request: ReleaseTerminalRequest,
    ) -> Result<ReleaseTerminalResponse, Error>;

    /// Wait for a terminal process to exit.
    async fn wait_for_terminal_exit(
        &self,
        request: WaitForTerminalExitRequest,
    ) -> Result<WaitForTerminalExitResponse, Error>;

    /// Stop a running terminal process.
    async fn kill_terminal(
        &self,
        request: KillTerminalRequest,
    ) -> Result<KillTerminalResponse, Error>;
}

pub(crate) fn capabilities(
    filesystem: Option<&Arc<dyn AcpFileSystem>>,
    terminal: Option<&Arc<dyn AcpTerminal>>,
) -> ClientCapabilities {
    let mut capabilities = ClientCapabilities::new();
    if let Some(filesystem) = filesystem {
        capabilities = capabilities.fs(FileSystemCapabilities::new()
            .read_text_file(filesystem.supports_read())
            .write_text_file(filesystem.supports_write()));
    }
    if terminal.is_some() {
        capabilities = capabilities.terminal(true);
    }
    capabilities
}

pub(crate) struct AcpHostHandler {
    filesystem: Option<Arc<dyn AcpFileSystem>>,
    terminal: Option<Arc<dyn AcpTerminal>>,
}

impl AcpHostHandler {
    pub(crate) fn new(
        filesystem: Option<Arc<dyn AcpFileSystem>>,
        terminal: Option<Arc<dyn AcpTerminal>>,
    ) -> Self {
        Self { filesystem, terminal }
    }
}

impl HandleDispatchFrom<Agent> for AcpHostHandler {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        connection: ConnectionTo<Agent>,
    ) -> Result<Handled<Dispatch>, Error> {
        let read_host = self.filesystem.clone();
        let write_host = self.filesystem.clone();
        let create_host = self.terminal.clone();
        let output_host = self.terminal.clone();
        let release_host = self.terminal.clone();
        let wait_host = self.terminal.clone();
        let kill_host = self.terminal.clone();
        let read_connection = connection.clone();
        let write_connection = connection.clone();
        let create_connection = connection.clone();
        let output_connection = connection.clone();
        let release_connection = connection.clone();
        let wait_connection = connection.clone();

        MatchDispatch::new(message)
            .if_request(async move |request: ReadTextFileRequest, responder| {
                read_connection.spawn(async move {
                    let result = match read_host {
                        Some(host) if host.supports_read() => host.read_text_file(request).await,
                        _ => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: WriteTextFileRequest, responder| {
                write_connection.spawn(async move {
                    let result = match write_host {
                        Some(host) if host.supports_write() => host.write_text_file(request).await,
                        _ => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: CreateTerminalRequest, responder| {
                create_connection.spawn(async move {
                    let result = match create_host {
                        Some(host) => host.create_terminal(request).await,
                        None => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: TerminalOutputRequest, responder| {
                output_connection.spawn(async move {
                    let result = match output_host {
                        Some(host) => host.terminal_output(request).await,
                        None => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: ReleaseTerminalRequest, responder| {
                release_connection.spawn(async move {
                    let result = match release_host {
                        Some(host) => host.release_terminal(request).await,
                        None => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: WaitForTerminalExitRequest, responder| {
                wait_connection.spawn(async move {
                    let cancellation = responder.cancellation();
                    let result = match wait_host {
                        Some(host) => tokio::select! {
                            result = host.wait_for_terminal_exit(request) => result,
                            _ = cancellation.cancelled() => return Ok(()),
                        },
                        None => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: KillTerminalRequest, responder| {
                connection.spawn(async move {
                    let result = match kill_host {
                        Some(host) => host.kill_terminal(request).await,
                        None => Err(Error::method_not_found()),
                    };
                    responder.respond_with_result(result)
                })?;
                Ok(())
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ADK-Rust ACP client host"
    }
}
