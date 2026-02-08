//! Action Node Code Generation
//!
//! Generates Rust code for action nodes (non-LLM programmatic nodes) in ADK Studio workflows.
//! Each action node type generates corresponding Rust code that integrates with the ADK runtime.
//!
//! Type definitions (structs/enums) are in the sibling `action_node_types` module.
//! This module contains the `ActionNodeCodeGen` trait and all code generation implementations.
//!
//! ## Requirements
//!
//! - 13.1: Generate valid, compilable Rust code for each action node
//! - 13.2: Integrate action nodes with ADK runtime
//! - 13.3: Generate appropriate driver code for database nodes

// Re-export all types from the dedicated types module so existing
// `use crate::codegen::action_nodes::*` imports continue to work.
pub use super::action_node_types::*;

use std::collections::HashMap;

// ============================================
// Code Generation Trait
// ============================================

/// Trait for generating Rust code from action node configurations
pub trait ActionNodeCodeGen {
    /// Generate the Rust code for this action node
    fn generate_code(&self, node_id: &str) -> String;

    /// Generate any required imports for this action node
    fn required_imports(&self) -> Vec<&'static str>;

    /// Generate any required Cargo dependencies for this action node
    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)>;
}

// ============================================
// Code Generation Implementations
// ============================================

/// Generate the error handling wrapper code
pub fn generate_error_handling_wrapper(node_id: &str, props: &StandardProperties) -> String {
    let mut code = String::new();

    match props.error_handling.mode {
        ErrorMode::Stop => {
            // Default behavior - errors propagate up
            code.push_str(&format!("    // Error handling: stop on error\n"));
        }
        ErrorMode::Continue => {
            code.push_str(&format!(
                "    // Error handling: continue on error\n\
                 let {}_result = match {}_execute(state).await {{\n\
                     Ok(v) => v,\n\
                     Err(e) => {{\n\
                         tracing::warn!(node = \"{}\", error = %e, \"Node failed, continuing\");\n\
                         serde_json::Value::Null\n\
                     }}\n\
                 }};\n",
                node_id, node_id, node_id
            ));
        }
        ErrorMode::Retry => {
            let retry_count = props.error_handling.retry_count.unwrap_or(3);
            let retry_delay = props.error_handling.retry_delay.unwrap_or(1000);
            code.push_str(&format!(
                "    // Error handling: retry up to {} times with {}ms delay\n\
                 let mut {}_attempts = 0u32;\n\
                 let {}_result = loop {{\n\
                     match {}_execute(state).await {{\n\
                         Ok(v) => break v,\n\
                         Err(e) => {{\n\
                             {}_attempts += 1;\n\
                             if {}_attempts >= {} {{\n\
                                 return Err(e.into());\n\
                             }}\n\
                             tracing::warn!(node = \"{}\", attempt = {}_attempts, error = %e, \"Retrying\");\n\
                             tokio::time::sleep(std::time::Duration::from_millis({})).await;\n\
                         }}\n\
                     }}\n\
                 }};\n",
                retry_count, retry_delay,
                node_id, node_id, node_id, node_id, node_id, retry_count, node_id, node_id, retry_delay
            ));
        }
        ErrorMode::Fallback => {
            let fallback = props
                .error_handling
                .fallback_value
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "serde_json::Value::Null".to_string());
            code.push_str(&format!(
                "    // Error handling: fallback on error\n\
                 let {}_result = match {}_execute(state).await {{\n\
                     Ok(v) => v,\n\
                     Err(e) => {{\n\
                         tracing::warn!(node = \"{}\", error = %e, \"Using fallback value\");\n\
                         serde_json::json!({})\n\
                     }}\n\
                 }};\n",
                node_id, node_id, node_id, fallback
            ));
        }
    }

    code
}

/// Generate skip condition check code
pub fn generate_skip_condition(node_id: &str, condition: &Option<String>) -> String {
    match condition {
        Some(cond) if !cond.is_empty() => {
            format!(
                "    // Skip condition check\n\
                 if !evaluate_condition(\"{}\", state)? {{\n\
                     tracing::info!(node = \"{}\", \"Skipping node due to condition\");\n\
                     return Ok(serde_json::Value::Null);\n\
                 }}\n\n",
                cond.replace('"', "\\\""),
                node_id
            )
        }
        _ => String::new(),
    }
}

/// Generate callback invocation code
pub fn generate_callbacks(node_id: &str, callbacks: &Callbacks, phase: &str) -> String {
    let callback = match phase {
        "start" => &callbacks.on_start,
        "complete" => &callbacks.on_complete,
        "error" => &callbacks.on_error,
        _ => return String::new(),
    };

    match callback {
        Some(cb) if !cb.is_empty() => {
            format!(
                "    // {} callback\n\
                 if let Err(e) = execute_callback(\"{}\", state).await {{\n\
                     tracing::warn!(node = \"{}\", callback = \"{}\", error = %e, \"Callback failed\");\n\
                 }}\n",
                phase,
                cb.replace('"', "\\\""),
                node_id,
                phase
            )
        }
        _ => String::new(),
    }
}

/// Generate timeout wrapper code
pub fn generate_timeout_wrapper(node_id: &str, timeout_ms: u64) -> String {
    format!(
        "    // Timeout: {}ms\n\
         let {}_future = {}_execute(state);\n\
         let {}_result = tokio::time::timeout(\n\
             std::time::Duration::from_millis({}),\n\
             {}_future\n\
         ).await.map_err(|_| ActionError::Timeout {{ node: \"{}\".to_string(), timeout_ms: {} }})??;\n",
        timeout_ms, node_id, node_id, node_id, timeout_ms, node_id, node_id, timeout_ms
    )
}

/// Generate variable interpolation helper
pub fn generate_interpolation_helper() -> &'static str {
    r#"
/// Interpolate {{variable}} patterns in a string with state values
fn interpolate_variables(template: &str, state: &State) -> String {
    let re = regex::Regex::new(r"\{\{(\w+(?:\.\w+)*)\}\}").unwrap();
    re.replace_all(template, |caps: &regex::Captures| {
        let path = &caps[1];
        get_nested_value(state, path)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    }).to_string()
}

/// Get a nested value from state using dot notation
fn get_nested_value(state: &State, path: &str) -> Option<&serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = state.get(parts[0])?;
    for part in &parts[1..] {
        current = current.get(part)?;
    }
    Some(current)
}
"#
}

// ============================================
// Trigger Node Code Generation
// ============================================

impl ActionNodeCodeGen for TriggerNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Trigger Node: {}\n", self.standard.name));
        code.push_str(&format!(
            "async fn {}_trigger(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n",
            node_id
        ));

        match self.trigger_type {
            TriggerType::Manual => {
                code.push_str("    // Manual trigger - workflow started by user\n");
                code.push_str("    tracing::info!(\"Manual trigger activated\");\n");
                code.push_str("    Ok(serde_json::json!({ \"trigger\": \"manual\", \"timestamp\": chrono::Utc::now().to_rfc3339() }))\n");
            }
            TriggerType::Webhook => {
                if let Some(webhook) = &self.webhook {
                    code.push_str(&format!(
                        "    // Webhook trigger: {} {}\n",
                        webhook.method, webhook.path
                    ));
                    code.push_str(&format!("    // Auth: {}\n", webhook.auth));
                    code.push_str("    // Note: Webhook handler is set up in the server routes\n");
                    code.push_str("    // This function processes the incoming webhook payload\n");
                    code.push_str("    let payload = state.get(\"webhook_payload\").cloned().unwrap_or(serde_json::Value::Null);\n");
                    code.push_str("    Ok(payload)\n");
                } else {
                    code.push_str("    Ok(serde_json::Value::Null)\n");
                }
            }
            TriggerType::Schedule => {
                if let Some(schedule) = &self.schedule {
                    code.push_str(&format!(
                        "    // Schedule trigger: {} ({})\n",
                        schedule.cron, schedule.timezone
                    ));
                    code.push_str("    // Note: Cron job is set up externally\n");
                    code.push_str(&format!("    Ok(serde_json::json!({{\n"));
                    code.push_str(&format!("        \"trigger\": \"schedule\",\n"));
                    code.push_str(&format!("        \"cron\": \"{}\",\n", schedule.cron));
                    code.push_str(&format!("        \"timezone\": \"{}\",\n", schedule.timezone));
                    code.push_str("        \"timestamp\": chrono::Utc::now().to_rfc3339()\n");
                    code.push_str("    }))\n");
                } else {
                    code.push_str("    Ok(serde_json::Value::Null)\n");
                }
            }
            TriggerType::Event => {
                if let Some(event) = &self.event {
                    code.push_str(&format!(
                        "    // Event trigger: {} from {}\n",
                        event.event_type, event.source
                    ));
                    code.push_str("    let event_data = state.get(\"event_data\").cloned().unwrap_or(serde_json::Value::Null);\n");
                    code.push_str("    Ok(event_data)\n");
                } else {
                    code.push_str("    Ok(serde_json::Value::Null)\n");
                }
            }
        }

        code.push_str("}\n\n");

        // Generate webhook route if needed
        if self.trigger_type == TriggerType::Webhook {
            if let Some(webhook) = &self.webhook {
                code.push_str(&generate_webhook_handler(node_id, webhook));
            }
        }

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        let mut imports = vec!["chrono"];
        if self.trigger_type == TriggerType::Webhook {
            imports.push("axum");
        }
        if self.trigger_type == TriggerType::Schedule {
            imports.push("tokio_cron_scheduler");
        }
        imports
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps = vec![("chrono", "0.4")];
        if self.trigger_type == TriggerType::Webhook {
            deps.push(("axum", "0.7"));
        }
        if self.trigger_type == TriggerType::Schedule {
            deps.push(("tokio-cron-scheduler", "0.10"));
        }
        deps
    }
}

fn generate_webhook_handler(node_id: &str, webhook: &WebhookConfig) -> String {
    let mut code = String::new();

    code.push_str(&format!("// Webhook handler for {}\n", node_id));
    code.push_str(&format!("async fn {}_webhook_handler(\n", node_id));

    match webhook.auth.as_str() {
        "bearer" => {
            code.push_str("    headers: axum::http::HeaderMap,\n");
        }
        "api_key" => {
            code.push_str("    headers: axum::http::HeaderMap,\n");
        }
        _ => {}
    }

    if webhook.method == "POST" {
        code.push_str("    axum::Json(payload): axum::Json<serde_json::Value>,\n");
    } else {
        code.push_str("    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,\n");
    }

    code.push_str(") -> impl axum::response::IntoResponse {\n");

    // Auth validation
    match webhook.auth.as_str() {
        "bearer" => {
            code.push_str("    // Validate bearer token\n");
            code.push_str("    let auth_header = headers.get(\"Authorization\").and_then(|v| v.to_str().ok());\n");
            code.push_str(
                "    if !auth_header.map(|h| h.starts_with(\"Bearer \")).unwrap_or(false) {\n",
            );
            code.push_str("        return (axum::http::StatusCode::UNAUTHORIZED, \"Invalid authorization\").into_response();\n");
            code.push_str("    }\n");
        }
        "api_key" => {
            let header_name = webhook
                .auth_config
                .as_ref()
                .and_then(|c| c.header_name.as_ref())
                .map(|s| s.as_str())
                .unwrap_or("X-API-Key");
            code.push_str(&format!("    // Validate API key\n"));
            code.push_str(&format!(
                "    let api_key = headers.get(\"{}\").and_then(|v| v.to_str().ok());\n",
                header_name
            ));
            code.push_str("    if api_key.is_none() {\n");
            code.push_str("        return (axum::http::StatusCode::UNAUTHORIZED, \"Missing API key\").into_response();\n");
            code.push_str("    }\n");
        }
        _ => {}
    }

    if webhook.method == "POST" {
        code.push_str("    axum::Json(payload).into_response()\n");
    } else {
        code.push_str("    axum::Json(serde_json::json!(params)).into_response()\n");
    }

    code.push_str("}\n\n");

    code
}

// ============================================
// HTTP Node Code Generation
// ============================================

