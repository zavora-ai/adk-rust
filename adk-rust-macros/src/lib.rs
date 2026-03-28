//! # adk-macros
//!
//! Proc macros for ADK-Rust that eliminate tool registration boilerplate.
//!
//! ## `#[tool]`
//!
//! Turns an async function into a fully-wired [`adk_tool::Tool`] implementation:
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
use syn::{FnArg, ItemFn, Type, parse_macro_input};

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
/// # Example
///
/// ```rust,ignore
/// /// Search the knowledge base for documents matching a query.
/// #[tool]
/// async fn search_docs(args: SearchArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
///     // ...
/// }
///
/// // Generated: pub struct SearchDocs; implements Tool
/// // Use: agent_builder.tool(Arc::new(SearchDocs))
/// ```
#[proc_macro_attribute]
pub fn tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

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
            if ty_str.contains("ToolContext") || ty_str.contains("Arc") {
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
