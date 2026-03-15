//! Auth Bridge Example — Demonstrates flowing authenticated identity into agent execution.
//!
//! This example shows how to:
//! 1. Implement a custom `RequestContextExtractor` that parses Bearer tokens
//! 2. Configure the server with the extractor
//! 3. Build a scope-aware tool that checks `user_scopes()` before executing
//! 4. Test the full flow with `curl` commands
//!
//! # Running
//!
//! ```bash
//! cargo run -p adk-examples --example auth_bridge
//! ```
//!
//! Then in another terminal:
//!
//! ```bash
//! # With valid token (has "admin" and "read" scopes)
//! curl -N -X POST http://localhost:8080/api/run_sse \
//!   -H "Content-Type: application/json" \
//!   -H "Authorization: Bearer admin-token-abc" \
//!   -d '{"appName":"auth_demo","userId":"ignored","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"Show me the secret data"}]}}'
//!
//! # Without token (401)
//! curl -X POST http://localhost:8080/api/run_sse \
//!   -H "Content-Type: application/json" \
//!   -d '{"appName":"auth_demo","userId":"u1","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]}}'
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{RequestContext, Tool, ToolContext};
use adk_model::GeminiModel;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerConfig, create_app};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// 1. Custom RequestContextExtractor
// ---------------------------------------------------------------------------

/// A simple token-based extractor for demonstration purposes.
///
/// In production you'd validate JWTs, call an IdP, etc. Here we use a
/// hardcoded token map to keep the example self-contained.
struct DemoTokenExtractor {
    /// Maps bearer tokens to (user_id, scopes).
    tokens: HashMap<String, (String, Vec<String>)>,
}

impl DemoTokenExtractor {
    fn new() -> Self {
        let mut tokens = HashMap::new();
        tokens.insert(
            "admin-token-abc".to_string(),
            ("alice".to_string(), vec!["admin".to_string(), "read".to_string()]),
        );
        tokens
            .insert("reader-token-xyz".to_string(), ("bob".to_string(), vec!["read".to_string()]));
        Self { tokens }
    }
}

#[async_trait]
impl RequestContextExtractor for DemoTokenExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        // Extract Bearer token from Authorization header
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| RequestContextError::InvalidToken("expected Bearer scheme".into()))?;

        // Look up the token
        let (user_id, scopes) = self
            .tokens
            .get(token)
            .ok_or_else(|| RequestContextError::InvalidToken("unknown token".into()))?;

        Ok(RequestContext {
            user_id: user_id.clone(),
            scopes: scopes.clone(),
            metadata: [("auth_method".to_string(), "bearer".to_string())].into(),
        })
    }
}

// ---------------------------------------------------------------------------
// 2. Scope-aware tool
// ---------------------------------------------------------------------------

/// A tool that requires the "admin" scope to return sensitive data.
///
/// This demonstrates how tools can use `ctx.user_scopes()` to enforce
/// authorization at the tool level.
struct SecretDataTool;

#[async_trait]
impl Tool for SecretDataTool {
    fn name(&self) -> &str {
        "get_secret_data"
    }

    fn description(&self) -> &str {
        "Returns secret data. Requires 'admin' scope."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _input: Value) -> adk_core::Result<Value> {
        let scopes = ctx.user_scopes();
        let user_id = ctx.user_id();

        if !scopes.contains(&"admin".to_string()) {
            return Ok(json!({
                "error": "access_denied",
                "message": format!(
                    "User '{}' lacks 'admin' scope. Current scopes: {:?}",
                    user_id, scopes
                )
            }));
        }

        Ok(json!({
            "status": "ok",
            "user": user_id,
            "scopes": scopes,
            "secret": "The launch code is 42."
        }))
    }
}

/// A tool that only requires the "read" scope.
struct PublicDataTool;

#[async_trait]
impl Tool for PublicDataTool {
    fn name(&self) -> &str {
        "get_public_data"
    }

    fn description(&self) -> &str {
        "Returns public data. Requires 'read' scope."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _input: Value) -> adk_core::Result<Value> {
        let scopes = ctx.user_scopes();
        let user_id = ctx.user_id();

        if !scopes.contains(&"read".to_string()) {
            return Ok(json!({
                "error": "access_denied",
                "message": format!("User '{}' lacks 'read' scope.", user_id)
            }));
        }

        Ok(json!({
            "status": "ok",
            "user": user_id,
            "data": "ADK-Rust is a modular agent framework."
        }))
    }
}

// ---------------------------------------------------------------------------
// 3. Agent loader
// ---------------------------------------------------------------------------

struct DemoAgentLoader {
    agent: Arc<dyn adk_core::Agent>,
}

#[async_trait]
impl adk_core::AgentLoader for DemoAgentLoader {
    async fn load_agent(&self, _name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Ok(self.agent.clone())
    }
    fn list_agents(&self) -> Vec<String> {
        vec![self.agent.name().to_string()]
    }
    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        self.agent.clone()
    }
}

// ---------------------------------------------------------------------------
// 4. Main — wire everything together
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("Set GOOGLE_API_KEY or GEMINI_API_KEY");

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let agent = Arc::new(
        LlmAgentBuilder::new("auth_demo")
            .model(model)
            .instruction(
                "You are a helpful assistant with two tools:\n\
                 - get_secret_data: returns secret info (admin only)\n\
                 - get_public_data: returns public info (any authenticated user)\n\n\
                 When the user asks for secret or sensitive data, call get_secret_data.\n\
                 When the user asks for general info, call get_public_data.\n\
                 Always relay the tool result to the user.",
            )
            .tool(Arc::new(SecretDataTool))
            .tool(Arc::new(PublicDataTool))
            .build()?,
    );

    let session_service = Arc::new(InMemorySessionService::new());
    let agent_loader = Arc::new(DemoAgentLoader { agent });

    // Wire the auth extractor into the server config
    let extractor = Arc::new(DemoTokenExtractor::new());
    let config = ServerConfig::new(agent_loader, session_service).with_request_context(extractor);

    let app = create_app(config);

    let port = 8080u16;
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;

    println!("🔐 Auth Bridge Example Server running on http://localhost:{port}");
    println!();
    println!("Try these curl commands:");
    println!();
    println!("  # Admin user (alice) — can access secret data:");
    println!("  curl -N -X POST http://localhost:{port}/api/run_sse \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -H 'Authorization: Bearer admin-token-abc' \\");
    println!(
        "    -d '{{\"appName\":\"auth_demo\",\"userId\":\"ignored\",\"sessionId\":\"s1\",\"newMessage\":{{\"role\":\"user\",\"parts\":[{{\"text\":\"Show me the secret data\"}}]}}}}'"
    );
    println!();
    println!("  # Read-only user (bob) — denied secret data:");
    println!("  curl -N -X POST http://localhost:{port}/api/run_sse \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -H 'Authorization: Bearer reader-token-xyz' \\");
    println!(
        "    -d '{{\"appName\":\"auth_demo\",\"userId\":\"ignored\",\"sessionId\":\"s2\",\"newMessage\":{{\"role\":\"user\",\"parts\":[{{\"text\":\"Show me the secret data\"}}]}}}}'"
    );
    println!();
    println!("  # No token (401 Unauthorized):");
    println!("  curl -X POST http://localhost:{port}/api/run_sse \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!(
        "    -d '{{\"appName\":\"auth_demo\",\"userId\":\"u1\",\"sessionId\":\"s3\",\"newMessage\":{{\"role\":\"user\",\"parts\":[{{\"text\":\"hello\"}}]}}}}'"
    );
    println!();

    axum::serve(listener, app).await?;

    Ok(())
}