impl ActionNodeCodeGen for HttpNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// HTTP Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_http(\n", node_id));
        code.push_str("    state: &mut State,\n");
        code.push_str("    client: &reqwest::Client,\n");
        code.push_str(") -> Result<serde_json::Value, ActionError> {\n");

        // URL with variable interpolation
        code.push_str(&format!(
            "    let url = interpolate_variables(\"{}\", state);\n",
            self.url.replace('"', "\\\"")
        ));
        code.push_str("    tracing::debug!(url = %url, \"Making HTTP request\");\n\n");

        // Build request
        let method = match self.method {
            HttpMethod::Get => "get",
            HttpMethod::Post => "post",
            HttpMethod::Put => "put",
            HttpMethod::Patch => "patch",
            HttpMethod::Delete => "delete",
        };
        code.push_str(&format!("    let mut request = client.{}(&url);\n\n", method));

        // Add headers
        if !self.headers.is_empty() {
            code.push_str("    // Headers\n");
            for (key, value) in &self.headers {
                code.push_str(&format!(
                    "    request = request.header(\"{}\", interpolate_variables(\"{}\", state));\n",
                    key,
                    value.replace('"', "\\\"")
                ));
            }
            code.push_str("\n");
        }

        // Add authentication
        match self.auth.auth_type.as_str() {
            "bearer" => {
                if let Some(bearer) = &self.auth.bearer {
                    code.push_str("    // Bearer authentication\n");
                    code.push_str(&format!(
                        "    let token = interpolate_variables(\"{}\", state);\n",
                        bearer.token.replace('"', "\\\"")
                    ));
                    code.push_str("    request = request.bearer_auth(&token);\n\n");
                }
            }
            "basic" => {
                if let Some(basic) = &self.auth.basic {
                    code.push_str("    // Basic authentication\n");
                    code.push_str(&format!(
                        "    let username = interpolate_variables(\"{}\", state);\n",
                        basic.username.replace('"', "\\\"")
                    ));
                    code.push_str(&format!(
                        "    let password = interpolate_variables(\"{}\", state);\n",
                        basic.password.replace('"', "\\\"")
                    ));
                    code.push_str(
                        "    request = request.basic_auth(&username, Some(&password));\n\n",
                    );
                }
            }
            "api_key" => {
                if let Some(api_key) = &self.auth.api_key {
                    code.push_str("    // API key authentication\n");
                    code.push_str(&format!(
                        "    let api_key_value = interpolate_variables(\"{}\", state);\n",
                        api_key.value.replace('"', "\\\"")
                    ));
                    code.push_str(&format!(
                        "    request = request.header(\"{}\", &api_key_value);\n\n",
                        api_key.header_name
                    ));
                }
            }
            _ => {}
        }

        // Add body
        match self.body.body_type.as_str() {
            "json" => {
                if let Some(content) = &self.body.content {
                    code.push_str("    // JSON body\n");
                    code.push_str(&format!(
                        "    let body_template = r#\"{}\"#;\n",
                        content.to_string().replace("\\", "\\\\")
                    ));
                    code.push_str(
                        "    let body_str = interpolate_variables(body_template, state);\n",
                    );
                    code.push_str(
                        "    let body: serde_json::Value = serde_json::from_str(&body_str)?;\n",
                    );
                    code.push_str("    request = request.json(&body);\n\n");
                }
            }
            "form" => {
                if let Some(content) = &self.body.content {
                    code.push_str("    // Form body\n");
                    code.push_str(&format!(
                        "    let form_data: std::collections::HashMap<String, String> = serde_json::from_value(serde_json::json!({}))?\n",
                        content
                    ));
                    code.push_str("        .into_iter()\n");
                    code.push_str("        .map(|(k, v)| (k, interpolate_variables(&v, state)))\n");
                    code.push_str("        .collect();\n");
                    code.push_str("    request = request.form(&form_data);\n\n");
                }
            }
            "raw" => {
                if let Some(content) = &self.body.content {
                    code.push_str("    // Raw body\n");
                    code.push_str(&format!(
                        "    let raw_body = interpolate_variables(\"{}\", state);\n",
                        content.to_string().replace('"', "\\\"")
                    ));
                    code.push_str("    request = request.body(raw_body);\n\n");
                }
            }
            _ => {}
        }

        // Send request
        code.push_str("    // Send request\n");
        code.push_str("    let response = request.send().await?;\n");
        code.push_str("    let status = response.status();\n\n");

        // Status validation
        if let Some(validation) = &self.response.status_validation {
            code.push_str("    // Validate status code\n");
            code.push_str(&format!(
                "    if !validate_status_code(status.as_u16(), \"{}\") {{\n",
                validation
            ));
            code.push_str("        return Err(ActionError::HttpStatus {\n");
            code.push_str("            status: status.as_u16(),\n");
            code.push_str(&format!("            expected: \"{}\".to_string(),\n", validation));
            code.push_str("        });\n");
            code.push_str("    }\n\n");
        }

        // Parse response
        match self.response.response_type.as_str() {
            "json" => {
                code.push_str("    // Parse JSON response\n");
                code.push_str("    let result: serde_json::Value = response.json().await?;\n");

                // JSONPath extraction
                if let Some(json_path) = &self.response.json_path {
                    code.push_str(&format!(
                        "    let extracted = jsonpath_lib::select(&result, \"{}\")?\n",
                        json_path
                    ));
                    code.push_str("        .into_iter().next().cloned().unwrap_or(serde_json::Value::Null);\n");
                    code.push_str(&format!(
                        "    state.insert(\"{}\".to_string(), extracted.clone());\n",
                        self.standard.mapping.output_key
                    ));
                    code.push_str("    Ok(extracted)\n");
                } else {
                    code.push_str(&format!(
                        "    state.insert(\"{}\".to_string(), result.clone());\n",
                        self.standard.mapping.output_key
                    ));
                    code.push_str("    Ok(result)\n");
                }
            }
            "text" => {
                code.push_str("    // Parse text response\n");
                code.push_str("    let text = response.text().await?;\n");
                code.push_str("    let result = serde_json::json!(text);\n");
                code.push_str(&format!(
                    "    state.insert(\"{}\".to_string(), result.clone());\n",
                    self.standard.mapping.output_key
                ));
                code.push_str("    Ok(result)\n");
            }
            "binary" => {
                code.push_str("    // Get binary response\n");
                code.push_str("    let bytes = response.bytes().await?;\n");
                code.push_str("    let result = serde_json::json!({\n");
                code.push_str("        \"size\": bytes.len(),\n");
                code.push_str("        \"data\": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)\n");
                code.push_str("    });\n");
                code.push_str(&format!(
                    "    state.insert(\"{}\".to_string(), result.clone());\n",
                    self.standard.mapping.output_key
                ));
                code.push_str("    Ok(result)\n");
            }
            _ => {
                code.push_str("    let result: serde_json::Value = response.json().await?;\n");
                code.push_str("    Ok(result)\n");
            }
        }

        code.push_str("}\n\n");

        // Generate status validation helper
        code.push_str(&generate_status_validation_helper());

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        let mut imports = vec!["reqwest", "serde_json"];
        if self.response.json_path.is_some() {
            imports.push("jsonpath_lib");
        }
        if self.response.response_type == "binary" {
            imports.push("base64");
        }
        imports
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps =
            vec![("reqwest", "{ version = \"0.12\", features = [\"json\"] }"), ("serde_json", "1")];
        if self.response.json_path.is_some() {
            deps.push(("jsonpath-lib", "0.3"));
        }
        if self.response.response_type == "binary" {
            deps.push(("base64", "0.21"));
        }
        deps
    }
}

fn generate_status_validation_helper() -> &'static str {
    r#"
/// Validate HTTP status code against a pattern (e.g., "200-299", "200,201,204")
fn validate_status_code(status: u16, pattern: &str) -> bool {
    for part in pattern.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let range: Vec<&str> = part.split('-').collect();
            if range.len() == 2 {
                if let (Ok(start), Ok(end)) = (range[0].parse::<u16>(), range[1].parse::<u16>()) {
                    if status >= start && status <= end {
                        return true;
                    }
                }
            }
        } else if let Ok(expected) = part.parse::<u16>() {
            if status == expected {
                return true;
            }
        }
    }
    false
}
"#
}

// ============================================
// Set Node Code Generation
// ============================================

impl ActionNodeCodeGen for SetNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Set Node: {}\n", self.standard.name));
        code.push_str(&format!(
            "async fn {}_set(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n",
            node_id
        ));

        // Load environment variables if configured
        if let Some(env_vars) = &self.env_vars {
            if env_vars.load_from_env {
                code.push_str("    // Load environment variables\n");
                if let Some(prefix) = &env_vars.prefix {
                    code.push_str(&format!(
                        "    for (key, value) in std::env::vars().filter(|(k, _)| k.starts_with(\"{}\")) {{\n",
                        prefix
                    ));
                } else {
                    code.push_str("    for (key, value) in std::env::vars() {\n");
                }
                code.push_str("        state.insert(key, serde_json::json!(value));\n");
                code.push_str("    }\n\n");
            }
        }

        match self.mode {
            SetMode::Set => {
                code.push_str("    // Set variables\n");
                for var in &self.variables {
                    let value_code = match var.value_type.as_str() {
                        "expression" => {
                            format!(
                                "interpolate_variables(\"{}\", state)",
                                var.value.to_string().replace('"', "\\\"")
                            )
                        }
                        "json" => {
                            format!("serde_json::json!({})", var.value)
                        }
                        _ => {
                            format!("serde_json::json!({})", var.value)
                        }
                    };

                    if var.is_secret {
                        code.push_str(&format!("    // Secret: {}\n", var.key));
                        code.push_str(&format!(
                            "    state.insert(\"{}\".to_string(), {});\n",
                            var.key, value_code
                        ));
                        code.push_str(&format!(
                            "    tracing::debug!(key = \"{}\", \"Set secret variable (value masked)\");\n",
                            var.key
                        ));
                    } else {
                        code.push_str(&format!(
                            "    state.insert(\"{}\".to_string(), {});\n",
                            var.key, value_code
                        ));
                    }
                }
            }
            SetMode::Merge => {
                code.push_str("    // Merge variables (deep merge with existing)\n");
                for var in &self.variables {
                    code.push_str(&format!(
                        "    if let Some(existing) = state.get(\"{}\").cloned() {{\n",
                        var.key
                    ));
                    code.push_str(&format!(
                        "        let new_value = serde_json::json!({});\n",
                        var.value
                    ));
                    code.push_str("        let merged = deep_merge(&existing, &new_value);\n");
                    code.push_str(&format!(
                        "        state.insert(\"{}\".to_string(), merged);\n",
                        var.key
                    ));
                    code.push_str("    } else {\n");
                    code.push_str(&format!(
                        "        state.insert(\"{}\".to_string(), serde_json::json!({}));\n",
                        var.key, var.value
                    ));
                    code.push_str("    }\n");
                }
            }
            SetMode::Delete => {
                code.push_str("    // Delete variables\n");
                for var in &self.variables {
                    code.push_str(&format!("    state.remove(\"{}\");\n", var.key));
                }
            }
        }

        // Return the set variables as result
        code.push_str("\n    // Return set variables\n");
        code.push_str("    let result = serde_json::json!({\n");
        for (i, var) in self.variables.iter().enumerate() {
            let comma = if i < self.variables.len() - 1 { "," } else { "" };
            if var.is_secret {
                code.push_str(&format!("        \"{}\": \"***\"{}\n", var.key, comma));
            } else {
                code.push_str(&format!(
                    "        \"{}\": state.get(\"{}\").cloned().unwrap_or(serde_json::Value::Null){}\n",
                    var.key, var.key, comma
                ));
            }
        }
        code.push_str("    });\n");
        code.push_str(&format!(
            "    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        // Generate deep merge helper
        code.push_str(&generate_deep_merge_helper());

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["serde_json"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        vec![("serde_json", "1")]
    }
}

fn generate_deep_merge_helper() -> &'static str {
    r#"
/// Deep merge two JSON values
fn deep_merge(base: &serde_json::Value, overlay: &serde_json::Value) -> serde_json::Value {
    match (base, overlay) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) => {
            let mut result = base_map.clone();
            for (key, value) in overlay_map {
                if let Some(base_value) = result.get(key) {
                    result.insert(key.clone(), deep_merge(base_value, value));
                } else {
                    result.insert(key.clone(), value.clone());
                }
            }
            serde_json::Value::Object(result)
        }
        _ => overlay.clone(),
    }
}
"#
}

// ============================================
// Transform Node Code Generation
// ============================================

impl ActionNodeCodeGen for TransformNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Transform Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_transform(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n", node_id));

        // Get input from state
        code.push_str("    // Get input data\n");
        if let Some(input_mapping) = &self.standard.mapping.input_mapping {
            if let Some(input_key) = input_mapping.get("input") {
                code.push_str(&format!(
                    "    let input = state.get(\"{}\").cloned().unwrap_or(serde_json::Value::Null);\n\n",
                    input_key
                ));
            } else {
                code.push_str("    let input = state.clone();\n\n");
            }
        } else {
            code.push_str("    let input = serde_json::json!(state.clone());\n\n");
        }

        match self.transform_type {
            TransformType::Jsonpath => {
                code.push_str("    // JSONPath transformation\n");
                code.push_str(&format!(
                    "    let result = jsonpath_lib::select(&input, \"{}\")\n",
                    self.expression.replace('"', "\\\"")
                ));
                code.push_str("        .map_err(|e| ActionError::Transform(e.to_string()))?\n");
                code.push_str("        .into_iter()\n");
                code.push_str("        .cloned()\n");
                code.push_str("        .collect::<Vec<_>>();\n");
                code.push_str("    let result = if result.len() == 1 { result.into_iter().next().unwrap() } else { serde_json::json!(result) };\n");
            }
            TransformType::Jmespath => {
                code.push_str("    // JMESPath transformation\n");
                code.push_str(&format!(
                    "    let expr = jmespath::compile(\"{}\").map_err(|e| ActionError::Transform(e.to_string()))?;\n",
                    self.expression.replace('"', "\\\"")
                ));
                code.push_str("    let result = expr.search(&input).map_err(|e| ActionError::Transform(e.to_string()))?;\n");
                code.push_str("    let result = serde_json::to_value(&result)?;\n");
            }
            TransformType::Template => {
                code.push_str("    // Template transformation (handlebars-style)\n");
                code.push_str(&format!(
                    "    let template = \"{}\";\n",
                    self.expression.replace('"', "\\\"").replace('\n', "\\n")
                ));
                code.push_str(
                    "    let result = serde_json::json!(interpolate_variables(template, state));\n",
                );
            }
            TransformType::Javascript => {
                code.push_str("    // JavaScript transformation (sandboxed)\n");
                code.push_str(&format!(
                    "    let code = r#\"{}\"#;\n",
                    self.expression.replace("\\", "\\\\")
                ));
                code.push_str("    let result = execute_js_transform(code, &input)?;\n");
            }
        }

        // Apply built-in operations if any
        if let Some(operations) = &self.operations {
            for op in operations {
                code.push_str(&format!("    // Built-in operation: {}\n", op.op_type));
                match op.op_type.as_str() {
                    "pick" => {
                        if let Some(fields) = op.config.get("fields") {
                            code.push_str(&format!(
                                "    let result = pick_fields(&result, &serde_json::json!({}));\n",
                                fields
                            ));
                        }
                    }
                    "omit" => {
                        if let Some(fields) = op.config.get("fields") {
                            code.push_str(&format!(
                                "    let result = omit_fields(&result, &serde_json::json!({}));\n",
                                fields
                            ));
                        }
                    }
                    "flatten" => {
                        code.push_str("    let result = flatten_object(&result);\n");
                    }
                    "sort" => {
                        if let Some(key) = op.config.get("key") {
                            code.push_str(&format!(
                                "    let result = sort_array(&result, {});\n",
                                key
                            ));
                        }
                    }
                    "unique" => {
                        code.push_str("    let result = unique_array(&result);\n");
                    }
                    _ => {}
                }
            }
        }

        // Apply type coercion if configured
        if let Some(coercion) = &self.type_coercion {
            code.push_str(&format!(
                "    let result = coerce_type(&result, \"{}\");\n",
                coercion.target_type
            ));
        }

        code.push_str(&format!(
            "\n    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        let mut imports = vec!["serde_json"];
        match self.transform_type {
            TransformType::Jsonpath => imports.push("jsonpath_lib"),
            TransformType::Jmespath => imports.push("jmespath"),
            TransformType::Javascript => imports.push("quickjs_rs"),
            _ => {}
        }
        imports
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps = vec![("serde_json", "1")];
        match self.transform_type {
            TransformType::Jsonpath => deps.push(("jsonpath-lib", "0.3")),
            TransformType::Jmespath => deps.push(("jmespath", "0.3")),
            TransformType::Javascript => deps.push(("quick-js", "0.4")),
            _ => {}
        }
        deps
    }
}

// ============================================
// Switch Node Code Generation
// ============================================

impl ActionNodeCodeGen for SwitchNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Switch Node: {}\n", self.standard.name));
        code.push_str(&format!(
            "async fn {}_switch(state: &State) -> Result<&'static str, ActionError> {{\n",
            node_id
        ));

        // Check for expression mode
        if let Some(expr_mode) = &self.expression_mode {
            if expr_mode.enabled && !expr_mode.expression.is_empty() {
                code.push_str("    // Expression-based routing\n");
                code.push_str(&format!(
                    "    let branch = evaluate_switch_expression(\"{}\", state)?;\n",
                    expr_mode.expression.replace('"', "\\\"")
                ));
                code.push_str("    Ok(branch)\n");
                code.push_str("}\n\n");
                return code;
            }
        }

        // Condition-based routing
        match self.evaluation_mode {
            EvaluationMode::FirstMatch => {
                code.push_str("    // First match evaluation\n");
                for condition in &self.conditions {
                    code.push_str(&format!("    // Condition: {}\n", condition.name));
                    code.push_str(&format!(
                        "    if let Some(value) = get_nested_value(state, \"{}\") {{\n",
                        condition.field
                    ));

                    let comparison =
                        generate_condition_comparison(&condition.operator, &condition.value);
                    code.push_str(&format!("        if {} {{\n", comparison));
                    code.push_str(&format!(
                        "            tracing::debug!(branch = \"{}\", \"Switch condition matched\");\n",
                        condition.output_port
                    ));
                    code.push_str(&format!(
                        "            return Ok(\"{}\");\n",
                        condition.output_port
                    ));
                    code.push_str("        }\n");
                    code.push_str("    }\n\n");
                }
            }
            EvaluationMode::AllMatch => {
                code.push_str("    // All match evaluation (fan-out: all branches execute via direct edges)\n");
                code.push_str("    // Store matched branches in state for observability\n");
                code.push_str("    let mut matched_branches: Vec<String> = Vec::new();\n\n");
                for condition in &self.conditions {
                    code.push_str(&format!(
                        "    if let Some(value) = get_nested_value(state, \"{}\") {{\n",
                        condition.field
                    ));
                    let comparison =
                        generate_condition_comparison(&condition.operator, &condition.value);
                    code.push_str(&format!("        if {} {{\n", comparison));
                    code.push_str(&format!(
                        "            matched_branches.push(\"{}\".to_string());\n",
                        condition.output_port
                    ));
                    code.push_str("        }\n");
                    code.push_str("    }\n");
                }
                code.push_str("\n    // All connected branches execute regardless — fan-out via direct edges\n");
                code.push_str("    // matched_branches is stored for debugging/observability\n");
                code.push_str(
                    "    Ok(serde_json::to_string(&matched_branches).unwrap_or_default())\n",
                );
            }
        }

        // Default branch
        if let Some(default) = &self.default_branch {
            code.push_str(&format!("    // Default branch\n    Ok(\"{}\")\n", default));
        } else {
            code.push_str("    Err(ActionError::NoMatchingBranch { node: \"");
            code.push_str(node_id);
            code.push_str("\".to_string() })\n");
        }

        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["serde_json"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        vec![("serde_json", "1")]
    }
}

