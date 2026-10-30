//! Group E, the kill switch: only an administrator freezes or unfreezes the server.
//!
//! A live `claude-sonnet-5-5` agent is served through `ServerBuilder` with the governance
//! endpoints and an extractor that admits two bearer tokens: `admin-token` as `root` with the
//! `admin` scope, and `member-token` as `mia` without it. A member's freeze and unfreeze are
//! refused and the agent keeps answering; an administrator's freeze refuses the agent's next
//! run and the unfreeze restores it. A request without credentials is refused, by a server
//! with an extractor and by one without.

use std::sync::Arc;

use adk_core::{GovernanceControl, Llm, RequestContext, SingleAgentLoader};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_server::{RequestContextError, RequestContextExtractor, ServerBuilder, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    Kind, Outcome, errored, event_text, http, outcome, plain_agent, run_body, serve, skip, truncate,
};

const ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
const APP: &str = "governed_agent";
const ADMIN: &str = "admin-token";
const MEMBER: &str = "member-token";
const MEMBER_FREEZE: &str = "E6 member-freeze-refused";
const ADMIN_FREEZE: &str = "E7 admin-freeze-stops-next-run";
const MEMBER_UNFREEZE: &str = "E8 member-unfreeze-refused";
const ADMIN_UNFREEZE: &str = "E9 admin-unfreeze-restores-run";
const ANONYMOUS: &str = "E10 anonymous-freeze-refused";

/// Admits `admin-token` as `root` with the `admin` scope and `member-token` as `mia` with
/// the `user` scope.
struct Tokens;

#[async_trait]
impl RequestContextExtractor for Tokens {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        let (user_id, scope) = match header.strip_prefix("Bearer ") {
            Some(ADMIN) => ("root", "admin"),
            Some(MEMBER) => ("mia", "user"),
            _ => return Err(RequestContextError::InvalidToken("unknown token".to_string())),
        };
        Ok(RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![scope.to_string()],
            metadata: Default::default(),
        })
    }
}

/// One HTTP exchange: the status and the JSON body.
async fn send(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<Value>,
) -> anyhow::Result<(u16, Value)> {
    let mut request = client.request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

/// The error code of a refusal, or the empty string.
fn code(body: &Value) -> String {
    body["error"]["code"].as_str().unwrap_or_default().to_string()
}

pub(crate) async fn group_e_governance_admin(out: &mut Vec<Outcome>) {
    let key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.trim().is_empty());
    match &key {
        Some(key) => match live(key).await {
            Ok(outcomes) => out.extend(outcomes),
            Err(error) => {
                for id in [MEMBER_FREEZE, ADMIN_FREEZE, MEMBER_UNFREEZE, ADMIN_UNFREEZE] {
                    out.push(errored(id, Kind::Live, anyhow::anyhow!("{error:#}")));
                }
            }
        },
        None => {
            for id in [MEMBER_FREEZE, ADMIN_FREEZE, MEMBER_UNFREEZE, ADMIN_UNFREEZE] {
                out.push(skip(id, Kind::Live, "ANTHROPIC_API_KEY not set"));
            }
        }
    }
    // No model call: the agent only gives the servers something to serve.
    out.push(match anonymous(key.as_deref().unwrap_or("no-model-call")).await {
        Ok(outcome) => outcome,
        Err(error) => errored(ANONYMOUS, Kind::Local, error),
    });
}

fn agent(key: &str) -> anyhow::Result<Arc<dyn adk_core::Agent>> {
    let model: Arc<dyn Llm> = Arc::new(AnthropicClient::new(
        AnthropicConfig::new(key, ANTHROPIC_MODEL).with_max_tokens(64),
    )?);
    plain_agent(APP, "You are governed_agent. Reply with one short sentence.", model)
}

