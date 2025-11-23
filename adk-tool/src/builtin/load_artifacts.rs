use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct LoadArtifactsTool {
    name: String,
    description: String,
}

impl LoadArtifactsTool {
    pub fn new() -> Self {
        Self {
            name: "load_artifacts".to_string(),
            description: "Loads artifacts by name and returns their content. Accepts an array of artifact names.".to_string(),
        }
    }
}

impl Default for LoadArtifactsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LoadArtifactsTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn is_long_running(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let artifact_service = ctx.artifacts()
            .ok_or_else(|| AdkError::Tool("ArtifactService not available".to_string()))?;

        let artifact_names = args["artifact_names"]
            .as_array()
            .ok_or_else(|| AdkError::Tool("artifact_names must be an array".to_string()))?;

        let mut results = Vec::new();

        for name_value in artifact_names {
            let name = name_value
                .as_str()
                .ok_or_else(|| AdkError::Tool("artifact name must be a string".to_string()))?;

            match artifact_service.load(name).await {
                Ok(part) => {
                    let content = match part {
                        adk_core::Part::Text { text } => json!(text),
                        adk_core::Part::InlineData { mime_type, data } => json!({
                            "mime_type": mime_type,
                            "data": data,
                        }),
                        _ => json!(null),
                    };
                    
                    results.push(json!({
                        "name": name,
                        "content": content,
                    }));
                }
                Err(_) => {
                    results.push(json!({
                        "name": name,
                        "error": "Artifact not found",
                    }));
                }
            }
        }

        Ok(json!({
            "artifacts": results,
        }))
    }
}