fn generate_condition_comparison(operator: &str, value: &Option<serde_json::Value>) -> String {
    let value_str = value.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "null".to_string());

    match operator {
        "eq" => format!("value == &serde_json::json!({})", value_str),
        "neq" => format!("value != &serde_json::json!({})", value_str),
        "gt" => format!("value.as_f64().map(|n| n > {}).unwrap_or(false)", value_str),
        "lt" => format!("value.as_f64().map(|n| n < {}).unwrap_or(false)", value_str),
        "gte" => format!("value.as_f64().map(|n| n >= {}).unwrap_or(false)", value_str),
        "lte" => format!("value.as_f64().map(|n| n <= {}).unwrap_or(false)", value_str),
        "contains" => format!("value.as_str().map(|s| s.contains({})).unwrap_or(false)", value_str),
        "startsWith" => {
            format!("value.as_str().map(|s| s.starts_with({})).unwrap_or(false)", value_str)
        }
        "endsWith" => {
            format!("value.as_str().map(|s| s.ends_with({})).unwrap_or(false)", value_str)
        }
        "matches" => format!(
            "value.as_str().map(|s| regex::Regex::new({}).map(|r| r.is_match(s)).unwrap_or(false)).unwrap_or(false)",
            value_str
        ),
        "in" => format!(
            "serde_json::json!({}).as_array().map(|arr| arr.contains(value)).unwrap_or(false)",
            value_str
        ),
        "empty" => "value.as_str().map(|s| s.is_empty()).unwrap_or(value.is_null())".to_string(),
        "exists" => "!value.is_null()".to_string(),
        _ => "false".to_string(),
    }
}

// ============================================
// Loop Node Code Generation
// ============================================

impl ActionNodeCodeGen for LoopNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Loop Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_loop(\n", node_id));
        code.push_str("    state: &mut State,\n");
        code.push_str("    executor: &WorkflowExecutor,\n");
        code.push_str(") -> Result<serde_json::Value, ActionError> {\n");

        match self.loop_type {
            LoopType::ForEach => {
                if let Some(for_each) = &self.for_each {
                    code.push_str(&format!(
                        "    // forEach loop over '{}'\n",
                        for_each.source_array
                    ));
                    code.push_str(&format!(
                        "    let source: Vec<serde_json::Value> = state.get(\"{}\")\n",
                        for_each.source_array
                    ));
                    code.push_str("        .and_then(|v| v.as_array())\n");
                    code.push_str("        .cloned()\n");
                    code.push_str("        .unwrap_or_default();\n\n");

                    if self.results.collect {
                        code.push_str("    let mut results = Vec::new();\n\n");
                    }

                    if self.parallel.enabled {
                        let batch_size = self.parallel.batch_size.unwrap_or(10);
                        code.push_str(&format!(
                            "    // Parallel execution with batch size {}\n",
                            batch_size
                        ));
                        code.push_str(&format!(
                            "    for chunk in source.chunks({}) {{\n",
                            batch_size
                        ));
                        code.push_str("        let futures: Vec<_> = chunk.iter().enumerate().map(|(idx, item)| {\n");
                        code.push_str("            let mut loop_state = state.clone();\n");
                        code.push_str(&format!(
                            "            loop_state.insert(\"{}\".to_string(), item.clone());\n",
                            for_each.item_var
                        ));
                        code.push_str(&format!(
                            "            loop_state.insert(\"{}\".to_string(), serde_json::json!(idx));\n",
                            for_each.index_var
                        ));
                        code.push_str("            executor.execute_loop_body(loop_state)\n");
                        code.push_str("        }).collect();\n\n");
                        code.push_str("        let chunk_results = futures::future::join_all(futures).await;\n");
                        if self.results.collect {
                            code.push_str("        results.extend(chunk_results.into_iter().filter_map(|r| r.ok()));\n");
                        }

                        if let Some(delay) = self.parallel.delay_between {
                            code.push_str(&format!(
                                "\n        tokio::time::sleep(std::time::Duration::from_millis({})).await;\n",
                                delay
                            ));
                        }
                        code.push_str("    }\n");
                    } else {
                        code.push_str("    // Sequential execution\n");
                        code.push_str("    for (idx, item) in source.iter().enumerate() {\n");
                        code.push_str(&format!(
                            "        state.insert(\"{}\".to_string(), item.clone());\n",
                            for_each.item_var
                        ));
                        code.push_str(&format!(
                            "        state.insert(\"{}\".to_string(), serde_json::json!(idx));\n",
                            for_each.index_var
                        ));
                        code.push_str("        let result = executor.execute_loop_body(state.clone()).await?;\n");
                        if self.results.collect {
                            code.push_str("        results.push(result);\n");
                        }
                        code.push_str("    }\n");
                    }
                }
            }
            LoopType::While => {
                if let Some(while_config) = &self.while_config {
                    code.push_str("    // while loop\n");
                    if self.results.collect {
                        code.push_str("    let mut results = Vec::new();\n");
                    }
                    code.push_str("    let mut iteration = 0;\n");
                    code.push_str("    const MAX_ITERATIONS: usize = 1000; // Safety limit\n\n");
                    code.push_str(&format!(
                        "    while evaluate_condition(\"{}\", state)? && iteration < MAX_ITERATIONS {{\n",
                        while_config.condition.replace('"', "\\\"")
                    ));
                    code.push_str(
                        "        let result = executor.execute_loop_body(state.clone()).await?;\n",
                    );
                    if self.results.collect {
                        code.push_str("        results.push(result);\n");
                    }
                    code.push_str("        iteration += 1;\n");
                    code.push_str("    }\n");
                }
            }
            LoopType::Times => {
                if let Some(times) = &self.times {
                    let count = match &times.count {
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::String(s) => format!(
                            "evaluate_expression(\"{}\", state)?.as_u64().unwrap_or(0) as usize",
                            s
                        ),
                        _ => "0".to_string(),
                    };
                    code.push_str(&format!("    // times loop ({} iterations)\n", count));
                    if self.results.collect {
                        code.push_str("    let mut results = Vec::new();\n");
                    }
                    code.push_str(&format!("    for i in 0..{} {{\n", count));
                    code.push_str(
                        "        state.insert(\"index\".to_string(), serde_json::json!(i));\n",
                    );
                    code.push_str(
                        "        let result = executor.execute_loop_body(state.clone()).await?;\n",
                    );
                    if self.results.collect {
                        code.push_str("        results.push(result);\n");
                    }
                    code.push_str("    }\n");
                }
            }
        }

        // Store results
        if self.results.collect {
            let agg_key = self
                .results
                .aggregation_key
                .as_deref()
                .unwrap_or(&self.standard.mapping.output_key);
            code.push_str(&format!(
                "\n    let result = serde_json::json!(results);\n\
                 state.insert(\"{}\".to_string(), result.clone());\n",
                agg_key
            ));
        } else {
            code.push_str("\n    let result = serde_json::Value::Null;\n");
        }

        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        let mut imports = vec!["serde_json"];
        if self.parallel.enabled {
            imports.push("futures");
        }
        imports
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps = vec![("serde_json", "1")];
        if self.parallel.enabled {
            deps.push(("futures", "0.3"));
        }
        deps
    }
}

// ============================================
// Merge Node Code Generation
// ============================================

