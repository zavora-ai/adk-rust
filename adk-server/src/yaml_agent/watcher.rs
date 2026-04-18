//! Hot reload watcher for YAML agent definitions.
//!
//! Uses the `notify` crate to detect filesystem changes and trigger
//! re-loading of affected agent definitions with debouncing.
//!
//! # Overview
//!
//! The [`HotReloadWatcher`] monitors a directory for changes to `.yaml` and
//! `.yml` files. When a change is detected, it debounces rapid edits (500ms
//! default), validates the updated YAML, and atomically replaces the active
//! agent instance via `Arc<RwLock<>>`. If validation fails, the previous
//! valid agent is kept and a warning is logged.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_server::yaml_agent::watcher::HotReloadWatcher;
//! use adk_server::yaml_agent::loader::AgentConfigLoader;
//! use std::sync::Arc;
//! use std::path::Path;
//!
//! let loader = Arc::new(AgentConfigLoader::new(registry, model_factory));
//! let watcher = HotReloadWatcher::new(loader);
//! let handle = watcher.watch(Path::new("./agents")).await?;
//! // The watcher runs in the background, reloading agents on file changes.
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::Agent;
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::loader::AgentConfigLoader;

/// Default debounce duration for filesystem events.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

/// Hot reload watcher for YAML agent definitions.
///
/// Monitors a directory for changes to `.yaml` and `.yml` files, debounces
/// rapid edits, validates updated definitions, and atomically swaps active
/// agent instances. Failed reloads log a warning and keep the previous valid
/// agent.
///
/// Concurrent requests are served from the current `active_agents` map without
/// blocking on reload operations — reads use `RwLock::read()` while reloads
/// acquire `RwLock::write()` only for the brief swap.
pub struct HotReloadWatcher {
    loader: Arc<AgentConfigLoader>,
    /// Active agent instances keyed by canonical file path.
    active_agents: Arc<RwLock<HashMap<PathBuf, Arc<dyn Agent>>>>,
    /// Debounce duration (default 500ms).
    debounce: Duration,
}

impl HotReloadWatcher {
    /// Create a new hot reload watcher with the given loader and default
    /// 500ms debounce.
    pub fn new(loader: Arc<AgentConfigLoader>) -> Self {
        Self {
            loader,
            active_agents: Arc::new(RwLock::new(HashMap::new())),
            debounce: DEFAULT_DEBOUNCE,
        }
    }

    /// Create a new hot reload watcher with a custom debounce duration.
    pub fn with_debounce(loader: Arc<AgentConfigLoader>, debounce: Duration) -> Self {
        Self { loader, active_agents: Arc::new(RwLock::new(HashMap::new())), debounce }
    }

