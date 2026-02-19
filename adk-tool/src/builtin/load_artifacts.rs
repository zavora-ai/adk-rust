use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use base64::Engine as _;
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
                            // Base64 encode binary data for JSON transport.
                            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                            json!({
                                "type": "inline_data",
                                "mime_type": mime_type,
                                "data_base64": encoded,
                                "size_bytes": data.len(),
                            })
                        }
                        adk_core::Part::InlineDataBase64 { mime_type, data_base64 } => {
                            // Preserve canonical base64 payload and avoid decode/re-encode.
                            let size_bytes = base64::engine::general_purpose::STANDARD
                                .decode(&data_base64)
                                .map_or(0, |decoded| decoded.len());
                            json!({
                                "type": "inline_data",
                                "mime_type": mime_type,
                                "data_base64": data_base64,
                                "size_bytes": size_bytes,
                            })
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext,
        ToolContext,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    struct TestArtifacts {
        items: HashMap<String, adk_core::Part>,
    }

    #[async_trait]
    impl Artifacts for TestArtifacts {
        async fn save(&self, _name: &str, _data: &adk_core::Part) -> adk_core::Result<i64> {
            Ok(1)
        }

        async fn load(&self, name: &str) -> adk_core::Result<adk_core::Part> {
            self.items.get(name).cloned().ok_or_else(|| AdkError::Artifact("not found".to_string()))
        }

        async fn list(&self) -> adk_core::Result<Vec<String>> {
            Ok(self.items.keys().cloned().collect())
        }
    }

    struct TestToolContext {
        content: Content,
        artifacts: Arc<TestArtifacts>,
        actions: Mutex<EventActions>,
    }

    impl TestToolContext {
        fn new(part: adk_core::Part) -> Self {
            let mut items = HashMap::new();
            items.insert("doc".to_string(), part);
            Self {
                content: Content::new("user"),
                artifacts: Arc::new(TestArtifacts { items }),
                actions: Mutex::new(EventActions::default()),
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestToolContext {
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
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for TestToolContext {
        fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
            Some(self.artifacts.clone())
        }
    }

    #[async_trait]
    impl ToolContext for TestToolContext {
        fn function_call_id(&self) -> &str {
            "call-123"
        }

        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }

        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }

        async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn execute_preserves_inline_data_base64_payload() {
        let tool = LoadArtifactsTool::new();
        let ctx = Arc::new(TestToolContext::new(adk_core::Part::InlineDataBase64 {
            mime_type: "application/pdf".to_string(),
            data_base64: "JVBERi0=".to_string(),
        })) as Arc<dyn ToolContext>;

        let output = tool
            .execute(ctx, json!({ "artifact_names": ["doc"] }))
            .await
            .expect("tool execution should succeed");

        assert_eq!(output["artifacts"][0]["content"]["data_base64"].as_str(), Some("JVBERi0="));
    }
}