impl ActionNodeCodeGen for MergeNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Merge Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_merge(\n", node_id));
        code.push_str("    branch_results: Vec<(String, serde_json::Value)>,\n");
        code.push_str("    state: &mut State,\n");
        code.push_str(") -> Result<serde_json::Value, ActionError> {\n");

        // Timeout handling
        if self.timeout.enabled {
            code.push_str(&format!("    // Timeout: {}ms\n", self.timeout.ms));
        }

        match self.mode {
            MergeMode::WaitAll => {
                code.push_str("    // Wait for all branches\n");
                code.push_str(
                    "    // Note: branch_results already contains all completed branches\n",
                );
            }
            MergeMode::WaitAny => {
                code.push_str("    // Wait for any branch (first to complete)\n");
                code.push_str("    if branch_results.is_empty() {\n");
                code.push_str("        return Err(ActionError::NoBranchCompleted);\n");
                code.push_str("    }\n");
            }
            MergeMode::WaitN => {
                let n = self.wait_count.unwrap_or(1);
                code.push_str(&format!("    // Wait for {} branches\n", n));
                code.push_str(&format!("    if branch_results.len() < {} {{\n", n));
                code.push_str(&format!(
                    "        return Err(ActionError::InsufficientBranches {{ expected: {}, got: branch_results.len() }});\n",
                    n
                ));
                code.push_str("    }\n");
            }
        }

        // Combine strategy
        code.push_str("\n    // Combine branch results\n");
        match self.combine_strategy {
            CombineStrategy::Array => {
                code.push_str("    let result: Vec<serde_json::Value> = branch_results.into_iter().map(|(_, v)| v).collect();\n");
                code.push_str("    let result = serde_json::json!(result);\n");
            }
            CombineStrategy::Object => {
                code.push_str("    let mut result_map = serde_json::Map::new();\n");
                code.push_str("    for (branch_key, value) in branch_results {\n");
                code.push_str("        result_map.insert(branch_key, value);\n");
                code.push_str("    }\n");
                code.push_str("    let result = serde_json::Value::Object(result_map);\n");
            }
            CombineStrategy::First => {
                code.push_str("    let result = branch_results.into_iter().next().map(|(_, v)| v).unwrap_or(serde_json::Value::Null);\n");
            }
            CombineStrategy::Last => {
                code.push_str("    let result = branch_results.into_iter().last().map(|(_, v)| v).unwrap_or(serde_json::Value::Null);\n");
            }
        }

        code.push_str(&format!(
            "\n    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["serde_json"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        vec![("serde_json", "1")]
    }
}

// ============================================
// Wait Node Code Generation
// ============================================

impl ActionNodeCodeGen for WaitNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Wait Node: {}\n", self.standard.name));
        code.push_str(&format!(
            "async fn {}_wait(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n",
            node_id
        ));

        match self.wait_type {
            WaitType::Fixed => {
                if let Some(fixed) = &self.fixed {
                    let ms = match fixed.unit.as_str() {
                        "ms" => fixed.duration,
                        "s" => fixed.duration * 1000,
                        "m" => fixed.duration * 60 * 1000,
                        "h" => fixed.duration * 60 * 60 * 1000,
                        _ => fixed.duration,
                    };
                    code.push_str(&format!(
                        "    // Fixed wait: {} {}\n",
                        fixed.duration, fixed.unit
                    ));
                    code.push_str(&format!(
                        "    tracing::debug!(duration_ms = {}, \"Waiting\");\n",
                        ms
                    ));
                    code.push_str(&format!(
                        "    tokio::time::sleep(std::time::Duration::from_millis({})).await;\n",
                        ms
                    ));
                }
            }
            WaitType::Until => {
                if let Some(until) = &self.until {
                    code.push_str("    // Wait until timestamp\n");
                    code.push_str(&format!(
                        "    let target = chrono::DateTime::parse_from_rfc3339(\"{}\")\n",
                        until.timestamp
                    ));
                    code.push_str(
                        "        .map_err(|e| ActionError::InvalidTimestamp(e.to_string()))?;\n",
                    );
                    code.push_str("    let now = chrono::Utc::now();\n");
                    code.push_str("    if target > now {\n");
                    code.push_str(
                        "        let duration = (target - now).to_std().unwrap_or_default();\n",
                    );
                    code.push_str(
                        "        tracing::debug!(until = %target, \"Waiting until timestamp\");\n",
                    );
                    code.push_str("        tokio::time::sleep(duration).await;\n");
                    code.push_str("    }\n");
                }
            }
            WaitType::Webhook => {
                if let Some(webhook) = &self.webhook {
                    code.push_str(&format!(
                        "    // Wait for webhook callback at '{}'\n",
                        webhook.path
                    ));
                    code.push_str(&format!("    // Timeout: {}ms\n", webhook.timeout));
                    code.push_str(
                        "    // Note: Webhook handler should signal completion via channel\n",
                    );
                    code.push_str("    let (tx, rx) = tokio::sync::oneshot::channel();\n");
                    code.push_str("    // Register webhook handler...\n");
                    code.push_str(&format!(
                        "    let result = tokio::time::timeout(\n\
                             std::time::Duration::from_millis({}),\n\
                             rx\n\
                         ).await\n\
                         .map_err(|_| ActionError::WebhookTimeout)?\n\
                         .map_err(|_| ActionError::WebhookCancelled)?;\n",
                        webhook.timeout
                    ));
                    code.push_str("    return Ok(result);\n");
                }
            }
            WaitType::Condition => {
                if let Some(condition) = &self.condition {
                    code.push_str("    // Poll until condition is true\n");
                    code.push_str(&format!(
                        "    let poll_interval = std::time::Duration::from_millis({});\n",
                        condition.poll_interval
                    ));
                    code.push_str(&format!(
                        "    let max_wait = std::time::Duration::from_millis({});\n",
                        condition.max_wait
                    ));
                    code.push_str("    let start = std::time::Instant::now();\n\n");
                    code.push_str("    loop {\n");
                    code.push_str(&format!(
                        "        if evaluate_condition(\"{}\", state)? {{\n",
                        condition.expression.replace('"', "\\\"")
                    ));
                    code.push_str("            tracing::debug!(\"Condition met\");\n");
                    code.push_str("            break;\n");
                    code.push_str("        }\n\n");
                    code.push_str("        if start.elapsed() >= max_wait {\n");
                    code.push_str("            return Err(ActionError::ConditionTimeout {\n");
                    code.push_str(&format!(
                        "                condition: \"{}\".to_string(),\n",
                        condition.expression.replace('"', "\\\"")
                    ));
                    code.push_str(&format!(
                        "                timeout_ms: {},\n",
                        condition.max_wait
                    ));
                    code.push_str("            });\n");
                    code.push_str("        }\n\n");
                    code.push_str("        tokio::time::sleep(poll_interval).await;\n");
                    code.push_str("    }\n");
                }
            }
        }

        code.push_str("\n    Ok(serde_json::json!({ \"waited\": true }))\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        let mut imports = vec!["tokio"];
        if self.wait_type == WaitType::Until {
            imports.push("chrono");
        }
        imports
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps = vec![("tokio", "{ version = \"1\", features = [\"time\"] }")];
        if self.wait_type == WaitType::Until {
            deps.push(("chrono", "0.4"));
        }
        deps
    }
}

// ============================================
// Code Node Code Generation
// ============================================

impl ActionNodeCodeGen for CodeNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Code Node: {}\n", self.standard.name));
        code.push_str(&format!(
            "async fn {}_code(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n",
            node_id
        ));

        // Sandbox configuration
        code.push_str("    // Sandbox configuration\n");
        code.push_str(&format!(
            "    let sandbox_config = SandboxConfig {{\n\
                     network_access: {},\n\
                     file_system_access: {},\n\
                     memory_limit_mb: {},\n\
                     time_limit_ms: {},\n\
                 }};\n\n",
            self.sandbox.network_access,
            self.sandbox.file_system_access,
            self.sandbox.memory_limit,
            self.sandbox.time_limit
        ));

        // Prepare input
        code.push_str("    // Prepare input for code execution\n");
        code.push_str("    let input = serde_json::json!(state.clone());\n\n");

        // Execute code in sandbox
        code.push_str("    // Execute code in sandbox\n");
        code.push_str(&format!(
            "    let code = r#\"{}\"#;\n",
            self.code.replace("\\", "\\\\").replace("#", "\\#")
        ));

        match self.language {
            CodeLanguage::Javascript => {
                code.push_str(
                    "    let result = execute_js_sandboxed(code, &input, &sandbox_config)?;\n",
                );
            }
            CodeLanguage::Typescript => {
                code.push_str("    // TypeScript is transpiled to JavaScript\n");
                code.push_str("    let js_code = transpile_typescript(code)?;\n");
                code.push_str(
                    "    let result = execute_js_sandboxed(&js_code, &input, &sandbox_config)?;\n",
                );
            }
        }

        code.push_str(&format!(
            "\n    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        // Generate sandbox execution helper
        code.push_str(&generate_sandbox_helper());

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["serde_json", "quick_js"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        vec![("serde_json", "1"), ("quick-js", "0.4")]
    }
}

fn generate_sandbox_helper() -> &'static str {
    r#"
/// Sandbox configuration for code execution
struct SandboxConfig {
    network_access: bool,
    file_system_access: bool,
    memory_limit_mb: u32,
    time_limit_ms: u64,
}

/// Execute JavaScript code in a sandboxed environment
fn execute_js_sandboxed(
    code: &str,
    input: &serde_json::Value,
    config: &SandboxConfig,
) -> Result<serde_json::Value, ActionError> {
    use quick_js::{Context, JsValue};
    
    let context = Context::new().map_err(|e| ActionError::SandboxInit(e.to_string()))?;
    
    // Set memory limit
    // Note: quick-js doesn't have direct memory limit API, this is a placeholder
    
    // Inject input as global variable
    let input_json = serde_json::to_string(input)?;
    context.eval(&format!("const input = {};", input_json))
        .map_err(|e| ActionError::CodeExecution(e.to_string()))?;
    
    // Disable network/fs if not allowed
    if !config.network_access {
        context.eval("globalThis.fetch = undefined; globalThis.XMLHttpRequest = undefined;")
            .map_err(|e| ActionError::CodeExecution(e.to_string()))?;
    }
    
    // Execute with timeout
    let result = std::thread::scope(|s| {
        let handle = s.spawn(|| {
            context.eval(code)
        });
        
        // Wait with timeout
        std::thread::sleep(std::time::Duration::from_millis(config.time_limit_ms));
        
        // Note: In production, would need proper timeout handling
        handle.join().unwrap_or(Err(quick_js::ExecutionError::Internal("Timeout".to_string())))
    });
    
    let js_result = result.map_err(|e| ActionError::CodeExecution(e.to_string()))?;
    
    // Convert JsValue to serde_json::Value
    js_value_to_json(js_result)
}

fn js_value_to_json(value: quick_js::JsValue) -> Result<serde_json::Value, ActionError> {
    use quick_js::JsValue;
    
    match value {
        JsValue::Null => Ok(serde_json::Value::Null),
        JsValue::Bool(b) => Ok(serde_json::json!(b)),
        JsValue::Int(i) => Ok(serde_json::json!(i)),
        JsValue::Float(f) => Ok(serde_json::json!(f)),
        JsValue::String(s) => Ok(serde_json::json!(s)),
        JsValue::Array(arr) => {
            let values: Result<Vec<_>, _> = arr.into_iter().map(js_value_to_json).collect();
            Ok(serde_json::json!(values?))
        }
        JsValue::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k, js_value_to_json(v)?);
            }
            Ok(serde_json::Value::Object(map))
        }
        _ => Ok(serde_json::Value::Null),
    }
}
"#
}

// ============================================
// Database Node Code Generation
// ============================================

impl ActionNodeCodeGen for DatabaseNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Database Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_database(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n", node_id));

        // Connection string (from state or direct)
        code.push_str("    // Get connection string\n");
        if let Some(cred_ref) = &self.connection.credential_ref {
            code.push_str(&format!(
                "    let connection_string = state.get(\"{}\")\n\
                     .and_then(|v| v.as_str())\n\
                     .ok_or_else(|| ActionError::MissingCredential(\"{}\".to_string()))?\n\
                     .to_string();\n\n",
                cred_ref, cred_ref
            ));
        } else {
            code.push_str(&format!(
                "    let connection_string = interpolate_variables(\"{}\", state);\n\n",
                self.connection.connection_string.replace('"', "\\\"")
            ));
        }

        match self.db_type {
            DatabaseType::Postgresql | DatabaseType::Mysql | DatabaseType::Sqlite => {
                code.push_str(&generate_sql_code(node_id, self));
            }
            DatabaseType::Mongodb => {
                code.push_str(&generate_mongodb_code(node_id, self));
            }
            DatabaseType::Redis => {
                code.push_str(&generate_redis_code(node_id, self));
            }
        }

        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        match self.db_type {
            DatabaseType::Postgresql | DatabaseType::Mysql | DatabaseType::Sqlite => {
                vec!["sqlx", "serde_json"]
            }
            DatabaseType::Mongodb => {
                vec!["mongodb", "serde_json"]
            }
            DatabaseType::Redis => {
                vec!["redis", "serde_json"]
            }
        }
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        match self.db_type {
            DatabaseType::Postgresql => {
                vec![
                    ("sqlx", "{ version = \"0.7\", features = [\"runtime-tokio\", \"postgres\"] }"),
                    ("serde_json", "1"),
                ]
            }
            DatabaseType::Mysql => {
                vec![
                    ("sqlx", "{ version = \"0.7\", features = [\"runtime-tokio\", \"mysql\"] }"),
                    ("serde_json", "1"),
                ]
            }
            DatabaseType::Sqlite => {
                vec![
                    ("sqlx", "{ version = \"0.7\", features = [\"runtime-tokio\", \"sqlite\"] }"),
                    ("serde_json", "1"),
                ]
            }
            DatabaseType::Mongodb => {
                vec![("mongodb", "2"), ("serde_json", "1")]
            }
            DatabaseType::Redis => {
                vec![
                    ("redis", "{ version = \"0.24\", features = [\"tokio-comp\"] }"),
                    ("serde_json", "1"),
                ]
            }
        }
    }
}

fn generate_sql_code(_node_id: &str, config: &DatabaseNodeConfig) -> String {
    let mut code = String::new();

    let db_type = match config.db_type {
        DatabaseType::Postgresql => "Postgres",
        DatabaseType::Mysql => "MySql",
        DatabaseType::Sqlite => "Sqlite",
        _ => "Postgres",
    };

    // Create connection pool
    let pool_size = config.connection.pool_size.unwrap_or(5);
    code.push_str(&format!("    // Create {} connection pool\n", db_type));
    code.push_str(&format!("    let pool = sqlx::{}Pool::connect_with(\n", db_type));
    code.push_str(&format!(
        "        sqlx::{}::{}ConnectOptions::from_str(&connection_string)?\n",
        db_type.to_lowercase(),
        db_type
    ));
    code.push_str(&format!("            .max_connections({})\n", pool_size));
    code.push_str("    ).await?;\n\n");

    if let Some(sql) = &config.sql {
        code.push_str(&format!("    // SQL operation: {}\n", sql.operation));

        match sql.operation.as_str() {
            "query" => {
                code.push_str(&format!(
                    "    let query = \"{}\";\n",
                    sql.query.replace('"', "\\\"")
                ));
                code.push_str("    let rows = sqlx::query(query)\n");

                // Bind parameters
                if let Some(params) = &sql.params {
                    for (_key, value) in params {
                        code.push_str(&format!("        .bind(serde_json::json!({}))\n", value));
                    }
                }

                code.push_str("        .fetch_all(&pool).await?;\n\n");
                code.push_str("    // Convert rows to JSON\n");
                code.push_str("    let result: Vec<serde_json::Value> = rows.iter().map(|row| {\n");
                code.push_str(
                    "        // Note: Actual implementation would use row.get() for each column\n",
                );
                code.push_str("        serde_json::json!({})\n");
                code.push_str("    }).collect();\n");
                code.push_str("    let result = serde_json::json!(result);\n");
            }
            "insert" | "update" | "delete" | "upsert" => {
                code.push_str(&format!(
                    "    let query = \"{}\";\n",
                    sql.query.replace('"', "\\\"")
                ));
                code.push_str("    let result = sqlx::query(query)\n");

                if let Some(params) = &sql.params {
                    for (_, value) in params {
                        code.push_str(&format!("        .bind(serde_json::json!({}))\n", value));
                    }
                }

                code.push_str("        .execute(&pool).await?;\n\n");
                code.push_str("    let result = serde_json::json!({\n");
                code.push_str("        \"rows_affected\": result.rows_affected()\n");
                code.push_str("    });\n");
            }
            _ => {
                code.push_str("    let result = serde_json::Value::Null;\n");
            }
        }
    } else {
        code.push_str("    let result = serde_json::Value::Null;\n");
    }

    code.push_str(&format!(
        "\n    state.insert(\"{}\".to_string(), result.clone());\n",
        config.standard.mapping.output_key
    ));
    code.push_str("    Ok(result)\n");

    code
}

