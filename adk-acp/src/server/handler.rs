//! ACP session lifecycle and ADK-Rust Runner bridge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Agent, Content, RunConfig, SessionId as AdkSessionId, Toolset, UserId};
use adk_runner::Runner;
use adk_session::{CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService};
use adk_tool::McpToolset;
use agent_client_protocol::RequestCancellation;
use agent_client_protocol::schema::v1::{
    ContentBlock, ListSessionsRequest, ListSessionsResponse, McpServer, NewSessionRequest,
    PromptRequest, SessionId, SessionInfo, SessionNotification, StopReason,
};
use agent_client_protocol::{Client, ConnectionTo};
use futures::StreamExt;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::config::AcpServerConfig;
use super::error::AcpServerError;
use super::streamer::ResponseStreamer;

const CWD_STATE_KEY: &str = "acp:cwd";
const ADDITIONAL_DIRS_STATE_KEY: &str = "acp:additional_directories";
const MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

struct SessionState {
    execution_token: Option<CancellationToken>,
    mcp: McpSessionResources,
}

#[derive(Default)]
struct McpSessionResources {
    toolsets: Vec<Arc<dyn Toolset>>,
    cancellations: Vec<Option<rmcp::service::RunningServiceCancellationToken>>,
}

impl Drop for McpSessionResources {
    fn drop(&mut self) {
        for cancellation in &mut self.cancellations {
            if let Some(cancellation) = cancellation.take() {
                cancellation.cancel();
            }
        }
    }
}

/// Maps official ACP v1 session requests to the ADK-Rust Runner and session
/// service. One ACP session maps one-to-one to one ADK-Rust session.
pub struct AcpSessionHandler {
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    app_name: String,
    user_id: String,
    max_sessions: usize,
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
    shutdown_token: CancellationToken,
}

impl AcpSessionHandler {
    /// Create a handler from validated server configuration.
    pub fn new(
        config: &AcpServerConfig,
        shutdown_token: CancellationToken,
    ) -> Result<Self, AcpServerError> {
        Ok(Self {
            agent: config.agent.clone(),
            session_service: config.session_service.clone(),
            app_name: config.agent_name.clone(),
            user_id: config.user_id.clone(),
            max_sessions: config.max_sessions,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            shutdown_token,
        })
    }

    /// Create an ACP session and its persistent ADK-Rust session.
    pub async fn create_session(
        &self,
        request: NewSessionRequest,
        request_cancellation: RequestCancellation,
    ) -> Result<SessionId, AcpServerError> {
        self.ensure_running()?;
        validate_absolute(&request.cwd, "cwd")?;
        for directory in &request.additional_directories {
            validate_absolute(directory, "additionalDirectories")?;
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut state = HashMap::new();
        state.insert(
            CWD_STATE_KEY.to_string(),
            serde_json::Value::String(request.cwd.display().to_string()),
        );
        state.insert(
            ADDITIONAL_DIRS_STATE_KEY.to_string(),
            serde_json::to_value(&request.additional_directories)
                .map_err(|e| AcpServerError::Internal(e.to_string()))?,
        );

        {
            let mut sessions = self.sessions.lock().await;
            if sessions.len() >= self.max_sessions {
                return Err(AcpServerError::MaxSessionsReached(self.max_sessions));
            }
            sessions.insert(
                session_id.clone(),
                SessionState { execution_token: None, mcp: McpSessionResources::default() },
            );
        }

        let mcp = match start_mcp_servers(&request.mcp_servers, &request.cwd, &request_cancellation)
            .await
        {
            Ok(resources) => resources,
            Err(error) => {
                self.sessions.lock().await.remove(&session_id);
                return Err(error);
            }
        };

        if let Err(error) = self
            .session_service
            .create(CreateRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: Some(session_id.clone()),
                state,
            })
            .await
        {
            self.sessions.lock().await.remove(&session_id);
            return Err(AcpServerError::Internal(format!("failed to create session: {error}")));
        }

        if let Some(session) = self.sessions.lock().await.get_mut(&session_id) {
            session.mcp = mcp;
        }
        info!(session_id, "created ACP session");
        Ok(SessionId::new(session_id))
    }

