//! The approval service (Phase 2): a held call routed to an inbox with a quorum of two,
//! decided by two approvers between runs, and executed once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalEventKind, ApprovalNotification, ApprovalNotifier, ApprovalPolicy,
    ApprovalService, ApprovalState, ApproverSelector, InboxQuery, Llm, RunConfig,
    StoreApprovalService, Tool, async_trait,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService, SqliteApprovalStore};
use serde_json::json;

use crate::common::{
    Config, CountingTool, Provider, Verdict, brief, fail, pair_calls, run_turn, run_turn_with,
    session_events,
};

const USER: &str = "user-approvals";

#[derive(Debug, Default)]
struct Recorder(Mutex<Vec<ApprovalEventKind>>);

#[async_trait]
impl ApprovalNotifier for Recorder {
    async fn notify(&self, notification: &ApprovalNotification) -> adk_core::Result<()> {
        self.0.lock().expect("recorder lock").push(notification.kind);
        Ok(())
    }
}

pub async fn approval_service(cfg: &Config, provider: Provider) -> Verdict {
    match quorum_of_two(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

fn finance(name: &str) -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human(name)).with_groups(["finance"])
}

async fn quorum_of_two(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let model = cfg.model(provider)?;
    let invoice = CountingTool::new(
        "send_invoice",
        "Sends an invoice to a customer.",
        json!({
            "type": "object",
            "properties": {
                "invoice_id": { "type": "string" },
                "customer": { "type": "string" },
                "amount_usd": { "type": "integer" }
            },
            "required": ["invoice_id", "customer", "amount_usd"]
        }),
        |_, args| Ok(json!({ "sent": args["invoice_id"] })),
    );
    let agent = LlmAgentBuilder::new("billing_agent")
        .model(model as Arc<dyn Llm>)
        .instruction(
            "You send invoices. When asked, call send_invoice exactly once with the invoice id, \
             customer, and amount the user gives. If the call needs approval or fails, tell the \
             user and stop; do not retry within the same request.",
        )
        .tool(invoice.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("send_invoice")
        .build()?;

    let app = "autonomy-approval-service";
    let session_id = "approval-service";
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    // A durable store, as a deployment would use.
    let dir = tempfile::tempdir()?;
    let store = SqliteApprovalStore::new(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("approvals.db").display()
    ))
    .await?;
    store.migrate().await?;
    let recorder = Arc::new(Recorder::default());
    let service = Arc::new(
        StoreApprovalService::new(Arc::new(store))
            .with_notifier(recorder.clone())
            .with_router(ApprovalPolicy::new([ApproverSelector::group("finance")]).with_quorum(2)),
    );
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().approval_service(service.clone()).build())
        .build()?;

    // Run 1: the call is held and submitted to the inbox.
    let first =
        run_turn(&runner, USER, session_id, "Send invoice INV-77 to customer acme for 1200 USD.")
            .await;
    if let Some(error) = &first.error {
        return Ok(Verdict::Fail(format!("run 1 failed: {}", brief(error))));
    }
    if invoice.executions() != 0 {
        return Ok(Verdict::Fail("send_invoice executed before any approval".to_string()));
    }
    let Some(held) = first.events.iter().find_map(|event| event.actions.tool_confirmation.clone())
    else {
        return Ok(Verdict::Retry("model never called send_invoice in run 1".to_string()));
    };
    let inbox = service.inbox(InboxQuery::for_group("finance")).await?;
    let [request] = inbox.items.as_slice() else {
        return Ok(Verdict::Fail(format!(
            "expected one inbox request, found {}",
            inbox.items.len()
        )));
    };
    if request.fingerprint != held.fingerprint() {
        return Ok(Verdict::Fail("the inbox request is for another call".to_string()));
    }

    // The requester cannot approve their own call.
    let own = service.decide(&request.id, request.version, finance(USER)).await;
    if !own.as_ref().is_err_and(|error| error.code == "approval.self_approval") {
        return Ok(Verdict::Fail(format!("self-approval was not refused: {own:?}")));
    }

    // First approval: quorum 1 of 2, so the call stays held through run 2.
    let half = service.decide(&request.id, request.version, finance("ana")).await?;
    if half.state != ApprovalState::Pending {
        return Ok(Verdict::Fail(format!("one approval resolved the request: {:?}", half.state)));
    }
    let second = run_turn_with(&runner, USER, session_id, "", None).await;
    if let Some(error) = &second.error {
        return Ok(Verdict::Fail(format!("run 2 failed: {}", brief(error))));
    }
    if invoice.executions() != 0 {
        return Ok(Verdict::Fail("one of two approvals executed the call".to_string()));
    }

    // Second approval resolves it; run 3 resumes the held call under its original ID.
    let resolved = service.decide(&half.id, half.version, finance("bo")).await?;
    if resolved.state != ApprovalState::Approved {
        return Ok(Verdict::Fail(format!("two approvals left it {:?}", resolved.state)));
    }
    let third = run_turn_with(&runner, USER, session_id, "", None).await;
    if let Some(error) = &third.error {
        return Ok(Verdict::Fail(format!("run 3 failed: {}", brief(error))));
    }
    let fourth = run_turn_with(&runner, USER, session_id, "", None).await;
    if let Some(error) = &fourth.error {
        return Ok(Verdict::Fail(format!("run 4 failed: {}", brief(error))));
    }
    let executed = invoice.executions();
    if executed != 1 {
        return Ok(Verdict::Fail(format!("the approved call executed {executed} times")));
    }
    let events = session_events(sessions.as_ref(), app, USER, session_id).await?;
    let (pairings, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
    }
    let held_id = held.function_call_id.as_deref().unwrap_or_default();
    let answered = pairings
        .iter()
        .find(|pairing| pairing.id == held_id)
        .and_then(|pairing| pairing.responses.first())
        .is_some_and(|response| response.get("sent").is_some());
    if !answered {
        return Ok(Verdict::Fail("the held call was not answered by its execution".to_string()));
    }
    let kinds = recorder.0.lock().expect("recorder lock").clone();
    let expected = [
        ApprovalEventKind::Submitted,
        ApprovalEventKind::Decided,
        ApprovalEventKind::Decided,
        ApprovalEventKind::Resolved,
    ];
    if kinds != expected {
        return Ok(Verdict::Fail(format!("notifications {kinds:?}, expected {expected:?}")));
    }
    Ok(Verdict::Pass(format!(
        "run 1 held send_invoice and submitted 1 request (quorum 2); self-approval refused; \
         1st approval kept it held through run 2 (0 executions); 2nd approval resolved it; run 3 \
         resumed and executed 1x, run 4 0x; notifications {kinds:?}"
    )))
}
