//! Scenarios that run an `#[entrypoint]` workflow whose tasks call an `LlmAgent`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::Llm;
use adk_graph::error::{GraphError, Result};
use adk_graph::functional::{ExecutionLog, TaskContext, TaskStatus};
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_rust_macros::{entrypoint, task};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::scripted::{Reply, ScriptedServer};
use crate::{Calls, Context, Harness, Verdict, gemini, llm_agent, scripted_model, verdict};

/// The model the workflow tasks call. Entrypoints take only a `TaskContext`, so each scenario
/// installs its model here before it invokes one.
static MODEL: Mutex<Option<Arc<dyn Llm>>> = Mutex::new(None);
static DRAFTS: AtomicUsize = AtomicUsize::new(0);
/// When the drafting task's model answered, so a pause is timed from there.
static DRAFTED_AT: Mutex<Option<Instant>> = Mutex::new(None);
static CLASSIFICATIONS: AtomicUsize = AtomicUsize::new(0);
static REPLIES: AtomicUsize = AtomicUsize::new(0);
static SENT: AtomicUsize = AtomicUsize::new(0);

/// One turn of an `LlmAgent` under its own `Runner`, as a task's unit of work.
async fn ask(name: &str, instruction: &str, prompt: &str) -> Result<String> {
    let model =
        MODEL.lock().unwrap().clone().ok_or_else(|| GraphError::Other("no model".into()))?;
    let work = async {
        let agent = llm_agent(name, model, instruction, &Calls::default())?;
        let harness = Harness::new(agent, &format!("{name}-task"), HashMap::new()).await?;
        let turn = harness.turn(prompt, None).await?;
        match turn.error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(turn.text()),
        }
    };
    work.await.map_err(|error: anyhow::Error| GraphError::Other(format!("{error:#}")))
}

#[task]
async fn draft_refund_note(ctx: &mut TaskContext) -> Result<Value> {
    let _ = ctx;
    DRAFTS.fetch_add(1, Ordering::SeqCst);
    let note = ask(
        "drafter",
        "[draft] Write a one-sentence refund note for the customer.",
        "Refund order A-1 for a damaged kettle.",
    )
    .await?;
    *DRAFTED_AT.lock().unwrap() = Some(Instant::now());
    Ok(json!(note))
}

/// A person approves the refund. The retry covers a flaky approvals inbox, not the pause.
#[task(retry(max_attempts = 3, backoff = "1s"))]
async fn approve_refund(ctx: &mut TaskContext) -> Result<Value> {
    let approved: bool = ctx.interrupt("approve the refund?").await?;
    Ok(json!(approved))
}

#[entrypoint]
async fn refund_workflow(ctx: &mut TaskContext) -> Result<Value> {
    let note = __task_draft_refund_note(ctx).await?;
    let approved = __task_approve_refund(ctx).await?;
    ctx.set("note", note);
    ctx.set("approved", approved.clone());
    Ok(approved)
}

fn install(live: bool, server: &ScriptedServer) -> anyhow::Result<Option<Arc<dyn Llm>>> {
    let model = if live { gemini()? } else { Some(scripted_model(server)?) };
    MODEL.lock().unwrap().clone_from(&model);
    Ok(model)
}

