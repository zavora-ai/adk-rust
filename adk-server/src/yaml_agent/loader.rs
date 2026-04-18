//! Agent config loader for YAML definitions.
//!
//! Parses YAML files, validates schemas, resolves tool and sub-agent references,
//! and produces live [`Agent`](adk_core::Agent) instances.
//!
//! # Overview
//!
//! The [`AgentConfigLoader`] reads YAML agent definition files, validates them
//! against the schema defined in [`super::schema`], resolves tool references
//! from a [`ToolRegistry`](adk_core::ToolRegistry), resolves sub-agent references
//! from previously loaded agents, and constructs [`Agent`](adk_core::Agent)
//! instances using [`LlmAgentBuilder`](adk_agent::LlmAgentBuilder).
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_server::yaml_agent::loader::AgentConfigLoader;
//! use std::sync::Arc;
//!
//! let registry: Arc<dyn adk_core::ToolRegistry> = /* ... */;
//! let model_factory: Arc<dyn ModelFactory> = /* ... */;
//! let loader = AgentConfigLoader::new(registry, model_factory);
//! let agent = loader.load_file(Path::new("agent.yaml")).await?;
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{Agent, Llm, ToolRegistry};
use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::schema::{ToolReference, YamlAgentDefinition};

/// Factory trait for creating LLM model instances from provider and model ID.
///
/// Implementations map provider names (e.g., "gemini", "openai", "anthropic")
/// and model IDs to concrete [`Llm`] instances.
#[async_trait]
pub trait ModelFactory: Send + Sync {
    /// Create an LLM model instance for the given provider and model ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider is unknown or the model cannot be created.
    async fn create_model(&self, provider: &str, model_id: &str) -> adk_core::Result<Arc<dyn Llm>>;
}

/// Loads YAML agent definitions, validates them, and produces live `Agent` instances.
///
/// The loader maintains a cache of previously loaded agents so that sub-agent
/// references can be resolved across files. Tool references are resolved via
/// the provided [`ToolRegistry`].
pub struct AgentConfigLoader {
    tool_registry: Arc<dyn ToolRegistry>,
    model_factory: Arc<dyn ModelFactory>,
    loaded_agents: Arc<RwLock<HashMap<String, Arc<dyn Agent>>>>,
}

impl AgentConfigLoader {
    /// Create a new loader with the given tool registry and model factory.
    pub fn new(tool_registry: Arc<dyn ToolRegistry>, model_factory: Arc<dyn ModelFactory>) -> Self {
        Self { tool_registry, model_factory, loaded_agents: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// Load a single YAML file, validate it, and produce an `Agent`.
    ///
    /// The agent is cached by name so that subsequent calls to [`load_file`](Self::load_file)
    /// or [`load_directory`](Self::load_directory) can resolve sub-agent references.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error if:
    /// - The file cannot be read
    /// - The YAML is malformed or fails schema validation
    /// - A tool reference cannot be resolved from the registry
    /// - A sub-agent reference cannot be resolved from previously loaded agents
    pub async fn load_file(&self, path: &Path) -> adk_core::Result<Arc<dyn Agent>> {
        let path_display = path.display();
        info!("loading YAML agent definition from {path_display}");

        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            adk_core::AdkError::config(format!(
                "failed to read YAML agent file '{path_display}': {e}"
            ))
        })?;

        let definition = self.parse_and_validate(&content, path)?;
        let agent = self.build_agent(definition, path).await?;

        // Cache the agent by name
        let name = agent.name().to_string();
        self.loaded_agents.write().await.insert(name.clone(), agent.clone());
        debug!("cached agent '{name}' from {path_display}");

        Ok(agent)
    }