    /// Start watching a directory for YAML file changes.
    ///
    /// Performs an initial load of all YAML files in the directory, then
    /// spawns a background task that watches for filesystem events and
    /// reloads affected agents.
    ///
    /// Returns a `JoinHandle` for the background watcher task.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial directory load fails or the filesystem
    /// watcher cannot be created.
    pub async fn watch(&self, dir: &Path) -> adk_core::Result<tokio::task::JoinHandle<()>> {
        let dir = dir.to_path_buf();
        let dir_display = dir.display().to_string();

        // Initial load of all agents in the directory
        let agents = self.loader.load_directory(&dir).await?;
        {
            let mut active = self.active_agents.write().await;
            // Populate active_agents keyed by file path
            // We need to re-scan the directory to map file paths to agents
            let yaml_files = collect_yaml_files(&dir)?;
            for file_path in yaml_files {
                if let Ok(agent) = self.loader.load_file(&file_path).await {
                    active.insert(file_path, agent);
                }
            }
        }
        info!("hot reload watcher initialized with {} agents from {dir_display}", agents.len());

        // Set up the notify watcher with a channel
        let (tx, mut rx) = tokio::sync::mpsc::channel::<PathBuf>(100);

        let dir_for_watcher = dir.clone();
        let notify_tx = tx.clone();

        // Spawn the notify watcher on a blocking thread since it uses std sync
        let _watcher_handle = std::thread::spawn(move || {
            let rt_tx = notify_tx;
            let mut watcher = match notify::recommended_watcher(
                move |res: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = res {
                        // Only react to content modifications and creations
                        match event.kind {
                            EventKind::Modify(_) | EventKind::Create(_) => {
                                for path in event.paths {
                                    if is_yaml_file(&path) {
                                        // Best-effort send; if the channel is full, the
                                        // event will be picked up on the next change.
                                        let _ = rt_tx.blocking_send(path);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                },
            ) {
                Ok(w) => w,
                Err(e) => {
                    warn!("failed to create filesystem watcher: {e}");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&dir_for_watcher, RecursiveMode::NonRecursive) {
                warn!("failed to watch directory {}: {e}", dir_for_watcher.display());
                return;
            }

            debug!("filesystem watcher started for {}", dir_for_watcher.display());

            // Keep the watcher alive until the thread is dropped.
            // We park the thread; it will be unparked (and exit) when the
            // process shuts down.
            loop {
                std::thread::park();
            }
        });

        // Spawn the debounce + reload task
        let loader = Arc::clone(&self.loader);
        let active_agents = Arc::clone(&self.active_agents);
        let debounce = self.debounce;

        let handle = tokio::spawn(async move {
            // Pending paths waiting for debounce to expire
            let mut pending: HashMap<PathBuf, tokio::time::Instant> = HashMap::new();

            loop {
                // Calculate the next deadline from pending entries
                let next_deadline = pending.values().min().copied();

                tokio::select! {
                    // Receive new file change events
                    Some(path) = rx.recv() => {
                        let deadline = tokio::time::Instant::now() + debounce;
                        pending.insert(path, deadline);
                    }
                    // Process debounced events when their deadline arrives
                    _ = async {
                        match next_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        let now = tokio::time::Instant::now();
                        let ready: Vec<PathBuf> = pending
                            .iter()
                            .filter(|(_, deadline)| **deadline <= now)
                            .map(|(path, _)| path.clone())
                            .collect();

                        for path in ready {
                            pending.remove(&path);
                            reload_agent(&loader, &active_agents, &path).await;
                        }
                    }
                }
            }
        });

        Ok(handle)
    }

    /// Get the current active agent for a given name.
    ///
    /// Searches all active agents (loaded from YAML files) for one matching
    /// the given name. This operation acquires a read lock and does not block
    /// reload operations.
    pub async fn get_agent(&self, name: &str) -> Option<Arc<dyn Agent>> {
        let agents = self.active_agents.read().await;
        agents.values().find(|agent| agent.name() == name).cloned()
    }

    /// Get all currently active agents.
    pub async fn all_agents(&self) -> Vec<Arc<dyn Agent>> {
        self.active_agents.read().await.values().cloned().collect()
    }
}

/// Reload a single agent from a YAML file, replacing the active instance
/// atomically if validation succeeds.
async fn reload_agent(
    loader: &AgentConfigLoader,
    active_agents: &RwLock<HashMap<PathBuf, Arc<dyn Agent>>>,
    path: &Path,
) {
    let path_display = path.display();
    info!("reloading agent from {path_display}");

    match loader.reload_file(path).await {
        Ok(agent) => {
            let agent_name = agent.name().to_string();
            // Atomically replace the agent — write lock is held only for
            // the brief HashMap insert.
            active_agents.write().await.insert(path.to_path_buf(), agent);
            info!("successfully reloaded agent '{agent_name}' from {path_display}");
        }
        Err(e) => {
            // Validation failed — keep the previous valid agent
            warn!("failed to reload agent from {path_display}: {e}");
        }
    }
}

/// Check if a path has a `.yaml` or `.yml` extension.
fn is_yaml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let ext = ext.to_lowercase();
            ext == "yaml" || ext == "yml"
        })
        .unwrap_or(false)
}

/// Collect all `.yaml` and `.yml` files from a directory (non-recursive).
fn collect_yaml_files(dir: &Path) -> adk_core::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| {
        adk_core::AdkError::config(format!("failed to read directory '{}': {e}", dir.display()))
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            adk_core::AdkError::config(format!(
                "failed to read directory entry in '{}': {e}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if path.is_file() && is_yaml_file(&path) {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Llm, LlmRequest, Tool};
    use async_trait::async_trait;
    use std::io::Write;

    use super::super::loader::ModelFactory;

    // --- Mock ModelFactory ---

    struct MockModelFactory;

    #[async_trait]
    impl ModelFactory for MockModelFactory {
        async fn create_model(
            &self,
            provider: &str,
            model_id: &str,
        ) -> adk_core::Result<Arc<dyn Llm>> {
            Ok(Arc::new(MockLlm { name: format!("{provider}/{model_id}") }))
        }
    }

    // --- Mock LLM ---

    struct MockLlm {
        name: String,
    }

    #[async_trait]
    impl Llm for MockLlm {
        fn name(&self) -> &str {
            &self.name
        }

        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<adk_core::LlmResponseStream> {
            unimplemented!("mock LLM")
        }
    }

    // --- Mock ToolRegistry ---

    struct MockToolRegistry;

    impl adk_core::ToolRegistry for MockToolRegistry {
        fn resolve(&self, _tool_name: &str) -> Option<Arc<dyn Tool>> {
            None
        }

        fn available_tools(&self) -> Vec<String> {
            vec![]
        }
    }

    // --- Helper ---

    fn write_yaml(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let path = dir.join(filename);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    // --- Tests ---

    #[test]
    fn test_is_yaml_file() {
        assert!(is_yaml_file(Path::new("agent.yaml")));
        assert!(is_yaml_file(Path::new("agent.yml")));
        assert!(is_yaml_file(Path::new("agent.YAML")));
        assert!(is_yaml_file(Path::new("agent.YML")));
        assert!(!is_yaml_file(Path::new("agent.json")));
        assert!(!is_yaml_file(Path::new("agent.txt")));
        assert!(!is_yaml_file(Path::new("agent")));
    }

    #[tokio::test]
    async fn test_watcher_initial_load() {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(
            dir.path(),
            "agent.yaml",
            r#"
name: test_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Hello"
"#,
        );

        let registry = Arc::new(MockToolRegistry);
        let factory = Arc::new(MockModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));
        let watcher = HotReloadWatcher::new(loader);

        let handle = watcher.watch(dir.path()).await.unwrap();

        // Agent should be loaded
        let agent = watcher.get_agent("test_agent").await;
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().name(), "test_agent");

        handle.abort();
    }

    #[tokio::test]
    async fn test_watcher_get_agent_not_found() {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(
            dir.path(),
            "agent.yaml",
            r#"
name: test_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
"#,
        );

        let registry = Arc::new(MockToolRegistry);
        let factory = Arc::new(MockModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));
        let watcher = HotReloadWatcher::new(loader);

        let handle = watcher.watch(dir.path()).await.unwrap();

        // Non-existent agent should return None
        let agent = watcher.get_agent("nonexistent").await;
        assert!(agent.is_none());

        handle.abort();
    }

    #[tokio::test]
    async fn test_watcher_all_agents() {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(
            dir.path(),
            "agent_a.yaml",
            r#"
name: agent_a
model:
  provider: gemini
  model_id: gemini-2.0-flash
"#,
        );
        write_yaml(
            dir.path(),
            "agent_b.yml",
            r#"
name: agent_b
model:
  provider: openai
  model_id: gpt-4
"#,
        );

        let registry = Arc::new(MockToolRegistry);
        let factory = Arc::new(MockModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));
        let watcher = HotReloadWatcher::new(loader);

        let handle = watcher.watch(dir.path()).await.unwrap();

        let agents = watcher.all_agents().await;
        assert_eq!(agents.len(), 2);

        let names: Vec<&str> = agents.iter().map(|a| a.name()).collect();
        assert!(names.contains(&"agent_a"));
        assert!(names.contains(&"agent_b"));

        handle.abort();
    }

    #[tokio::test]
    async fn test_reload_agent_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agent.yaml",
            r#"
name: reloadable
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Version 1"
"#,
        );