/// The `interrupt-N` key a suspension reports.
fn continuation_key(message: &str) -> String {
    message
        .find("interrupt-")
        .map(|start| {
            message[start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// WG12: a task's retry policy does not retry a pause: the workflow suspends at once, with the
/// interrupt's own key, and the task is not recorded as failed.
pub async fn suspension_skips_retry(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_, _| {
        Reply::Text("We are refunding your damaged kettle in full.".into())
    }))
    .await?;
    if install(live, &server)?.is_none() {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    }
    DRAFTS.store(0, Ordering::SeqCst);
    let workflow = RefundWorkflowAgent::new(Arc::clone(&cx.checkpointer));
    let thread = cx.id("refund-workflow", live);

    let first = workflow.invoke(State::new(), ExecutionConfig::new(&thread)).await;
    // From the drafting task's answer, so a live model's latency is not counted.
    let first_elapsed = DRAFTED_AT.lock().unwrap().map(|at| at.elapsed()).unwrap_or_default();
    let message = first.err().map(|error| error.to_string()).unwrap_or_default();
    let key = continuation_key(&message);
    let latest = cx
        .checkpointer
        .load(&thread)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the suspended workflow left no checkpoint"))?;
    let log: ExecutionLog = latest
        .metadata
        .get("execution_log")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let failed: Vec<&String> = log
        .tasks
        .iter()
        .filter(|(_, record)| record.status == TaskStatus::Failed)
        .map(|(task, _)| task)
        .collect();

    let started = Instant::now();
    let second = workflow
        .invoke(
            State::new(),
            ExecutionConfig::new(&thread)
                .with_resume_from(&latest.checkpoint_id)
                .with_resume_value(&key, json!(true)),
        )
        .await;
    let second_elapsed = started.elapsed();
    let quick = Duration::from_millis(900);
    let approved = second.as_ref().ok().and_then(|state| state.get("approved").cloned());
    Ok(verdict(vec![
        (message.contains("suspended"), format!("run 1: {message:?}")),
        (
            first_elapsed < quick,
            format!("run 1 suspended {first_elapsed:?} after the draft was written"),
        ),
        (key == "interrupt-1", format!("continuation key reported: {key:?}")),
        (failed.is_empty(), format!("tasks the execution log records as failed: {failed:?}")),
        (
            approved == Some(json!(true)) && second_elapsed < quick,
            format!(
                "run 2, resumed with {key}=true, finished after {second_elapsed:?}: approved {approved:?} ({:?})",
                second.as_ref().err().map(ToString::to_string)
            ),
        ),
        (
            DRAFTS.load(Ordering::SeqCst) == 1,
            format!("the drafting task ran {} time(s)", DRAFTS.load(Ordering::SeqCst)),
        ),
    ]))
}

#[task]
async fn classify_ticket(ctx: &mut TaskContext) -> Result<Value> {
    let _ = ctx;
    CLASSIFICATIONS.fetch_add(1, Ordering::SeqCst);
    let category = ask(
        "classifier",
        "[classify] Classify the support ticket as billing, shipping, or other. Reply with the \
         one word.",
        "My kettle arrived broken.",
    )
    .await?;
    Ok(json!(category))
}

#[task]
async fn draft_reply(ctx: &mut TaskContext) -> Result<Value> {
    let _ = ctx;
    REPLIES.fetch_add(1, Ordering::SeqCst);
    let reply = ask(
        "replier",
        "[reply] Write a one-sentence reply to the customer.",
        "My kettle arrived broken.",
    )
    .await?;
    Ok(json!(reply))
}

#[task]
async fn send_reply(ctx: &mut TaskContext) -> Result<Value> {
    let _ = ctx;
    SENT.fetch_add(1, Ordering::SeqCst);
    Ok(json!("sent"))
}

#[entrypoint]
async fn triage_workflow(ctx: &mut TaskContext) -> Result<Value> {
    let category = __task_classify_ticket(ctx).await?;
    let reply = __task_draft_reply(ctx).await?;
    __task_send_reply(ctx).await?;
    ctx.set("category", category);
    ctx.set("reply", reply);
    Ok(json!("done"))
}

/// WG18: a caller cancels a running workflow, and no task starts after it.
pub async fn cancellation_stops_workflow(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_, _| {
        Reply::Slow(Duration::from_millis(800), Box::new(Reply::Text("shipping".into())))
    }))
    .await?;
    if install(live, &server)?.is_none() {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    }
    for count in [&CLASSIFICATIONS, &REPLIES, &SENT] {
        count.store(0, Ordering::SeqCst);
    }
    let workflow = TriageWorkflowAgent::new(Arc::clone(&cx.checkpointer));
    let token = CancellationToken::new();
    let config = ExecutionConfig::new(&cx.id("triage-workflow", live))
        .with_cancellation_token(token.clone());
    let cancel_after = if live { Duration::from_millis(100) } else { Duration::from_millis(200) };
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(cancel_after).await;
        canceller.cancel();
    });

    let started = Instant::now();
    let outcome = workflow.invoke(State::new(), config).await;
    let elapsed = started.elapsed();
    let error = outcome.as_ref().err().map(ToString::to_string).unwrap_or_default();
    let (classified, replied, sent) = (
        CLASSIFICATIONS.load(Ordering::SeqCst),
        REPLIES.load(Ordering::SeqCst),
        SENT.load(Ordering::SeqCst),
    );
    Ok(verdict(vec![
        (
            error.contains("cancelled"),
            format!("the workflow ended with: {:?}", if error.is_empty() { "Ok" } else { &error }),
        ),
        (
            classified == 1,
            format!("classify_ticket, running when cancelled, ran {classified} time(s)"),
        ),
        (
            replied == 0 && sent == 0,
            format!("tasks started after the cancel: draft_reply {replied}, send_reply {sent}"),
        ),
        (elapsed < Duration::from_secs(5), format!("invoke returned after {elapsed:?}")),
    ]))
}
