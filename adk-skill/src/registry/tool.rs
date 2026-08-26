//! Agent tool for semantic skill discovery against the Skill Registry.

use crate::registry::client::SkillRegistryClient;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    top_k: Option<u32>,
}

/// Read-only agent tool that searches the Vertex AI Skill Registry.
///
/// Wraps [`SkillRegistryClient::search_skills`] (`skills:retrieve`): semantic
/// search over skill display names and descriptions, ranked by array order.
/// The registry returns no scores, versions, or revision names on search
/// results, so each hit carries the skill's bare ID, full resource name, and
/// description.
///
/// The tool is read-only and concurrency-safe, so it participates in
/// parallel tool dispatch.
///
/// # Example
///
/// ```no_run
/// use adk_skill::registry::{SkillRegistryClient, SkillRegistryConfig, SkillSearchTool};
/// use std::sync::Arc;
///
/// # fn main() -> adk_core::Result<()> {
/// let config = SkillRegistryConfig::new("my-project", "us-central1");
/// let client = SkillRegistryClient::new_with_adc(config)?;
/// let tool = SkillSearchTool::new(Arc::new(client));
/// # let _ = tool;
/// # Ok(())
/// # }
/// ```
pub struct SkillSearchTool {
    client: Arc<SkillRegistryClient>,
}

impl SkillSearchTool {
    /// Creates the tool over a Skill Registry client.
    pub fn new(client: Arc<SkillRegistryClient>) -> Self {
        Self { client }
    }
}

impl std::fmt::Debug for SkillSearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillSearchTool")
            .field("parent", &self.client.parent_resource_name())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for SkillSearchTool {
    fn name(&self) -> &str {
        "search_skills"
    }

    fn description(&self) -> &str {
        "Semantically search the organization's Skill Registry for reusable agent skills. \
         Returns matching skills (best match first) with their name and description."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language description of the capability to find",
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of results (default 10, max 100)",
                    "minimum": 1,
                    "maximum": 100,
                },
            },
            "required": ["query"],
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let args: SearchArgs = serde_json::from_value(args).map_err(|error| {
            self.client.error_context().invalid_input(format!(
                "invalid search_skills arguments: {error}. Pass {{\"query\": string, \"top_k\"?: integer}}",
            ))
        })?;
        let results = self.client.search_skills(&args.query, args.top_k).await?;
        tracing::debug!(
            skill_search.query = %args.query,
            skill_search.results = results.len(),
            "skill search tool completed"
        );
        let results: Vec<Value> = results
            .into_iter()
            .map(|skill| {
                let name =
                    skill.skill_name.rsplit('/').next().unwrap_or(&skill.skill_name).to_string();
                json!({
                    "name": name,
                    "skillName": skill.skill_name,
                    "description": skill.description,
                })
            })
            .collect();
        Ok(Value::Array(results))
    }
}
