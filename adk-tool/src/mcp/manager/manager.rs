//! McpServerManager implementation.
//!
//! This module contains the main [`McpServerManager`] struct and its
//! construction/builder methods, as well as lifecycle methods (start, stop,
//! restart) for individual servers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::AdkError;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::super::elicitation::{AutoDeclineElicitationHandler, ElicitationHandler};
use super::super::toolset::McpToolset;
use super::config::McpServerConfig;
use super::entry::{BackoffState, McpServerEntry};
use super::status::ServerStatus;

/// Manages the full lifecycle of multiple local MCP server child processes.
///
/// `McpServerManager` spawns processes, connects them via `TokioChildProcess`
/// transport into [`McpToolset`](super::super::McpToolset) instances, monitors
/// health, auto-restarts on crash with exponential backoff, and aggregates tools
/// from all managed servers behind the [`Toolset`](adk_core::Toolset) trait.
///
/// # Construction
///
/// Use [`McpServerManager::new`] with a map of server configurations, then chain
/// builder methods to configure handlers and intervals:
///
/// ```rust,ignore
/// use adk_tool::mcp::manager::{McpServerConfig, McpServerManager};
/// use std::collections::HashMap;
/// use std::time::Duration;
///
/// let configs = HashMap::from([
///     ("my-server".to_string(), McpServerConfig {
///         command: "npx".to_string(),
///         args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
///         ..Default::default()
///     }),
/// ]);
///
/// let manager = McpServerManager::new(configs)
///     .with_health_check_interval(Duration::from_secs(15))
///     .with_grace_period(Duration::from_secs(3))
///     .with_name("my_manager");
/// ```
#[allow(dead_code)] // Fields used by lifecycle methods in later tasks
pub struct McpServerManager {
    /// Thread-safe map of server ID to per-server state.
    pub(crate) servers: Arc<RwLock<HashMap<String, McpServerEntry>>>,

    /// Optional elicitation handler shared across all managed server connections.
    pub(crate) elicitation_handler: Option<Arc<dyn ElicitationHandler>>,

    /// Optional sampling handler shared across all managed server connections.
    /// Only available when the `mcp-sampling` feature is enabled.
    #[cfg(feature = "mcp-sampling")]
    pub(crate) sampling_handler: Option<Arc<dyn crate::sampling::SamplingHandler>>,

    /// Interval between health check cycles. Default: 30 seconds.
    pub(crate) health_check_interval: Duration,

    /// Grace period to wait for a child process to exit before force-killing. Default: 5 seconds.
    pub(crate) grace_period: Duration,

    /// Cancellation token used to stop the health monitoring background task.
    pub(crate) monitor_cancel: CancellationToken,

    /// Name returned by the `Toolset::name()` implementation. Default: `"mcp_server_manager"`.
    pub(crate) name: String,
}

impl McpServerManager {
    /// Create a new `McpServerManager` from a map of server configurations.
    ///
    /// Each entry is keyed by a unique server ID. Servers with `disabled: true`
    /// are initialized with [`ServerStatus::Disabled`]; all others start as
    /// [`ServerStatus::Stopped`].
    ///
    /// No servers are started automatically — call [`start_server`](Self::start_server)
    /// or [`start_all`](Self::start_all) to begin spawning processes.
    pub fn new(configs: HashMap<String, McpServerConfig>) -> Self {
        let servers: HashMap<String, McpServerEntry> = configs
            .into_iter()
            .map(|(id, config)| {
                let status =
                    if config.disabled { ServerStatus::Disabled } else { ServerStatus::Stopped };
                let backoff = BackoffState::new(&config.restart_policy);
                let entry = McpServerEntry { config, status, toolset: None, child: None, backoff };
                (id, entry)
            })
            .collect();

        Self {
            servers: Arc::new(RwLock::new(servers)),
            elicitation_handler: None,
            #[cfg(feature = "mcp-sampling")]
            sampling_handler: None,
            health_check_interval: Duration::from_secs(30),
            grace_period: Duration::from_secs(5),
            monitor_cancel: CancellationToken::new(),
            name: "mcp_server_manager".to_string(),
        }
    }

    /// Create a new `McpServerManager` by parsing a JSON string in Kiro `mcp.json` format.
    ///
    /// The JSON must contain a top-level `mcpServers` object mapping server IDs
    /// to their configurations. CamelCase JSON field names are automatically
    /// mapped to snake_case Rust fields.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if the JSON is malformed or missing required fields.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let json = r#"{
    ///     "mcpServers": {
    ///         "filesystem": {
    ///             "command": "npx",
    ///             "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    ///         }
    ///     }
    /// }"#;
    /// let manager = McpServerManager::from_json(json)?;
    /// ```
    pub fn from_json(json: &str) -> adk_core::Result<Self> {
        let file: super::config::McpJsonFile = serde_json::from_str(json)
            .map_err(|e| AdkError::tool(format!("failed to parse MCP server config: {e}")))?;
        Ok(Self::new(file.mcp_servers))
    }

