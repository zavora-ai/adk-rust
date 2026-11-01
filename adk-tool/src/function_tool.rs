use crate::arguments::ArgumentCheck;
use adk_core::egress::ToolEgress;
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use schemars::{
    JsonSchema,
    generate::{SchemaGenerator, SchemaSettings},
};
use serde::Serialize;
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
///
/// # Argument validation
///
/// A tool given a schema with [`with_parameters_schema`](Self::with_parameters_schema)
/// checks each call's arguments against it before the handler runs, as `#[tool]` does:
/// a key the type does not declare, a value outside a `#[schemars(range(...))]`,
/// `length(...)` or `pattern(...)` bound, or a missing required field fails the call
/// with an `ErrorCategory::InvalidInput` error with code
/// [`TOOL_INVALID_ARGUMENTS_CODE`](adk_core::TOOL_INVALID_ARGUMENTS_CODE) that the model
/// sees and can correct. An integral float at an integer field, such as `3.0`, reaches
/// the handler as the integer. [`with_lenient_args`](Self::with_lenient_args) turns the
/// check off.
pub struct FunctionTool {
    name: String,
    description: String,
    handler: AsyncHandler,
    long_running: bool,
    read_only: bool,
    concurrency_safe: bool,
    effect: Option<ToolEffect>,
    parameters_schema: Option<Value>,
    arguments: Option<ArgumentCheck>,
    lenient_args: bool,
    response_schema: Option<Value>,
    scopes: Vec<&'static str>,
    egress: Option<EgressFn>,
}

/// What a [`FunctionTool`] declares about a call's network destinations, from its arguments.
type EgressFn = Box<dyn Fn(&Value) -> ToolEgress + Send + Sync>;

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
            effect: None,
            parameters_schema: None,
            arguments: None,
            lenient_args: false,
            response_schema: None,
            scopes: Vec::new(),
            egress: None,
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

    /// Declare the tool's side-effect class.
    ///
    /// Without this, the effect follows [`with_read_only`](Self::with_read_only):
    /// [`ToolEffect::ReadOnly`] for a read-only tool, otherwise
    /// [`ToolEffect::NonIdempotent`]. Only `ReadOnly` and `Idempotent` tools are
    /// retried under a [`RetryBudget`](adk_core::RetryBudget).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Tool, ToolEffect};
    /// use adk_tool::FunctionTool;
    ///
    /// let tool = FunctionTool::new("set_status", "Sets the ticket status", |_ctx, args| async move {
    ///     Ok(args)
    /// })
    /// .with_effect(ToolEffect::Idempotent);
    /// assert_eq!(tool.effect(), ToolEffect::Idempotent);
    /// ```
    pub fn with_effect(mut self, effect: ToolEffect) -> Self {
        self.effect = Some(effect);
        self
    }

    /// Declares the network destinations every call reaches ([`Tool::egress`]); without a
    /// declaration a run with an egress policy refuses the tool.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Tool;
    /// use adk_core::egress::ToolEgress;
    /// use adk_tool::FunctionTool;
    /// use serde_json::json;
    ///
    /// let add = FunctionTool::new("add", "Adds two numbers", |_ctx, args| async move {
    ///     Ok(json!(args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0)))
    /// })
    /// .with_egress(ToolEgress::None);
    /// assert_eq!(add.egress(&json!({})), ToolEgress::None);
    /// ```
    pub fn with_egress(self, egress: ToolEgress) -> Self {
        self.with_egress_fn(move |_args| egress.clone())
    }

    /// Declares the network destinations of each call from its arguments ([`Tool::egress`]),
    /// for a tool whose destination is an argument.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Tool;
    /// use adk_core::egress::ToolEgress;
    /// use adk_tool::FunctionTool;
    /// use serde_json::json;
    ///
    /// let fetch = FunctionTool::new("fetch", "Fetches a URL", |_ctx, args| async move { Ok(args) })
    ///     .with_egress_fn(|args| ToolEgress::for_urls([args["url"].as_str().unwrap_or_default()]));
    /// assert_eq!(
    ///     fetch.egress(&json!({ "url": "https://docs.rs/adk" })),
    ///     ToolEgress::for_urls(["https://docs.rs/"])
    /// );
    /// ```
    pub fn with_egress_fn(
        mut self,
        egress: impl Fn(&Value) -> ToolEgress + Send + Sync + 'static,
    ) -> Self {
        self.egress = Some(Box::new(egress));
        self
    }

    /// Derive the parameters JSON Schema from a type implementing `JsonSchema`, and
    /// check each call's arguments against it; see [Argument validation](Self#argument-validation).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{TOOL_INVALID_ARGUMENTS_CODE, Tool};
    /// use adk_tool::{FunctionTool, SimpleToolContext};
    /// use schemars::JsonSchema;
    /// use serde::Serialize;
    /// use serde_json::json;
    /// use std::sync::Arc;
    ///
    /// #[derive(Serialize, JsonSchema)]
    /// struct Order {
    ///     #[schemars(range(min = 1, max = 5))]
    ///     quantity: u32,
    /// }
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let tool = FunctionTool::new("order", "Orders widgets", |_ctx, args| async move { Ok(args) })
    ///     .with_parameters_schema::<Order>();
    /// let ctx = Arc::new(SimpleToolContext::new("doc"));
    ///
    /// let err = tool.execute(ctx.clone(), json!({ "quantity": 50 })).await.unwrap_err();
    /// assert_eq!(err.code, TOOL_INVALID_ARGUMENTS_CODE);
    /// assert_eq!(tool.execute(ctx, json!({ "quantity": 3.0 })).await.unwrap(), json!({ "quantity": 3 }));
    /// # }
    /// ```
    pub fn with_parameters_schema<T>(mut self) -> Self
    where
        T: JsonSchema + Serialize,
    {
        self.parameters_schema = Some(generate_schema::<T>());
        self.arguments = Some(ArgumentCheck::for_type::<T>());
        self
    }

    /// Turn off the argument check of [`with_parameters_schema`](Self::with_parameters_schema).
    ///
    /// The handler then receives the arguments as the model sent them, apart from integral
    /// floats at integer fields: undeclared keys are kept and schema bounds are advisory.
    pub fn with_lenient_args(mut self, lenient: bool) -> Self {
        self.lenient_args = lenient;
        self
    }

    /// Derive the response JSON Schema from a type implementing `JsonSchema`.
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

    fn effect(&self) -> ToolEffect {
        match self.effect {
            Some(effect) => effect,
            None if self.read_only => ToolEffect::ReadOnly,
            None => ToolEffect::NonIdempotent,
        }
    }

    fn egress(&self, args: &Value) -> ToolEgress {
        self.egress.as_ref().map_or(ToolEgress::Undeclared, |egress| egress(args))
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
    async fn execute(&self, ctx: Arc<dyn ToolContext>, mut args: Value) -> Result<Value> {
        adk_telemetry::debug!("Executing tool");
        if let Some(check) = &self.arguments {
            check.apply(&self.name, self.lenient_args, &mut args)?;
        }
        (self.handler)(ctx, args).await
    }
}

fn generate_schema<T>() -> Value
where
    T: JsonSchema + Serialize,
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
    serde_json::to_value(schema).unwrap()
}