fn generate_mongodb_code(_node_id: &str, config: &DatabaseNodeConfig) -> String {
    let mut code = String::new();

    code.push_str("    // Create MongoDB client\n");
    code.push_str("    let client = mongodb::Client::with_uri_str(&connection_string).await?;\n");
    code.push_str(
        "    let db = client.default_database().ok_or_else(|| ActionError::NoDatabase)?;\n\n",
    );

    if let Some(mongo) = &config.mongodb {
        code.push_str(&format!(
            "    let collection = db.collection::<mongodb::bson::Document>(\"{}\");\n\n",
            mongo.collection
        ));

        match mongo.operation.as_str() {
            "find" => {
                let filter = mongo
                    .filter
                    .as_ref()
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                code.push_str(&format!("    let filter = mongodb::bson::doc! {};\n", filter));
                code.push_str("    let cursor = collection.find(filter, None).await?;\n");
                code.push_str("    let docs: Vec<_> = cursor.try_collect().await?;\n");
                code.push_str("    let result = serde_json::to_value(&docs)?;\n");
            }
            "findOne" => {
                let filter = mongo
                    .filter
                    .as_ref()
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                code.push_str(&format!("    let filter = mongodb::bson::doc! {};\n", filter));
                code.push_str("    let doc = collection.find_one(filter, None).await?;\n");
                code.push_str("    let result = serde_json::to_value(&doc)?;\n");
            }
            "insert" => {
                let doc = mongo
                    .document
                    .as_ref()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                code.push_str(&format!("    let doc = mongodb::bson::doc! {};\n", doc));
                code.push_str("    let result = collection.insert_one(doc, None).await?;\n");
                code.push_str("    let result = serde_json::json!({ \"inserted_id\": result.inserted_id.to_string() });\n");
            }
            "update" => {
                let filter = mongo
                    .filter
                    .as_ref()
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let doc = mongo
                    .document
                    .as_ref()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                code.push_str(&format!("    let filter = mongodb::bson::doc! {};\n", filter));
                code.push_str(&format!(
                    "    let update = mongodb::bson::doc! {{ \"$set\": {} }};\n",
                    doc
                ));
                code.push_str(
                    "    let result = collection.update_many(filter, update, None).await?;\n",
                );
                code.push_str("    let result = serde_json::json!({\n");
                code.push_str("        \"matched_count\": result.matched_count,\n");
                code.push_str("        \"modified_count\": result.modified_count\n");
                code.push_str("    });\n");
            }
            "delete" => {
                let filter = mongo
                    .filter
                    .as_ref()
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                code.push_str(&format!("    let filter = mongodb::bson::doc! {};\n", filter));
                code.push_str("    let result = collection.delete_many(filter, None).await?;\n");
                code.push_str("    let result = serde_json::json!({ \"deleted_count\": result.deleted_count });\n");
            }
            _ => {
                code.push_str("    let result = serde_json::Value::Null;\n");
            }
        }
    } else {
        code.push_str("    let result = serde_json::Value::Null;\n");
    }

    code.push_str(&format!(
        "\n    state.insert(\"{}\".to_string(), result.clone());\n",
        config.standard.mapping.output_key
    ));
    code.push_str("    Ok(result)\n");

    code
}

fn generate_redis_code(_node_id: &str, config: &DatabaseNodeConfig) -> String {
    let mut code = String::new();

    code.push_str("    // Create Redis client\n");
    code.push_str("    let client = redis::Client::open(connection_string.as_str())?;\n");
    code.push_str("    let mut con = client.get_async_connection().await?;\n\n");

    if let Some(redis) = &config.redis {
        code.push_str(&format!("    // Redis operation: {}\n", redis.operation));

        match redis.operation.as_str() {
            "get" => {
                code.push_str(&format!(
                    "    let value: Option<String> = redis::cmd(\"GET\").arg(\"{}\").query_async(&mut con).await?;\n",
                    redis.key
                ));
                code.push_str("    let result = serde_json::json!(value);\n");
            }
            "set" => {
                let value = redis
                    .value
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string());
                code.push_str(&format!(
                    "    let _: () = redis::cmd(\"SET\").arg(\"{}\").arg({}).query_async(&mut con).await?;\n",
                    redis.key, value
                ));
                if let Some(ttl) = redis.ttl {
                    code.push_str(&format!(
                        "    let _: () = redis::cmd(\"EXPIRE\").arg(\"{}\").arg({}).query_async(&mut con).await?;\n",
                        redis.key, ttl
                    ));
                }
                code.push_str("    let result = serde_json::json!({ \"ok\": true });\n");
            }
            "del" => {
                code.push_str(&format!(
                    "    let deleted: i64 = redis::cmd(\"DEL\").arg(\"{}\").query_async(&mut con).await?;\n",
                    redis.key
                ));
                code.push_str("    let result = serde_json::json!({ \"deleted\": deleted });\n");
            }
            "hget" => {
                let field = redis.value.as_ref().and_then(|v| v.as_str()).unwrap_or("field");
                code.push_str(&format!(
                    "    let value: Option<String> = redis::cmd(\"HGET\").arg(\"{}\").arg(\"{}\").query_async(&mut con).await?;\n",
                    redis.key, field
                ));
                code.push_str("    let result = serde_json::json!(value);\n");
            }
            "hset" => {
                let value =
                    redis.value.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
                code.push_str(&format!(
                    "    let _: () = redis::cmd(\"HSET\").arg(\"{}\").arg({}).query_async(&mut con).await?;\n",
                    redis.key, value
                ));
                code.push_str("    let result = serde_json::json!({ \"ok\": true });\n");
            }
            "lpush" => {
                let value = redis
                    .value
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string());
                code.push_str(&format!(
                    "    let len: i64 = redis::cmd(\"LPUSH\").arg(\"{}\").arg({}).query_async(&mut con).await?;\n",
                    redis.key, value
                ));
                code.push_str("    let result = serde_json::json!({ \"length\": len });\n");
            }
            "rpop" => {
                code.push_str(&format!(
                    "    let value: Option<String> = redis::cmd(\"RPOP\").arg(\"{}\").query_async(&mut con).await?;\n",
                    redis.key
                ));
                code.push_str("    let result = serde_json::json!(value);\n");
            }
            _ => {
                code.push_str("    let result = serde_json::Value::Null;\n");
            }
        }
    } else {
        code.push_str("    let result = serde_json::Value::Null;\n");
    }

    code.push_str(&format!(
        "\n    state.insert(\"{}\".to_string(), result.clone());\n",
        config.standard.mapping.output_key
    ));
    code.push_str("    Ok(result)\n");

    code
}

// ============================================
// Email Node Code Generation
// ============================================

impl ActionNodeCodeGen for EmailNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Email Node: {}\n", self.standard.name));
        code.push_str(&format!(
            "async fn {}_email(state: &mut State) -> Result<serde_json::Value, ActionError> {{\n",
            node_id
        ));

        match self.mode {
            EmailMode::Monitor => {
                code.push_str(&generate_imap_monitor_code(node_id, self));
            }
            EmailMode::Send => {
                code.push_str(&generate_smtp_send_code(node_id, self));
            }
        }

        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        match self.mode {
            EmailMode::Monitor => vec!["imap", "native_tls", "mailparse", "serde_json"],
            EmailMode::Send => vec!["lettre", "serde_json"],
        }
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        match self.mode {
            EmailMode::Monitor => vec![
                ("imap", "3"),
                ("native-tls", "0.2"),
                ("mailparse", "0.14"),
                ("serde_json", "1"),
            ],
            EmailMode::Send => vec![
                (
                    "lettre",
                    "{ version = \"0.11\", features = [\"tokio1-native-tls\", \"builder\"] }",
                ),
                ("serde_json", "1"),
            ],
        }
    }
}