    /// Resume a persisted session and make it active on this ACP connection.
    pub async fn resume_session(
        &self,
        session_id: &SessionId,
        cwd: PathBuf,
        additional_directories: Vec<PathBuf>,
        mcp_servers: Vec<McpServer>,
        request_cancellation: RequestCancellation,
    ) -> Result<(), AcpServerError> {
        self.ensure_running()?;
        validate_absolute(&cwd, "cwd")?;
        for directory in &additional_directories {
            validate_absolute(directory, "additionalDirectories")?;
        }

        let id = session_id.to_string();
        let persisted = self
            .session_service
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .map_err(|_| AcpServerError::SessionNotFound(id.clone()))?;

        let stored_cwd = persisted
            .state()
            .get(CWD_STATE_KEY)
            .and_then(|value| value.as_str().map(PathBuf::from))
            .unwrap_or_else(|| cwd.clone());
        if stored_cwd != cwd {
            return Err(AcpServerError::MalformedMessage(
                "resumed session cwd does not match its original cwd".into(),
            ));
        }
        {
            let mut sessions = self.sessions.lock().await;
            if sessions.contains_key(&id) {
                return Err(AcpServerError::Execution(
                    "the session is already active on this ACP connection".into(),
                ));
            }
            if sessions.len() >= self.max_sessions {
                return Err(AcpServerError::MaxSessionsReached(self.max_sessions));
            }
            sessions.insert(
                id.clone(),
                SessionState { execution_token: None, mcp: McpSessionResources::default() },
            );
        }
        let mcp = match start_mcp_servers(&mcp_servers, &cwd, &request_cancellation).await {
            Ok(resources) => resources,
            Err(error) => {
                self.sessions.lock().await.remove(&id);
                return Err(error);
            }
        };
        if let Some(session) = self.sessions.lock().await.get_mut(&id) {
            session.mcp = mcp;
        }
        Ok(())
    }

