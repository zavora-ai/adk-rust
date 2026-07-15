use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use schemars::{
    JsonSchema,
    generate::{SchemaGenerator, SchemaSettings},
};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type AsyncHandler = Box<
    dyn Fn(Arc<dyn ToolContext>, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>>
        + Send
        + Sync,
>;

/// A tool created from an async Rust function.
///
/// `FunctionTool` wraps an async closure and exposes it as a [`Tool`] that
/// LLM agents can invoke. Use the builder methods to configure schema,
/// execution flags, and required scopes.
pub struct FunctionTool {
    name: String,
    description: String,
    handler: AsyncHandler,
    long_running: bool,
    read_only: bool,
    concurrency_safe: bool,
    parameters_schema: Option<Value>,
    response_schema: Option<Value>,
    scopes: Vec<&'static str>,
}

impl FunctionTool {
    /// Create a new `FunctionTool` from an async handler function.
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
            read_only: false,
            concurrency_safe: false,
            parameters_schema: None,
            response_schema: None,
            scopes: Vec::new(),
        }
    }

    /// Mark this tool as long-running (prevents duplicate invocations).
    pub fn with_long_running(mut self, long_running: bool) -> Self {
        self.long_running = long_running;
        self
    }

    /// Mark this tool as read-only (safe for parallel dispatch).
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Mark this tool as concurrency-safe (can run in parallel with other tools).
    pub fn with_concurrency_safe(mut self, concurrency_safe: bool) -> Self {
        self.concurrency_safe = concurrency_safe;
        self
    }

    /// Derive the parameters JSON Schema from a type implementing `JsonSchema`.
    ///
    /// # Panics
    ///
    /// Panics if schema generation fails. For a fallible version, use `schema_for::<T>()`
    /// directly and set the field.
    pub fn with_parameters_schema<T>(mut self) -> Self
    where
        T: JsonSchema,
    {
        self.parameters_schema =
            Some(schema_for::<T>().expect("failed to generate parameters schema"));
        self
    }

    /// Derive the response JSON Schema from a type implementing `JsonSchema`.
    ///
    /// # Panics
    ///
    /// Panics if schema generation fails. For a fallible version, use `schema_for::<T>()`
    /// directly and set the field.
    pub fn with_response_schema<T>(mut self) -> Self
    where
        T: JsonSchema,
    {
        self.response_schema = Some(schema_for::<T>().expect("failed to generate response schema"));
        self
    }

    /// Declare the scopes required to execute this tool.
    ///
    /// When set, the framework will enforce that the calling user possesses
    /// **all** listed scopes before dispatching `execute()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let tool = FunctionTool::new("transfer", "Transfer funds", handler)
    ///     .with_scopes(&["finance:write", "verified"]);
    /// ```
    pub fn with_scopes(mut self, scopes: &[&'static str]) -> Self {
        self.scopes = scopes.to_vec();
        self
    }

    /// Get the parameters schema, if set.
    pub fn parameters_schema(&self) -> Option<&Value> {
        self.parameters_schema.as_ref()
    }

    /// Get the response schema, if set.
    pub fn response_schema(&self) -> Option<&Value> {
        self.response_schema.as_ref()
    }
}

/// The note appended to long-running tool descriptions to prevent duplicate calls.
const LONG_RUNNING_NOTE: &str = "NOTE: This is a long-running operation. Do not call this tool again if it has already returned some intermediate or pending status.";

#[async_trait]
impl Tool for FunctionTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// Returns an enhanced description for long-running tools that includes
    /// a note warning the model not to call the tool again if it's already pending.
    fn enhanced_description(&self) -> String {
        if self.long_running {
            if self.description.is_empty() {
                LONG_RUNNING_NOTE.to_string()
            } else {
                format!("{}\n\n{}", self.description, LONG_RUNNING_NOTE)
            }
        } else {
            self.description.clone()
        }
    }

    fn is_long_running(&self) -> bool {
        self.long_running
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn is_concurrency_safe(&self) -> bool {
        self.concurrency_safe
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.parameters_schema.clone()
    }

    fn response_schema(&self) -> Option<Value> {
        self.response_schema.clone()
    }

    fn required_scopes(&self) -> &[&str] {
        &self.scopes
    }

    #[adk_telemetry::instrument(
        skip(self, ctx, args),
        fields(
            tool.name = %self.name,
            tool.description = %self.description,
            tool.long_running = %self.long_running,
            function_call.id = %ctx.function_call_id()
        )
    )]
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        adk_telemetry::debug!("Executing tool");
        (self.handler)(ctx, args).await
    }
}

/// Derive the JSON Schema from a type implementing `JsonSchema`.
///
/// This helper uses OpenAPI 3 settings with inlined subschemas and
/// removes the "title" field from the root object to match standard
/// ADK tool expectations.
pub fn schema_for<T>() -> adk_core::Result<Value>
where
    T: JsonSchema,
{
    let settings = SchemaSettings::openapi3().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    });
    let generator = SchemaGenerator::new(settings);
    let mut schema = generator.into_root_schema_for::<T>();
    if let Some(object) = schema.as_object_mut() {
        object.remove("title");
    }
    serde_json::to_value(schema)
        .map_err(|e| adk_core::AdkError::tool(format!("failed to serialize JSON Schema: {e}")))
}
