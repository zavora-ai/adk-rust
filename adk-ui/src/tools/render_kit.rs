use crate::kit::{KitArtifacts, KitGenerator, KitSpec};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// Parameters for the render_kit tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderKitParams {
    #[serde(flatten)]
    pub spec: KitSpec,
    /// Optional output format; "json" (default) or "catalog_only"
    #[serde(default)]
    pub output: Option<String>,
}

/// Tool for generating a UI kit (catalog + tokens + templates + theme).
pub struct RenderKitTool;

impl RenderKitTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RenderKitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for RenderKitTool {
    fn name(&self) -> &str {
        "render_kit"
    }

    fn description(&self) -> &str {
        "Generate a UI kit from a KitSpec. Returns catalog, tokens, templates, and theme CSS."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(super::generate_gemini_schema::<RenderKitParams>())
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: RenderKitParams = serde_json::from_value(args.clone()).map_err(|e| {
            adk_core::AdkError::Tool(format!("Invalid parameters: {}. Got: {}", e, args))
        })?;

        let generator = KitGenerator::new();
        let artifacts = generator.generate(&params.spec);

        Ok(format_output(&artifacts, params.output.as_deref()))
    }
}

fn format_output(artifacts: &KitArtifacts, output: Option<&str>) -> Value {
    match output {
        Some("catalog_only") => artifacts.catalog.clone(),
        _ => json!({
            "catalog": artifacts.catalog,
            "tokens": artifacts.tokens,
            "templates": artifacts.templates,
            "theme_css": artifacts.theme_css,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, EventActions, ReadonlyContext};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct TestContext {
        content: Content,
        actions: Mutex<EventActions>,
    }

    impl TestContext {
        fn new() -> Self {
            Self { content: Content::new("user"), actions: Mutex::new(EventActions::default()) }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn invocation_id(&self) -> &str {
            "test"
        }
        fn agent_name(&self) -> &str {
            "test"
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
    impl adk_core::CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for TestContext {
        fn function_call_id(&self) -> &str {
            "call-123"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<adk_core::MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn render_kit_emits_catalog() {
        let tool = RenderKitTool::new();
        let args = serde_json::json!({
            "name": "Fintech Pro",
            "version": "0.1.0",
            "brand": { "vibe": "trustworthy", "industry": "fintech" },
            "colors": { "primary": "#2F6BFF" },
            "typography": { "family": "Source Sans 3" },
            "templates": ["auth_login"]
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();
        assert!(value.get("catalog").is_some());
        assert!(value.get("tokens").is_some());
    }
}
