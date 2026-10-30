//! Group F, debug traces: `GET /api/debug/trace/{event_id}` returns a span to its owner only.
//!
//! The server authenticates the `x-user` header, exposes the admin debug routes, and keeps
//! runtime spans in an `AdkSpanExporter`. A live `claude-sonnet-5-5` agent runs one turn as
//! `alice`; `bob` then asks for her span by its event ID and by her session ID.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Llm, RequestContext, SingleAgentLoader};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_server::{
    RequestContextError, RequestContextExtractor, SecurityConfig, ServerBuilder, ServerConfig,
};
use adk_session::InMemorySessionService;
use adk_telemetry::{AdkSpanExporter, AdkSpanLayer};
use async_trait::async_trait;
use serde_json::Value;
use tracing_subscriber::layer::SubscriberExt;

use crate::{
    Kind, Outcome, errored, event_text, http, outcome, plain_agent, run_body, serve, skip, truncate,
};

const ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
const APP: &str = "trace_owner";
const OWNER_READS: &str = "F3 debug-trace-owner-reads";
const EVENT_ID_REFUSED: &str = "F4 debug-trace-event-id-cross-user-refused";
const SESSION_ID_REFUSED: &str = "F5 debug-trace-session-id-cross-user-refused";

/// Authenticates the user named in the `x-user` header.
struct HeaderUser;

#[async_trait]
impl RequestContextExtractor for HeaderUser {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let user_id = parts
            .headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        Ok(RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

pub(crate) async fn group_f_debug_traces(out: &mut Vec<Outcome>) {
    let Some(key) = std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.trim().is_empty())
    else {
        for id in [OWNER_READS, EVENT_ID_REFUSED, SESSION_ID_REFUSED] {
            out.push(skip(id, Kind::Live, "ANTHROPIC_API_KEY not set"));
        }
        return;
    };

    let exporter = Arc::new(AdkSpanExporter::new());
    let subscriber = tracing_subscriber::registry().with(AdkSpanLayer::new(exporter.clone()));
    // A current-thread runtime runs the server, the agent, and the client on one thread, so
    // this subscriber sees every span without becoming the process-wide default.
    let ran = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        tracing::subscriber::with_default(subscriber, || runtime.block_on(scenarios(key, exporter)))
    })
    .await;
    match ran {
        Ok(Ok(outcomes)) => out.extend(outcomes),
        Ok(Err(error)) => {
            for id in [OWNER_READS, EVENT_ID_REFUSED, SESSION_ID_REFUSED] {
                out.push(errored(id, Kind::Live, anyhow::anyhow!("{error:#}")));
            }
        }
        Err(error) => {
            for id in [OWNER_READS, EVENT_ID_REFUSED, SESSION_ID_REFUSED] {
                out.push(errored(id, Kind::Live, anyhow::anyhow!("scenario thread: {error}")));
            }
        }
    }
}

async fn scenarios(key: String, exporter: Arc<AdkSpanExporter>) -> anyhow::Result<Vec<Outcome>> {
    let model: Arc<dyn Llm> = Arc::new(AnthropicClient::new(
        AnthropicConfig::new(key, ANTHROPIC_MODEL).with_max_tokens(64),
    )?);
    let agent = plain_agent(APP, "You are trace_owner. Reply with one short sentence.", model)?;
    let (base, task) = serve(|_| {
        let config = ServerConfig::new(
            Arc::new(SingleAgentLoader::new(agent)),
            Arc::new(InMemorySessionService::new()),
        )
        .with_span_exporter(exporter)
        .with_request_context(Arc::new(HeaderUser))
        .with_security(SecurityConfig { expose_admin_debug: true, ..SecurityConfig::default() })
        .with_request_timeout(Duration::from_secs(90));
        ServerBuilder::new(config).build()
    })
    .await?;
    let client = http();

    // alice runs one turn; her `agent.execute` span is recorded under the invocation ID.
    let response = client
        .post(format!("{base}/api/run"))
        .header("x-user", "alice")
        .json(&run_body(APP, "alice", "s-alice", "Say hello."))
        .send()
        .await?;
    let status = response.status();
    let events: Vec<adk_core::Event> = response.json().await.unwrap_or_default();
    let text = event_text(&events);
    anyhow::ensure!(
        status.is_success() && !text.trim().is_empty(),
        "alice's run: HTTP {status}, agent said {text:?}"
    );
    let invocation = events
        .iter()
        .map(|event| event.invocation_id.clone())
        .find(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("alice's run returned no invocation ID"))?;

    let trace = |id: String, user: &'static str| {
        let request = client.get(format!("{base}/api/debug/trace/{id}")).header("x-user", user);
        async move {
            let response = request.send().await?;
            let status = response.status();
            let body: Option<Value> = response.json().await.ok();
            Ok::<_, anyhow::Error>((status, body))
        }
    };

    // The span closes as the run's stream is dropped, just after the response.
    let mut own = trace(invocation.clone(), "alice").await;
    for _ in 0..20 {
        if matches!(&own, Ok((status, _)) if status.is_success()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        own = trace(invocation.clone(), "alice").await;
    }
    let by_event_id = trace(invocation.clone(), "bob").await;
    let by_session_id = trace("s-alice".to_string(), "bob").await;
    task.abort();

    let owner = |body: &Option<Value>| {
        body.as_ref()
            .and_then(|body| body.get("adk.user_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let leaked = |status: reqwest::StatusCode, body: &Option<Value>| {
        let attributes = body.as_ref().and_then(Value::as_object).map_or(0, |map| map.len());
        let session = body
            .as_ref()
            .and_then(|body| body.get("gcp.vertex.agent.session_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        format!(
            "HTTP {status}: bob read a span of {attributes} attributes, owner {:?}, session {session:?}",
            owner(body)
        )
    };

    let mut outcomes = Vec::new();
    outcomes.push(match own {
        Ok((status, body)) => outcome(
            OWNER_READS,
            Kind::Live,
            status.is_success() && owner(&body) == "alice",
            format!(
                "{ANTHROPIC_MODEL} said {:?}; alice reads her span: HTTP {status}, owner {:?}",
                truncate(&text, 40),
                owner(&body)
            ),
        ),
        Err(error) => errored(OWNER_READS, Kind::Live, error),
    });
    outcomes.push(match by_event_id {
        Ok((status, body)) => outcome(
            EVENT_ID_REFUSED,
            Kind::Live,
            status == reqwest::StatusCode::FORBIDDEN,
            if status.is_success() { leaked(status, &body) } else { format!("HTTP {status}") },
        ),
        Err(error) => errored(EVENT_ID_REFUSED, Kind::Live, error),
    });
    outcomes.push(match by_session_id {
        Ok((status, body)) => outcome(
            SESSION_ID_REFUSED,
            Kind::Live,
            status.is_client_error() && owner(&body).is_empty(),
            if status.is_success() { leaked(status, &body) } else { format!("HTTP {status}") },
        ),
        Err(error) => errored(SESSION_ID_REFUSED, Kind::Live, error),
    });
    Ok(outcomes)
}
