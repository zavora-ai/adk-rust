//! Group I: background runs and cron jobs in open mode.
//!
//! A server with no `RequestContextExtractor` serves `/api/runs` and `/api/cron` in open mode.
//! These scenarios check that building such a server warns before any request, that an
//! open-mode `idempotencyKey` belongs to the user the request names, that open-mode keys stay
//! apart from cron firings and authenticated callers, and that any client cancels and wakes
//! any run.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::schedule::firing_idempotency_key;
use adk_core::{Agent, Enqueued, RequestContext, SingleAgentLoader};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_runner::Runner;
use adk_server::background::{
    BackgroundState, CronState, WorkflowRegistry, background_runs_router_with_state,
    cron_jobs_router_with_state,
};
use adk_server::{RequestContextError, RequestContextExtractor, ServerBuilder, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};

use crate::scripted::ScriptedModel;
use crate::{
    Kind, Outcome, errored, http, outcome, plain_agent, scripted_model, serve, skip, truncate,
};

const ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";

/// Formatted tracing output, shared with the subscriber that writes it.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The warnings `adk_server::background` logs while `work` runs.
fn background_warnings(work: impl FnOnce()) -> Vec<String> {
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .finish();
    tracing::subscriber::with_default(subscriber, work);
    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    text.lines()
        .filter(|line| line.contains("WARN") && line.contains("adk_server::background"))
        .map(str::to_string)
        .collect()
}

/// Authenticates the user named in `x-user`.
struct HeaderUser;

