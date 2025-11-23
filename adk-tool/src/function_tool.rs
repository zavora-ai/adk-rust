use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use schemars::{schema::RootSchema, JsonSchema};
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type AsyncHandler = Box<dyn Fn(Arc<dyn ToolContext>, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync>;

pub struct FunctionTool {
    name: String,
    description: String,
    handler: AsyncHandler,
    long_running: bool,
    parameters_schema: Option<Value>,
    response_schema: Option<Value>,
}

impl FunctionTool {
    pub fn new<F, Fut>(name: impl Into<String>, description: impl Into<String>, handler: F) -> Self
    where
        F: Fn(Arc<dyn ToolContext>, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            handler: Box::new(move |ctx, args| Box::pin(handler(ctx, args))),
            long_running: false,
            parameters_schema: None,
            response_schema: None,
        }
    }

    pub fn with_long_running(mut self, long_running: bool) -> Self {
        self.long_running = long_running;
        self
    }

    pub fn with_parameters_schema<T>(mut self) -> Self
    where
        T: JsonSchema + Serialize,
    {
        self.parameters_schema = Some(generate_schema::<T>());
        self
    }

    pub fn with_response_schema<T>(mut self) -> Self
    where
        T: JsonSchema + Serialize,
    {
        self.response_schema = Some(generate_schema::<T>());
        self
    }

    pub fn parameters_schema(&self) -> Option<&Value> {
        self.parameters_schema.as_ref()
    }

    pub fn response_schema(&self) -> Option<&Value> {
        self.response_schema.as_ref()
    }
}

#[async_trait]
impl Tool for FunctionTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn is_long_running(&self) -> bool {
        self.long_running
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        (self.handler)(ctx, args).await
    }
}

fn generate_schema<T>() -> Value
where
    T: JsonSchema + Serialize,
{
    let settings = schemars::gen::SchemaSettings::openapi3().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    });
    let generator = schemars::gen::SchemaGenerator::new(settings);
    let mut schema: RootSchema = generator.into_root_schema_for::<T>();
    schema.schema.metadata().title = None;
    serde_json::to_value(schema).unwrap()
}