async fn live(key: &str) -> anyhow::Result<Vec<Outcome>> {
    let agent = agent(key)?;
    let control = GovernanceControl::new();
    let (base, task) = serve(|_| {
        let config = ServerConfig::new(
            Arc::new(SingleAgentLoader::new(agent)),
            Arc::new(InMemorySessionService::new()),
        )
        .with_request_context(Arc::new(Tokens))
        .with_governance(control.clone());
        ServerBuilder::new(config).enable_governance_endpoints().build()
    })
    .await?;
    let client = http();
    let freeze = |token: &'static str, reason: &str| {
        send(
            &client,
            reqwest::Method::POST,
            format!("{base}/api/admin/freeze"),
            Some(token),
            Some(json!({ "reason": reason })),
        )
    };
    let unfreeze = |token: &'static str| {
        send(
            &client,
            reqwest::Method::POST,
            format!("{base}/api/admin/unfreeze"),
            Some(token),
            None,
        )
    };
    let run = |session: &str| {
        send(
            &client,
            reqwest::Method::POST,
            format!("{base}/api/run"),
            Some(MEMBER),
            Some(run_body(APP, "mia", session, "Say hello in five words.")),
        )
    };
    let text = |body: &Value| {
        let events: Vec<adk_core::Event> = serde_json::from_value(body.clone()).unwrap_or_default();
        event_text(&events)
    };
    let mut outcomes = Vec::new();

    // E6: a member's freeze is refused, and the agent keeps answering.
    let (status, body) = freeze(MEMBER, "member drill").await?;
    let (ran, answer) = run("s-before").await?;
    let answer = text(&answer);
    outcomes.push(outcome(
        MEMBER_FREEZE,
        Kind::Live,
        status == 403 && !control.is_frozen() && ran == 200 && !answer.trim().is_empty(),
        format!(
            "member freeze HTTP {status} {}; frozen={}; next run HTTP {ran}, agent said {:?}",
            code(&body),
            control.is_frozen(),
            truncate(&answer, 50)
        ),
    ));

    // E7: an administrator's freeze refuses the agent's next run.
    let (status, _) = freeze(ADMIN, "incident drill").await?;
    let (ran, body) = run("s-frozen").await?;
    outcomes.push(outcome(
        ADMIN_FREEZE,
        Kind::Live,
        status == 200
            && control.is_frozen()
            && ran == 403
            && body.to_string().contains("governance.frozen"),
        format!(
            "admin freeze HTTP {status}; frozen={}; next run HTTP {ran} {}",
            control.is_frozen(),
            truncate(&body.to_string(), 80)
        ),
    ));

    // E8: a member's unfreeze is refused, and the freeze stays in force.
    let (status, body) = unfreeze(MEMBER).await?;
    outcomes.push(outcome(
        MEMBER_UNFREEZE,
        Kind::Live,
        status == 403 && control.is_frozen(),
        format!("member unfreeze HTTP {status} {}; frozen={}", code(&body), control.is_frozen()),
    ));

    // E9: the administrator's unfreeze restores the agent.
    let (status, _) = unfreeze(ADMIN).await?;
    let (ran, answer) = run("s-after").await?;
    let answer = text(&answer);
    outcomes.push(outcome(
        ADMIN_UNFREEZE,
        Kind::Live,
        status == 200 && !control.is_frozen() && ran == 200 && !answer.trim().is_empty(),
        format!(
            "admin unfreeze HTTP {status}; frozen={}; next run HTTP {ran}, agent said {:?}",
            control.is_frozen(),
            truncate(&answer, 50)
        ),
    ));
    task.abort();
    Ok(outcomes)
}

/// E10: a freeze without credentials, on a server with an extractor and on one without.
async fn anonymous(key: &str) -> anyhow::Result<Outcome> {
    let client = http();
    let mut details = Vec::new();
    let mut refused = true;
    for extractor in [true, false] {
        let agent = agent(key)?;
        let control = GovernanceControl::new();
        let (base, task) = serve(|_| {
            let mut config = ServerConfig::new(
                Arc::new(SingleAgentLoader::new(agent)),
                Arc::new(InMemorySessionService::new()),
            )
            .with_governance(control.clone());
            if extractor {
                config = config.with_request_context(Arc::new(Tokens));
            }
            ServerBuilder::new(config).enable_governance_endpoints().build()
        })
        .await?;
        let (status, body) = send(
            &client,
            reqwest::Method::POST,
            format!("{base}/api/admin/freeze"),
            None,
            Some(json!({ "reason": "anonymous drill" })),
        )
        .await?;
        task.abort();
        refused &= status == 401 && !control.is_frozen();
        let server = if extractor { "with an extractor" } else { "without an extractor" };
        details.push(format!(
            "{server}: HTTP {status} {}, frozen={}",
            code(&body),
            control.is_frozen()
        ));
    }
    Ok(outcome(ANONYMOUS, Kind::Local, refused, details.join("; ")))
}