    /// Load all `.yaml` and `.yml` files from a directory, resolving cross-references.
    ///
    /// Files are loaded in two passes:
    /// 1. Parse all definitions to discover agent names
    /// 2. Build agents, resolving sub-agent references from the full set
    ///
    /// # Errors
    ///
    /// Returns a descriptive error if any file fails to parse, validate, or resolve.
    pub async fn load_directory(&self, dir: &Path) -> adk_core::Result<Vec<Arc<dyn Agent>>> {
        let dir_display = dir.display();
        info!("loading YAML agent definitions from directory {dir_display}");

        if !dir.is_dir() {
            return Err(adk_core::AdkError::config(format!("'{dir_display}' is not a directory")));
        }

        // Collect all YAML files
        let yaml_files = collect_yaml_files(dir)?;
        if yaml_files.is_empty() {
            warn!("no YAML agent definition files found in {dir_display}");
            return Ok(vec![]);
        }

        debug!("found {} YAML files in {dir_display}", yaml_files.len());

        // Pass 1: Parse all definitions
        let mut definitions: Vec<(PathBuf, YamlAgentDefinition)> = Vec::new();
        for file_path in &yaml_files {
            let content = tokio::fs::read_to_string(file_path).await.map_err(|e| {
                adk_core::AdkError::config(format!(
                    "failed to read YAML agent file '{}': {e}",
                    file_path.display()
                ))
            })?;
            let def = self.parse_and_validate(&content, file_path)?;
            definitions.push((file_path.clone(), def));
        }

        // Check for duplicate agent names
        let mut seen_names: HashMap<&str, &Path> = HashMap::new();
        for (path, def) in &definitions {
            if let Some(prev_path) = seen_names.insert(&def.name, path) {
                return Err(adk_core::AdkError::config(format!(
                    "duplicate agent name '{}' found in '{}' and '{}'",
                    def.name,
                    prev_path.display(),
                    path.display()
                )));
            }
        }

        // Pass 2: Build agents in dependency order (agents without sub-agent refs first)
        let mut agents: Vec<Arc<dyn Agent>> = Vec::new();
        let mut remaining: Vec<(PathBuf, YamlAgentDefinition)> = definitions;
        let mut progress = true;

        while !remaining.is_empty() && progress {
            progress = false;
            let mut still_remaining = Vec::new();

            for (path, def) in remaining {
                // Check if all sub-agent references are resolvable
                let all_resolved = {
                    let loaded = self.loaded_agents.read().await;
                    def.sub_agents.iter().all(|sa| loaded.contains_key(&sa.reference))
                };

                if all_resolved {
                    let agent = self.build_agent(def, &path).await?;
                    let name = agent.name().to_string();
                    self.loaded_agents.write().await.insert(name, agent.clone());
                    agents.push(agent);
                    progress = true;
                } else {
                    still_remaining.push((path, def));
                }
            }

            remaining = still_remaining;
        }

        // If there are still unresolved agents, report the error
        if !remaining.is_empty() {
            let mut unresolved: Vec<String> = Vec::new();
            for (path, def) in &remaining {
                let loaded = self.loaded_agents.read().await;
                let missing: Vec<&str> = def
                    .sub_agents
                    .iter()
                    .filter(|sa| !loaded.contains_key(&sa.reference))
                    .map(|sa| sa.reference.as_str())
                    .collect();
                unresolved.push(format!(
                    "'{}' in '{}' (unresolved sub-agents: {})",
                    def.name,
                    path.display(),
                    missing.join(", ")
                ));
            }
            return Err(adk_core::AdkError::config(format!(
                "circular or unresolvable sub-agent references: {}",
                unresolved.join("; ")
            )));
        }

        info!("loaded {} agents from {dir_display}", agents.len());
        Ok(agents)
    }

    /// Replace a loaded agent by re-reading its YAML file.
    ///
    /// Used by the hot reload watcher to update agents when files change.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, parsed, or validated.
    pub async fn reload_file(&self, path: &Path) -> adk_core::Result<Arc<dyn Agent>> {
        debug!("reloading YAML agent definition from {}", path.display());
        self.load_file(path).await
    }

    /// Get a previously loaded agent by name.
    pub async fn get_agent(&self, name: &str) -> Option<Arc<dyn Agent>> {
        self.loaded_agents.read().await.get(name).cloned()
    }

    /// Get all loaded agents.
    pub async fn all_agents(&self) -> Vec<Arc<dyn Agent>> {
        self.loaded_agents.read().await.values().cloned().collect()
    }

    /// Parse YAML content and validate the schema.
    fn parse_and_validate(
        &self,
        content: &str,
        path: &Path,
    ) -> adk_core::Result<YamlAgentDefinition> {
        let definition: YamlAgentDefinition = serde_yaml::from_str(content).map_err(|e| {
            adk_core::AdkError::config(format!("invalid YAML in '{}': {e}", path.display()))
        })?;

        self.validate_definition(&definition, path)?;
        Ok(definition)
    }