fn generate_imap_monitor_code(_node_id: &str, config: &EmailNodeConfig) -> String {
    let mut code = String::new();

    if let Some(imap) = &config.imap {
        code.push_str("    // IMAP email monitoring\n");
        code.push_str(&format!(
            "    let host = interpolate_variables(\"{}\", state);\n",
            imap.host.replace('"', "\\\"")
        ));
        code.push_str(&format!(
            "    let username = interpolate_variables(\"{}\", state);\n",
            imap.username.replace('"', "\\\"")
        ));
        code.push_str(&format!(
            "    let password = interpolate_variables(\"{}\", state);\n",
            imap.password.replace('"', "\\\"")
        ));

        // Create TLS connector
        if imap.secure {
            code.push_str("\n    // Create TLS connection\n");
            code.push_str("    let tls = native_tls::TlsConnector::builder().build()?;\n");
            code.push_str(&format!(
                "    let client = imap::connect((\"{}\", {}), &host, &tls)?;\n",
                imap.host, imap.port
            ));
        } else {
            code.push_str("\n    // Create plain connection\n");
            code.push_str(&format!(
                "    let client = imap::connect_insecure((\"{}\", {}))?;\n",
                imap.host, imap.port
            ));
        }

        // Login
        code.push_str("\n    // Login\n");
        code.push_str("    let mut session = client.login(&username, &password)\n");
        code.push_str("        .map_err(|e| ActionError::EmailAuth(e.0.to_string()))?;\n");

        // Select folder
        code.push_str(&format!("\n    // Select folder: {}\n", imap.folder));
        code.push_str(&format!("    session.select(\"{}\")?;\n", imap.folder));

        // Build search criteria
        code.push_str("\n    // Build search criteria\n");
        let mut search_criteria = Vec::new();

        if let Some(filters) = &config.filters {
            if filters.unread_only {
                search_criteria.push("UNSEEN".to_string());
            }
            if let Some(from) = &filters.from {
                search_criteria.push(format!("FROM \"{}\"", from));
            }
            if let Some(subject) = &filters.subject {
                search_criteria.push(format!("SUBJECT \"{}\"", subject));
            }
            if let Some(date_from) = &filters.date_from {
                search_criteria.push(format!("SINCE \"{}\"", date_from));
            }
            if let Some(date_to) = &filters.date_to {
                search_criteria.push(format!("BEFORE \"{}\"", date_to));
            }
        }

        let search_str =
            if search_criteria.is_empty() { "ALL".to_string() } else { search_criteria.join(" ") };

        code.push_str(&format!("    let search_result = session.search(\"{}\")?;\n", search_str));

        // Fetch messages
        code.push_str("\n    // Fetch messages\n");
        code.push_str("    let mut emails = Vec::new();\n");
        code.push_str("    for uid in search_result.iter() {\n");
        code.push_str(
            "        let messages = session.fetch(uid.to_string(), \"(RFC822 ENVELOPE)\")?;\n",
        );
        code.push_str("        for message in messages.iter() {\n");
        code.push_str("            if let Some(body) = message.body() {\n");
        code.push_str("                let parsed = mailparse::parse_mail(body)?;\n");
        code.push_str("                let email_data = serde_json::json!({\n");
        code.push_str("                    \"uid\": uid,\n");
        code.push_str("                    \"from\": parsed.headers.iter()\n");
        code.push_str("                        .find(|h| h.get_key() == \"From\")\n");
        code.push_str("                        .map(|h| h.get_value()),\n");
        code.push_str("                    \"to\": parsed.headers.iter()\n");
        code.push_str("                        .find(|h| h.get_key() == \"To\")\n");
        code.push_str("                        .map(|h| h.get_value()),\n");
        code.push_str("                    \"subject\": parsed.headers.iter()\n");
        code.push_str("                        .find(|h| h.get_key() == \"Subject\")\n");
        code.push_str("                        .map(|h| h.get_value()),\n");
        code.push_str("                    \"date\": parsed.headers.iter()\n");
        code.push_str("                        .find(|h| h.get_key() == \"Date\")\n");
        code.push_str("                        .map(|h| h.get_value()),\n");
        code.push_str("                    \"body\": parsed.get_body()?,\n");
        code.push_str("                    \"attachments\": parsed.subparts.iter()\n");
        code.push_str("                        .filter(|p| p.get_content_disposition().disposition == mailparse::DispositionType::Attachment)\n");
        code.push_str("                        .map(|p| serde_json::json!({\n");
        code.push_str("                            \"filename\": p.get_content_disposition().params.get(\"filename\"),\n");
        code.push_str("                            \"content_type\": p.ctype.mimetype.clone(),\n");
        code.push_str("                            \"size\": p.get_body_raw()?.len()\n");
        code.push_str("                        }))\n");
        code.push_str("                        .collect::<Vec<_>>()\n");
        code.push_str("                });\n");
        code.push_str("                emails.push(email_data);\n");

        // Mark as read if configured
        if imap.mark_as_read {
            code.push_str("\n                // Mark as read\n");
            code.push_str(
                "                session.store(uid.to_string(), \"+FLAGS (\\\\Seen)\")?;\n",
            );
        }

        code.push_str("            }\n");
        code.push_str("        }\n");
        code.push_str("    }\n");

        // Logout
        code.push_str("\n    // Logout\n");
        code.push_str("    session.logout()?;\n");

        // Return result
        code.push_str("\n    let result = serde_json::json!({\n");
        code.push_str("        \"count\": emails.len(),\n");
        code.push_str("        \"emails\": emails\n");
        code.push_str("    });\n");
        code.push_str(&format!(
            "    state.insert(\"{}\".to_string(), result.clone());\n",
            config.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
    } else {
        code.push_str("    // No IMAP configuration provided\n");
        code.push_str("    Ok(serde_json::Value::Null)\n");
    }

    code
}

fn generate_smtp_send_code(_node_id: &str, config: &EmailNodeConfig) -> String {
    let mut code = String::new();

    if let Some(smtp) = &config.smtp {
        code.push_str("    // SMTP email sending\n");
        code.push_str("    use lettre::{Message, SmtpTransport, Transport};\n");
        code.push_str("    use lettre::transport::smtp::authentication::Credentials;\n");
        code.push_str("    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};\n\n");

        // Get SMTP configuration
        code.push_str(&format!(
            "    let host = interpolate_variables(\"{}\", state);\n",
            smtp.host.replace('"', "\\\"")
        ));
        code.push_str(&format!(
            "    let username = interpolate_variables(\"{}\", state);\n",
            smtp.username.replace('"', "\\\"")
        ));
        code.push_str(&format!(
            "    let password = interpolate_variables(\"{}\", state);\n",
            smtp.password.replace('"', "\\\"")
        ));
        code.push_str(&format!(
            "    let from_email = interpolate_variables(\"{}\", state);\n",
            smtp.from_email.replace('"', "\\\"")
        ));

        if let Some(from_name) = &smtp.from_name {
            code.push_str(&format!(
                "    let from_name = interpolate_variables(\"{}\", state);\n",
                from_name.replace('"', "\\\"")
            ));
        }

        // Get recipients
        if let Some(recipients) = &config.recipients {
            code.push_str(&format!(
                "\n    let to = interpolate_variables(\"{}\", state);\n",
                recipients.to.replace('"', "\\\"")
            ));

            if let Some(cc) = &recipients.cc {
                code.push_str(&format!(
                    "    let cc = interpolate_variables(\"{}\", state);\n",
                    cc.replace('"', "\\\"")
                ));
            }

            if let Some(bcc) = &recipients.bcc {
                code.push_str(&format!(
                    "    let bcc = interpolate_variables(\"{}\", state);\n",
                    bcc.replace('"', "\\\"")
                ));
            }
        }

        // Get content
        if let Some(content) = &config.content {
            code.push_str(&format!(
                "\n    let subject = interpolate_variables(\"{}\", state);\n",
                content.subject.replace('"', "\\\"")
            ));
            code.push_str(&format!(
                "    let body = interpolate_variables(\"{}\", state);\n",
                content.body.replace('"', "\\\"").replace('\n', "\\n")
            ));
        }

        // Build message
        code.push_str("\n    // Build email message\n");

        if smtp.from_name.is_some() {
            // from_name variable was generated earlier, use it in the format
            code.push_str("    let from = format!(\"{} <{}>\", from_name, from_email).parse()?;\n");
        } else {
            code.push_str("    let from = from_email.parse()?;\n");
        }

        code.push_str("    let mut message_builder = Message::builder()\n");
        code.push_str("        .from(from)\n");

        // Add recipients
        code.push_str("        .to(to.parse()?);\n");

        if config.recipients.as_ref().and_then(|r| r.cc.as_ref()).is_some() {
            code.push_str("    message_builder = message_builder.cc(cc.parse()?);\n");
        }

        if config.recipients.as_ref().and_then(|r| r.bcc.as_ref()).is_some() {
            code.push_str("    message_builder = message_builder.bcc(bcc.parse()?);\n");
        }

        code.push_str("    message_builder = message_builder.subject(&subject);\n");

        // Set body based on type
        if let Some(content) = &config.content {
            match content.body_type {
                EmailBodyType::Html => {
                    code.push_str("\n    // HTML body\n");
                    code.push_str("    let body_part = SinglePart::builder()\n");
                    code.push_str("        .header(ContentType::TEXT_HTML)\n");
                    code.push_str("        .body(body);\n");
                }
                EmailBodyType::Text => {
                    code.push_str("\n    // Plain text body\n");
                    code.push_str("    let body_part = SinglePart::builder()\n");
                    code.push_str("        .header(ContentType::TEXT_PLAIN)\n");
                    code.push_str("        .body(body);\n");
                }
            }
        } else {
            code.push_str("    let body_part = SinglePart::builder()\n");
            code.push_str("        .header(ContentType::TEXT_PLAIN)\n");
            code.push_str("        .body(String::new());\n");
        }

        // Handle attachments
        if let Some(attachments) = &config.attachments {
            if !attachments.is_empty() {
                code.push_str("\n    // Build multipart message with attachments\n");
                code.push_str(
                    "    let mut multipart = MultiPart::mixed().singlepart(body_part);\n\n",
                );

                for (i, attachment) in attachments.iter().enumerate() {
                    code.push_str(&format!(
                        "    // Attachment {}: {}\n",
                        i + 1,
                        attachment.filename
                    ));
                    code.push_str(&format!(
                        "    if let Some(attachment_data) = state.get(\"{}\") {{\n",
                        attachment.state_key
                    ));
                    code.push_str(
                        "        let data = if let Some(s) = attachment_data.as_str() {\n",
                    );
                    code.push_str("            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s)?\n");
                    code.push_str("        } else {\n");
                    code.push_str("            serde_json::to_vec(attachment_data)?\n");
                    code.push_str("        };\n");

                    let mime_type =
                        attachment.mime_type.as_deref().unwrap_or("application/octet-stream");
                    code.push_str(&format!(
                        "        let attachment = Attachment::new(\"{}\".to_string())\n",
                        attachment.filename
                    ));
                    code.push_str(&format!(
                        "            .body(data, \"{}\".parse()?);\n",
                        mime_type
                    ));
                    code.push_str("        multipart = multipart.singlepart(attachment);\n");
                    code.push_str("    }\n\n");
                }

                code.push_str("    let email = message_builder.multipart(multipart)?;\n");
            } else {
                code.push_str("\n    let email = message_builder.singlepart(body_part)?;\n");
            }
        } else {
            code.push_str("\n    let email = message_builder.singlepart(body_part)?;\n");
        }

        // Create SMTP transport
        code.push_str("\n    // Create SMTP transport\n");
        code.push_str("    let creds = Credentials::new(username, password);\n");

        if smtp.secure {
            code.push_str(&format!(
                "    let mailer = SmtpTransport::relay(&host)?\n\
                     .port({})\n\
                     .credentials(creds)\n\
                     .build();\n",
                smtp.port
            ));
        } else {
            code.push_str(&format!(
                "    let mailer = SmtpTransport::builder_dangerous(&host)\n\
                     .port({})\n\
                     .credentials(creds)\n\
                     .build();\n",
                smtp.port
            ));
        }

        // Send email
        code.push_str("\n    // Send email\n");
        code.push_str("    let response = mailer.send(&email)?;\n");
        code.push_str("    tracing::info!(\"Email sent successfully\");\n");

        // Return result
        code.push_str("\n    let result = serde_json::json!({\n");
        code.push_str("        \"success\": true,\n");
        code.push_str("        \"message_id\": response.message().next().map(|s| s.to_string())\n");
        code.push_str("    });\n");
        code.push_str(&format!(
            "    state.insert(\"{}\".to_string(), result.clone());\n",
            config.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
    } else {
        code.push_str("    // No SMTP configuration provided\n");
        code.push_str("    Ok(serde_json::Value::Null)\n");
    }

    code
}

// ============================================
// Notification Node Code Generation
// ============================================

impl ActionNodeCodeGen for NotificationNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// Notification Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_notification(\n", node_id));
        code.push_str("    state: &mut State,\n");
        code.push_str("    client: &reqwest::Client,\n");
        code.push_str(") -> Result<serde_json::Value, ActionError> {\n");

        // Get webhook URL with variable interpolation
        code.push_str(&format!(
            "    let webhook_url = interpolate_variables(\"{}\", state);\n",
            self.webhook_url.replace('"', "\\\"")
        ));
        code.push_str("    tracing::debug!(channel = \"{}\", \"Sending notification\");\n\n");

        // Build the message payload based on channel
        match self.channel {
            NotificationChannel::Slack => {
                code.push_str(&generate_slack_payload(self));
            }
            NotificationChannel::Discord => {
                code.push_str(&generate_discord_payload(self));
            }
            NotificationChannel::Teams => {
                code.push_str(&generate_teams_payload(self));
            }
            NotificationChannel::Webhook => {
                code.push_str(&generate_generic_webhook_payload(self));
            }
        }

        // Send the request
        code.push_str("\n    // Send notification\n");
        code.push_str("    let response = client.post(&webhook_url)\n");
        code.push_str("        .header(\"Content-Type\", \"application/json\")\n");
        code.push_str("        .json(&payload)\n");
        code.push_str("        .send()\n");
        code.push_str("        .await\n");
        code.push_str("        .map_err(|e| ActionError::NotificationSend(e.to_string()))?;\n\n");

        // Check response status
        code.push_str("    let status = response.status();\n");
        code.push_str("    if !status.is_success() {\n");
        code.push_str("        let error_body = response.text().await.unwrap_or_default();\n");
        code.push_str("        return Err(ActionError::NotificationSend(format!(\n");
        code.push_str("            \"Notification failed with status {}: {}\",\n");
        code.push_str("            status, error_body\n");
        code.push_str("        )));\n");
        code.push_str("    }\n\n");

        // Return result
        code.push_str("    let result = serde_json::json!({\n");
        code.push_str("        \"success\": true,\n");
        code.push_str(&format!("        \"channel\": \"{:?}\",\n", self.channel));
        code.push_str("        \"status\": status.as_u16()\n");
        code.push_str("    });\n");
        code.push_str(&format!(
            "    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["reqwest", "serde_json"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        vec![("reqwest", "{ version = \"0.12\", features = [\"json\"] }"), ("serde_json", "1")]
    }
}

fn generate_slack_payload(config: &NotificationNodeConfig) -> String {
    let mut code = String::new();

    code.push_str("    // Build Slack payload\n");
    code.push_str(&format!(
        "    let text = interpolate_variables(\"{}\", state);\n",
        config.message.text.replace('"', "\\\"").replace('\n', "\\n")
    ));

    // Check if using blocks
    if let Some(blocks) = &config.message.blocks {
        if !blocks.is_empty() {
            code.push_str("    let blocks = serde_json::json!(");
            code.push_str(&serde_json::to_string(blocks).unwrap_or_else(|_| "[]".to_string()));
            code.push_str(");\n");
            code.push_str("    let mut payload = serde_json::json!({\n");
            code.push_str("        \"text\": text,\n");
            code.push_str("        \"blocks\": blocks\n");
            code.push_str("    });\n");
        } else {
            code.push_str("    let mut payload = serde_json::json!({ \"text\": text });\n");
        }
    } else {
        // Format text based on message format
        match config.message.format {
            MessageFormat::Markdown => {
                code.push_str("    // Slack uses mrkdwn format\n");
                code.push_str("    let mut payload = serde_json::json!({\n");
                code.push_str("        \"text\": text,\n");
                code.push_str("        \"mrkdwn\": true\n");
                code.push_str("    });\n");
            }
            _ => {
                code.push_str("    let mut payload = serde_json::json!({ \"text\": text });\n");
            }
        }
    }

    // Add optional fields
    if let Some(username) = &config.username {
        code.push_str(&format!(
            "    payload[\"username\"] = serde_json::json!(interpolate_variables(\"{}\", state));\n",
            username.replace('"', "\\\"")
        ));
    }

    if let Some(icon_url) = &config.icon_url {
        code.push_str(&format!(
            "    payload[\"icon_url\"] = serde_json::json!(interpolate_variables(\"{}\", state));\n",
            icon_url.replace('"', "\\\"")
        ));
    }

    if let Some(channel) = &config.target_channel {
        code.push_str(&format!(
            "    payload[\"channel\"] = serde_json::json!(interpolate_variables(\"{}\", state));\n",
            channel.replace('"', "\\\"")
        ));
    }

    code
}

fn generate_discord_payload(config: &NotificationNodeConfig) -> String {
    let mut code = String::new();

    code.push_str("    // Build Discord payload\n");
    code.push_str(&format!(
        "    let content = interpolate_variables(\"{}\", state);\n",
        config.message.text.replace('"', "\\\"").replace('\n', "\\n")
    ));

    // Check if using embeds (blocks)
    if let Some(blocks) = &config.message.blocks {
        if !blocks.is_empty() {
            code.push_str("    let embeds = serde_json::json!(");
            code.push_str(&serde_json::to_string(blocks).unwrap_or_else(|_| "[]".to_string()));
            code.push_str(");\n");
            code.push_str("    let mut payload = serde_json::json!({\n");
            code.push_str("        \"content\": content,\n");
            code.push_str("        \"embeds\": embeds\n");
            code.push_str("    });\n");
        } else {
            code.push_str("    let mut payload = serde_json::json!({ \"content\": content });\n");
        }
    } else {
        code.push_str("    let mut payload = serde_json::json!({ \"content\": content });\n");
    }

    // Add optional fields
    if let Some(username) = &config.username {
        code.push_str(&format!(
            "    payload[\"username\"] = serde_json::json!(interpolate_variables(\"{}\", state));\n",
            username.replace('"', "\\\"")
        ));
    }

    if let Some(icon_url) = &config.icon_url {
        code.push_str(&format!(
            "    payload[\"avatar_url\"] = serde_json::json!(interpolate_variables(\"{}\", state));\n",
            icon_url.replace('"', "\\\"")
        ));
    }

    code
}

fn generate_teams_payload(config: &NotificationNodeConfig) -> String {
    let mut code = String::new();

    code.push_str("    // Build Microsoft Teams payload (Adaptive Card format)\n");
    code.push_str(&format!(
        "    let text = interpolate_variables(\"{}\", state);\n",
        config.message.text.replace('"', "\\\"").replace('\n', "\\n")
    ));

    // Check if using adaptive cards (blocks)
    if let Some(blocks) = &config.message.blocks {
        if !blocks.is_empty() {
            code.push_str("    // Using custom Adaptive Card\n");
            code.push_str("    let payload = serde_json::json!(");
            code.push_str(&serde_json::to_string(blocks).unwrap_or_else(|_| "{}".to_string()));
            code.push_str(");\n");
        } else {
            code.push_str(&generate_teams_simple_card());
        }
    } else {
        code.push_str(&generate_teams_simple_card());
    }

    code
}

fn generate_teams_simple_card() -> String {
    let mut code = String::new();

    code.push_str("    // Simple message card format\n");
    code.push_str("    let payload = serde_json::json!({\n");
    code.push_str("        \"@type\": \"MessageCard\",\n");
    code.push_str("        \"@context\": \"http://schema.org/extensions\",\n");
    code.push_str("        \"summary\": &text,\n");
    code.push_str("        \"sections\": [{\n");
    code.push_str("            \"activityTitle\": \"Notification\",\n");
    code.push_str("            \"text\": &text\n");
    code.push_str("        }]\n");
    code.push_str("    });\n");

    code
}

fn generate_generic_webhook_payload(config: &NotificationNodeConfig) -> String {
    let mut code = String::new();

    code.push_str("    // Build generic webhook payload\n");
    code.push_str(&format!(
        "    let message = interpolate_variables(\"{}\", state);\n",
        config.message.text.replace('"', "\\\"").replace('\n', "\\n")
    ));

    // Check if using custom payload (blocks)
    if let Some(blocks) = &config.message.blocks {
        if !blocks.is_empty() {
            code.push_str("    // Using custom payload structure\n");
            code.push_str("    let payload = serde_json::json!(");
            code.push_str(&serde_json::to_string(blocks).unwrap_or_else(|_| "{}".to_string()));
            code.push_str(");\n");
        } else {
            code.push_str("    let payload = serde_json::json!({\n");
            code.push_str("        \"message\": message,\n");
            code.push_str("        \"timestamp\": chrono::Utc::now().to_rfc3339()\n");
            code.push_str("    });\n");
        }
    } else {
        code.push_str("    let payload = serde_json::json!({\n");
        code.push_str("        \"message\": message,\n");
        code.push_str("        \"timestamp\": chrono::Utc::now().to_rfc3339()\n");
        code.push_str("    });\n");
    }

    code
}

// ============================================
// RSS/Feed Node Code Generation
// ============================================

impl ActionNodeCodeGen for RssNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// RSS/Feed Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_rss(\n", node_id));
        code.push_str("    state: &mut State,\n");
        code.push_str("    client: &reqwest::Client,\n");
        code.push_str(") -> Result<serde_json::Value, ActionError> {\n");

        // Get feed URL with variable interpolation
        code.push_str(&format!(
            "    let feed_url = interpolate_variables(\"{}\", state);\n",
            self.feed_url.replace('"', "\\\"")
        ));
        code.push_str("    tracing::debug!(url = %feed_url, \"Fetching RSS feed\");\n\n");

        // Fetch the feed
        code.push_str("    // Fetch feed content\n");
        code.push_str("    let response = client.get(&feed_url)\n");
        code.push_str("        .header(\"User-Agent\", \"ADK-Studio-RSS/1.0\")\n");
        code.push_str("        .send()\n");
        code.push_str("        .await\n");
        code.push_str("        .map_err(|e| ActionError::RssFetch(e.to_string()))?;\n\n");

        code.push_str("    if !response.status().is_success() {\n");
        code.push_str("        return Err(ActionError::RssFetch(format!(\n");
        code.push_str("            \"Feed returned status {}\", response.status()\n");
        code.push_str("        )));\n");
        code.push_str("    }\n\n");

        code.push_str("    let content = response.bytes().await\n");
        code.push_str("        .map_err(|e| ActionError::RssFetch(e.to_string()))?;\n\n");

        // Parse the feed using feed-rs
        code.push_str("    // Parse feed using feed-rs\n");
        code.push_str("    let feed = feed_rs::parser::parse(&content[..])\n");
        code.push_str("        .map_err(|e| ActionError::RssParse(e.to_string()))?;\n\n");

        // Get seen items if tracking is enabled
        if let Some(tracking) = &self.seen_tracking {
            if tracking.enabled {
                code.push_str("    // Load seen items for deduplication\n");
                code.push_str(&format!("    let seen_key = \"{}\";\n", tracking.state_key));
                code.push_str(
                    "    let mut seen_items: std::collections::HashSet<String> = state\n",
                );
                code.push_str("        .get(seen_key)\n");
                code.push_str("        .and_then(|v| v.as_array())\n");
                code.push_str("        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())\n");
                code.push_str("        .unwrap_or_default();\n\n");
            }
        }

        // Process entries
        code.push_str("    // Process feed entries\n");
        code.push_str("    let mut entries = Vec::new();\n");
        code.push_str("    for entry in feed.entries.iter() {\n");

        // Apply seen tracking filter
        if let Some(tracking) = &self.seen_tracking {
            if tracking.enabled {
                code.push_str("        // Skip already seen items\n");
                code.push_str("        let entry_id = entry.id.clone();\n");
                code.push_str("        if seen_items.contains(&entry_id) {\n");
                code.push_str("            continue;\n");
                code.push_str("        }\n\n");
            }
        }

        // Apply filters
        if let Some(filters) = &self.filters {
            code.push_str(&generate_rss_filters(filters));
        }

        // Build entry JSON
        code.push_str("        // Build entry data\n");
        code.push_str("        let entry_data = serde_json::json!({\n");
        code.push_str("            \"id\": entry.id,\n");
        code.push_str("            \"title\": entry.title.as_ref().map(|t| t.content.clone()),\n");
        code.push_str("            \"link\": entry.links.first().map(|l| l.href.clone()),\n");
        code.push_str("            \"published\": entry.published.map(|d| d.to_rfc3339()),\n");
        code.push_str("            \"updated\": entry.updated.map(|d| d.to_rfc3339()),\n");
        code.push_str(
            "            \"summary\": entry.summary.as_ref().map(|s| s.content.clone()),\n",
        );

        if self.include_content {
            code.push_str(
                "            \"content\": entry.content.as_ref().map(|c| c.body.clone()),\n",
            );
        }

        code.push_str(
            "            \"authors\": entry.authors.iter().map(|a| serde_json::json!({\n",
        );
        code.push_str("                \"name\": a.name.clone(),\n");
        code.push_str("                \"email\": a.email.clone(),\n");
        code.push_str("                \"uri\": a.uri.clone()\n");
        code.push_str("            })).collect::<Vec<_>>(),\n");
        code.push_str("            \"categories\": entry.categories.iter().map(|c| c.term.clone()).collect::<Vec<_>>(),\n");

        if self.parse_media {
            code.push_str(
                "            \"media\": entry.media.iter().map(|m| serde_json::json!({\n",
            );
            code.push_str(
                "                \"title\": m.title.as_ref().map(|t| t.content.clone()),\n",
            );
            code.push_str(
                "                \"content\": m.content.iter().map(|c| serde_json::json!({\n",
            );
            code.push_str("                    \"url\": c.url.as_ref().map(|u| u.to_string()),\n");
            code.push_str("                    \"content_type\": c.content_type.as_ref().map(|t| t.to_string()),\n");
            code.push_str("                    \"size\": c.size\n");
            code.push_str("                })).collect::<Vec<_>>(),\n");
            code.push_str(
                "                \"thumbnails\": m.thumbnails.iter().map(|t| serde_json::json!({\n",
            );
            code.push_str("                    \"url\": t.image.uri.clone(),\n");
            code.push_str("                    \"width\": t.image.width,\n");
            code.push_str("                    \"height\": t.image.height\n");
            code.push_str("                })).collect::<Vec<_>>()\n");
            code.push_str("            })).collect::<Vec<_>>(),\n");
        }

        code.push_str("        });\n\n");

        code.push_str("        entries.push(entry_data);\n");

        // Mark as seen
        if let Some(tracking) = &self.seen_tracking {
            if tracking.enabled {
                code.push_str("        seen_items.insert(entry_id);\n");
            }
        }

        // Apply max entries limit
        if let Some(max) = self.max_entries {
            code.push_str(&format!("\n        // Limit to {} entries\n", max));
            code.push_str(&format!("        if entries.len() >= {} {{\n", max));
            code.push_str("            break;\n");
            code.push_str("        }\n");
        }

        code.push_str("    }\n\n");

        // Update seen items in state
        if let Some(tracking) = &self.seen_tracking {
            if tracking.enabled {
                code.push_str("    // Update seen items in state (with max limit)\n");
                code.push_str(&format!("    let max_seen = {};\n", tracking.max_items));
                code.push_str("    let seen_vec: Vec<String> = seen_items.into_iter()\n");
                code.push_str("        .take(max_seen as usize)\n");
                code.push_str("        .collect();\n");
                code.push_str(
                    "    state.insert(seen_key.to_string(), serde_json::json!(seen_vec));\n\n",
                );
            }
        }

        // Build result
        code.push_str("    // Build result\n");
        code.push_str("    let result = serde_json::json!({\n");
        code.push_str("        \"feed\": {\n");
        code.push_str("            \"title\": feed.title.as_ref().map(|t| t.content.clone()),\n");
        code.push_str(
            "            \"description\": feed.description.as_ref().map(|d| d.content.clone()),\n",
        );
        code.push_str("            \"link\": feed.links.first().map(|l| l.href.clone()),\n");
        code.push_str("            \"updated\": feed.updated.map(|d| d.to_rfc3339()),\n");
        code.push_str("            \"language\": feed.language.clone()\n");
        code.push_str("        },\n");
        code.push_str("        \"count\": entries.len(),\n");
        code.push_str("        \"entries\": entries\n");
        code.push_str("    });\n\n");

        code.push_str(&format!(
            "    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str(
            "    tracing::info!(count = entries.len(), \"Processed RSS feed entries\");\n",
        );
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["reqwest", "feed_rs", "serde_json", "chrono"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("reqwest", "{ version = \"0.12\", features = [\"json\"] }"),
            ("feed-rs", "2"),
            ("serde_json", "1"),
            ("chrono", "0.4"),
        ]
    }
}

fn generate_rss_filters(filters: &FeedFilter) -> String {
    let mut code = String::new();

    // Keyword filter
    if let Some(keywords) = &filters.keywords {
        if !keywords.is_empty() {
            code.push_str("        // Keyword filter\n");
            code.push_str("        let title_text = entry.title.as_ref().map(|t| t.content.to_lowercase()).unwrap_or_default();\n");
            code.push_str("        let summary_text = entry.summary.as_ref().map(|s| s.content.to_lowercase()).unwrap_or_default();\n");
            code.push_str("        let keywords = vec![");
            for (i, kw) in keywords.iter().enumerate() {
                if i > 0 {
                    code.push_str(", ");
                }
                code.push_str(&format!("\"{}\"", kw.to_lowercase().replace('"', "\\\"")));
            }
            code.push_str("];\n");
            code.push_str("        let has_keyword = keywords.iter().any(|kw| {\n");
            code.push_str("            title_text.contains(kw) || summary_text.contains(kw)\n");
            code.push_str("        });\n");
            code.push_str("        if !has_keyword {\n");
            code.push_str("            continue;\n");
            code.push_str("        }\n\n");
        }
    }

    // Author filter
    if let Some(author) = &filters.author {
        code.push_str("        // Author filter\n");
        code.push_str(&format!(
            "        let author_filter = \"{}\".to_lowercase();\n",
            author.to_lowercase().replace('"', "\\\"")
        ));
        code.push_str("        let has_author = entry.authors.iter().any(|a| {\n");
        code.push_str("            a.name.to_lowercase().contains(&author_filter)\n");
        code.push_str("        });\n");
        code.push_str("        if !has_author {\n");
        code.push_str("            continue;\n");
        code.push_str("        }\n\n");
    }

    // Date from filter
    if let Some(date_from) = &filters.date_from {
        code.push_str("        // Date from filter\n");
        code.push_str(&format!(
            "        let date_from = chrono::DateTime::parse_from_rfc3339(\"{}\")\n",
            date_from
        ));
        code.push_str("            .map(|d| d.with_timezone(&chrono::Utc))\n");
        code.push_str("            .ok();\n");
        code.push_str("        if let Some(from) = date_from {\n");
        code.push_str("            let entry_date = entry.published.or(entry.updated);\n");
        code.push_str("            if let Some(ed) = entry_date {\n");
        code.push_str("                if ed < from {\n");
        code.push_str("                    continue;\n");
        code.push_str("                }\n");
        code.push_str("            }\n");
        code.push_str("        }\n\n");
    }

    // Date to filter
    if let Some(date_to) = &filters.date_to {
        code.push_str("        // Date to filter\n");
        code.push_str(&format!(
            "        let date_to = chrono::DateTime::parse_from_rfc3339(\"{}\")\n",
            date_to
        ));
        code.push_str("            .map(|d| d.with_timezone(&chrono::Utc))\n");
        code.push_str("            .ok();\n");
        code.push_str("        if let Some(to) = date_to {\n");
        code.push_str("            let entry_date = entry.published.or(entry.updated);\n");
        code.push_str("            if let Some(ed) = entry_date {\n");
        code.push_str("                if ed > to {\n");
        code.push_str("                    continue;\n");
        code.push_str("                }\n");
        code.push_str("            }\n");
        code.push_str("        }\n\n");
    }

    // Category filter
    if let Some(categories) = &filters.categories {
        if !categories.is_empty() {
            code.push_str("        // Category filter\n");
            code.push_str("        let category_filters: Vec<String> = vec![");
            for (i, cat) in categories.iter().enumerate() {
                if i > 0 {
                    code.push_str(", ");
                }
                code.push_str(&format!("\"{}\"", cat.to_lowercase().replace('"', "\\\"")));
            }
            code.push_str("].into_iter().map(String::from).collect();\n");
            code.push_str("        let entry_categories: Vec<String> = entry.categories.iter()\n");
            code.push_str("            .map(|c| c.term.to_lowercase())\n");
            code.push_str("            .collect();\n");
            code.push_str("        let has_category = category_filters.iter().any(|cf| {\n");
            code.push_str("            entry_categories.iter().any(|ec| ec.contains(cf))\n");
            code.push_str("        });\n");
            code.push_str("        if !has_category {\n");
            code.push_str("            continue;\n");
            code.push_str("        }\n\n");
        }
    }

    code
}

// ============================================
// File Node Code Generation
// ============================================

impl ActionNodeCodeGen for FileNodeConfig {
    fn generate_code(&self, node_id: &str) -> String {
        let mut code = String::new();

        code.push_str(&format!("// File Node: {}\n", self.standard.name));
        code.push_str(&format!("async fn {}_file(\n", node_id));
        code.push_str("    state: &mut State,\n");
        code.push_str(") -> Result<serde_json::Value, ActionError> {\n");

        match self.operation {
            FileOperation::Read => {
                code.push_str("    // Read file operation\n");
                if let Some(local) = &self.local {
                    code.push_str(&format!(
                        "    let path = interpolate_variables(\"{}\", state);\n",
                        local.path.replace('"', "\\\"")
                    ));
                    code.push_str("    tracing::debug!(path = %path, \"Reading file\");\n\n");
                    code.push_str("    let content = tokio::fs::read_to_string(&path).await\n");
                    code.push_str(
                        "        .map_err(|e| ActionError::FileRead(e.to_string()))?;\n\n",
                    );

                    // Parse based on config
                    if let Some(parse) = &self.parse {
                        match parse.format {
                            FileFormat::Json => {
                                code.push_str("    let parsed: serde_json::Value = serde_json::from_str(&content)\n");
                                code.push_str("        .map_err(|e| ActionError::FileParse(e.to_string()))?;\n");
                            }
                            FileFormat::Csv => {
                                code.push_str("    // Parse CSV content\n");
                                code.push_str("    let mut reader = csv::Reader::from_reader(content.as_bytes());\n");
                                code.push_str("    let records: Vec<serde_json::Value> = reader.deserialize()\n");
                                code.push_str("        .filter_map(|r: Result<serde_json::Value, _>| r.ok())\n");
                                code.push_str("        .collect();\n");
                                code.push_str("    let parsed = serde_json::json!(records);\n");
                            }
                            FileFormat::Xml => {
                                code.push_str("    // XML parsing - convert to JSON\n");
                                code.push_str("    let parsed = serde_json::json!({ \"content\": content });\n");
                            }
                            FileFormat::Text | FileFormat::Binary => {
                                code.push_str("    let parsed = serde_json::json!({ \"content\": content });\n");
                            }
                        }
                    } else {
                        code.push_str(
                            "    let parsed = serde_json::json!({ \"content\": content });\n",
                        );
                    }

                    code.push_str("\n    let result = serde_json::json!({\n");
                    code.push_str("        \"path\": path,\n");
                    code.push_str("        \"data\": parsed\n");
                    code.push_str("    });\n");
                } else {
                    code.push_str("    let result = serde_json::json!({ \"error\": \"No file path configured\" });\n");
                }
            }
            FileOperation::Write => {
                code.push_str("    // Write file operation\n");
                if let Some(local) = &self.local {
                    code.push_str(&format!(
                        "    let path = interpolate_variables(\"{}\", state);\n",
                        local.path.replace('"', "\\\"")
                    ));

                    if let Some(write) = &self.write {
                        code.push_str(&format!(
                            "    let content = interpolate_variables(\"{}\", state);\n",
                            write.content.replace('"', "\\\"").replace('\n', "\\n")
                        ));
                    } else {
                        code.push_str("    let content = String::new();\n");
                    }

                    code.push_str("    tracing::debug!(path = %path, \"Writing file\");\n\n");
                    code.push_str("    tokio::fs::write(&path, &content).await\n");
                    code.push_str(
                        "        .map_err(|e| ActionError::FileWrite(e.to_string()))?;\n\n",
                    );
                    code.push_str("    let result = serde_json::json!({\n");
                    code.push_str("        \"path\": path,\n");
                    code.push_str("        \"bytes_written\": content.len()\n");
                    code.push_str("    });\n");
                } else {
                    code.push_str("    let result = serde_json::json!({ \"error\": \"No file path configured\" });\n");
                }
            }
            FileOperation::Delete => {
                code.push_str("    // Delete file operation\n");
                if let Some(local) = &self.local {
                    code.push_str(&format!(
                        "    let path = interpolate_variables(\"{}\", state);\n",
                        local.path.replace('"', "\\\"")
                    ));
                    code.push_str("    tracing::debug!(path = %path, \"Deleting file\");\n\n");
                    code.push_str("    tokio::fs::remove_file(&path).await\n");
                    code.push_str(
                        "        .map_err(|e| ActionError::FileDelete(e.to_string()))?;\n\n",
                    );
                    code.push_str("    let result = serde_json::json!({\n");
                    code.push_str("        \"path\": path,\n");
                    code.push_str("        \"deleted\": true\n");
                    code.push_str("    });\n");
                } else {
                    code.push_str("    let result = serde_json::json!({ \"error\": \"No file path configured\" });\n");
                }
            }
            FileOperation::List => {
                code.push_str("    // List files operation\n");
                if let Some(local) = &self.local {
                    code.push_str(&format!(
                        "    let path = interpolate_variables(\"{}\", state);\n",
                        local.path.replace('"', "\\\"")
                    ));
                    code.push_str("    tracing::debug!(path = %path, \"Listing directory\");\n\n");
                    code.push_str("    let mut entries = Vec::new();\n");
                    code.push_str("    let mut dir = tokio::fs::read_dir(&path).await\n");
                    code.push_str(
                        "        .map_err(|e| ActionError::FileRead(e.to_string()))?;\n\n",
                    );
                    code.push_str("    while let Some(entry) = dir.next_entry().await\n");
                    code.push_str(
                        "        .map_err(|e| ActionError::FileRead(e.to_string()))? {\n",
                    );
                    code.push_str("        let metadata = entry.metadata().await.ok();\n");
                    code.push_str("        entries.push(serde_json::json!({\n");
                    code.push_str("            \"name\": entry.file_name().to_string_lossy(),\n");
                    code.push_str("            \"path\": entry.path().to_string_lossy(),\n");
                    code.push_str(
                        "            \"is_file\": metadata.as_ref().map(|m| m.is_file()),\n",
                    );
                    code.push_str(
                        "            \"is_dir\": metadata.as_ref().map(|m| m.is_dir()),\n",
                    );
                    code.push_str("            \"size\": metadata.as_ref().map(|m| m.len())\n");
                    code.push_str("        }));\n");
                    code.push_str("    }\n\n");
                    code.push_str("    let result = serde_json::json!({\n");
                    code.push_str("        \"path\": path,\n");
                    code.push_str("        \"count\": entries.len(),\n");
                    code.push_str("        \"entries\": entries\n");
                    code.push_str("    });\n");
                } else {
                    code.push_str("    let result = serde_json::json!({ \"error\": \"No directory path configured\" });\n");
                }
            }
        }

        code.push_str(&format!(
            "\n    state.insert(\"{}\".to_string(), result.clone());\n",
            self.standard.mapping.output_key
        ));
        code.push_str("    Ok(result)\n");
        code.push_str("}\n\n");

        code
    }

    fn required_imports(&self) -> Vec<&'static str> {
        vec!["tokio", "serde_json"]
    }

    fn required_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps =
            vec![("tokio", "{ version = \"1\", features = [\"full\"] }"), ("serde_json", "1")];

        // Add format-specific dependencies
        if let Some(parse) = &self.parse {
            match parse.format {
                FileFormat::Csv => deps.push(("csv", "1")),
                _ => {}
            }
        }

        deps
    }
}

