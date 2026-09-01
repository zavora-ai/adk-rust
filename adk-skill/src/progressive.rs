//! Google ADK-style progressive disclosure for skills.
//!
//! [`SkillToolset`] exposes a small, always-available tool surface:
//!
//! - **L1** [`list_skills`](ListSkillsTool): names and descriptions only.
//! - **L2** [`load_skill`](LoadSkillTool): instructions and frontmatter for one skill.
//! - **L3** [`load_skill_resource`](LoadSkillResourceTool): a single bundled resource.
//!
//! Loading a skill records its content ID and hash in agent-scoped session state.
//! Business tools declared by `allowed-tools` become visible only after that
//! activation is persisted and the runtime begins the next model turn. This is
//! intentionally compatible with Google ADK's `SkillToolset`, while retaining
//! ADK-Rust's strict [`ValidationMode`] and content-hash provenance guarantees.

use crate::{SkillDocument, SkillIndex, SkillSummary, ToolRegistry, ValidationMode};
use adk_core::{AdkError, ReadonlyContext, Result, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "vertex-skill-registry")]
use crate::registry::{SkillContent, SkillRegistryClient, SkillSearchTool};
#[cfg(feature = "vertex-skill-registry")]
use std::collections::{BTreeMap, HashMap};
#[cfg(feature = "vertex-skill-registry")]
use tokio::sync::RwLock;

const STATE_KEY_PREFIX: &str = "skill:active:";
#[cfg(feature = "vertex-skill-registry")]
const MAX_REMOTE_CACHE_INVOCATIONS: usize = 16;

/// Rules for reading bundled skill resources.
///
/// [`ActivatedOnly`](Self::ActivatedOnly) is the secure default: a model must
/// first receive the skill's instructions before it can inspect its auxiliary
/// files. [`GoogleCompatible`](Self::GoogleCompatible) exists for applications
/// that need the more permissive behavior of Google ADK's current toolset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResourceAccessPolicy {
    /// Require an earlier successful `load_skill` call for this agent.
    #[default]
    ActivatedOnly,
    /// Permit direct reads of a known skill, matching Google ADK's current behavior.
    GoogleCompatible,
}

/// Serialized activation provenance retained in agent-scoped session state.
///
/// The content hash pins an activation to the exact skill revision the model
/// loaded. When a skill is edited or removed, [`SkillToolset`] discards the
/// stale record rather than silently retaining its tools or resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivatedSkill {
    /// Content-addressed document ID.
    pub skill_id: String,
    /// Canonical skill name.
    pub name: String,
    /// Content hash captured at activation time.
    pub hash: String,
    /// Source-specific address used to reload the skill.
    ///
    /// Local skills use their canonical name. Registry-backed skills retain
    /// their fully-qualified resource name so a frontmatter name change does
    /// not prevent a later model turn from validating the activation. Older
    /// serialized activations omit this field and fall back to [`Self::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Configuration for [`SkillToolset`].
///
/// Defaults favor safe, bounded disclosure: resources require activation,
/// missing declared tools reject an activation, and instructions/resources are
/// character-limited before being returned to the model.
///
/// # Example
///
/// ```rust
/// use adk_skill::{ResourceAccessPolicy, SkillToolsetConfig};
///
/// let config = SkillToolsetConfig {
///     resource_access: ResourceAccessPolicy::ActivatedOnly,
///     max_instruction_chars: 4_000,
///     ..SkillToolsetConfig::default()
/// };
/// assert_eq!(config.max_instruction_chars, 4_000);
/// ```
#[derive(Debug, Clone)]
pub struct SkillToolsetConfig {
    /// Whether resources require activation first.
    pub resource_access: ResourceAccessPolicy,
    /// Reject or tolerate skills whose declared business tools cannot be resolved.
    pub validation_mode: ValidationMode,
    /// Maximum number of active skills for one agent.
    pub max_active_skills: usize,
    /// Maximum number of characters returned by `load_skill`.
    pub max_instruction_chars: usize,
    /// Maximum number of characters returned by `load_skill_resource`.
    pub max_resource_chars: usize,
}

impl Default for SkillToolsetConfig {
    fn default() -> Self {
        Self {
            resource_access: ResourceAccessPolicy::ActivatedOnly,
            validation_mode: ValidationMode::Strict,
            max_active_skills: 8,
            max_instruction_chars: 8_000,
            max_resource_chars: 16_000,
        }
    }
}