    /// Create a new `McpServerManager` by reading and parsing a JSON file from disk.
    ///
    /// The file must contain JSON in Kiro `mcp.json` format (see [`from_json`](Self::from_json)).
    /// File reading is synchronous, which is acceptable for config loading at startup.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if the file cannot be read or the JSON is malformed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let manager = McpServerManager::from_json_file("mcp.json")?;
    /// ```
    pub fn from_json_file(path: impl AsRef<std::path::Path>) -> adk_core::Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| {
            AdkError::tool(format!("failed to read config file '{}': {e}", path.display()))
        })?;
        Self::from_json(&content)
    }

    /// Set the elicitation handler used for all managed server connections.
    ///
    /// The handler is preserved across server restarts via `Arc` sharing.
    pub fn with_elicitation_handler(mut self, handler: Arc<dyn ElicitationHandler>) -> Self {
        self.elicitation_handler = Some(handler);
        self
    }

    /// Set the sampling handler used for all managed server connections.
    ///
    /// The handler is preserved across server restarts via `Arc` sharing.
    /// Only available when the `mcp-sampling` feature is enabled.
    #[cfg(feature = "mcp-sampling")]
    pub fn with_sampling_handler(
        mut self,
        handler: Arc<dyn crate::sampling::SamplingHandler>,
    ) -> Self {
        self.sampling_handler = Some(handler);
        self
    }

    /// Set the interval between health check cycles.
    ///
    /// Default: 30 seconds.
    pub fn with_health_check_interval(mut self, interval: Duration) -> Self {
        self.health_check_interval = interval;
        self
    }

    /// Set the grace period to wait for a child process to exit before force-killing.
    ///
    /// Default: 5 seconds.
    pub fn with_grace_period(mut self, period: Duration) -> Self {
        self.grace_period = period;
        self
    }

    /// Set the name returned by the `Toolset::name()` implementation.
    ///
    /// Default: `"mcp_server_manager"`.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Start a managed MCP server by ID.
    ///
    /// Spawns the configured command as a child process, creates a
    /// `TokioChildProcess` transport, and connects via `McpToolset` with the
    /// configured elicitation (and optionally sampling) handler.
    ///
    /// If the server is already `Running`, this is a no-op and returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if:
    /// - The server ID does not exist
    /// - The child process fails to spawn
    /// - The MCP handshake fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.start_server("my-server").await?;
    /// ```
    pub async fn start_server(&self, id: &str) -> adk_core::Result<()> {
        let mut servers = self.servers.write().await;
        let entry = servers
            .get_mut(id)
            .ok_or_else(|| AdkError::tool(format!("unknown server ID: '{id}'")))?;

        Self::start_server_inner(
            id,
            entry,
            &self.elicitation_handler,
            #[cfg(feature = "mcp-sampling")]
            &self.sampling_handler,
        )
        .await
    }

    /// Internal start logic operating on a mutable entry reference.
    ///
    /// This avoids double-locking when called from `restart_server`.
    async fn start_server_inner(
        id: &str,
        entry: &mut McpServerEntry,
        elicitation_handler: &Option<Arc<dyn ElicitationHandler>>,
        #[cfg(feature = "mcp-sampling")] sampling_handler: &Option<
            Arc<dyn crate::sampling::SamplingHandler>,
        >,
    ) -> adk_core::Result<()> {
        // If already running, nothing to do
        if entry.status == ServerStatus::Running {
            return Ok(());
        }

        let config = &entry.config;

        // Build the command
        let mut cmd = tokio::process::Command::new(&config.command);
        cmd.args(&config.args);
        cmd.envs(&config.env);

        // Create transport — TokioChildProcess::new spawns the child internally
        let transport = rmcp::transport::TokioChildProcess::new(cmd).map_err(|e| {
            entry.status = ServerStatus::FailedToStart;
            AdkError::tool(format!(
                "failed to spawn server '{id}': command '{}' not found. Verify it is installed and on PATH: {e}",
                config.command
            ))
        })?;

        // Connect via McpToolset with the appropriate handler
        let handler: Arc<dyn ElicitationHandler> =
            elicitation_handler.clone().unwrap_or_else(|| Arc::new(AutoDeclineElicitationHandler));

        #[cfg(feature = "mcp-sampling")]
        let toolset_result = if let Some(sampling) = sampling_handler {
            McpToolset::with_sampling_handler(transport, handler, Arc::clone(sampling)).await
        } else {
            McpToolset::with_elicitation_handler(transport, handler).await
        };

        #[cfg(not(feature = "mcp-sampling"))]
        let toolset_result = McpToolset::with_elicitation_handler(transport, handler).await;

        let toolset = toolset_result.map_err(|e| {
            entry.status = ServerStatus::FailedToStart;
            AdkError::tool(format!("MCP handshake failed for server '{id}': {e}"))
        })?;

        // Success — update entry
        entry.status = ServerStatus::Running;
        entry.toolset = Some(toolset);
        entry.child = None; // Child is owned by the transport/toolset

        tracing::info!(
            server.id = id,
            server.command = config.command,
            server.args = ?config.args,
            "started MCP server"
        );

        Ok(())
    }

    /// Stop a managed MCP server by ID.
    ///
    /// Cancels the MCP session via the toolset's cancellation token, drops the
    /// `McpToolset` connection, and sets the status to `Stopped`.
    ///
    /// If the server is not running, this is a no-op and returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if the server ID does not exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.stop_server("my-server").await?;
    /// ```
    pub async fn stop_server(&self, id: &str) -> adk_core::Result<()> {
        let mut servers = self.servers.write().await;
        let entry = servers
            .get_mut(id)
            .ok_or_else(|| AdkError::tool(format!("unknown server ID: '{id}'")))?;

        Self::stop_server_inner(id, entry, "manual").await;
        Ok(())
    }

    /// Internal stop logic operating on a mutable entry reference.
    ///
    /// This avoids double-locking when called from `restart_server`.
    async fn stop_server_inner(id: &str, entry: &mut McpServerEntry, reason: &str) {
        // If not running, nothing to do
        if entry.status != ServerStatus::Running && entry.status != ServerStatus::Restarting {
            return;
        }

        // Cancel the MCP session and drop the toolset
        if let Some(ref toolset) = entry.toolset {
            let cancel_token = toolset.cancellation_token().await;
            cancel_token.cancel();
        }

        // Drop the toolset — this cleans up the transport and child process
        entry.toolset = None;
        entry.child = None;

        // Only set to Stopped if we're not in a Restarting transition
        if entry.status != ServerStatus::Restarting {
            entry.status = ServerStatus::Stopped;
        }

        tracing::info!(server.id = id, stop.reason = reason, "stopped MCP server");
    }

    /// Restart a managed MCP server by ID.
    ///
    /// Sets the status to `Restarting`, stops the server, then starts it again.
    /// The same `ElicitationHandler` and `SamplingHandler` `Arc`s are preserved
    /// across the restart.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if:
    /// - The server ID does not exist
    /// - The start phase fails (status set to `FailedToStart`)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.restart_server("my-server").await?;
    /// ```
    pub async fn restart_server(&self, id: &str) -> adk_core::Result<()> {
        let mut servers = self.servers.write().await;
        let entry = servers
            .get_mut(id)
            .ok_or_else(|| AdkError::tool(format!("unknown server ID: '{id}'")))?;

        // Set status to Restarting
        entry.status = ServerStatus::Restarting;

        // Stop the server (inline to avoid double-locking)
        Self::stop_server_inner(id, entry, "restart").await;

        // Start the server again
        Self::start_server_inner(
            id,
            entry,
            &self.elicitation_handler,
            #[cfg(feature = "mcp-sampling")]
            &self.sampling_handler,
        )
        .await
    }

    /// Return the current [`ServerStatus`] for a given server ID.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if the server ID does not exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let status = manager.server_status("my-server").await?;
    /// assert_eq!(status, ServerStatus::Running);
    /// ```
    pub async fn server_status(&self, id: &str) -> adk_core::Result<ServerStatus> {
        let servers = self.servers.read().await;
        servers
            .get(id)
            .map(|entry| entry.status)
            .ok_or_else(|| AdkError::tool(format!("unknown server ID: '{id}'")))
    }

    /// Return a map of all server IDs to their current [`ServerStatus`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let statuses = manager.all_statuses().await;
    /// for (id, status) in &statuses {
    ///     println!("{id}: {status:?}");
    /// }
    /// ```
    pub async fn all_statuses(&self) -> HashMap<String, ServerStatus> {
        let servers = self.servers.read().await;
        servers.iter().map(|(id, entry)| (id.clone(), entry.status)).collect()
    }

    /// Return the number of servers currently in [`ServerStatus::Running`] status.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let count = manager.running_server_count().await;
    /// println!("{count} servers running");
    /// ```
    pub async fn running_server_count(&self) -> usize {
        let servers = self.servers.read().await;
        servers.values().filter(|entry| entry.status == ServerStatus::Running).count()
    }

    /// Start the background health monitoring task.
    ///
    /// Spawns a `tokio::spawn` task that periodically checks each `Running`
    /// server by calling [`McpToolset::is_closed()`](super::super::McpToolset::is_closed).
    /// If a server's connection is closed, the monitor sets its status to
    /// `Crashed` and, if a [`RestartPolicy`] is configured, attempts auto-restart
    /// with exponential backoff.
    ///
    /// The monitoring loop runs until [`stop_monitoring`](Self::stop_monitoring)
    /// is called, which cancels the background task via the internal
    /// `CancellationToken`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.start_monitoring();
    /// // ... later ...
    /// manager.stop_monitoring();
    /// ```
    pub fn start_monitoring(&self) {
        let servers = Arc::clone(&self.servers);
        let cancel = self.monitor_cancel.clone();
        let interval = self.health_check_interval;
        let elicitation_handler = self.elicitation_handler.clone();
        #[cfg(feature = "mcp-sampling")]
        let sampling_handler = self.sampling_handler.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("health monitor stopped");
                        break;
                    }
                    _ = tokio::time::sleep(interval) => {
                        // Phase 1: Detect crashed servers under a read lock
                        let crashed_ids: Vec<String> = {
                            let servers = servers.read().await;
                            let mut crashed = Vec::new();
                            for (id, entry) in servers.iter() {
                                if entry.status != ServerStatus::Running {
                                    continue;
                                }
                                if let Some(ref toolset) = entry.toolset {
                                    if toolset.is_closed().await {
                                        crashed.push(id.clone());
                                    }
                                } else {
                                    // No toolset but status is Running — treat as crashed
                                    crashed.push(id.clone());
                                }
                            }
                            crashed
                        };

                        if crashed_ids.is_empty() {
                            continue;
                        }

                        // Phase 2: Mark crashed servers and attempt auto-restart
                        for id in crashed_ids {
                            // Mark as Crashed under write lock
                            let restart_info = {
                                let mut servers = servers.write().await;
                                if let Some(entry) = servers.get_mut(&id) {
                                    // Only process if still Running (could have been
                                    // stopped between read and write lock)
                                    if entry.status != ServerStatus::Running {
                                        continue;
                                    }

                                    tracing::warn!(
                                        server.id = id,
                                        failure.reason = "connection closed",
                                        "health check failed"
                                    );

                                    entry.status = ServerStatus::Crashed;
                                    entry.toolset = None;
                                    entry.child = None;

                                    // Check if auto-restart is configured
                                    entry.config.restart_policy.clone()
                                } else {
                                    continue;
                                }
                            };

                            // Attempt auto-restart if policy allows
                            if let Some(ref policy) = restart_info {
                                // Check if max attempts exceeded
                                let exceeded = {
                                    let servers = servers.read().await;
                                    servers.get(&id)
                                        .map(|e| e.backoff.exceeded_max_attempts(policy))
                                        .unwrap_or(true)
                                };

                                if exceeded {
                                    let mut servers = servers.write().await;
                                    if let Some(entry) = servers.get_mut(&id) {
                                        tracing::error!(
                                            server.id = id,
                                            restart.total_attempts = entry.backoff.consecutive_failures,
                                            "max restart attempts exceeded, giving up"
                                        );
                                        entry.status = ServerStatus::FailedToStart;
                                    }
                                    continue;
                                }

                                // Compute backoff delay and increment failure counter
                                let (delay_ms, attempt) = {
                                    let mut servers = servers.write().await;
                                    if let Some(entry) = servers.get_mut(&id) {
                                        let attempt = entry.backoff.consecutive_failures + 1;
                                        let delay = entry.backoff.next_delay(policy);
                                        (delay, attempt)
                                    } else {
                                        continue;
                                    }
                                };

                                tracing::info!(
                                    server.id = id,
                                    restart.attempt = attempt,
                                    restart.delay_ms = delay_ms,
                                    "auto-restarting crashed server after backoff"
                                );

                                // Wait for backoff delay (without holding any lock)
                                tokio::time::sleep(Duration::from_millis(delay_ms)).await;

                                // Check if monitoring was cancelled during the sleep
                                if cancel.is_cancelled() {
                                    break;
                                }

                                // Attempt restart under write lock
                                let restart_result = {
                                    let mut servers = servers.write().await;
                                    if let Some(entry) = servers.get_mut(&id) {
                                        entry.status = ServerStatus::Restarting;
                                        Self::start_server_inner(
                                            &id,
                                            entry,
                                            &elicitation_handler,
                                            #[cfg(feature = "mcp-sampling")]
                                            &sampling_handler,
                                        )
                                        .await
                                    } else {
                                        continue;
                                    }
                                };

                                match restart_result {
                                    Ok(()) => {
                                        // Reset backoff on success
                                        let mut servers = servers.write().await;
                                        if let Some(entry) = servers.get_mut(&id) {
                                            entry.backoff.reset(policy);
                                            tracing::info!(
                                                server.id = id,
                                                "auto-restart succeeded"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            server.id = id,
                                            error = %e,
                                            "auto-restart failed"
                                        );
                                        // Status already set to FailedToStart by start_server_inner
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Stop the background health monitoring task.
    ///
    /// Cancels the monitoring loop spawned by [`start_monitoring`](Self::start_monitoring).
    /// This is a no-op if monitoring was never started or has already been stopped.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.stop_monitoring();
    /// ```
    pub fn stop_monitoring(&self) {
        self.monitor_cancel.cancel();
    }

    /// Register a new server configuration at runtime.
    ///
    /// The new server is initialized with [`ServerStatus::Disabled`] if
    /// `config.disabled` is `true`, or [`ServerStatus::Stopped`] otherwise.
    /// It will not be started automatically — call
    /// [`start_server`](Self::start_server) to begin spawning the process.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if a server with the given ID already exists.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = McpServerConfig {
    ///     command: "npx".to_string(),
    ///     args: vec!["-y".to_string(), "server".to_string()],
    ///     ..Default::default()
    /// };
    /// manager.add_server("new-server".to_string(), config).await?;
    /// ```
    pub async fn add_server(&self, id: String, config: McpServerConfig) -> adk_core::Result<()> {
        let mut servers = self.servers.write().await;
        if servers.contains_key(&id) {
            return Err(AdkError::tool(format!("server ID '{id}' already exists")));
        }
        let status = if config.disabled { ServerStatus::Disabled } else { ServerStatus::Stopped };
        let backoff = BackoffState::new(&config.restart_policy);
        let entry = McpServerEntry { config, status, toolset: None, child: None, backoff };
        servers.insert(id, entry);
        Ok(())
    }

    /// Remove a server configuration at runtime.
    ///
    /// If the server is currently running, it is stopped first using the
    /// graceful stop sequence before being removed from the manager.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` if the server ID does not exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.remove_server("old-server").await?;
    /// ```
    pub async fn remove_server(&self, id: &str) -> adk_core::Result<()> {
        let mut servers = self.servers.write().await;
        let entry = servers
            .get_mut(id)
            .ok_or_else(|| AdkError::tool(format!("unknown server ID: '{id}'")))?;

        // If the server is running, stop it first
        Self::stop_server_inner(id, entry, "removal").await;

        servers.remove(id);
        Ok(())
    }

    /// Start all non-disabled servers concurrently.
    ///
    /// Collects all server IDs where `disabled == false`, then starts each one
    /// via [`start_server`](Self::start_server). Failures are logged but do not
    /// prevent other servers from starting.
    ///
    /// # Returns
    ///
    /// A `HashMap<String, Result<()>>` with per-server outcomes. Disabled servers
    /// are not included in the result.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let results = manager.start_all().await;
    /// for (id, result) in &results {
    ///     match result {
    ///         Ok(()) => println!("{id}: started"),
    ///         Err(e) => eprintln!("{id}: failed to start: {e}"),
    ///     }
    /// }
    /// ```
    pub async fn start_all(&self) -> HashMap<String, adk_core::Result<()>> {
        // Collect IDs of non-disabled servers under a read lock
        let ids_to_start: Vec<String> = {
            let servers = self.servers.read().await;
            servers
                .iter()
                .filter(|(_, entry)| !entry.config.disabled)
                .map(|(id, _)| id.clone())
                .collect()
        };

        // Start each server concurrently — each start_server call acquires
        // its own write lock internally
        let futures: Vec<_> = ids_to_start
            .iter()
            .map(|id| {
                let id = id.clone();
                async move {
                    let result = self.start_server(&id).await;
                    if let Err(ref e) = result {
                        tracing::error!(
                            server.id = id,
                            error = %e,
                            "failed to start server during start_all"
                        );
                    }
                    (id, result)
                }
            })
            .collect();

        futures::future::join_all(futures).await.into_iter().collect()
    }

    /// Shut down all managed servers and stop health monitoring.
    ///
    /// This method first stops the health monitoring task, then stops all
    /// running servers using the graceful stop sequence (cancel token → grace
    /// period → force-kill). After shutdown, all server statuses are set to
    /// `Stopped`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// manager.shutdown().await?;
    /// // All servers are now stopped, safe to drop the manager
    /// ```
    pub async fn shutdown(&self) -> adk_core::Result<()> {
        // Step 1: Stop health monitoring first
        self.stop_monitoring();

        // Step 2: Acquire write lock and stop all running servers
        let mut servers = self.servers.write().await;
        let ids: Vec<String> = servers
            .iter()
            .filter(|(_, entry)| entry.status == ServerStatus::Running)
            .map(|(id, _)| id.clone())
            .collect();

        for id in &ids {
            if let Some(entry) = servers.get_mut(id) {
                Self::stop_server_inner(id, entry, "shutdown").await;
            }
        }

        // Step 3: Set all server statuses to Stopped (except Disabled)
        for entry in servers.values_mut() {
            if entry.status != ServerStatus::Disabled {
                entry.status = ServerStatus::Stopped;
            }
        }

        Ok(())
    }
}

impl Drop for McpServerManager {
    fn drop(&mut self) {
        // Use try_read() to avoid blocking in Drop
        if let Ok(servers) = self.servers.try_read() {
            let running = servers.values().filter(|e| e.status == ServerStatus::Running).count();
            if running > 0 {
                tracing::warn!(
                    running_count = running,
                    "McpServerManager dropped with {running} servers still running. \
                     Call shutdown() before dropping to ensure clean process cleanup."
                );
            }
        }
    }
}

// Static assertions: McpServerManager must be Send + Sync so it can be
// shared across async tasks via Arc.
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}
    fn _assert_send_sync() {
        _assert_send::<McpServerManager>();
        _assert_sync::<McpServerManager>();
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_empty_configs() {
        let manager = McpServerManager::new(HashMap::new());
        assert_eq!(manager.name, "mcp_server_manager");
        assert_eq!(manager.health_check_interval, Duration::from_secs(30));
        assert_eq!(manager.grace_period, Duration::from_secs(5));
        assert!(manager.elicitation_handler.is_none());
    }

    #[test]
    fn test_new_disabled_server_gets_disabled_status() {
        let configs = HashMap::from([(
            "disabled-server".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: true,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);
        let servers = manager.servers.try_read().unwrap();
        assert_eq!(servers["disabled-server"].status, ServerStatus::Disabled);
    }

    #[test]
    fn test_new_enabled_server_gets_stopped_status() {
        let configs = HashMap::from([(
            "enabled-server".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);
        let servers = manager.servers.try_read().unwrap();
        assert_eq!(servers["enabled-server"].status, ServerStatus::Stopped);
    }

    #[test]
    fn test_builder_with_name() {
        let manager = McpServerManager::new(HashMap::new()).with_name("custom_name");
        assert_eq!(manager.name, "custom_name");
    }

    #[test]
    fn test_builder_with_health_check_interval() {
        let manager = McpServerManager::new(HashMap::new())
            .with_health_check_interval(Duration::from_secs(10));
        assert_eq!(manager.health_check_interval, Duration::from_secs(10));
    }

    #[test]
    fn test_builder_with_grace_period() {
        let manager =
            McpServerManager::new(HashMap::new()).with_grace_period(Duration::from_secs(2));
        assert_eq!(manager.grace_period, Duration::from_secs(2));
    }

    #[test]
    fn test_builder_with_elicitation_handler() {
        use super::super::super::elicitation::AutoDeclineElicitationHandler;
        let handler: Arc<dyn ElicitationHandler> = Arc::new(AutoDeclineElicitationHandler);
        let manager = McpServerManager::new(HashMap::new()).with_elicitation_handler(handler);
        assert!(manager.elicitation_handler.is_some());
    }

    #[tokio::test]
    async fn test_server_status_returns_correct_status() {
        let configs = HashMap::from([(
            "server-a".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);
        let status = manager.server_status("server-a").await.unwrap();
        assert_eq!(status, ServerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_server_status_unknown_id_returns_error() {
        let manager = McpServerManager::new(HashMap::new());
        let result = manager.server_status("nonexistent").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("unknown server ID: 'nonexistent'"));
    }

    #[tokio::test]
    async fn test_all_statuses_returns_all_servers() {
        let configs = HashMap::from([
            (
                "server-a".to_string(),
                McpServerConfig {
                    command: "echo".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
            (
                "server-b".to_string(),
                McpServerConfig {
                    command: "echo".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: true,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
        ]);
        let manager = McpServerManager::new(configs);
        let statuses = manager.all_statuses().await;
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses["server-a"], ServerStatus::Stopped);
        assert_eq!(statuses["server-b"], ServerStatus::Disabled);
    }

    #[tokio::test]
    async fn test_all_statuses_empty_manager() {
        let manager = McpServerManager::new(HashMap::new());
        let statuses = manager.all_statuses().await;
        assert!(statuses.is_empty());
    }

    #[tokio::test]
    async fn test_running_server_count_no_running() {
        let configs = HashMap::from([(
            "server-a".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);
        assert_eq!(manager.running_server_count().await, 0);
    }

    #[tokio::test]
    async fn test_running_server_count_empty_manager() {
        let manager = McpServerManager::new(HashMap::new());
        assert_eq!(manager.running_server_count().await, 0);
    }

    #[tokio::test]
    async fn test_start_all_skips_disabled_servers() {
        let configs = HashMap::from([
            (
                "enabled".to_string(),
                McpServerConfig {
                    command: "nonexistent-command-xyz".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
            (
                "disabled".to_string(),
                McpServerConfig {
                    command: "echo".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: true,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
        ]);
        let manager = McpServerManager::new(configs);
        let results = manager.start_all().await;

        // Only the enabled server should be in the results
        assert!(results.contains_key("enabled"));
        assert!(!results.contains_key("disabled"));

        // The enabled server should fail (nonexistent command)
        assert!(results["enabled"].is_err());

        // The disabled server should still be Disabled
        let status = manager.server_status("disabled").await.unwrap();
        assert_eq!(status, ServerStatus::Disabled);
    }

    #[tokio::test]
    async fn test_start_all_empty_manager() {
        let manager = McpServerManager::new(HashMap::new());
        let results = manager.start_all().await;
        assert!(results.is_empty());
    }

    #[test]
    fn test_from_json_valid() {
        let json = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                    "env": { "NODE_ENV": "production" },
                    "disabled": false,
                    "autoApprove": ["read_file", "list_directory"]
                },
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": { "GITHUB_TOKEN": "ghp_xxx" },
                    "disabled": true,
                    "autoApprove": []
                }
            }
        }"#;
        let manager = McpServerManager::from_json(json).unwrap();
        let servers = manager.servers.try_read().unwrap();
        assert_eq!(servers.len(), 2);

        let fs_entry = &servers["filesystem"];
        assert_eq!(fs_entry.config.command, "npx");
        assert_eq!(
            fs_entry.config.args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert_eq!(fs_entry.config.env["NODE_ENV"], "production");
        assert!(!fs_entry.config.disabled);
        assert_eq!(fs_entry.config.auto_approve, vec!["read_file", "list_directory"]);
        assert_eq!(fs_entry.status, ServerStatus::Stopped);

        let gh_entry = &servers["github"];
        assert_eq!(gh_entry.config.command, "npx");
        assert!(gh_entry.config.disabled);
        assert_eq!(gh_entry.status, ServerStatus::Disabled);
    }

    #[test]
    fn test_from_json_malformed() {
        let json = r#"{ this is not valid json }"#;
        let result = McpServerManager::from_json(json);
        let err = result.err().expect("should fail on malformed JSON");
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("failed to parse MCP server config"),
            "error message was: {err_msg}"
        );
    }

    #[test]
    fn test_from_json_missing_command() {
        let json = r#"{
            "mcpServers": {
                "bad-server": {
                    "args": ["--flag"]
                }
            }
        }"#;
        let result = McpServerManager::from_json(json);
        let err = result.err().expect("should fail on missing command field");
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("failed to parse MCP server config"),
            "error message was: {err_msg}"
        );
    }

    #[test]
    fn test_from_json_file_not_found() {
        let result = McpServerManager::from_json_file("/nonexistent/path/mcp.json");
        let err = result.err().expect("should fail on nonexistent file");
        let err_msg = format!("{err}");
        assert!(err_msg.contains("failed to read config file"), "error message was: {err_msg}");
        assert!(
            err_msg.contains("/nonexistent/path/mcp.json"),
            "error message should contain the path: {err_msg}"
        );
    }

    #[test]
    fn test_mixed_disabled_and_enabled_servers() {
        let configs = HashMap::from([
            (
                "server-a".to_string(),
                McpServerConfig {
                    command: "cmd-a".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
            (
                "server-b".to_string(),
                McpServerConfig {
                    command: "cmd-b".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: true,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
            (
                "server-c".to_string(),
                McpServerConfig {
                    command: "cmd-c".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
        ]);
        let manager = McpServerManager::new(configs);
        let servers = manager.servers.try_read().unwrap();
        assert_eq!(servers["server-a"].status, ServerStatus::Stopped);
        assert_eq!(servers["server-b"].status, ServerStatus::Disabled);
        assert_eq!(servers["server-c"].status, ServerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_add_server_success() {
        let manager = McpServerManager::new(HashMap::new());
        let config = McpServerConfig {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            env: HashMap::new(),
            disabled: false,
            auto_approve: vec![],
            restart_policy: None,
        };
        let result = manager.add_server("new-server".to_string(), config).await;
        assert!(result.is_ok());

        let status = manager.server_status("new-server").await.unwrap();
        assert_eq!(status, ServerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_add_server_duplicate_id() {
        let configs = HashMap::from([(
            "existing".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);

        let config = McpServerConfig {
            command: "echo".to_string(),
            args: vec![],
            env: HashMap::new(),
            disabled: false,
            auto_approve: vec![],
            restart_policy: None,
        };
        let result = manager.add_server("existing".to_string(), config).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("server ID 'existing' already exists"));
    }

    #[tokio::test]
    async fn test_add_server_disabled() {
        let manager = McpServerManager::new(HashMap::new());
        let config = McpServerConfig {
            command: "echo".to_string(),
            args: vec![],
            env: HashMap::new(),
            disabled: true,
            auto_approve: vec![],
            restart_policy: None,
        };
        let result = manager.add_server("disabled-server".to_string(), config).await;
        assert!(result.is_ok());

        let status = manager.server_status("disabled-server").await.unwrap();
        assert_eq!(status, ServerStatus::Disabled);
    }

    #[tokio::test]
    async fn test_remove_server_success() {
        let configs = HashMap::from([(
            "to-remove".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);

        // Verify it exists first
        assert!(manager.server_status("to-remove").await.is_ok());

        // Remove it
        let result = manager.remove_server("to-remove").await;
        assert!(result.is_ok());

        // Verify it no longer exists
        let status_result = manager.server_status("to-remove").await;
        assert!(status_result.is_err());
    }

    #[tokio::test]
    async fn test_remove_server_unknown_id() {
        let manager = McpServerManager::new(HashMap::new());
        let result = manager.remove_server("nonexistent").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("unknown server ID: 'nonexistent'"));
    }

    #[tokio::test]
    async fn test_shutdown_sets_all_to_stopped() {
        // Create a manager with a mix of statuses
        let configs = HashMap::from([
            (
                "server-a".to_string(),
                McpServerConfig {
                    command: "echo".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
            (
                "server-b".to_string(),
                McpServerConfig {
                    command: "echo".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: true,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
            (
                "server-c".to_string(),
                McpServerConfig {
                    command: "echo".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    disabled: false,
                    auto_approve: vec![],
                    restart_policy: None,
                },
            ),
        ]);
        let manager = McpServerManager::new(configs);

        // Manually set server-a to FailedToStart to test that shutdown
        // resets non-disabled statuses to Stopped
        {
            let mut servers = manager.servers.write().await;
            servers.get_mut("server-a").unwrap().status = ServerStatus::FailedToStart;
        }

        // Call shutdown
        let result = manager.shutdown().await;
        assert!(result.is_ok());

        // Verify all non-disabled servers are Stopped
        let statuses = manager.all_statuses().await;
        assert_eq!(statuses["server-a"], ServerStatus::Stopped);
        assert_eq!(statuses["server-b"], ServerStatus::Disabled); // Disabled stays Disabled
        assert_eq!(statuses["server-c"], ServerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_shutdown_stops_monitoring() {
        let manager = McpServerManager::new(HashMap::new());

        // Start monitoring
        manager.start_monitoring();

        // Shutdown should cancel the monitoring token
        let result = manager.shutdown().await;
        assert!(result.is_ok());

        // Verify the cancellation token is cancelled
        assert!(manager.monitor_cancel.is_cancelled());
    }

    #[tokio::test]
    async fn test_shutdown_empty_manager() {
        let manager = McpServerManager::new(HashMap::new());
        let result = manager.shutdown().await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_drop_no_warning_when_no_running_servers() {
        // This test verifies Drop doesn't panic when no servers are running.
        // The warning is only logged (not observable in test), but we verify
        // the Drop impl runs without error.
        let configs = HashMap::from([(
            "server-a".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                disabled: false,
                auto_approve: vec![],
                restart_policy: None,
            },
        )]);
        let manager = McpServerManager::new(configs);
        // Drop happens here — should not panic
        drop(manager);
    }
}
