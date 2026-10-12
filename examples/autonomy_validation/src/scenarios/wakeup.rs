//! Wake-ups: a signed webhook starts an agent run, and a replay of the same delivery
//! starts nothing.
//!
//! The server's `POST /hooks/{name}` verifies the Standard Webhooks signature and
//! enqueues a durable run keyed by the `webhook-id`. A minimal claim loop stands in for
//! the durable worker (integration point: the server's embedded worker over the same
//! `DurableRunStore`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::run_store::{DurableRunStore, InMemoryRunStore, RunFilter, RunOutcome, RunState};
use adk_core::webhook::{SignedWebhook, WebhookSecret, WebhookSigner, WebhookVerifier};
use adk_core::{Agent, Content, Llm, SingleAgentLoader, Tool};
use adk_runner::Runner;
use adk_server::webhooks::{WebhookHook, WebhookIngress};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::common::{Config, CountingTool, Provider, Verdict, fail};

const APP: &str = "autonomy-wakeup";
const RUN_TIMEOUT: Duration = Duration::from_secs(180);

/// Claims every queued run and executes it through `runner`; aborted when the scenario ends.
async fn claim_loop(
    store: Arc<InMemoryRunStore>,
    sessions: Arc<InMemorySessionService>,
    runner: Arc<Runner>,
    claims: Arc<std::sync::atomic::AtomicUsize>,
) {
    loop {
        let claimed = match store.claim("default", "validation-worker", Duration::from_secs(120)).await {
            Ok(Some(claimed)) => claimed,
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(error) => {
                tracing::error!(error = %error, "claim failed");
                return;
            }
        };
        claims.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let record = claimed.record;
        let session_id = record.session_id.clone().unwrap_or_else(|| record.run_id.clone());
        let outcome = async {
            sessions
                .create(CreateRequest {
                    app_name: record.app_name.clone(),
                    user_id: record.user_id.clone(),
                    session_id: Some(session_id.clone()),
                    state: HashMap::new(),
                })
                .await?;
            let text = record.input["text"].as_str().unwrap_or_default().to_string();
            let mut stream = runner
                .run_str(&record.user_id, &session_id, Content::new("user").with_text(text))
                .await?;
            let mut answer = String::new();
            while let Some(event) = stream.next().await {
                if let Some(content) = event?.llm_response.content {
                    answer.extend(content.parts.iter().filter_map(|part| part.text()));
                }
            }
            Ok::<_, adk_core::AdkError>(answer)
        }
        .await;
        let outcome = match outcome {
            Ok(answer) => RunOutcome::Completed(json!({"text": answer})),
            Err(error) => RunOutcome::Failed { error: error.to_string(), retryable: false },
        };
        if let Err(error) = store.finish(&record.run_id, claimed.epoch, outcome).await {
            tracing::error!(error = %error, "finish failed");
        }
    }
}