/// A context-aware toolset implementing L1/L2/L3 progressive disclosure.
///
/// Register this type with an [`adk_core::Toolset`] consumer, such as an
/// `LlmAgentBuilder::toolset` integration. It always contributes the three
/// discovery/loading tools. After `load_skill` succeeds, a subsequent call to
/// [`Toolset::tools`] additionally returns that skill's validated
/// `allowed-tools` from the supplied [`ToolRegistry`].
///
/// This is opt-in; it does not alter the behavior of [`crate::SkillInjector`]
/// or [`crate::ContextCoordinator`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_agent::LlmAgent;
/// use adk_skill::{SkillToolset, SkillToolsetConfig, load_skill_index};
/// use std::sync::Arc;
///
/// let index = Arc::new(load_skill_index(".")?);
/// let skills = SkillToolset::new(index, Arc::new(tool_registry), SkillToolsetConfig::default());
/// let agent = LlmAgent::builder().toolset(Arc::new(skills)).build()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
pub struct SkillToolset {
    index: Arc<SkillIndex>,
    tool_registry: Arc<dyn ToolRegistry>,
    config: SkillToolsetConfig,
    #[cfg(feature = "vertex-skill-registry")]
    registry_client: Option<Arc<SkillRegistryClient>>,
    #[cfg(feature = "vertex-skill-registry")]
    remote_cache: SharedRemoteSkillCache,
}

#[cfg(feature = "vertex-skill-registry")]
#[derive(Clone)]
struct RemoteSkill {
    document: SkillDocument,
    files: BTreeMap<String, Vec<u8>>,
    locator: String,
}

#[cfg(feature = "vertex-skill-registry")]
type RemoteSkillCache = HashMap<String, HashMap<String, Arc<RemoteSkill>>>;
#[cfg(feature = "vertex-skill-registry")]
type SharedRemoteSkillCache = Arc<RwLock<RemoteSkillCache>>;

#[derive(Clone)]
enum LoadedSkill {
    Local(Arc<SkillDocument>),
    #[cfg(feature = "vertex-skill-registry")]
    Remote(Arc<RemoteSkill>),
}

impl LoadedSkill {
    fn document(&self) -> &SkillDocument {
        match self {
            Self::Local(document) => document,
            #[cfg(feature = "vertex-skill-registry")]
            Self::Remote(skill) => &skill.document,
        }
    }

    fn locator(&self) -> &str {
        match self {
            Self::Local(document) => &document.name,
            #[cfg(feature = "vertex-skill-registry")]
            Self::Remote(skill) => &skill.locator,
        }
    }

    fn resource_content(&self, requested: &str) -> Result<String> {
        match self {
            Self::Local(skill) => {
                let resource = SkillToolset::resource_path(skill, requested)?;
                fs::read_to_string(&resource).map_err(|error| {
                    AdkError::tool(format!("read skill resource '{requested}': {error}"))
                })
            }
            #[cfg(feature = "vertex-skill-registry")]
            Self::Remote(skill) => {
                SkillToolset::validate_resource_path(requested)?;
                let bytes = skill.files.get(requested).ok_or_else(|| {
                    AdkError::tool(format!(
                        "resource '{requested}' was not found in skill '{}'",
                        skill.document.name
                    ))
                })?;
                String::from_utf8(bytes.clone()).map_err(|_| {
                    AdkError::tool(format!(
                        "resource '{requested}' is binary and cannot be returned as text"
                    ))
                })
            }
        }
    }
}

