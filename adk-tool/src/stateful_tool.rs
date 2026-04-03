use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use schemars::{JsonSchema, schema::RootSchema};
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type AsyncStatefulHandler<S> = Box<
    dyn Fn(
            Arc<S>,
            Arc<dyn ToolContext>,
            Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>>
        + Send
        + Sync,
>;

/// A generic tool wrapper that manages shared state for stateful closures.
///
/// `StatefulTool<S>` accepts an `Arc<S>` and a handler closure that receives
/// the state alongside the tool context and arguments. The `Arc<S>` is cloned
/// (cheap reference count bump) on each invocation, so all executions share
/// the same underlying state.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::StatefulTool;
/// use adk_core::{ToolContext, Result};
/// use serde_json::{json, Value};
/// use std::sync::Arc;
/// use tokio::sync::RwLock;
///
/// struct Counter { count: RwLock<u64> }
///
/// let state = Arc::new(Counter { count: RwLock::new(0) });
///
/// let tool = StatefulTool::new(
///     "increment",
///     "Increment a counter",
///     state,
///     |s, _ctx, _args| async move {
///         let mut count = s.count.write().await;
///         *count += 1;
///         Ok(json!({ "count": *count }))
///     },
/// );
/// ```
pub struct StatefulTool<S: Send + Sync + 'static> {
    name: String,
    description: String,
    state: Arc<S>,
    handler: AsyncStatefulHandler<S>,
    long_running: bool,
    read_only: bool,
    concurrency_safe: bool,
    parameters_schema: Option<Value>,
    response_schema: Option<Value>,
    scopes: Vec<&'static str>,
}

impl<S: Send + Sync + 'static> StatefulTool<S> {
    /// Create a new stateful tool.
    ///
    /// # Arguments
    ///
    /// * `name` - Tool name exposed to the LLM
    /// * `description` - Human-readable description of what the tool does
    /// * `state` - Shared state wrapped in `Arc<S>`
    /// * `handler` - Async closure receiving `(Arc<S>, Arc<dyn ToolContext>, Value)`
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        state: Arc<S>,
        handler: F,
    ) -> Self
    where
        F: Fn(Arc<S>, Arc<dyn ToolContext>, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            state,
            handler: Box::new(move |s, ctx, args| Box::pin(handler(s, ctx, args))),
            long_running: false,
            read_only: false,
            concurrency_safe: false,
            parameters_schema: None,
            response_schema: None,
            scopes: Vec::new(),
        }
    }

    pub fn with_long_running(mut self, long_running: bool) -> Self {
        self.long_running = long_running;
        self
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn with_concurrency_safe(mut self, concurrency_safe: bool) -> Self {
        self.concurrency_safe = concurrency_safe;
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

    /// Declare the scopes required to execute this tool.
    ///
    /// When set, the framework will enforce that the calling user possesses
    /// **all** listed scopes before dispatching `execute()`.
    pub fn with_scopes(mut self, scopes: &[&'static str]) -> Self {
        self.scopes = scopes.to_vec();
        self
    }

    pub fn parameters_schema(&self) -> Option<&Value> {
        self.parameters_schema.as_ref()
    }

    pub fn response_schema(&self) -> Option<&Value> {
        self.response_schema.as_ref()
    }
}

/// The note appended to long-running tool descriptions to prevent duplicate calls.
const LONG_RUNNING_NOTE: &str = "NOTE: This is a long-running operation. Do not call this tool again if it has already returned some intermediate or pending status.";

#[async_trait]
impl<S: Send + Sync + 'static> Tool for StatefulTool<S> {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

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
        adk_telemetry::debug!("Executing stateful tool");
        let state = Arc::clone(&self.state);
        (self.handler)(state, ctx, args).await
    }
}

fn generate_schema<T>() -> Value
where
    T: JsonSchema + Serialize,
{
    let settings = schemars::r#gen::SchemaSettings::openapi3().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    });
    let generator = schemars::r#gen::SchemaGenerator::new(settings);
    let mut schema: RootSchema = generator.into_root_schema_for::<T>();
    schema.schema.metadata().title = None;
    serde_json::to_value(schema).unwrap()
}