    /// Validate a parsed definition for semantic correctness.
    fn validate_definition(&self, def: &YamlAgentDefinition, path: &Path) -> adk_core::Result<()> {
        let path_display = path.display();

        if def.name.is_empty() {
            return Err(adk_core::AdkError::config(format!(
                "field 'name' is required and cannot be empty in '{path_display}'"
            )));
        }

        if def.model.provider.is_empty() {
            return Err(adk_core::AdkError::config(format!(
                "field 'model.provider' is required and cannot be empty in '{path_display}'"
            )));
        }

        if def.model.model_id.is_empty() {
            return Err(adk_core::AdkError::config(format!(
                "field 'model.model_id' is required and cannot be empty in '{path_display}'"
            )));
        }

        // Validate temperature range if provided
        if let Some(temp) = def.model.temperature {
            if !(0.0..=2.0).contains(&temp) {
                return Err(adk_core::AdkError::config(format!(
                    "field 'model.temperature' must be between 0.0 and 2.0, got {temp} in '{path_display}'"
                )));
            }
        }

        Ok(())
    }

    /// Build an `Agent` from a validated YAML definition.
    async fn build_agent(
        &self,
        def: YamlAgentDefinition,
        path: &Path,
    ) -> adk_core::Result<Arc<dyn Agent>> {
        let path_display = path.display();

        // Create the LLM model
        let model = self
            .model_factory
            .create_model(&def.model.provider, &def.model.model_id)
            .await
            .map_err(|e| {
                adk_core::AdkError::config(format!(
                    "failed to create model for provider '{}' with model_id '{}' in '{path_display}': {e}",
                    def.model.provider, def.model.model_id
                ))
            })?;

        // Start building the agent
        let mut builder = adk_agent::LlmAgentBuilder::new(&def.name).model(model);

        // Set description
        if let Some(desc) = &def.description {
            builder = builder.description(desc);
        }

        // Set instructions
        if let Some(instructions) = &def.instructions {
            builder = builder.instruction(instructions);
        }

        // Set generation config from model config
        if def.model.temperature.is_some() || def.model.max_tokens.is_some() {
            let mut config = adk_core::GenerateContentConfig::default();
            if let Some(temp) = def.model.temperature {
                config.temperature = Some(temp as f32);
            }
            if let Some(max_tokens) = def.model.max_tokens {
                config.max_output_tokens = Some(max_tokens as i32);
            }
            builder = builder.generate_content_config(config);
        }

        // Resolve tool references
        for tool_ref in &def.tools {
            match tool_ref {
                ToolReference::Named { name } => {
                    let tool = self.tool_registry.resolve(name).ok_or_else(|| {
                        adk_core::AdkError::config(format!(
                            "unresolved tool reference '{name}' in '{path_display}'. \
                             Available tools: {:?}",
                            self.tool_registry.available_tools()
                        ))
                    })?;
                    builder = builder.tool(tool);
                }
                ToolReference::Mcp { mcp } => {
                    // MCP tool references are logged but not resolved at load time.
                    // They require runtime MCP connection setup which is handled
                    // by the server infrastructure.
                    debug!(
                        "MCP tool reference in '{path_display}': endpoint='{}', args={:?}",
                        mcp.endpoint, mcp.args
                    );
                    warn!(
                        "MCP tool references in YAML definitions are not yet resolved at load time. \
                         Endpoint '{}' in '{path_display}' will be skipped.",
                        mcp.endpoint
                    );
                }
            }
        }

        // Resolve sub-agent references
        for sub_ref in &def.sub_agents {
            let loaded = self.loaded_agents.read().await;
            let sub_agent = loaded.get(&sub_ref.reference).cloned().ok_or_else(|| {
                let available: Vec<String> = loaded.keys().cloned().collect();
                adk_core::AdkError::config(format!(
                    "unresolved sub-agent reference '{}' in '{path_display}'. \
                     Loaded agents: {available:?}",
                    sub_ref.reference,
                ))
            })?;
            builder = builder.sub_agent(sub_agent);
        }

        let agent = builder.build().map_err(|e| {
            adk_core::AdkError::config(format!(
                "failed to build agent '{}' from '{path_display}': {e}",
                def.name
            ))
        })?;

        info!("built agent '{}' from '{path_display}'", def.name);
        Ok(Arc::new(agent))
    }
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
        if path.is_file() {
            if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if ext == "yaml" || ext == "yml" {
                    files.push(path);
                }
            }
        }
    }

    // Sort for deterministic loading order
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{AdkError, Llm, LlmRequest, Tool, ToolContext};
    use serde_json::Value;
    use std::io::Write;

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

    struct FailingModelFactory {
        fail_provider: String,
    }

    #[async_trait]
    impl ModelFactory for FailingModelFactory {
        async fn create_model(
            &self,
            provider: &str,
            _model_id: &str,
        ) -> adk_core::Result<Arc<dyn Llm>> {
            if provider == self.fail_provider {
                Err(AdkError::config(format!("unknown provider '{provider}'")))
            } else {
                Ok(Arc::new(MockLlm { name: format!("{provider}/{_model_id}") }))
            }
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

    struct MockToolRegistry {
        tools: HashMap<String, Arc<dyn Tool>>,
    }

    impl MockToolRegistry {
        fn new() -> Self {
            Self { tools: HashMap::new() }
        }

        fn with_tool(mut self, name: &str) -> Self {
            self.tools.insert(name.to_string(), Arc::new(MockTool { name: name.to_string() }));
            self
        }
    }

    impl ToolRegistry for MockToolRegistry {
        fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
            self.tools.get(tool_name).cloned()
        }

        fn available_tools(&self) -> Vec<String> {
            self.tools.keys().cloned().collect()
        }
    }

    // --- Mock Tool ---

    struct MockTool {
        name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "mock tool"
        }

        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    // --- Helper to write temp YAML files ---

    fn write_temp_yaml(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let path = dir.join(filename);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    // --- Tests ---

    #[tokio::test]
    async fn test_load_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test_agent
description: "A test agent"
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "You are a helpful assistant."
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agent = loader.load_file(&path).await.unwrap();
        assert_eq!(agent.name(), "test_agent");
        assert_eq!(agent.description(), "A test agent");
    }

    #[tokio::test]
    async fn test_load_file_with_tools() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: tool_agent
model:
  provider: openai
  model_id: gpt-4
tools:
  - name: get_weather
  - name: search
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry =
            Arc::new(MockToolRegistry::new().with_tool("get_weather").with_tool("search"));
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agent = loader.load_file(&path).await.unwrap();
        assert_eq!(agent.name(), "tool_agent");
    }

    #[tokio::test]
    async fn test_load_file_unresolved_tool() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: bad_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