        let registry = Arc::new(MockToolRegistry);
        let factory = Arc::new(MockModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));

        let active_agents: Arc<RwLock<HashMap<PathBuf, Arc<dyn Agent>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Initial load
        let agent = loader.load_file(&path).await.unwrap();
        active_agents.write().await.insert(path.clone(), agent);

        // Update the file
        std::fs::write(
            &path,
            r#"
name: reloadable
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Version 2"
"#,
        )
        .unwrap();

        // Reload
        reload_agent(&loader, &active_agents, &path).await;

        let agents = active_agents.read().await;
        let agent = agents.get(&path).unwrap();
        assert_eq!(agent.name(), "reloadable");
    }

    #[tokio::test]
    async fn test_reload_agent_validation_failure_keeps_previous() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agent.yaml",
            r#"
name: stable_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Valid agent"
"#,
        );

        let registry = Arc::new(MockToolRegistry);
        let factory = Arc::new(MockModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));

        let active_agents: Arc<RwLock<HashMap<PathBuf, Arc<dyn Agent>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Initial load
        let agent = loader.load_file(&path).await.unwrap();
        active_agents.write().await.insert(path.clone(), agent);

        // Write invalid YAML
        std::fs::write(&path, "invalid: yaml: content: [broken").unwrap();

        // Reload should fail but keep the previous agent
        reload_agent(&loader, &active_agents, &path).await;

        let agents = active_agents.read().await;
        let agent = agents.get(&path).unwrap();
        assert_eq!(agent.name(), "stable_agent");
    }

    #[test]
    fn test_collect_yaml_files() {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(dir.path(), "a.yaml", "name: a\n");
        write_yaml(dir.path(), "b.yml", "name: b\n");
        write_yaml(dir.path(), "c.json", "{}");
        write_yaml(dir.path(), "d.txt", "hello");

        let files = collect_yaml_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.file_name().unwrap() == "a.yaml"));
        assert!(files.iter().any(|f| f.file_name().unwrap() == "b.yml"));
    }

    #[tokio::test]
    async fn test_watcher_with_custom_debounce() {
        let registry = Arc::new(MockToolRegistry);
        let factory = Arc::new(MockModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));

        let watcher = HotReloadWatcher::with_debounce(loader, Duration::from_millis(100));
        assert_eq!(watcher.debounce, Duration::from_millis(100));
    }
}