impl SkillToolset {
    /// Creates a progressive skill toolset from an existing index.
    ///
    /// `tool_registry` is consulted at skill activation time and on each later
    /// model turn. This permits a host to enforce availability, authorization,
    /// or tenant-specific tool policies without placing provider logic in this
    /// crate.
    pub fn new(
        index: Arc<SkillIndex>,
        tool_registry: Arc<dyn ToolRegistry>,
        config: SkillToolsetConfig,
    ) -> Self {
        Self {
            index,
            tool_registry,
            config,
            #[cfg(feature = "vertex-skill-registry")]
            registry_client: None,
            #[cfg(feature = "vertex-skill-registry")]
            remote_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Enables lazy loading and semantic discovery from Vertex Skill Registry.
    ///
    /// Remote packages are fetched only when the model calls `load_skill` or
    /// `load_skill_resource`. Each verified package is cached per invocation;
    /// the cache is discarded with the toolset, not persisted into sessions.
    /// It also adds the registry's `search_skills` tool to the L1 surface.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_skill::{SkillToolset, registry::SkillRegistryClient};
    /// use std::sync::Arc;
    ///
    /// let toolset = SkillToolset::new(index, tool_registry, config)
    ///     .with_registry_client(Arc::new(SkillRegistryClient::new(registry_config)?));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "vertex-skill-registry")]
    #[must_use]
    pub fn with_registry_client(mut self, client: Arc<SkillRegistryClient>) -> Self {
        self.registry_client = Some(client);
        self
    }

    /// Returns the state key used to persist activations for an agent.
    ///
    /// This is public for hosts that need to inspect, migrate, or clear an
    /// activation explicitly. The agent name is part of the key so transferred
    /// or nested agents do not inherit another agent's selected skills.
    pub fn activation_state_key(agent_name: &str) -> String {
        format!("{STATE_KEY_PREFIX}{agent_name}")
    }

    fn core_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ListSkillsTool { toolset: self.clone() }),
            Arc::new(LoadSkillTool { toolset: self.clone() }),
            Arc::new(LoadSkillResourceTool { toolset: self.clone() }),
        ]
    }

    fn summaries(&self) -> Vec<SkillSummary> {
        self.index.summaries()
    }

    async fn load_skill(&self, ctx: &dyn ReadonlyContext, name: &str) -> Result<LoadedSkill> {
        if let Some(skill) = self.index.find_by_name(name) {
            return Ok(LoadedSkill::Local(Arc::new(skill.clone())));
        }

        #[cfg(feature = "vertex-skill-registry")]
        if let Some(client) = &self.registry_client {
            return Ok(LoadedSkill::Remote(
                self.fetch_remote_skill(client, ctx.invocation_id(), name).await?,
            ));
        }

        #[cfg(not(feature = "vertex-skill-registry"))]
        let _ = ctx;

        Err(AdkError::tool(format!("skill '{name}' was not found")))
    }

    #[cfg(feature = "vertex-skill-registry")]
    async fn fetch_remote_skill(
        &self,
        client: &SkillRegistryClient,
        invocation_id: &str,
        name: &str,
    ) -> Result<Arc<RemoteSkill>> {
        if let Some(skill) = self
            .remote_cache
            .read()
            .await
            .get(invocation_id)
            .and_then(|skills| skills.get(name))
            .cloned()
        {
            return Ok(skill);
        }

        let content = client.fetch_skill_content(name).await?;
        let remote = Arc::new(Self::remote_skill_from_content(content)?);
        let mut cache = self.remote_cache.write().await;
        // Keep cache retention bounded even when a long-lived toolset serves
        // many invocations. Each inner map may contain aliases for one package;
        // eviction order is irrelevant because invocation caches are advisory.
        if !cache.contains_key(invocation_id)
            && cache.len() >= MAX_REMOTE_CACHE_INVOCATIONS
            && let Some(evicted) = cache.keys().next().cloned()
        {
            cache.remove(&evicted);
        }
        let entries = cache.entry(invocation_id.to_string()).or_default();
        entries.insert(name.to_string(), remote.clone());
        entries.insert(remote.document.name.clone(), remote.clone());
        Ok(remote)
    }

    #[cfg(feature = "vertex-skill-registry")]
    fn remote_skill_from_content(content: SkillContent) -> Result<RemoteSkill> {
        let document = crate::registry::load::document_from_content(&content)
            .map_err(adk_core::AdkError::from)?;
        let locator = content.skill.name;
        Ok(RemoteSkill { document, files: content.files, locator })
    }

    fn activated_skills(&self, ctx: &dyn ReadonlyContext) -> Vec<ActivatedSkill> {
        let key = Self::activation_state_key(ctx.agent_name());
        ctx.state()
            .and_then(|state| state.get(&key).and_then(|value| serde_json::from_value(value).ok()))
            .unwrap_or_default()
    }

    async fn valid_activated_skills(&self, ctx: &dyn ReadonlyContext) -> Vec<LoadedSkill> {
        let mut valid = Vec::new();
        for activation in self.activated_skills(ctx) {
            let source = activation.source.as_deref().unwrap_or(&activation.name);
            let Ok(skill) = self.load_skill(ctx, source).await else {
                continue;
            };
            let document = skill.document();
            if document.id == activation.skill_id && document.hash == activation.hash {
                valid.push(skill);
            }
        }
        valid
    }

    async fn activate(&self, ctx: &dyn ToolContext, skill: &LoadedSkill) -> Result<()> {
        let document = skill.document();
        let mut active = self
            .valid_activated_skills(ctx)
            .await
            .into_iter()
            .map(|skill| ActivatedSkill {
                skill_id: skill.document().id.clone(),
                name: skill.document().name.clone(),
                hash: skill.document().hash.clone(),
                source: Some(skill.locator().to_string()),
            })
            .collect::<Vec<_>>();
        if !active.iter().any(|item| item.skill_id == document.id) {
            if active.len() >= self.config.max_active_skills {
                return Err(AdkError::tool(format!(
                    "cannot activate skill '{}': the {}-skill limit has been reached",
                    document.name, self.config.max_active_skills
                )));
            }
            active.push(ActivatedSkill {
                skill_id: document.id.clone(),
                name: document.name.clone(),
                hash: document.hash.clone(),
                source: Some(skill.locator().to_string()),
            });
        }

        let mut actions = ctx.actions();
        actions.state_delta.insert(
            Self::activation_state_key(ctx.agent_name()),
            serde_json::to_value(active)
                .map_err(|error| AdkError::tool(format!("serialize skill activation: {error}")))?,
        );
        ctx.set_actions(actions);
        Ok(())
    }

    fn validate_skill_tools(&self, skill: &SkillDocument) -> Result<()> {
        if self.config.validation_mode != ValidationMode::Strict {
            return Ok(());
        }
        let missing = skill
            .allowed_tools
            .iter()
            .filter(|name| self.tool_registry.resolve(name).is_none())
            .cloned()
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(AdkError::tool(format!(
                "skill '{}' requires unavailable tools: {}",
                skill.name,
                missing.join(", ")
            )))
        }
    }

    fn resource_path(skill: &SkillDocument, requested: &str) -> Result<PathBuf> {
        let relative = Self::validate_resource_path(requested)?;
        let root = skill.path.parent().ok_or_else(|| {
            AdkError::tool(format!("skill '{}' has no parent resource directory", skill.name))
        })?;
        let root = fs::canonicalize(root)
            .map_err(|error| AdkError::tool(format!("resolve skill resource root: {error}")))?;
        let candidate = fs::canonicalize(root.join(relative)).map_err(|error| {
            AdkError::tool(format!("resource '{requested}' could not be resolved: {error}"))
        })?;
        if !candidate.starts_with(&root) {
            return Err(AdkError::tool("resource path escapes the skill directory"));
        }
        Ok(candidate)
    }

    fn validate_resource_path(requested: &str) -> Result<&Path> {
        let relative = Path::new(requested);
        // Check lexical components before filesystem resolution. Canonicalization
        // below then catches symlinks that would otherwise escape this package.
        if !matches!(
            relative.components().next(),
            Some(Component::Normal(prefix))
                if prefix == "references" || prefix == "assets" || prefix == "scripts"
        ) || relative.is_absolute()
            || relative.components().any(|component| matches!(component, Component::ParentDir))
        {
            return Err(AdkError::tool(
                "resource path must be a relative path under references/, assets/, or scripts/",
            ));
        }
        Ok(relative)
    }

    fn truncate(value: String, limit: usize) -> String {
        if value.chars().count() <= limit {
            return value;
        }
        let mut truncated = value.chars().take(limit).collect::<String>();
        truncated.push_str("\n[... truncated]");
        truncated
    }

    async fn resolve_business_tools(
        &self,
        ctx: &dyn ReadonlyContext,
    ) -> Result<Vec<Arc<dyn Tool>>> {
        let mut names = HashSet::new();
        let mut tools = Vec::new();
        let mut missing = Vec::new();

        for active in self.valid_activated_skills(ctx).await {
            for name in &active.document().allowed_tools {
                if !names.insert(name.clone()) {
                    continue;
                }
                match self.tool_registry.resolve(name) {
                    Some(tool) => tools.push(tool),
                    None => missing.push(name.clone()),
                }
            }
        }

        if !missing.is_empty() && self.config.validation_mode == ValidationMode::Strict {
            return Err(AdkError::tool(format!(
                "activated skills require unavailable tools: {}",
                missing.join(", ")
            )));
        }
        if !missing.is_empty() {
            tracing::warn!(missing_tools = ?missing, "omitting unavailable skill tools");
        }
        Ok(tools)
    }
}

