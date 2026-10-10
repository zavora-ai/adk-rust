//! `graph_resume_once`: a functional workflow whose first task calls the model and whose second
//! task charges a customer survives two crashes without repeating either (#757).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_graph::checkpoint::{Checkpointer, SqliteCheckpointer};
use adk_graph::error::{GraphError, Result as GraphResult};
use adk_graph::functional::TaskContext;
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_rust_macros::{entrypoint, task};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::common::{Config, Provider, Verdict, brief, fail};

/// The model the summarising task calls; set per provider before each run.
static MODEL: Mutex<Option<Arc<dyn Llm>>> = Mutex::new(None);
static SUMMARIES: AtomicUsize = AtomicUsize::new(0);
static CHARGE_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static CHARGES: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn summarize_ticket(ctx: &mut TaskContext) -> GraphResult<Value> {
    SUMMARIES.fetch_add(1, Ordering::SeqCst);
    let model = MODEL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(|| GraphError::Other("no model configured".to_string()))?;
    let ticket = ctx.get::<String>("ticket").unwrap_or_default();
    let request = LlmRequest::new(
        model.name(),
        vec![Content::new("user").with_text(format!(
            "Summarize this support ticket in at most eight words. Reply with the summary only.\n\nTicket: {ticket}"
        ))],
    );
    let mut stream = model
        .generate_content(request, false)
        .await
        .map_err(|error| GraphError::Other(error.to_string()))?;
    let mut summary = String::new();
    while let Some(response) = stream.next().await {
        let response = response.map_err(|error| GraphError::Other(error.to_string()))?;
        for part in response.content.map(|content| content.parts).unwrap_or_default() {
            if let Part::Text { text } = part {
                summary.push_str(&text);
            }
        }
    }
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err(GraphError::Other("model returned an empty summary".to_string()));
    }
    Ok(json!(summary))
}

/// The first two attempts never finish, standing in for a process that dies before the charge.
#[task]
async fn charge_customer(_ctx: &mut TaskContext, amount_cents: i64) -> GraphResult<Value> {
    if CHARGE_ATTEMPTS.fetch_add(1, Ordering::SeqCst) < 2 {
        std::future::pending::<()>().await;
    }
    CHARGES.fetch_add(1, Ordering::SeqCst);
    Ok(json!({ "charged_cents": amount_cents }))
}

#[entrypoint]
async fn settle_ticket(ctx: &mut TaskContext) -> GraphResult<Value> {
    let summary = __task_summarize_ticket(ctx).await?;
    ctx.set("summary", summary);
    let receipt = __task_charge_customer(ctx, 1999).await?;
    ctx.set("receipt", receipt);
    Ok(Value::Null)
}

/// Drops the run once `charge_customer` has started its `attempt`-th try, like a process crash.
async fn crash_during_charge(
    run: impl std::future::Future<Output = GraphResult<State>>,
    attempt: usize,
) -> Result<(), String> {
    let charging = async {
        while CHARGE_ATTEMPTS.load(Ordering::SeqCst) < attempt {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(180), async {
        tokio::select! {
            result = run => Err(match result {
                Ok(_) => "the run completed instead of crashing".to_string(),
                Err(error) => format!("the run failed before the crash point: {error}"),
            }),
            () = charging => Ok(()),
        }
    })
    .await;
    outcome.unwrap_or_else(|_| Err("timed out waiting for the crash point".to_string()))
}

async fn latest(checkpointer: &Arc<dyn Checkpointer>, thread: &str) -> anyhow::Result<String> {
    Ok(checkpointer
        .load(thread)
        .await?
        .ok_or_else(|| anyhow::anyhow!("thread {thread} has no checkpoint"))?
        .checkpoint_id)
}

pub async fn run(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    *MODEL.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(cfg.model(provider)? as Arc<dyn Llm>);
    for counter in [&SUMMARIES, &CHARGE_ATTEMPTS, &CHARGES] {
        counter.store(0, Ordering::SeqCst);
    }

    let dir = tempfile::tempdir()?;
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("checkpoints.db").display());
    let checkpointer = Arc::new(SqliteCheckpointer::new(&url).await?) as Arc<dyn Checkpointer>;
    let workflow = SettleTicketAgent::new(Arc::clone(&checkpointer));
    let thread = format!("ticket-{}", provider.label());
    let mut initial = State::new();
    initial.insert(
        "ticket".to_string(),
        json!(
            "Customer was billed twice for order ORD-7 and wants one charge refunded before Friday."
        ),
    );

    // Run 1: the summary completes, then the process dies inside the charge.
    if let Err(reason) =
        crash_during_charge(workflow.invoke(initial.clone(), ExecutionConfig::new(&thread)), 1)
            .await
    {
        return Ok(Verdict::Fail(format!("run 1: {}", brief(&reason))));
    }
    // Run 2: resumed, and dies inside the charge again.
    let resume_from = latest(&checkpointer, &thread).await?;
    if let Err(reason) = crash_during_charge(
        workflow
            .invoke(initial.clone(), ExecutionConfig::new(&thread).with_resume_from(&resume_from)),
        2,
    )
    .await
    {
        return Ok(Verdict::Fail(format!("run 2: {}", brief(&reason))));
    }
    // Run 3: resumed from whatever run 2 left as the latest checkpoint.
    let resume_from = latest(&checkpointer, &thread).await?;
    let state = match workflow
        .invoke(initial, ExecutionConfig::new(&thread).with_resume_from(&resume_from))
        .await
    {
        Ok(state) => state,
        Err(error) => {
            return Ok(Verdict::Fail(format!("run 3 failed: {}", brief(&error.to_string()))));
        }
    };

    let (summaries, attempts, charges) = (
        SUMMARIES.load(Ordering::SeqCst),
        CHARGE_ATTEMPTS.load(Ordering::SeqCst),
        CHARGES.load(Ordering::SeqCst),
    );
    let summary = state.get("summary").and_then(Value::as_str).unwrap_or_default().to_string();
    if summaries != 1 || charges != 1 || attempts != 3 {
        return Ok(Verdict::Fail(format!(
            "after two crashes: model task ran {summaries}x, charge ran {charges}x (attempts {attempts}); expected 1, 1, 3"
        )));
    }
    if summary.is_empty() || state.get("receipt") != Some(&json!({ "charged_cents": 1999 })) {
        return Ok(Verdict::Fail(format!("final state incomplete: {state:?}")));
    }
    Ok(Verdict::Pass(format!(
        "2 crashes + 2 resumes on SqliteCheckpointer: LLM task 1x, charge 1x; summary \"{}\"",
        summary.chars().take(60).collect::<String>()
    )))
}