#[async_trait]
impl RequestContextExtractor for HeaderUser {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let user = parts
            .headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        Ok(RequestContext {
            user_id: user.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

fn config(agent: Arc<dyn Agent>, authenticated: bool) -> ServerConfig {
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(agent)),
        Arc::new(InMemorySessionService::new()),
    );
    if authenticated { config.with_request_context(Arc::new(HeaderUser)) } else { config }
}

/// A run store no worker claims from, with a `report` workflow and cron jobs over it.
fn idle_background() -> (BackgroundState, CronState) {
    let workflows =
        WorkflowRegistry::new().register("report", |_input, _cancel| async { Ok(json!({})) });
    let background =
        BackgroundState::new().with_executor(Arc::new(workflows)).with_embedded_worker(false);
    let cron = CronState::new(background.clone());
    (background, cron)
}

fn open_server(agent: Arc<dyn Agent>, background: BackgroundState, cron: CronState) -> Router {
    ServerBuilder::new(config(agent, false))
        .with_background_runs(background)
        .with_cron_jobs(cron)
        .build()
}

async fn call(
    method: reqwest::Method,
    url: String,
    user: Option<&str>,
    body: Option<Value>,
) -> anyhow::Result<(u16, Value)> {
    let mut request = http().request(method, url);
    if let Some(user) = user {
        request = request.header("x-user", user);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

fn id(body: &Value) -> String {
    body["runId"].as_str().unwrap_or_default().to_string()
}

pub(crate) async fn group_i(model: &ScriptedModel, out: &mut Vec<Outcome>) {
    let idle = match scripted_model(model).and_then(|m| plain_agent("idle", "You are idle.", m)) {
        Ok(agent) => agent,
        Err(error) => {
            out.push(errored("I1 open-mode-warned-at-mount", Kind::Local, error));
            return;
        }
    };
    out.push(warned_at_mount(idle.clone()));
    out.push(match keys_per_user_live().await {
        Ok(outcome) => outcome,
        Err(error) => errored("I2 open-mode-key-per-user-live", Kind::Live, error),
    });
    out.push(match keys_kept_apart(idle.clone()).await {
        Ok(outcome) => outcome,
        Err(error) => errored("I3 open-mode-keys-kept-apart", Kind::Local, error),
    });
    out.push(match any_client_cancels_and_wakes(idle).await {
        Ok(outcome) => outcome,
        Err(error) => errored("I4 open-mode-delete-and-wake", Kind::Local, error),
    });
}

/// Building an open-mode server warns before any request; an authenticated one does not.
fn warned_at_mount(agent: Arc<dyn Agent>) -> Outcome {
    let (background, cron) = idle_background();
    let open = background_warnings(|| drop(open_server(agent.clone(), background, cron)));
    let (background, cron) = idle_background();
    let authenticated = background_warnings(|| {
        drop(
            ServerBuilder::new(config(agent, true))
                .with_background_runs(background)
                .with_cron_jobs(cron)
                .build(),
        );
    });
    let (background, cron) = idle_background();
    let standalone = background_warnings(|| {
        drop(background_runs_router_with_state(background));
        drop(cron_jobs_router_with_state(cron));
    });
    let named = open.iter().any(|line| line.contains("/api/runs") && line.contains("/api/cron"));
    outcome(
        "I1 open-mode-warned-at-mount",
        Kind::Local,
        named && authenticated.is_empty() && standalone.len() == 2,
        format!(
            "open build: {} warning(s){}; authenticated build: {}; standalone routers: {}",
            open.len(),
            open.first().map(|line| format!(" — `{line}`")).unwrap_or_default(),
            authenticated.len(),
            standalone.len(),
        ),
    )
}

/// Two users submit background runs of a live agent with the same idempotency key.
async fn keys_per_user_live() -> anyhow::Result<Outcome> {
    const ID: &str = "I2 open-mode-key-per-user-live";
    let Some(key) =
        std::env::var("ANTHROPIC_API_KEY").ok().filter(|value| !value.trim().is_empty())
    else {
        return Ok(skip(ID, Kind::Live, "no ANTHROPIC_API_KEY"));
    };
    let model = Arc::new(AnthropicClient::new(
        AnthropicConfig::new(key, ANTHROPIC_MODEL).with_max_tokens(32),
    )?);
    let agent = plain_agent(
        "echo_agent",
        "Reply with exactly the one word the user names, in capitals, and nothing else.",
        model,
    )?;
    let runner = Arc::new(
        Runner::builder()
            .app_name("background_probe")
            .agent(agent.clone())
            .session_service(Arc::new(InMemorySessionService::new()))
            .build()?,
    );
    let background = BackgroundState::new().with_agent("echo", runner);
    let (base, task) = serve(|_| {
        ServerBuilder::new(config(agent, false)).with_background_runs(background).build()
    })
    .await?;

    let runs = format!("{base}/api/runs");
    let submit = |user: &'static str, word: &'static str| {
        let body = json!({
            "target": "echo",
            "input": format!("The word is {word}."),
            "userId": user,
            "idempotencyKey": "evt-1",
        });
        call(reqwest::Method::POST, runs.clone(), None, Some(body))
    };
    let (alice_status, alice) = submit("alice", "ALPHA").await?;
    let (bob_status, bob) = submit("bob", "BRAVO").await?;
    let (retry_status, retry) = submit("alice", "ALPHA").await?;

    let mut answers = Vec::new();
    for run in [id(&alice), id(&bob)] {
        answers.push(finished(&base, &run).await?);
    }
    task.abort();

    let alice_text = answers[0]["result"]["text"].as_str().unwrap_or_default().to_string();
    let bob_text = answers[1]["result"]["text"].as_str().unwrap_or_default().to_string();
    let apart = alice_status == 201
        && bob_status == 201
        && bob["existing"] == json!(false)
        && id(&alice) != id(&bob);
    let retried = retry_status == 200 && id(&retry) == id(&alice);
    let answered = alice_text.contains("ALPHA") && bob_text.contains("BRAVO");
    let detail = if id(&alice) == id(&bob) {
        format!(
            "bob's key returned alice's run ({bob_status}, existing {}); bob read alice's answer \
             `{}`",
            bob["existing"],
            truncate(&bob_text, 40)
        )
    } else {
        format!(
            "alice {alice_status} → `{}`, bob {bob_status} (existing {}) → `{}`; alice's retry \
             {retry_status} returns her run: {retried}",
            truncate(&alice_text, 40),
            bob["existing"],
            truncate(&bob_text, 40)
        )
    };
    Ok(outcome(ID, Kind::Live, apart && retried && answered, detail))
}

/// Polls `run` until it finishes.
async fn finished(base: &str, run: &str) -> anyhow::Result<Value> {
    for _ in 0..180 {
        let (_, body) =
            call(reqwest::Method::GET, format!("{base}/api/runs/{run}"), None, None).await?;
        if matches!(body["status"].as_str(), Some("completed" | "failed" | "cancelled")) {
            return Ok(body);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("run {run} did not finish")
}

/// An open-mode key never takes a cron firing's key, nor an authenticated caller's.
async fn keys_kept_apart(agent: Arc<dyn Agent>) -> anyhow::Result<Outcome> {
    const ID: &str = "I3 open-mode-keys-kept-apart";
    let (background, cron) = idle_background();
    let (open, open_task) =
        serve(|_| open_server(agent.clone(), background.clone(), cron.clone())).await?;
    let (authed, authed_task) = serve(|_| {
        ServerBuilder::new(config(agent, true)).with_background_runs(background.clone()).build()
    })
    .await?;

    let nightly =
        json!({ "name": "nightly", "workflowId": "report", "cronExpression": "0 0 3 * * *" });
    call(reqwest::Method::POST, format!("{open}/api/cron"), None, Some(nightly)).await?;
    let schedule = cron
        .schedules
        .get("nightly")
        .await?
        .ok_or_else(|| anyhow::anyhow!("the cron job was not stored"))?;
    let fire_at = schedule.next_fire_at.ok_or_else(|| anyhow::anyhow!("no next fire time"))?;
    let firing_key = firing_idempotency_key("nightly", fire_at);
    let body =
        json!({ "target": "report", "input": {}, "userId": "cron", "idempotencyKey": firing_key });
    call(reqwest::Method::POST, format!("{open}/api/runs"), None, Some(body)).await?;
    let fired = background.run_store().enqueue(schedule.run_for(fire_at)).await?;
    let firing_ran = matches!(fired, Enqueued::Created(_));

    let body = json!({ "target": "report", "input": {}, "idempotencyKey": "k" });
    let (_, by_alice) =
        call(reqwest::Method::POST, format!("{authed}/api/runs"), Some("alice"), Some(body))
            .await?;
    let body = json!({ "target": "report", "input": {}, "userId": "alice", "idempotencyKey": "k" });
    let (claim_status, by_claim) =
        call(reqwest::Method::POST, format!("{open}/api/runs"), None, Some(body)).await?;
    open_task.abort();
    authed_task.abort();
    let claim_apart = claim_status == 201 && id(&by_claim) != id(&by_alice);

    Ok(outcome(
        ID,
        Kind::Local,
        firing_ran && claim_apart,
        format!(
            "cron firing after a client took its key: {}; open-mode `alice` reusing \
             authenticated alice's key: {claim_status}, {}",
            if firing_ran { "its own run" } else { "deduplicated into the client's run" },
            if claim_apart { "its own run" } else { "her run" },
        ),
    ))
}

/// In open mode any client cancels and wakes any run.
async fn any_client_cancels_and_wakes(agent: Arc<dyn Agent>) -> anyhow::Result<Outcome> {
    const ID: &str = "I4 open-mode-delete-and-wake";
    let (background, cron) = idle_background();
    let (base, task) = serve(|_| open_server(agent, background, cron)).await?;
    let body = json!({ "target": "report", "input": {}, "userId": "alice" });
    let (_, created) =
        call(reqwest::Method::POST, format!("{base}/api/runs"), None, Some(body)).await?;
    let run = format!("{base}/api/runs/{}", id(&created));
    let (wake_status, woken) =
        call(reqwest::Method::POST, format!("{run}/wake"), None, None).await?;
    let (delete_status, deleted) = call(reqwest::Method::DELETE, run, None, None).await?;
    task.abort();
    Ok(outcome(
        ID,
        Kind::Local,
        wake_status == 200 && delete_status == 200 && deleted["status"] == "cancelled",
        format!(
            "an unidentified client woke alice's run ({wake_status}, {}) and cancelled it \
             ({delete_status}, {})",
            woken["status"], deleted["status"]
        ),
    ))
}
