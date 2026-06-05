//! # adk-macros
//!
//! Proc macros for ADK-Rust that eliminate tool registration boilerplate.
//!
//! ## `#[tool]`
//!
//! Turns an async function into a fully-wired `adk_tool::Tool` implementation:
//!
//! ```rust,ignore
//! use adk_macros::tool;
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize, JsonSchema)]
//! struct WeatherArgs {
//!     /// The city to look up
//!     city: String,
//! }
//!
//! /// Get the current weather for a city.
//! #[tool]
//! async fn get_weather(args: WeatherArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
//!     Ok(serde_json::json!({ "temp": 72, "city": args.city }))
//! }
//!
//! // This generates a struct `GetWeather` that implements `adk_tool::Tool`.
//! // Use it like: Arc::new(GetWeather)
//! ```
//!
//! The macro:
//! - Uses the function's doc comment as the tool description
//! - Derives the JSON schema from the argument type via `schemars::schema_for!`
//! - Names the tool after the function (snake_case)
//! - Generates a zero-sized struct (PascalCase) implementing `Tool`

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Meta, Type, parse_macro_input};

/// Attribute macro that generates a `Tool` implementation from an async function.
///
/// # Requirements
///
/// - The function must be `async`
/// - It must take exactly one argument (the args struct) that implements
///   `serde::de::DeserializeOwned` and `schemars::JsonSchema`
/// - It must return `Result<serde_json::Value, adk_tool::AdkError>`
/// - Doc comments become the tool description
///
/// # Attributes
///
/// Optional attributes can be passed to configure tool metadata:
///
/// - `read_only` — marks the tool as having no side effects (`is_read_only() → true`)
/// - `concurrency_safe` — marks the tool as safe for concurrent execution (`is_concurrency_safe() → true`)
/// - `long_running` — marks the tool as long-running (`is_long_running() → true`)
///
/// # Examples
///
/// ```rust,ignore
/// /// Search the knowledge base for documents matching a query.
/// #[tool]
/// async fn search_docs(args: SearchArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
///     // ...
/// }
///
/// /// Look up cached data (read-only, safe for parallel dispatch).
/// #[tool(read_only, concurrency_safe)]
/// async fn cache_lookup(args: LookupArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
///     // ...
/// }
///
/// // Generated: pub struct SearchDocs; implements Tool
/// // Use: agent_builder.tool(Arc::new(SearchDocs))
/// ```
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // Parse optional attributes: #[tool(read_only, concurrency_safe, long_running)]
    let mut is_read_only = false;
    let mut is_concurrency_safe = false;
    let mut is_long_running = false;

    if !attr.is_empty() {
        let meta = parse_macro_input!(attr as ToolAttrs);
        is_read_only = meta.read_only;
        is_concurrency_safe = meta.concurrency_safe;
        is_long_running = meta.long_running;
    }

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;

    // Extract doc comments for description
    let doc_lines: Vec<String> = input_fn
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(lit) = &nv.value {
                    if let syn::Lit::Str(s) = &lit.lit {
                        return Some(s.value().trim().to_string());
                    }
                }
            }
            None
        })
        .collect();

    let description = if doc_lines.is_empty() {
        fn_name.to_string().replace('_', " ")
    } else {
        doc_lines.join(" ")
    };

    let tool_name_str = fn_name.to_string();

    // Generate PascalCase struct name: get_weather → GetWeather
    let struct_name = format_ident!(
        "{}",
        tool_name_str
            .split('_')
            .map(|seg| {
                let mut chars = seg.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<String>()
    );

    // Extract the single argument type
    let args_type = extract_args_type(&input_fn);

    // Check if we have a typed args parameter or no params
    let (schema_gen, deserialize_call) = if let Some(args_ty) = &args_type {
        (
            quote! {
                {
                    let mut schema = serde_json::to_value(
                        schemars::schema_for!(#args_ty)
                    ).unwrap_or_default();
                    // Strip fields that Gemini/LLM APIs don't accept
                    if let Some(obj) = schema.as_object_mut() {
                        obj.remove("$schema");
                        obj.remove("title");
                    }
                    // Simplify nullable types: {"type": ["string", "null"]} → {"type": "string"}
                    fn simplify_nullable(v: &mut serde_json::Value) {
                        match v {
                            serde_json::Value::Object(map) => {
                                if let Some(serde_json::Value::Array(types)) = map.get("type") {
                                    let non_null: Vec<_> = types.iter()
                                        .filter(|t| t.as_str() != Some("null"))
                                        .cloned()
                                        .collect();
                                    if non_null.len() == 1 {
                                        map.insert("type".to_string(), non_null[0].clone());
                                    }
                                }
                                // Remove anyOf wrappers for simple nullable types
                                if let Some(serde_json::Value::Array(any_of)) = map.remove("anyOf") {
                                    for variant in &any_of {
                                        if let Some(obj) = variant.as_object() {
                                            if obj.get("type").and_then(|t| t.as_str()) != Some("null") {
                                                for (k, val) in obj {
                                                    map.insert(k.clone(), val.clone());
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }
                                for val in map.values_mut() {
                                    simplify_nullable(val);
                                }
                            }
                            serde_json::Value::Array(arr) => {
                                for item in arr {
                                    simplify_nullable(item);
                                }
                            }
                            _ => {}
                        }
                    }
                    simplify_nullable(&mut schema);
                    Some(schema)
                }
            },
            quote! {
                let typed_args: #args_ty = serde_json::from_value(args)
                    .map_err(|e| adk_tool::AdkError::tool(
                        format!("invalid arguments for '{}': {e}", #tool_name_str)
                    ))?;
                #fn_name(typed_args).await
            },
        )
    } else {
        (
            quote! { None },
            quote! {
                let _ = args;
                #fn_name().await
            },
        )
    };

    // Check if the function signature includes ctx: Arc<dyn ToolContext>
    let has_ctx = has_tool_context_param(&input_fn);
    let execute_body = if has_ctx {
        if let Some(args_ty) = &args_type {
            quote! {
                let typed_args: #args_ty = serde_json::from_value(args)
                    .map_err(|e| adk_tool::AdkError::tool(
                        format!("invalid arguments for '{}': {e}", #tool_name_str)
                    ))?;
                #fn_name(ctx, typed_args).await
            }
        } else {
            quote! {
                let _ = args;
                #fn_name(ctx).await
            }
        }
    } else {
        deserialize_call
    };

    // Generate optional trait method overrides
    let read_only_override = if is_read_only {
        quote! {
            fn is_read_only(&self) -> bool { true }
        }
    } else {
        quote! {}
    };

    let concurrency_safe_override = if is_concurrency_safe {
        quote! {
            fn is_concurrency_safe(&self) -> bool { true }
        }
    } else {
        quote! {}
    };

    let long_running_override = if is_long_running {
        quote! {
            fn is_long_running(&self) -> bool { true }
        }
    } else {
        quote! {}
    };

    let output = quote! {
        // Keep the original function
        #input_fn

        /// Auto-generated tool struct for [`#fn_name`].
        #fn_vis struct #struct_name;

        #[adk_tool::async_trait]
        impl adk_tool::Tool for #struct_name {
            fn name(&self) -> &str {
                #tool_name_str
            }

            fn description(&self) -> &str {
                #description
            }

            fn parameters_schema(&self) -> Option<serde_json::Value> {
                #schema_gen
            }

            #read_only_override
            #concurrency_safe_override
            #long_running_override

            async fn execute(
                &self,
                ctx: std::sync::Arc<dyn adk_tool::ToolContext>,
                args: serde_json::Value,
            ) -> adk_tool::Result<serde_json::Value> {
                #execute_body
            }
        }
    };

    output.into()
}

/// Extract the args type from the function signature.
/// Skips any `Arc<dyn ToolContext>` parameter.
fn extract_args_type(func: &ItemFn) -> Option<Type> {
    for arg in &func.sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            // Skip context parameters (Arc<dyn ToolContext>)
            let ty = &pat_type.ty;
            let ty_str = quote!(#ty).to_string();
            if ty_str.contains("ToolContext") {
                continue;
            }
            return Some((*pat_type.ty).clone());
        }
    }
    None
}

/// Check if the function has an Arc<dyn ToolContext> parameter.
fn has_tool_context_param(func: &ItemFn) -> bool {
    func.sig.inputs.iter().any(|arg| {
        if let FnArg::Typed(pat_type) = arg {
            let ty = &pat_type.ty;
            let ty_str = quote!(#ty).to_string();
            ty_str.contains("ToolContext")
        } else {
            false
        }
    })
}

/// Parsed attributes from `#[tool(read_only, concurrency_safe, long_running)]`.
struct ToolAttrs {
    read_only: bool,
    concurrency_safe: bool,
    long_running: bool,
}

impl syn::parse::Parse for ToolAttrs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut attrs =
            ToolAttrs { read_only: false, concurrency_safe: false, long_running: false };

        let punctuated =
            syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated(input)?;

        for meta in punctuated {
            if let Meta::Path(path) = &meta {
                if path.is_ident("read_only") {
                    attrs.read_only = true;
                } else if path.is_ident("concurrency_safe") {
                    attrs.concurrency_safe = true;
                } else if path.is_ident("long_running") {
                    attrs.long_running = true;
                } else {
                    return Err(syn::Error::new_spanned(
                        path,
                        "unknown tool attribute; expected `read_only`, `concurrency_safe`, or `long_running`",
                    ));
                }
            } else {
                return Err(syn::Error::new_spanned(
                    meta,
                    "expected identifier (e.g., `read_only`), not key-value",
                ));
            }
        }

        Ok(attrs)
    }
}

// ─── Functional API Macros ─────────────────────────────────────────────────────

/// Attribute macro that generates a workflow agent struct from an async function.
///
/// The annotated function becomes the workflow body. The macro generates:
/// - A PascalCase struct (e.g., `my_workflow` → `MyWorkflowAgent`)
/// - A `new()` constructor accepting `Arc<dyn Checkpointer>`
/// - An `invoke()` method that creates/restores `TaskContext`, validates state,
///   creates checkpoints, calls the function, and persists the final checkpoint
///
/// # Requirements
///
/// - The function **must** be `async`
/// - The function **must** accept `&mut TaskContext` as its sole parameter
/// - The function **must** return `Result<Value>` (or equivalent)
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::TaskContext;
/// use adk_graph::error::Result;
/// use serde_json::Value;
///
/// #[entrypoint]
/// async fn my_workflow(ctx: &mut TaskContext) -> Result<Value> {
///     let data = step_a(ctx, "input").await?;
///     let result = step_b(ctx, data).await?;
///     Ok(result)
/// }
///
/// // Generates: pub struct MyWorkflowAgent { ... }
/// // with MyWorkflowAgent::new(checkpointer) and invoke(initial_state, config)
/// ```
#[proc_macro_attribute]
pub fn entrypoint(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // Validate: must be async
    if input_fn.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            &input_fn.sig.fn_token,
            "#[entrypoint] functions must be async",
        )
        .to_compile_error()
        .into();
    }

    // Validate: must accept &mut TaskContext
    let has_task_context = input_fn.sig.inputs.iter().any(|arg| {
        if let FnArg::Typed(pat_type) = arg {
            let full_str = quote!(#pat_type).to_string();
            full_str.contains("TaskContext")
        } else {
            false
        }
    });

    if !has_task_context {
        return syn::Error::new_spanned(
            &input_fn.sig,
            "#[entrypoint] functions must accept `&mut TaskContext` as a parameter",
        )
        .to_compile_error()
        .into();
    }

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_name_str = fn_name.to_string();

    // Generate PascalCase struct name: my_workflow → MyWorkflowAgent
    let struct_name = format_ident!(
        "{}Agent",
        fn_name_str
            .split('_')
            .map(|seg| {
                let mut chars = seg.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<String>()
    );

    let output = quote! {
        // Preserve the original function for direct testing
        #input_fn

        /// Auto-generated workflow agent struct for [`#fn_name`].
        ///
        /// Created by the `#[entrypoint]` macro. Provides `new()` and `invoke()`
        /// methods for executing the workflow with automatic checkpointing.
        #fn_vis struct #struct_name {
            checkpointer: std::sync::Arc<dyn adk_graph::checkpoint::Checkpointer>,
        }

        impl #struct_name {
            /// Create a new workflow agent with the given checkpointer.
            pub fn new(checkpointer: std::sync::Arc<dyn adk_graph::checkpoint::Checkpointer>) -> Self {
                Self { checkpointer }
            }

            /// Invoke the workflow with an initial state and execution configuration.
            ///
            /// This method:
            /// 1. Creates or restores a `TaskContext` from the last checkpoint
            /// 2. Validates initial state against the configured schema
            /// 3. Creates a checkpoint before execution
            /// 4. Calls the annotated workflow function
            /// 5. Persists the final checkpoint
            /// 6. Returns the final workflow state
            pub async fn invoke(
                &self,
                initial_state: adk_graph::state::State,
                execution_config: adk_graph::node::ExecutionConfig,
            ) -> adk_graph::error::Result<adk_graph::state::State> {
                use adk_graph::checkpoint::Checkpointer;
                use adk_graph::functional::ExecutionLog;
                use adk_graph::state::Checkpoint;
                use adk_graph::stream::StreamEvent;

                let thread_id = execution_config.thread_id.clone();

                // Try to restore from checkpoint if resuming
                let (state, execution_log) = if execution_config.resume_from.is_some() {
                    match self.checkpointer.load(&thread_id).await? {
                        Some(checkpoint) => {
                            let log: ExecutionLog = checkpoint
                                .metadata
                                .get("execution_log")
                                .and_then(|v| serde_json::from_value(v.clone()).ok())
                                .unwrap_or_default();
                            (checkpoint.state, log)
                        }
                        None => (initial_state, ExecutionLog::new()),
                    }
                } else {
                    (initial_state, ExecutionLog::new())
                };

                // Create broadcast channel for stream events
                let (event_tx, _) = tokio::sync::broadcast::channel::<StreamEvent>(256);
                let cancel_token = tokio_util::sync::CancellationToken::new();
                let execution_log = std::sync::Arc::new(tokio::sync::RwLock::new(execution_log));

                // Create TaskContext
                let mut ctx = adk_graph::functional::TaskContext::new(
                    thread_id.clone(),
                    state,
                    self.checkpointer.clone(),
                    event_tx.clone(),
                    execution_log.clone(),
                    cancel_token,
                    None,
                );

                // Validate initial state against schema (if configured)
                ctx.validate_state().map_err(|e| adk_graph::error::GraphError::Other(e.to_string()))?;

                // Create pre-execution checkpoint
                let pre_checkpoint = Checkpoint::new(
                    &thread_id,
                    ctx.state().clone(),
                    0,
                    vec![],
                )
                .with_metadata("phase", serde_json::Value::String("pre_execution".to_string()));
                self.checkpointer.save(&pre_checkpoint).await?;

                // Emit workflow start event
                let _ = event_tx.send(StreamEvent::node_start(#fn_name_str, 0));

                // Call the workflow function
                let start = std::time::Instant::now();
                let result = #fn_name(&mut ctx).await;

                let duration = start.elapsed().as_millis() as u64;

                match result {
                    Ok(_value) => {
                        // Persist final checkpoint
                        let step = execution_log.read().await.current_step();
                        let final_checkpoint = Checkpoint::new(
                            &thread_id,
                            ctx.state().clone(),
                            step,
                            vec![],
                        )
                        .with_metadata("phase", serde_json::Value::String("completed".to_string()))
                        .with_metadata(
                            "execution_log",
                            serde_json::to_value(&*execution_log.read().await)
                                .unwrap_or(serde_json::Value::Null),
                        );
                        self.checkpointer.save(&final_checkpoint).await?;

                        // Emit workflow end event
                        let _ = event_tx.send(StreamEvent::node_end(#fn_name_str, step, duration));

                        Ok(ctx.state().clone())
                    }
                    Err(e) => {
                        // Persist failure checkpoint
                        let step = execution_log.read().await.current_step();
                        let fail_checkpoint = Checkpoint::new(
                            &thread_id,
                            ctx.state().clone(),
                            step,
                            vec![],
                        )
                        .with_metadata("phase", serde_json::Value::String("failed".to_string()))
                        .with_metadata("error", serde_json::Value::String(e.to_string()))
                        .with_metadata(
                            "execution_log",
                            serde_json::to_value(&*execution_log.read().await)
                                .unwrap_or(serde_json::Value::Null),
                        );
                        let _ = self.checkpointer.save(&fail_checkpoint).await;

                        // Emit error event
                        let _ = event_tx.send(StreamEvent::error(&e.to_string(), Some(#fn_name_str)));

                        Err(e)
                    }
                }
            }
        }
    };

    output.into()
}

/// Attribute macro that generates a task wrapper with checkpointing, retry, and streaming.
///
/// The annotated function becomes the inner task body. The macro generates a wrapper
/// function (prefixed with `__task_`) that:
/// - Checks `ExecutionLog` for cached results (resume-skip path)
/// - Emits `StreamEvent::node_start` and `StreamEvent::node_end` events
/// - Implements retry logic when `retry(max_attempts, backoff)` is specified
/// - Calls `record_completion()` on success
/// - Calls `record_failure()` after all retries are exhausted
///
/// # Requirements
///
/// - The function **must** be `async`
/// - The function **must** accept `&mut TaskContext` as its first argument
///
/// # Attributes
///
/// - `retry(max_attempts = N, backoff = "Xs")` — retry on failure with exponential backoff
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::TaskContext;
/// use adk_graph::error::Result;
/// use serde_json::Value;
///
/// #[task(retry(max_attempts = 3, backoff = "1s"))]
/// async fn step_a(ctx: &mut TaskContext, input: &str) -> Result<Value> {
///     Ok(serde_json::json!({"processed": input}))
/// }
///
/// // Generates: async fn __task_step_a(ctx: &mut TaskContext, input: &str) -> Result<Value>
/// // which wraps step_a with checkpoint/retry/streaming logic.
/// ```
#[proc_macro_attribute]
pub fn task(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // Validate: must be async
    if input_fn.sig.asyncness.is_none() {
        return syn::Error::new_spanned(&input_fn.sig.fn_token, "#[task] functions must be async")
            .to_compile_error()
            .into();
    }

    // Validate: first argument must be &mut TaskContext
    let has_task_context_first = input_fn
        .sig
        .inputs
        .first()
        .map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                let full_str = quote!(#pat_type).to_string();
                full_str.contains("TaskContext")
            } else {
                false
            }
        })
        .unwrap_or(false);

    if !has_task_context_first {
        return syn::Error::new_spanned(
            &input_fn.sig,
            "#[task] functions must accept `&mut TaskContext` as the first argument",
        )
        .to_compile_error()
        .into();
    }

    // Parse retry attributes from #[task(retry(max_attempts = N, backoff = "Xs"))]
    let task_attrs = parse_task_attrs(attr);

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_name_str = fn_name.to_string();
    let wrapper_name = format_ident!("__task_{}", fn_name);

    // Collect function parameters (all of them for the wrapper signature)
    let params = &input_fn.sig.inputs;
    let return_type = &input_fn.sig.output;

    // Collect the argument names for forwarding the call (skip `ctx`)
    let forward_args: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .skip(1) // Skip ctx
        .filter_map(|arg| if let FnArg::Typed(pat_type) = arg { Some(&pat_type.pat) } else { None })
        .collect();

    // Build the call expression
    let call_expr = if forward_args.is_empty() {
        quote! { #fn_name(ctx).await }
    } else {
        quote! { #fn_name(ctx, #(#forward_args),*).await }
    };

    // Generate retry logic or single-attempt logic
    let execution_body = if let Some(retry_config) = &task_attrs.retry {
        let max_attempts = retry_config.max_attempts;
        let backoff_secs = retry_config.backoff_secs;
        quote! {
            let mut attempts: u32 = 0;
            let max_attempts: u32 = #max_attempts;
            let backoff = std::time::Duration::from_secs(#backoff_secs);

            let result = loop {
                attempts += 1;
                match #call_expr {
                    Ok(value) => break Ok(value),
                    Err(e) if attempts < max_attempts => {
                        tokio::time::sleep(backoff * attempts).await;
                        continue;
                    }
                    Err(e) => {
                        ctx.record_failure(task_id, &e.to_string()).await?;
                        ctx.emit(adk_graph::stream::StreamEvent::error(
                            &e.to_string(),
                            Some(task_id),
                        ));
                        break Err(e);
                    }
                }
            };
        }
    } else {
        quote! {
            let result = match #call_expr {
                Ok(value) => Ok(value),
                Err(e) => {
                    ctx.record_failure(task_id, &e.to_string()).await?;
                    ctx.emit(adk_graph::stream::StreamEvent::error(
                        &e.to_string(),
                        Some(task_id),
                    ));
                    Err(e)
                }
            };
        }
    };

    let output = quote! {
        // Preserve the original function for direct testing
        #input_fn

        /// Auto-generated task wrapper for [`#fn_name`].
        ///
        /// Wraps the original function with:
        /// - Resume-skip logic (checks `ExecutionLog` for cached results)
        /// - `StreamEvent::node_start` / `StreamEvent::node_end` emission
        /// - Retry logic (if configured)
        /// - `record_completion()` on success
        /// - `record_failure()` after all retries exhausted
        #fn_vis async fn #wrapper_name(#params) #return_type {
            let task_id = #fn_name_str;

            // Check if already completed (resume path)
            if let Some(cached_result) = ctx.get_cached_result(task_id).await {
                return Ok(cached_result);
            }

            // Emit task start event
            let current_step = ctx.current_step().await;
            ctx.emit(adk_graph::stream::StreamEvent::node_start(task_id, current_step));

            let start = std::time::Instant::now();

            #execution_body

            if let Ok(ref value) = result {
                // Record completion and checkpoint
                ctx.record_completion(task_id, value).await?;
                let duration = start.elapsed().as_millis() as u64;
                let step = ctx.current_step().await;
                ctx.emit(adk_graph::stream::StreamEvent::node_end(task_id, step, duration));
            }

            result
        }
    };

    output.into()
}

// ─── Task Attribute Parsing ────────────────────────────────────────────────────

/// Parsed retry configuration from `#[task(retry(max_attempts = N, backoff = "Xs"))]`.
struct RetryConfig {
    max_attempts: u32,
    backoff_secs: u64,
}

/// Parsed attributes from `#[task(...)]`.
struct TaskAttrs {
    retry: Option<RetryConfig>,
}

/// Parse task attributes from the attribute token stream.
///
/// Supports:
/// - `#[task]` — no retry
/// - `#[task(retry(max_attempts = 3, backoff = "1s"))]` — with retry
fn parse_task_attrs(attr: TokenStream) -> TaskAttrs {
    if attr.is_empty() {
        return TaskAttrs { retry: None };
    }

    // Parse the attribute as a Meta list
    let attr_meta: syn::Result<syn::Meta> = syn::parse(attr.clone());
    if let Ok(syn::Meta::List(meta_list)) = attr_meta {
        if meta_list.path.is_ident("retry") {
            if let Some(retry) = parse_retry_from_meta_list(&meta_list) {
                return TaskAttrs { retry: Some(retry) };
            }
        }
    }

    // Try parsing as just the inner content of task(...)
    // e.g., the attr stream is: `retry(max_attempts = 3, backoff = "1s")`
    let attr2: proc_macro2::TokenStream = attr.into();
    let parsed: syn::Result<TaskAttrContent> = syn::parse2(attr2);
    if let Ok(content) = parsed {
        return TaskAttrs { retry: content.retry };
    }

    TaskAttrs { retry: None }
}

/// Inner content parsed from `#[task(retry(...))]`.
struct TaskAttrContent {
    retry: Option<RetryConfig>,
}

impl syn::parse::Parse for TaskAttrContent {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let ident: syn::Ident = input.parse()?;
        if ident != "retry" {
            return Ok(TaskAttrContent { retry: None });
        }

        let content;
        syn::parenthesized!(content in input);

        let mut max_attempts: u32 = 3;
        let mut backoff_secs: u64 = 1;

        // Parse key = value pairs separated by commas
        let pairs =
            syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated(
                &content,
            )?;

        for pair in pairs {
            if pair.path.is_ident("max_attempts") {
                if let syn::Expr::Lit(expr_lit) = &pair.value {
                    if let syn::Lit::Int(lit_int) = &expr_lit.lit {
                        max_attempts = lit_int.base10_parse().unwrap_or(3);
                    }
                }
            } else if pair.path.is_ident("backoff") {
                if let syn::Expr::Lit(expr_lit) = &pair.value {
                    if let syn::Lit::Str(lit_str) = &expr_lit.lit {
                        backoff_secs = parse_duration_str(&lit_str.value());
                    }
                }
            }
        }

        Ok(TaskAttrContent { retry: Some(RetryConfig { max_attempts, backoff_secs }) })
    }
}

/// Parse retry config from a `Meta::List` (e.g., `retry(max_attempts = 3, backoff = "1s")`).
fn parse_retry_from_meta_list(meta_list: &syn::MetaList) -> Option<RetryConfig> {
    let mut max_attempts: u32 = 3;
    let mut backoff_secs: u64 = 1;

    let pairs: syn::Result<syn::punctuated::Punctuated<syn::MetaNameValue, syn::Token![,]>> =
        meta_list.parse_args_with(syn::punctuated::Punctuated::parse_terminated);

    if let Ok(pairs) = pairs {
        for pair in pairs {
            if pair.path.is_ident("max_attempts") {
                if let syn::Expr::Lit(expr_lit) = &pair.value {
                    if let syn::Lit::Int(lit_int) = &expr_lit.lit {
                        max_attempts = lit_int.base10_parse().unwrap_or(3);
                    }
                }
            } else if pair.path.is_ident("backoff") {
                if let syn::Expr::Lit(expr_lit) = &pair.value {
                    if let syn::Lit::Str(lit_str) = &expr_lit.lit {
                        backoff_secs = parse_duration_str(&lit_str.value());
                    }
                }
            }
        }
        Some(RetryConfig { max_attempts, backoff_secs })
    } else {
        None
    }
}

/// Parse a duration string like "1s", "500ms", "2s" into seconds.
/// Defaults to 1 second if parsing fails.
fn parse_duration_str(s: &str) -> u64 {
    let s = s.trim();
    // Check "ms" suffix first (before "s" since "ms" ends with 's')
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.parse::<u64>().ok().map(|v| v / 1000).unwrap_or(1);
    }
    if let Some(secs) = s.strip_suffix('s') {
        return secs.parse::<u64>().unwrap_or(1);
    }
    // Try parsing as plain number (assume seconds)
    s.parse::<u64>().unwrap_or(1)
}