    /// List persisted ACP sessions for the configured ADK application and user.
    pub async fn list_sessions(
        &self,
        request: ListSessionsRequest,
    ) -> Result<ListSessionsResponse, AcpServerError> {
        let offset = request
            .cursor
            .as_deref()
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| AcpServerError::MalformedMessage("invalid session cursor".into()))?
            .unwrap_or(0);
        let page_size = 50;
        let sessions = self
            .session_service
            .list(ListRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                limit: Some(page_size + 1),
                offset: Some(offset),
            })
            .await
            .map_err(|e| AcpServerError::Internal(format!("failed to list sessions: {e}")))?;

        let has_more = sessions.len() > page_size;
        let mut result = Vec::new();
        for session in sessions.into_iter().take(page_size) {
            let cwd = session
                .state()
                .get(CWD_STATE_KEY)
                .and_then(|value| value.as_str().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/"));
            if request.cwd.as_ref().is_some_and(|filter| filter != &cwd) {
                continue;
            }
            let additional_directories = session
                .state()
                .get(ADDITIONAL_DIRS_STATE_KEY)
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            result.push(
                SessionInfo::new(session.id().to_string(), cwd)
                    .additional_directories(additional_directories)
                    .updated_at(session.last_update_time().to_rfc3339()),
            );
        }
        Ok(ListSessionsResponse::new(result)
            .next_cursor(has_more.then(|| (offset + page_size).to_string())))
    }

    /// Delete persisted session history and release any active execution.
    pub async fn delete_session(&self, session_id: &SessionId) -> Result<(), AcpServerError> {
        if let Some(state) = self.sessions.lock().await.remove(&session_id.to_string())
            && let Some(token) = state.execution_token
        {
            token.cancel();
        }
        self.session_service
            .delete(DeleteRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: session_id.to_string(),
            })
            .await
            .map_err(|_| AcpServerError::SessionNotFound(session_id.to_string()))
    }

    /// Execute one prompt and stream official `session/update` notifications
    /// through the SDK connection before returning the turn stop reason.
    pub async fn handle_prompt(
        &self,
        request: PromptRequest,
        connection: ConnectionTo<Client>,
    ) -> Result<StopReason, AcpServerError> {
        self.ensure_running()?;
        let id = request.session_id.to_string();
        let (cancellation_token, runtime_toolsets) = {
            let mut sessions = self.sessions.lock().await;
            let state =
                sessions.get_mut(&id).ok_or_else(|| AcpServerError::SessionNotFound(id.clone()))?;
            if state.execution_token.is_some() {
                return Err(AcpServerError::Execution(
                    "a prompt is already running in this session".into(),
                ));
            }
            let token = CancellationToken::new();
            state.execution_token = Some(token.clone());
            (token, state.mcp.toolsets.clone())
        };

        let result = self
            .execute_prompt(&request, connection, cancellation_token.clone(), runtime_toolsets)
            .await;

        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(&id) {
            state.execution_token = None;
        }
        drop(sessions);

        if cancellation_token.is_cancelled() {
            return Ok(StopReason::Cancelled);
        }
        result
    }

    async fn execute_prompt(
        &self,
        request: &PromptRequest,
        connection: ConnectionTo<Client>,
        cancellation_token: CancellationToken,
        runtime_toolsets: Vec<Arc<dyn Toolset>>,
    ) -> Result<StopReason, AcpServerError> {
        let content = prompt_content(&request.prompt)?;
        let runner = Runner::builder()
            .app_name(&self.app_name)
            .agent(self.agent.clone())
            .session_service(self.session_service.clone())
            .cancellation_token(cancellation_token.clone())
            .run_config(RunConfig::builder().runtime_toolsets(runtime_toolsets).build())
            .build()
            .map_err(|e| AcpServerError::Execution(format!("failed to create runner: {e}")))?;

        let mut stream = runner
            .run(
                UserId::new(self.user_id.clone())
                    .map_err(|e| AcpServerError::Execution(e.to_string()))?,
                AdkSessionId::new(request.session_id.to_string())
                    .map_err(|e| AcpServerError::Execution(e.to_string()))?,
                content,
            )
            .await
            .map_err(|e| AcpServerError::Execution(format!("runner.run failed: {e}")))?;

        loop {
            let result = tokio::select! {
                _ = cancellation_token.cancelled() => return Ok(StopReason::Cancelled),
                result = stream.next() => result,
            };
            let Some(result) = result else {
                break;
            };
            match result {
                Ok(event) => {
                    for update in ResponseStreamer::map_event(&event) {
                        connection
                            .send_notification(SessionNotification::new(
                                request.session_id.clone(),
                                update,
                            ))
                            .map_err(|e| AcpServerError::Transport(e.to_string()))?;
                    }
                }
                Err(error) => {
                    if cancellation_token.is_cancelled() {
                        return Ok(StopReason::Cancelled);
                    }
                    warn!(%error, session_id = %request.session_id, "ACP Runner event failed");
                    return Err(AcpServerError::Execution(error.to_string()));
                }
            }
        }
        Ok(StopReason::EndTurn)
    }

    /// Cancel the prompt currently running in a session.
    pub async fn cancel_session(&self, session_id: &SessionId) {
        if let Some(token) = self
            .sessions
            .lock()
            .await
            .get(&session_id.to_string())
            .and_then(|state| state.execution_token.clone())
        {
            token.cancel();
        }
    }

    /// Close an active session without deleting its persisted history.
    pub async fn close_session(&self, session_id: &SessionId) -> Result<(), AcpServerError> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions
            .remove(&session_id.to_string())
            .ok_or_else(|| AcpServerError::SessionNotFound(session_id.to_string()))?;
        if let Some(token) = state.execution_token {
            token.cancel();
        }
        Ok(())
    }

    /// Number of active sessions on the connection.
    pub async fn active_session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Cancel active work and release all connection-scoped session state.
    pub async fn drain_sessions(&self, _timeout: std::time::Duration) {
        let mut sessions = self.sessions.lock().await;
        for state in sessions.values() {
            if let Some(token) = &state.execution_token {
                token.cancel();
            }
        }
        sessions.clear();
    }

    fn ensure_running(&self) -> Result<(), AcpServerError> {
        if self.shutdown_token.is_cancelled() { Err(AcpServerError::ShuttingDown) } else { Ok(()) }
    }
}