async fn post(client: &reqwest::Client, url: &str, signed: &SignedWebhook, body: &[u8]) -> anyhow::Result<(u16, Value)> {
    let mut request = client.post(url).header("content-type", "application/json").body(body.to_vec());
    for (name, value) in signed.header_pairs() {
        request = request.header(name, value);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    Ok((status, body))
}

pub async fn webhook_wakeup(cfg: &Config, provider: Provider) -> Verdict {
    match run(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("webhook_wakeup", error),
    }
}

async fn run(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let model = cfg.model(provider)?;
    let tickets = CountingTool::new(
        "file_ticket",
        "Files a triage ticket for an issue number.",
        json!({
            "type": "object",
            "properties": { "issue_number": { "type": "integer" } },
            "required": ["issue_number"]
        }),
        |_, args| Ok(json!({ "ticket": format!("T-{}", args["issue_number"]) })),
    );
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("triage_agent")
            .model(model.clone() as Arc<dyn Llm>)
            .instruction(
                "You triage new issues. For every message, call file_ticket exactly once with the \
                 issue number from the message, then reply with the ticket id the tool returned.",
            )
            .tool(tickets.clone() as Arc<dyn Tool>)
            .build()?,
    );

    let store = Arc::new(InMemoryRunStore::new());
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(agent.clone())
            .session_service(sessions.clone() as Arc<dyn SessionService>)
            .build()?,
    );

    // The receiver's secret; the sender signs with the same bytes.
    let secret = WebhookSecret::from_bytes(b"autonomy-validation-webhook-key!".to_vec())?;
    let ingress = WebhookIngress::new(store.clone() as Arc<dyn DurableRunStore>).with_hook(
        WebhookHook::new("issues", WebhookVerifier::new(secret.clone()), APP, "triage_agent")
            .with_input_template(json!({"text": "New issue #{{/payload/issue/number}}: {{/payload/issue/title}}"}))
            .with_concurrency_key("repo:{{/payload/repository}}", adk_core::OnConflict::Enqueue),
    );
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(agent)),
        sessions.clone() as Arc<dyn SessionService>,
    );
    let app = ServerBuilder::new(config).with_webhook_ingress(ingress).build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/hooks/issues", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let claims = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker = tokio::spawn(claim_loop(store.clone(), sessions, runner, claims.clone()));
    let _guard = AbortOnDrop(vec![server.abort_handle(), worker.abort_handle()]);

    let client = reqwest::Client::new();
    let body = serde_json::to_vec(&json!({"issue": {"number": 4242, "title": "Checkout times out"}, "repository": "shop"}))?;
    let signer = WebhookSigner::new(secret);
    let delivery = signer.sign_now("evt_wakeup_1", &body);

    // An unsigned-by-us delivery is refused before anything is enqueued.
    let forged = WebhookSigner::new(WebhookSecret::from_bytes(b"forged-key-forged-key-forged-key".to_vec())?)
        .sign_now("evt_forged", &body);
    let (forged_status, _) = post(&client, &url, &forged, &body).await?;
    if forged_status != 401 {
        return Ok(Verdict::Fail(format!("a forged signature answered {forged_status}, not 401")));
    }

    let (status, accepted) = post(&client, &url, &delivery, &body).await?;
    if status != 202 {
        return Ok(Verdict::Fail(format!("the signed delivery answered {status}: {accepted}")));
    }
    let run_id = accepted["runId"].as_str().unwrap_or_default().to_string();

    let started = Instant::now();
    let record = loop {
        let record = store.get(&run_id).await?.ok_or_else(|| anyhow::anyhow!("run {run_id} vanished"))?;
        if record.state.is_terminal() {
            break record;
        }
        if started.elapsed() > RUN_TIMEOUT {
            return Ok(Verdict::Fail(format!("run {run_id} still {:?} after {}s", record.state, RUN_TIMEOUT.as_secs())));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    if record.state != RunState::Completed {
        return Ok(Verdict::Fail(format!("run ended {:?}: {:?}", record.state, record.error)));
    }
    let model_calls = model.calls();

    // The exact captured delivery replayed, then a sender retry re-signed under the same id.
    let (replay_status, replayed) = post(&client, &url, &delivery, &body).await?;
    let retry = signer.sign_now("evt_wakeup_1", &body);
    let (retry_status, retried) = post(&client, &url, &retry, &body).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let runs = store.list(RunFilter::default()).await?;
    let claimed = claims.load(std::sync::atomic::Ordering::SeqCst);
    let evidence = format!(
        "202 run {} completed ({} model calls, file_ticket ×{}); replay {replay_status} duplicate={} retry {retry_status} same_run={}; runs={} claims={} model calls after replay={}; forged 401",
        &run_id[..8.min(run_id.len())],
        model_calls,
        tickets.executions(),
        replayed["duplicate"],
        retried["runId"] == accepted["runId"],
        runs.len(),
        claimed,
        model.calls(),
    );
    let replay_ok = replay_status == 200
        && retry_status == 200
        && replayed["runId"] == accepted["runId"]
        && retried["runId"] == accepted["runId"]
        && replayed["state"] == "completed";
    if !replay_ok || runs.len() != 1 || claimed != 1 || model.calls() != model_calls {
        return Ok(Verdict::Fail(evidence));
    }
    if tickets.executions() != 1 {
        return Ok(Verdict::Retry(format!("the model called file_ticket {} times: {evidence}", tickets.executions())));
    }
    Ok(Verdict::Pass(evidence))
}

struct AbortOnDrop(Vec<tokio::task::AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for handle in &self.0 {
            handle.abort();
        }
    }
}