#[async_trait]
impl Toolset for SkillToolset {
    fn name(&self) -> &str {
        "skills"
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let mut tools = self.core_tools();
        #[cfg(feature = "vertex-skill-registry")]
        if let Some(client) = &self.registry_client {
            tools.push(Arc::new(SkillSearchTool::new(client.clone())));
        }
        tools.extend(self.resolve_business_tools(ctx.as_ref()).await?);
        Ok(tools)
    }
}

struct ListSkillsTool {
    toolset: SkillToolset,
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }
    fn description(&self) -> &str {
        "Lists available skills with their names and descriptions."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn is_concurrency_safe(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({"type": "object", "properties": {}}))
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        serde_json::to_value(self.toolset.summaries())
            .map_err(|error| AdkError::tool(format!("serialize skill catalog: {error}")))
    }
}

struct LoadSkillTool {
    toolset: SkillToolset,
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }
    fn description(&self) -> &str {
        "Loads the SKILL.md instructions for a given skill."
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({"type":"object","properties":{"skill_name":{"type":"string"}},"required":["skill_name"]}),
        )
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let name = args
            .get("skill_name")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("missing required string argument 'skill_name'"))?;
        let skill = self.toolset.load_skill(ctx.as_ref(), name).await?;
        let document = skill.document();
        self.toolset.validate_skill_tools(document)?;
        self.toolset.activate(ctx.as_ref(), &skill).await?;
        Ok(json!({
            "skillName": document.name,
            "skillId": document.id,
            "hash": document.hash,
            "instructions": SkillToolset::truncate(document.body.clone(), self.toolset.config.max_instruction_chars),
            "frontmatter": SkillSummary::from(document),
        }))
    }
}