async fn start_mcp_servers(
    servers: &[McpServer],
    cwd: &Path,
    cancellation: &RequestCancellation,
) -> Result<McpSessionResources, AcpServerError> {
    validate_mcp_servers(servers)?;
    let mut resources = McpSessionResources {
        toolsets: Vec::with_capacity(servers.len()),
        cancellations: Vec::with_capacity(servers.len()),
    };
    for server in servers {
        let McpServer::Stdio(config) = server else {
            return Err(AcpServerError::MalformedMessage(
                "this ACP agent supports the required MCP stdio transport; HTTP and SSE were not advertised"
                    .into(),
            ));
        };
        if !config.command.is_absolute() {
            return Err(AcpServerError::MalformedMessage(format!(
                "MCP server '{}' command must be an absolute path",
                config.name
            )));
        }

        let mut command = tokio::process::Command::new(&config.command);
        command.args(&config.args).current_dir(cwd);
        for variable in &config.env {
            command.env(&variable.name, &variable.value);
        }
        let transport = TokioChildProcess::new(command).map_err(|error| {
            AcpServerError::Execution(format!(
                "failed to start MCP server '{}': {error}",
                config.name
            ))
        })?;
        let startup = tokio::time::timeout(MCP_STARTUP_TIMEOUT, ().serve(transport));
        let client = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(AcpServerError::Execution(
                    "ACP session creation was cancelled while starting MCP servers".into(),
                ));
            }
            result = startup => result,
        }
        .map_err(|_| {
            AcpServerError::Execution(format!(
                "MCP server '{}' did not initialize within {} seconds",
                config.name,
                MCP_STARTUP_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| {
            AcpServerError::Execution(format!(
                "failed to initialize MCP server '{}': {error}",
                config.name
            ))
        })?;
        let toolset = McpToolset::new(client).with_name(format!("acp:{}", config.name));
        let cancellation = toolset.cancellation_token().await;
        resources.cancellations.push(Some(cancellation));
        resources.toolsets.push(Arc::new(toolset));
    }
    Ok(resources)
}

fn validate_mcp_servers(servers: &[McpServer]) -> Result<(), AcpServerError> {
    const MAX_MCP_SERVERS: usize = 16;
    if servers.len() > MAX_MCP_SERVERS {
        return Err(AcpServerError::MalformedMessage(format!(
            "at most {MAX_MCP_SERVERS} MCP servers may be attached to one ACP session"
        )));
    }
    let mut names = std::collections::HashSet::new();
    for server in servers {
        let McpServer::Stdio(config) = server else {
            return Err(AcpServerError::MalformedMessage(
                "this ACP agent supports the required MCP stdio transport; HTTP and SSE were not advertised"
                    .into(),
            ));
        };
        if config.name.trim().is_empty() {
            return Err(AcpServerError::MalformedMessage(
                "MCP server names cannot be empty".into(),
            ));
        }
        if !names.insert(config.name.as_str()) {
            return Err(AcpServerError::MalformedMessage(format!(
                "duplicate MCP server name: {}",
                config.name
            )));
        }
        let mut environment_names = std::collections::HashSet::new();
        for variable in &config.env {
            if variable.name.trim().is_empty() {
                return Err(AcpServerError::MalformedMessage(format!(
                    "MCP server '{}' has an empty environment variable name",
                    config.name
                )));
            }
            if !environment_names.insert(variable.name.as_str()) {
                return Err(AcpServerError::MalformedMessage(format!(
                    "MCP server '{}' repeats environment variable '{}'",
                    config.name, variable.name
                )));
            }
        }
    }
    Ok(())
}

fn validate_absolute(path: &Path, field: &str) -> Result<(), AcpServerError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(AcpServerError::MalformedMessage(format!("{field} must be an absolute path")))
    }
}

fn prompt_content(blocks: &[ContentBlock]) -> Result<Content, AcpServerError> {
    let mut content = Content::new("user");
    for block in blocks {
        match block {
            ContentBlock::Text(text) => {
                content.parts.push(adk_core::Part::Text { text: text.text.clone() })
            }
            ContentBlock::ResourceLink(link) => {
                let description = link
                    .description
                    .as_deref()
                    .map(|value| format!(" — {value}"))
                    .unwrap_or_default();
                content.parts.push(adk_core::Part::Text {
                    text: format!("Referenced resource: {} ({}){description}", link.name, link.uri),
                });
            }
            _ => {
                return Err(AcpServerError::MalformedMessage(
                    "prompt contains a content type this agent did not advertise".into(),
                ));
            }
        }
    }
    if content.parts.is_empty() {
        return Err(AcpServerError::MalformedMessage("prompt must contain content".into()));
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{EnvVariable, McpServerStdio};

    use super::*;

    #[test]
    fn validates_session_mcp_configuration_before_process_start() {
        let duplicate_names = vec![
            McpServer::Stdio(McpServerStdio::new("tools", "/bin/echo")),
            McpServer::Stdio(McpServerStdio::new("tools", "/bin/echo")),
        ];
        assert!(
            validate_mcp_servers(&duplicate_names)
                .expect_err("duplicate names")
                .to_string()
                .contains("duplicate MCP server name")
        );

        let duplicate_environment = vec![McpServer::Stdio(
            McpServerStdio::new("tools", "/bin/echo")
                .env(vec![EnvVariable::new("TOKEN", "one"), EnvVariable::new("TOKEN", "two")]),
        )];
        assert!(
            validate_mcp_servers(&duplicate_environment)
                .expect_err("duplicate environment")
                .to_string()
                .contains("repeats environment variable")
        );
    }
}