// ============================================
// Main Code Generation Functions
// ============================================

/// Generate Rust code for all action nodes in a workflow
pub fn generate_action_nodes_code(action_nodes: &HashMap<String, ActionNodeConfig>) -> String {
    let mut code = String::new();

    // Generate header
    code.push_str("// Action Nodes - Generated Code\n");
    code.push_str("// This code was generated by ADK Studio\n\n");

    // Collect all required imports
    let mut imports: std::collections::HashSet<&str> = std::collections::HashSet::new();
    imports.insert("serde_json");
    imports.insert("tracing");

    for node in action_nodes.values() {
        match node {
            ActionNodeConfig::Trigger(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Http(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Set(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Transform(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Switch(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Loop(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Merge(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Wait(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Code(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Database(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Email(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Notification(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::Rss(n) => imports.extend(n.required_imports()),
            ActionNodeConfig::File(n) => imports.extend(n.required_imports()),
        }
    }

    // Generate imports
    code.push_str("use std::collections::HashMap;\n");
    code.push_str("use serde_json::json;\n");
    code.push_str("use tracing;\n\n");

    // Generate type alias for State
    code.push_str("type State = HashMap<String, serde_json::Value>;\n\n");

    // Generate helper functions
    code.push_str(generate_interpolation_helper());
    code.push_str("\n");

    // Generate code for each action node
    for (node_id, node) in action_nodes {
        let node_code = match node {
            ActionNodeConfig::Trigger(n) => n.generate_code(node_id),
            ActionNodeConfig::Http(n) => n.generate_code(node_id),
            ActionNodeConfig::Set(n) => n.generate_code(node_id),
            ActionNodeConfig::Transform(n) => n.generate_code(node_id),
            ActionNodeConfig::Switch(n) => n.generate_code(node_id),
            ActionNodeConfig::Loop(n) => n.generate_code(node_id),
            ActionNodeConfig::Merge(n) => n.generate_code(node_id),
            ActionNodeConfig::Wait(n) => n.generate_code(node_id),
            ActionNodeConfig::Code(n) => n.generate_code(node_id),
            ActionNodeConfig::Database(n) => n.generate_code(node_id),
            ActionNodeConfig::Email(n) => n.generate_code(node_id),
            ActionNodeConfig::Notification(n) => n.generate_code(node_id),
            ActionNodeConfig::Rss(n) => n.generate_code(node_id),
            ActionNodeConfig::File(n) => n.generate_code(node_id),
        };
        code.push_str(&node_code);
    }

    code
}

/// Collect all required Cargo dependencies for action nodes
pub fn collect_action_node_dependencies(
    action_nodes: &HashMap<String, ActionNodeConfig>,
) -> Vec<(String, String)> {
    let mut deps: HashMap<String, String> = HashMap::new();

    // Always include these
    deps.insert("serde_json".to_string(), "1".to_string());
    deps.insert("tracing".to_string(), "0.1".to_string());
    deps.insert("tokio".to_string(), "{ version = \"1\", features = [\"full\"] }".to_string());
    deps.insert("regex".to_string(), "1".to_string());

    for node in action_nodes.values() {
        let node_deps: Vec<(&str, &str)> = match node {
            ActionNodeConfig::Trigger(n) => n.required_dependencies(),
            ActionNodeConfig::Http(n) => n.required_dependencies(),
            ActionNodeConfig::Set(n) => n.required_dependencies(),
            ActionNodeConfig::Transform(n) => n.required_dependencies(),
            ActionNodeConfig::Switch(n) => n.required_dependencies(),
            ActionNodeConfig::Loop(n) => n.required_dependencies(),
            ActionNodeConfig::Merge(n) => n.required_dependencies(),
            ActionNodeConfig::Wait(n) => n.required_dependencies(),
            ActionNodeConfig::Code(n) => n.required_dependencies(),
            ActionNodeConfig::Database(n) => n.required_dependencies(),
            ActionNodeConfig::Email(n) => n.required_dependencies(),
            ActionNodeConfig::Notification(n) => n.required_dependencies(),
            ActionNodeConfig::Rss(n) => n.required_dependencies(),
            ActionNodeConfig::File(n) => n.required_dependencies(),
        };

        for (name, version) in node_deps {
            deps.insert(name.to_string(), version.to_string());
        }
    }

    deps.into_iter().collect()
}

/// Validate that generated code would compile
pub fn validate_generated_code(code: &str) -> Result<(), String> {
    // Basic validation checks

    // Check for balanced braces
    let open_braces = code.matches('{').count();
    let close_braces = code.matches('}').count();
    if open_braces != close_braces {
        return Err(format!("Unbalanced braces: {} open, {} close", open_braces, close_braces));
    }

    // Check for balanced parentheses
    let open_parens = code.matches('(').count();
    let close_parens = code.matches(')').count();
    if open_parens != close_parens {
        return Err(format!(
            "Unbalanced parentheses: {} open, {} close",
            open_parens, close_parens
        ));
    }

    // Check for common syntax errors
    if code.contains(";;") {
        return Err("Double semicolon found".to_string());
    }

    // Note: We intentionally don't check for async functions without await.
    // Action nodes generate async functions to match the ADK runtime trait,
    // but some nodes (like Manual Trigger, Switch) don't need async operations
    // internally. This is valid Rust - async functions can return immediately
    // without awaiting anything.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_node_codegen() {
        let config = TriggerNodeConfig {
            standard: StandardProperties {
                id: "trigger_1".to_string(),
                name: "Start".to_string(),
                ..Default::default()
            },
            trigger_type: TriggerType::Manual,
            webhook: None,
            schedule: None,
            event: None,
        };

        let code = config.generate_code("trigger_1");
        assert!(code.contains("async fn trigger_1_trigger"));
        assert!(code.contains("Manual trigger"));
    }

    #[test]
    fn test_http_node_codegen() {
        let config = HttpNodeConfig {
            standard: StandardProperties {
                id: "http_1".to_string(),
                name: "API Call".to_string(),
                mapping: InputOutputMapping {
                    output_key: "api_result".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            method: HttpMethod::Get,
            url: "https://api.example.com/data".to_string(),
            auth: HttpAuth {
                auth_type: "none".to_string(),
                bearer: None,
                basic: None,
                api_key: None,
            },
            headers: HashMap::new(),
            body: HttpBody { body_type: "none".to_string(), content: None },
            response: HttpResponse {
                response_type: "json".to_string(),
                status_validation: Some("200-299".to_string()),
                json_path: None,
            },
            rate_limit: None,
        };

        let code = config.generate_code("http_1");
        assert!(code.contains("async fn http_1_http"));
        assert!(code.contains("client.get"));
        assert!(code.contains("api.example.com"));
    }

    #[test]
    fn test_switch_node_codegen() {
        let config = SwitchNodeConfig {
            standard: StandardProperties {
                id: "switch_1".to_string(),
                name: "Router".to_string(),
                ..Default::default()
            },
            evaluation_mode: EvaluationMode::FirstMatch,
            conditions: vec![SwitchCondition {
                id: "cond_1".to_string(),
                name: "High".to_string(),
                field: "score".to_string(),
                operator: "gt".to_string(),
                value: Some(serde_json::json!(80)),
                output_port: "high".to_string(),
            }],
            default_branch: Some("default".to_string()),
            expression_mode: None,
        };

        let code = config.generate_code("switch_1");
        assert!(code.contains("async fn switch_1_switch"));
        assert!(code.contains("First match"));
        assert!(code.contains("\"high\""));
    }

    #[test]
    fn test_validate_generated_code() {
        let valid_code = r#"
async fn test() {
    let x = 1;
    something().await;
}
"#;
        assert!(validate_generated_code(valid_code).is_ok());

        let unbalanced = "fn test() { { }";
        assert!(validate_generated_code(unbalanced).is_err());
    }

    #[test]
    fn test_condition_comparison_generation() {
        assert!(generate_condition_comparison("eq", &Some(serde_json::json!(5))).contains("=="));
        assert!(generate_condition_comparison("gt", &Some(serde_json::json!(10))).contains(">"));
        assert!(
            generate_condition_comparison("contains", &Some(serde_json::json!("test")))
                .contains("contains")
        );
    }
}