struct LoadSkillResourceTool {
    toolset: SkillToolset,
}

#[async_trait]
impl Tool for LoadSkillResourceTool {
    fn name(&self) -> &str {
        "load_skill_resource"
    }
    fn description(&self) -> &str {
        "Loads a resource file from references/, assets/, or scripts/ within a skill."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({"type":"object","properties":{"skill_name":{"type":"string"},"file_path":{"type":"string"}},"required":["skill_name","file_path"]}),
        )
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let name = args
            .get("skill_name")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("missing required string argument 'skill_name'"))?;
        let path = args
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("missing required string argument 'file_path'"))?;
        let skill = self.toolset.load_skill(ctx.as_ref(), name).await?;
        let document = skill.document();
        if self.toolset.config.resource_access == ResourceAccessPolicy::ActivatedOnly
            && !self
                .toolset
                .valid_activated_skills(ctx.as_ref())
                .await
                .iter()
                .any(|item| item.document().id == document.id)
        {
            return Err(AdkError::tool(format!(
                "skill '{name}' must be loaded before reading its resources"
            )));
        }
        let content = skill.resource_content(path)?;
        Ok(json!({
            "skillName": document.name,
            "filePath": path,
            "content": SkillToolset::truncate(content, self.toolset.config.max_resource_chars),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Artifacts, CallbackContext, Content, EventActions, MemoryEntry, State};
    use async_trait::async_trait;
    use std::{collections::HashMap, sync::Mutex};

    #[derive(Default)]
    struct TestState {
        values: HashMap<String, Value>,
    }

    impl State for TestState {
        fn get(&self, key: &str) -> Option<Value> {
            self.values.get(key).cloned()
        }

        fn set(&mut self, key: String, value: Value) {
            self.values.insert(key, value);
        }

        fn all(&self) -> HashMap<String, Value> {
            self.values.clone()
        }
    }

    struct TestContext {
        state: TestState,
        actions: Mutex<EventActions>,
        user_content: Content,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                state: TestState::default(),
                actions: Mutex::new(EventActions::default()),
                user_content: Content::new("user").with_text("test"),
            }
        }

        fn with_state(key: String, value: Value) -> Self {
            let mut state = TestState::default();
            state.set(key, value);
            Self {
                state,
                actions: Mutex::new(EventActions::default()),
                user_content: Content::new("user").with_text("test"),
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn invocation_id(&self) -> &str {
            "invocation"
        }
        fn agent_name(&self) -> &str {
            "agent"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.user_content
        }
        fn state(&self) -> Option<&dyn State> {
            Some(&self.state)
        }
    }

    #[async_trait]
    impl CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for TestContext {
        fn function_call_id(&self) -> &str {
            "call"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().expect("actions lock").clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().expect("actions lock") = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
    }

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "weather_lookup"
        }
        fn description(&self) -> &str {
            "Looks up weather."
        }
        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    struct TestRegistry;

    impl ToolRegistry for TestRegistry {
        fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
            (tool_name == "weather_lookup").then(|| Arc::new(DummyTool) as Arc<dyn Tool>)
        }
    }

    fn setup() -> (tempfile::TempDir, SkillToolset) {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join(".skills/weather");
        fs::create_dir_all(skill_dir.join("references")).expect("skill directories");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: weather\ndescription: Answer weather questions\nallowed-tools: [weather_lookup]\n---\nLoad references/units.md before answering.\n",
        )
        .expect("skill file");
        fs::write(skill_dir.join("references/units.md"), "Use Celsius.").expect("reference");
        let index = Arc::new(crate::load_skill_index(temp.path()).expect("skill index"));
        (temp, SkillToolset::new(index, Arc::new(TestRegistry), SkillToolsetConfig::default()))
    }

    fn tool_by_name(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        tools.iter().find(|tool| tool.name() == name).expect("tool present").clone()
    }

    #[tokio::test]
    async fn loading_a_skill_activates_its_declared_tool_and_resource_access() {
        let (_temp, toolset) = setup();
        let first_context = Arc::new(TestContext::new());
        let initial_tools = toolset.tools(first_context.clone()).await.expect("initial tools");
        assert_eq!(initial_tools.len(), 3);
        assert!(!initial_tools.iter().any(|tool| tool.name() == "weather_lookup"));

        let load = tool_by_name(&initial_tools, "load_skill");
        let loaded = load
            .execute(first_context.clone(), json!({"skill_name": "weather"}))
            .await
            .expect("load skill");
        assert_eq!(loaded["skillName"], "weather");
        assert!(loaded["instructions"].as_str().expect("instructions").contains("units.md"));

        let state_key = SkillToolset::activation_state_key("agent");
        let activation =
            first_context.actions().state_delta.remove(&state_key).expect("activation state");
        let second_context = Arc::new(TestContext::with_state(state_key, activation));
        let activated_tools = toolset.tools(second_context.clone()).await.expect("activated tools");
        assert!(activated_tools.iter().any(|tool| tool.name() == "weather_lookup"));

        let resource = tool_by_name(&activated_tools, "load_skill_resource");
        let loaded_resource = resource
            .execute(
                second_context,
                json!({"skill_name": "weather", "file_path": "references/units.md"}),
            )
            .await
            .expect("load resource");
        assert_eq!(loaded_resource["content"], "Use Celsius.");
    }

    #[tokio::test]
    async fn resources_require_activation_and_reject_path_traversal() {
        let (_temp, toolset) = setup();
        let context = Arc::new(TestContext::new());
        let tools = toolset.tools(context.clone()).await.expect("tools");
        let resource = tool_by_name(&tools, "load_skill_resource");
        assert!(
            resource
                .execute(
                    context.clone(),
                    json!({"skill_name": "weather", "file_path": "references/units.md"})
                )
                .await
                .is_err()
        );

        let load = tool_by_name(&tools, "load_skill");
        load.execute(context.clone(), json!({"skill_name": "weather"})).await.expect("load skill");
        let state_key = SkillToolset::activation_state_key("agent");
        let activation =
            context.actions().state_delta.remove(&state_key).expect("activation state");
        let active_context = Arc::new(TestContext::with_state(state_key, activation));
        let error = resource
            .execute(
                active_context,
                json!({"skill_name": "weather", "file_path": "references/../SKILL.md"}),
            )
            .await
            .expect_err("traversal must fail");
        assert!(error.to_string().contains("resource path"));
    }
}
