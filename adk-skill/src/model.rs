use adk_core::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Frontmatter metadata for a skill, following the `agentskills.io` specification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillFrontmatter {
    /// A required short identifier (1-64 chars) containing only lowercase letters, numbers, and hyphens.
    pub name: String,
    /// A required concise description of what the skill does and when an agent should use it.
    pub description: String,
    /// An optional version identifier for the skill (e.g., "1.0.0").
    pub version: Option<String>,
    /// An optional license identifier or reference to a bundled license file.
    pub license: Option<String>,
    /// Optional environment requirements (e.g., "Requires system packages, network access").
    pub compatibility: Option<String>,
    /// A collection of categorizing labels for discovery and filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Experimental: A list of space-delimited pre-approved tools the skill may use.
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Vec<String>,
    /// Optional list of paths to supporting resources (e.g., "references/data.json").
    #[serde(default)]
    pub references: Vec<String>,
    /// If true, the skill is only included when explicitly invoked by name.
    pub trigger: Option<bool>,
    /// Optional hint text displayed for slash command guided input.
    pub hint: Option<String>,
    /// Arbitrary key-value mapping for custom extension metadata.
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
}

/// A parsed skill before it is assigned an ID and indexed.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    /// The unique identifier from the frontmatter or filename.
    pub name: String,
    /// Description of the skill's purpose.
    pub description: String,
    /// Optional versioning string.
    pub version: Option<String>,
    /// Optional license information.
    pub license: Option<String>,
    /// Optional compatibility requirements.
    pub compatibility: Option<String>,
    /// Discovery tags.
    pub tags: Vec<String>,
    /// Pre-approved tool names.
    pub allowed_tools: Vec<String>,
    /// Supporting resource paths.
    pub references: Vec<String>,
    /// Whether the skill requires explicit invocation.
    pub trigger: bool,
    /// Guided input hint.
    pub hint: Option<String>,
    /// Extension metadata.
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
    /// The raw Markdown body content (instructions).
    pub body: String,
}

/// A fully indexed skill document with a content-based unique ID.
#[derive(Debug, Clone, Serialize)]
pub struct SkillDocument {
    /// A unique ID derived from the name and content hash.
    pub id: String,
    /// The canonical name of the skill.
    pub name: String,
    /// Description used for agent discovery.
    pub description: String,
    /// Semantic version.
    pub version: Option<String>,
    /// License tag.
    pub license: Option<String>,
    /// Environment constraints.
    pub compatibility: Option<String>,
    /// List of discovery tags.
    pub tags: Vec<String>,
    /// Tools allowed for this skill.
    pub allowed_tools: Vec<String>,
    /// External resources required by the skill.
    pub references: Vec<String>,
    /// If true, requires explicit `@name` invocation.
    pub trigger: bool,
    /// Input guidance for users.
    pub hint: Option<String>,
    /// Custom extension metadata.
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
    /// The instructional Markdown body.
    pub body: String,
    /// File system path where the skill was loaded from.
    pub path: PathBuf,
    /// SHA256 hash of the content.
    pub hash: String,
    /// Optional Unix timestamp of last file modification.
    pub last_modified: Option<i64>,
}

impl SkillDocument {
    /// Engineers a full system instruction from the skill body, with truncation
    /// and optionally including tool capability hints.
    pub fn engineer_instruction(&self, max_chars: usize, active_tools: &[Arc<dyn Tool>]) -> String {
        let mut body = self.body.clone();
        if body.chars().count() > max_chars {
            body = body.chars().take(max_chars).collect();
            body.push_str("\n[... truncated]");
        }

        let mut parts = Vec::new();
        parts.push(format!("[skill:{}]", self.name));
        parts.push(format!("# {}\n{}", self.name, self.description));

        // Tool capability hint (so the LLM knows what it can do)
        if !active_tools.is_empty() {
            let names: Vec<_> = active_tools.iter().map(|t: &Arc<dyn Tool>| t.name()).collect();
            parts.push(format!("You have access to the following tools: {}.", names.join(", ")));
        }

        parts.push(format!("## Instructions\n{}", body));
        parts.push("[/skill]".to_string());

        parts.join("\n\n")
    }