tools:
  - name: nonexistent_tool
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_file(&path).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("nonexistent_tool"), "error should mention the tool name: {msg}");
        assert!(msg.contains("unresolved tool reference"), "error should be descriptive: {msg}");
    }

    #[tokio::test]
    async fn test_load_file_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: 123\nmodel: not_a_map\n";
        let path = write_temp_yaml(dir.path(), "bad.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_file(&path).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("invalid YAML"), "error should mention YAML: {msg}");
    }

    #[tokio::test]
    async fn test_load_file_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: ""
model:
  provider: gemini
  model_id: gemini-2.0-flash
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_file(&path).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("name"), "error should mention the field: {msg}");
    }

    #[tokio::test]
    async fn test_load_directory_basic() {
        let dir = tempfile::tempdir().unwrap();

        write_temp_yaml(
            dir.path(),
            "agent_a.yaml",
            r#"
name: agent_a
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Agent A"
"#,
        );

        write_temp_yaml(
            dir.path(),
            "agent_b.yml",
            r#"
name: agent_b
model:
  provider: openai
  model_id: gpt-4
instructions: "Agent B"
"#,
        );

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agents = loader.load_directory(dir.path()).await.unwrap();
        assert_eq!(agents.len(), 2);

        let names: Vec<&str> = agents.iter().map(|a| a.name()).collect();
        assert!(names.contains(&"agent_a"));
        assert!(names.contains(&"agent_b"));
    }

    #[tokio::test]
    async fn test_load_directory_with_sub_agents() {
        let dir = tempfile::tempdir().unwrap();

        // agent_b depends on agent_a as a sub-agent
        write_temp_yaml(
            dir.path(),
            "01_agent_a.yaml",
            r#"
name: researcher
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Research agent"
"#,
        );

        write_temp_yaml(
            dir.path(),
            "02_agent_b.yaml",
            r#"
name: orchestrator
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Orchestrator agent"
sub_agents:
  - ref: researcher
"#,
        );

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agents = loader.load_directory(dir.path()).await.unwrap();
        assert_eq!(agents.len(), 2);

        // The orchestrator should have the researcher as a sub-agent
        let orchestrator = agents.iter().find(|a| a.name() == "orchestrator").unwrap();
        assert_eq!(orchestrator.sub_agents().len(), 1);
        assert_eq!(orchestrator.sub_agents()[0].name(), "researcher");
    }

    #[tokio::test]
    async fn test_load_directory_unresolved_sub_agent() {
        let dir = tempfile::tempdir().unwrap();

        write_temp_yaml(
            dir.path(),
            "agent.yaml",
            r#"
name: orchestrator
model:
  provider: gemini
  model_id: gemini-2.0-flash
sub_agents:
  - ref: nonexistent_agent
"#,
        );

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_directory(dir.path()).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent_agent"),
            "error should mention the unresolved agent: {msg}"
        );
    }

    #[tokio::test]
    async fn test_load_directory_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();

        write_temp_yaml(
            dir.path(),
            "agent_a.yaml",
            r#"
name: duplicate
model:
  provider: gemini
  model_id: gemini-2.0-flash
"#,
        );

        write_temp_yaml(
            dir.path(),
            "agent_b.yaml",
            r#"
name: duplicate
model:
  provider: openai
  model_id: gpt-4
"#,
        );

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_directory(dir.path()).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("duplicate"), "error should mention duplicate name: {msg}");
    }

    #[tokio::test]
    async fn test_load_directory_empty() {
        let dir = tempfile::tempdir().unwrap();

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agents = loader.load_directory(dir.path()).await.unwrap();
        assert!(agents.is_empty());
    }

    #[tokio::test]
    async fn test_load_directory_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_directory(&file_path).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("not a directory"), "error should mention not a directory: {msg}");
    }

    #[tokio::test]
    async fn test_reload_file() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: reloadable
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Version 1"
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agent = loader.load_file(&path).await.unwrap();
        assert_eq!(agent.name(), "reloadable");

        // Update the file
        let yaml_v2 = r#"
