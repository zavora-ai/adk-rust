use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
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

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "artifact_names": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "List of artifact names to load"
                }
            },
            "required": ["artifact_names"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let artifact_service = ctx
            .artifacts()
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
                        adk_core::Part::Text { text } => json!({
                            "type": "text",
                            "text": text,
                        }),
                        adk_core::Part::InlineData { mime_type, data } => {
                            // Base64 encode binary data for JSON transport
                            let encoded = STANDARD.encode(&data);
                            json!({
                                "type": "inline_data",
                                "mime_type": mime_type,
                                "data_base64": encoded,
                                "size_bytes": data.len(),
                            })
                        }
                        adk_core::Part::CodeExecutionResult { code_execution_result } => json!({
                            "type": "code_execution_result",
                            "outcome": code_execution_result.outcome,
                            "output": code_execution_result.output,
                        }),
                        _ => json!({ "type": "unknown" }),
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
