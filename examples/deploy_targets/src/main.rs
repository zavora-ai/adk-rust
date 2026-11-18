//! An agent that runs unchanged on every `adk-deploy` target.
//!
//! The target sets the deploy contract (`adk_deploy::targets::contract`) on the container, and
//! this binary reads its configuration from it:
//!
//! | Variable | Used for |
//! |----------|----------|
//! | `BIND_HOST`, `PORT` | The listen address |
//! | `ADK_ALLOWED_HOSTS` | `SecurityConfig::allowed_hosts`, so the public host name is answered |
//! | `ADK_SERVICE_SESSIONS_URL` | The session store, from the `sessions` binding |
//! | `ADK_AUTH_MODE`, `ADK_AUTH_REQUIRED_SCOPES` | Bearer authentication of every caller |
//! | `ADK_API_TOKEN`, `ANTHROPIC_API_KEY` | Secrets, injected from the provider's secret store |
//!
//! The token holder gets the `admin` scope, so `POST /api/admin/freeze` works for the operator
//! and refuses everyone else.

use std::net::SocketAddr;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{GovernanceControl, RequestContext, SingleAgentLoader};
use adk_deploy::targets::contract;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ADMIN_SCOPE, ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService, SqliteSessionService};
use anyhow::{Context, bail};
use async_trait::async_trait;
use tracing_subscriber::EnvFilter;

/// Admits `Authorization: Bearer <ADK_API_TOKEN>` as the operator.
struct BearerToken {
    token: String,
    scopes: Vec<String>,
}

#[async_trait]
impl RequestContextExtractor for BearerToken {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let presented = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(RequestContextError::MissingAuth)?;
        if !constant_time_eq(presented.as_bytes(), self.token.as_bytes()) {
            return Err(RequestContextError::InvalidToken("unknown token".to_string()));
        }
        Ok(RequestContext {
            user_id: "operator".to_string(),
            scopes: self.scopes.clone(),
            metadata: Default::default(),
        })
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The session store the `sessions` binding names.
async fn session_service() -> anyhow::Result<Arc<dyn SessionService>> {
    match contract::service_url("sessions") {
        Some(url) if url.starts_with("sqlite:") => {
            let sessions = SqliteSessionService::new(&url)
                .await
                .with_context(|| format!("opening the session store at {url}"))?;
            sessions.migrate().await?;
            tracing::info!(store = "sqlite", "sessions are durable");
            Ok(Arc::new(sessions))
        }
        Some(url) => {
            let scheme = url.split(':').next().unwrap_or_default();
            bail!(
                "this example opens SQLite session stores; a {scheme} store needs its adk-session backend"
            )
        }
        None => {
            tracing::warn!("no ADK_SERVICE_SESSIONS_URL: sessions are kept in memory");
            Ok(Arc::new(InMemorySessionService::new()))
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let api_key = std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY is not set")?;
    let model_id =
        std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".to_string());
    let model = Arc::new(AnthropicClient::new(AnthropicConfig::new(api_key, &model_id))?);
    let agent = LlmAgentBuilder::new("support_desk")
        .description("Answers support questions")
        .instruction("You are a concise support agent. Answer in one or two sentences.")
        .model(model)
        .build()?;

    let loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));
    let mut config = ServerConfig::new(loader, session_service().await?)
        .with_governance(GovernanceControl::new())
        .with_allowed_hosts(contract::allowed_hosts());
    match std::env::var(contract::AUTH_MODE).as_deref() {
        Ok("bearer") => {
            let token = std::env::var("ADK_API_TOKEN")
                .context("ADK_AUTH_MODE=bearer needs the ADK_API_TOKEN secret")?;
            let mut scopes = contract::required_scopes();
            scopes.push(ADMIN_SCOPE.to_string());
            config = config.with_request_context(Arc::new(BearerToken { token, scopes }));
        }
        Ok(mode) => bail!(
            "this example authenticates bearer tokens; ADK_AUTH_MODE={mode} needs an OIDC \
             extractor such as adk_auth::auth_bridge::JwtRequestContextExtractor"
        ),
        Err(_) => tracing::warn!("no ADK_AUTH_MODE: callers are not authenticated"),
    }
    let app = ServerBuilder::new(config).enable_governance_endpoints().build();

    let host = std::env::var(contract::BIND_HOST).unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var(contract::PORT).unwrap_or_else(|_| "8080".to_string());
    let address: SocketAddr = format!("{host}:{port}").parse().context("BIND_HOST or PORT")?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, model = %model_id, "support desk serving");
    axum::serve(listener, app).await?;
    Ok(())
}