    /// Engineers a lightweight prompt block for Tier 1 injection.
    pub fn engineer_prompt_block(&self, max_chars: usize) -> String {
        let mut body = self.body.clone();
        if body.chars().count() > max_chars {
            body = body.chars().take(max_chars).collect();
        }
        format!("[skill:{}]\n{}\n[/skill]", self.name, body)
    }
}

/// A lightweight summary of a skill, excluding the heavy body content.
#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    /// Content-based unique ID.
    pub id: String,
    /// Skill name.
    pub name: String,
    /// Discovery description.
    pub description: String,
    /// Optional version.
    pub version: Option<String>,
    /// Optional license.
    pub license: Option<String>,
    /// Optional compatibility.
    pub compatibility: Option<String>,
    /// Discovery tags.
    pub tags: Vec<String>,
    /// Allowed tools.
    pub allowed_tools: Vec<String>,
    /// External references.
    pub references: Vec<String>,
    /// Trigger status.
    pub trigger: bool,
    /// Guided hint.
    pub hint: Option<String>,
    /// Extension metadata.
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
    /// Associated file path.
    pub path: PathBuf,
    /// Content signature.
    pub hash: String,
    /// Last modified timestamp.
    pub last_modified: Option<i64>,
}

impl From<&SkillDocument> for SkillSummary {
    fn from(value: &SkillDocument) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            description: value.description.clone(),
            version: value.version.clone(),
            license: value.license.clone(),
            compatibility: value.compatibility.clone(),
            tags: value.tags.clone(),
            allowed_tools: value.allowed_tools.clone(),
            references: value.references.clone(),
            trigger: value.trigger,
            hint: value.hint.clone(),
            metadata: value.metadata.clone(),
            path: value.path.clone(),
            hash: value.hash.clone(),
            last_modified: value.last_modified,
        }
    }
}

/// A collection of indexed skills, providing efficient access to metadata and summaries.
#[derive(Debug, Clone, Default)]
pub struct SkillIndex {
    skills: Vec<SkillDocument>,
}

impl SkillIndex {
    pub fn new(skills: Vec<SkillDocument>) -> Self {
        Self { skills }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Returns the raw list of fully indexed skill documents.
    pub fn skills(&self) -> &[SkillDocument] {
        &self.skills
    }

    /// Returns a list of lightweight skill summaries, suitable for passing to agents or UI components.
    pub fn summaries(&self) -> Vec<SkillSummary> {
        self.skills.iter().map(SkillSummary::from).collect()
    }

    /// Find a skill by its canonical name.
    pub fn find_by_name(&self, name: &str) -> Option<&SkillDocument> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Find a skill by its unique ID (name + hash).
    pub fn find_by_id(&self, id: &str) -> Option<&SkillDocument> {
        self.skills.iter().find(|s| s.id == id)
    }
}

/// Criteria used to filter and score skills during selection.
#[derive(Debug, Clone)]
pub struct SelectionPolicy {
    /// Number of top-scoring matches to return.
    pub top_k: usize,
    /// Minimum score threshold for a skill to be included.
    pub min_score: f32,
    /// Optional list of tags that MUST be present on the skill.
    pub include_tags: Vec<String>,
    /// Optional list of tags that MUST NOT be present on the skill.
    pub exclude_tags: Vec<String>,
}

impl Default for SelectionPolicy {
    fn default() -> Self {
        Self { top_k: 1, min_score: 1.0, include_tags: Vec::new(), exclude_tags: Vec::new() }
    }
}

/// A ranked result representing a skill that matched a selection query.
#[derive(Debug, Clone, Serialize)]
pub struct SkillMatch {
    /// Numerical relevance score calculated using weighted lexical overlap.
    ///
    /// The algorithm weights matches as follows:
    /// - **Name Match**: +4.0
    /// - **Description Match**: +2.5
    /// - **Tag Match**: +2.0
    /// - **Instruction Body Match**: +1.0
    ///
    /// The final score is normalized by `sqrt(unique_token_count)` of the body to
    /// ensure long-form instructions do not unfairly drown out concise skills.
    pub score: f32,
    /// The lightweight summary of the matched skill.
    pub skill: SkillSummary,
}
