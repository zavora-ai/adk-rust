//! Item 8: `SecurityConfig::with_allowed_hosts` and `ServerConfig::with_allowed_hosts` both set
//! the list, replacing an earlier one.

use std::sync::Arc;

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
use adk_server::{SecurityConfig, ServerConfig, create_app};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::common::{Mode, Report, Verdict, check};

struct QuietAgent;

#[async_trait]
impl Agent for QuietAgent {
    fn name(&self) -> &str {
        "quiet"
    }

    fn description(&self) -> &str {
        "answers nothing"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

fn server_config() -> ServerConfig {
    ServerConfig::new(
        Arc::new(SingleAgentLoader::new(Arc::new(QuietAgent))),
        Arc::new(InMemorySessionService::new()),
    )
}

pub async fn run(report: &mut Report) {
    report.record_result(
        "hosts-security-config",
        8,
        Mode::Local,
        answered(
            server_config().with_security(
                SecurityConfig::default()
                    .with_allowed_hosts(["old.example.com"])
                    .with_allowed_hosts(["agents.example.com"]),
            ),
        )
        .await,
    );
    report.record_result(
        "hosts-server-config",
        8,
        Mode::Local,
        answered(
            server_config()
                .with_security(SecurityConfig::default().with_allowed_hosts(["old.example.com"]))
                .with_allowed_hosts(vec!["agents.example.com".to_string()]),
        )
        .await,
    );
}

/// The later list is answered and the earlier one is not.
async fn answered(config: ServerConfig) -> anyhow::Result<Verdict> {
    let app = create_app(config);
    let mut statuses = Vec::new();
    for host in ["agents.example.com", "old.example.com"] {
        let request =
            Request::builder().uri("/api/health").header("host", host).body(Body::empty())?;
        let status = app.clone().oneshot(request).await?.status();
        statuses.push((host, status.as_u16()));
    }
    Ok(check(
        statuses == [("agents.example.com", StatusCode::OK.as_u16()), ("old.example.com", 403)],
        format!("{statuses:?}"),
    ))
}