name: reloadable
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: "Version 2"
"#;
        std::fs::write(&path, yaml_v2).unwrap();

        let agent_v2 = loader.reload_file(&path).await.unwrap();
        assert_eq!(agent_v2.name(), "reloadable");

        // The cached agent should be updated
        let cached = loader.get_agent("reloadable").await.unwrap();
        assert!(Arc::ptr_eq(&agent_v2, &cached));
    }

    #[tokio::test]
    async fn test_load_file_with_model_config() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: configured_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
  temperature: 0.7
  max_tokens: 4096
instructions: "You are helpful."
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let agent = loader.load_file(&path).await.unwrap();
        assert_eq!(agent.name(), "configured_agent");
    }

    #[tokio::test]
    async fn test_load_file_model_factory_error() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: bad_model_agent
model:
  provider: unknown_provider
  model_id: some-model
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory =
            Arc::new(FailingModelFactory { fail_provider: "unknown_provider".to_string() });
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader.load_file(&path).await.err().expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("unknown_provider"), "error should mention the provider: {msg}");
    }

    #[tokio::test]
    async fn test_load_file_with_mcp_tool_reference() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: mcp_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
tools:
  - mcp:
      endpoint: "npx @modelcontextprotocol/server-filesystem"
      args: ["/data"]
"#;
        let path = write_temp_yaml(dir.path(), "agent.yaml", yaml);

        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        // MCP references are logged but not resolved — agent should still load
        let agent = loader.load_file(&path).await.unwrap();
        assert_eq!(agent.name(), "mcp_agent");
    }

    #[tokio::test]
    async fn test_load_file_nonexistent() {
        let registry = Arc::new(MockToolRegistry::new());
        let factory = Arc::new(MockModelFactory);
        let loader = AgentConfigLoader::new(registry, factory);

        let err = loader
            .load_file(Path::new("/nonexistent/agent.yaml"))
            .await
            .err()
            .expect("expected error");
        let msg = err.to_string();
        assert!(msg.contains("failed to read"), "error should mention read failure: {msg}");
    }
}
