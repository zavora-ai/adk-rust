//! `transfer_roundtrip`: coordinator → specialist → coordinator across two user turns
//! (#747, #749, #759).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result as AdkResult,
    SchemaAdapter,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::json;

use crate::common::{
    Config, CountingTool, Provider, Verdict, brief, fail, pair_calls, run_turn, session_events,
};

const APP: &str = "autonomy-transfer";
const USER: &str = "user-transfer";

const COORDINATOR: &str = "You are coordinator, the front desk of a support team. You never answer \
billing or shipping questions yourself and you have no tools for them. For an invoice or billing \
question, call transfer_to_agent with agent_name \"billing_agent\". For a shipping, order-location \
or delivery question, call transfer_to_agent with agent_name \"shipping_agent\". If your own most \
recent action for the user's latest message was already a transfer and control has now come back \
to you, do not transfer again: reply with one short sentence saying the specialist has handled the \
request.";

const BILLING: &str = "You are billing_agent. For the user's latest message, first call \
lookup_invoice with the invoice id it mentions. After the tool returns, call transfer_to_agent \
with agent_name \"coordinator\" to hand control back. Do not write any text to the user; only make \
those two tool calls.";

const SHIPPING: &str = "You are shipping_agent. For the user's latest message, first call \
track_shipment with the order id it mentions. After the tool returns, call transfer_to_agent with \
agent_name \"coordinator\" to hand control back. Do not write any text to the user; only make \
those two tool calls.";

struct Team {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
    invoices: Arc<CountingTool>,
    shipments: Arc<CountingTool>,
}

async fn team(
    specialist_model: Arc<dyn Llm>,
    coordinator_model: Arc<dyn Llm>,
    session_id: &str,
) -> anyhow::Result<Team> {
    let invoices = CountingTool::new(
        "lookup_invoice",
        "Looks up an invoice by id and returns the amount due and its status.",
        json!({
            "type": "object",
            "properties": { "invoice_id": { "type": "string", "description": "Invoice id, e.g. INV-1001" } },
            "required": ["invoice_id"]
        }),
        |_, args| {
            Ok(
                json!({ "invoice_id": args["invoice_id"], "amount_due": "42.00 USD", "status": "unpaid" }),
            )
        },
    );
    let shipments = CountingTool::new(
        "track_shipment",
        "Returns the current location and status of an order's shipment.",
        json!({
            "type": "object",
            "properties": { "order_id": { "type": "string", "description": "Order id, e.g. ORD-7" } },
            "required": ["order_id"]
        }),
        |_, args| {
            Ok(
                json!({ "order_id": args["order_id"], "location": "Nairobi hub", "status": "in transit" }),
            )
        },
    );
    let billing = LlmAgentBuilder::new("billing_agent")
        .description("Handles invoices, amounts due and billing questions.")
        .model(Arc::clone(&specialist_model))
        .instruction(BILLING)
        .tool(invoices.clone())
        .build()?;
    let shipping = LlmAgentBuilder::new("shipping_agent")
        .description("Handles shipping, order location and delivery questions.")
        .model(specialist_model)
        .instruction(SHIPPING)
        .tool(shipments.clone())
        .build()?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Front desk that routes requests to specialists.")
        .model(coordinator_model)
        .instruction(COORDINATOR)
        .sub_agent(Arc::new(billing))
        .sub_agent(Arc::new(shipping))
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(coordinator) as Arc<dyn Agent>)
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .build()?;
    Ok(Team { runner, sessions, invoices, shipments })
}

/// Hands back to `coordinator` twice in one session and checks every call is answered once.
async fn natural(cfg: &Config, provider: Provider) -> Verdict {
    let model = match cfg.model(provider) {
        Ok(model) => model as Arc<dyn Llm>,
        Err(error) => return fail("model setup", error),
    };
    let session_id = "transfer-natural";
    let team = match team(Arc::clone(&model), model, session_id).await {
        Ok(team) => team,
        Err(error) => return fail("agent setup", error),
    };

    let first =
        run_turn(&team.runner, USER, session_id, "What is the amount due on invoice INV-1001?")
            .await;
    if let Some(error) = &first.error {
        return Verdict::Fail(format!("turn 1 failed: {}", brief(error)));
    }
    let transfers = first.transfers();
    if !transfers.iter().any(|(from, to)| from == "coordinator" && to == "billing_agent") {
        return Verdict::Retry(format!(
            "coordinator did not transfer to billing_agent: {transfers:?}"
        ));
    }
    if !transfers.iter().any(|(from, to)| from == "billing_agent" && to == "coordinator") {
        return Verdict::Retry(format!("billing_agent did not hand back: {transfers:?}"));
    }

    let second =
        run_turn(&team.runner, USER, session_id, "Thanks. Where is my order ORD-7 right now?")
            .await;
    if let Some(error) = &second.error {
        return Verdict::Fail(format!("turn 2 (after hand-back) rejected: {}", brief(error)));
    }

    let events = match session_events(team.sessions.as_ref(), APP, USER, session_id).await {
        Ok(events) => events,
        Err(error) => return fail("session read", error),
    };
    let (pairings, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Verdict::Fail(format!("history pairing: {}", problems.join("; ")));
    }
    let hops: Vec<String> = first
        .transfers()
        .into_iter()
        .chain(second.transfers())
        .map(|(from, to)| format!("{from}->{to}"))
        .collect();
    Verdict::Pass(format!(
        "2 turns accepted; hops {}; {} calls each answered once; lookup_invoice={} track_shipment={}",
        hops.join(","),
        pairings.len(),
        team.invoices.executions(),
        team.shipments.executions()
    ))
}

/// Replays scripted coordinator responses, then defers to the real model.
struct ScriptedThenReal {
    script: Mutex<VecDeque<LlmResponse>>,
    real: Arc<dyn Llm>,
}

impl ScriptedThenReal {
    fn remaining(&self) -> usize {
        self.script.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len()
    }
}

#[async_trait]
impl Llm for ScriptedThenReal {
    fn name(&self) -> &str {
        self.real.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        let scripted =
            self.script.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pop_front();
        match scripted {
            Some(response) => Ok(Box::pin(futures::stream::iter([Ok(response)]))),
            None => self.real.generate_content(req, stream).await,
        }
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.real.schema_adapter()
    }
}

/// `[transfer("ghost_agent"), transfer("billing_agent")]` in one turn, then a real second turn
/// whose request carries both calls.
async fn invalid_then_valid(cfg: &Config, provider: Provider) -> Verdict {
    let model = match cfg.model(provider) {
        Ok(model) => model as Arc<dyn Llm>,
        Err(error) => return fail("model setup", error),
    };
    let call = |id: &str, target: &str| Part::FunctionCall {
        name: "transfer_to_agent".to_string(),
        args: json!({ "agent_name": target }),
        id: Some(id.to_string()),
        thought_signature: None,
    };
    let scripted = Arc::new(ScriptedThenReal {
        script: Mutex::new(VecDeque::from([
            LlmResponse {
                content: Some(Content {
                    role: "model".to_string(),
                    parts: vec![
                        call("call_ghost_1", "ghost_agent"),
                        call("call_billing_1", "billing_agent"),
                    ],
                }),
                turn_complete: true,
                ..Default::default()
            },
            LlmResponse {
                content: Some(
                    Content::new("model")
                        .with_text("The billing specialist has handled your request."),
                ),
                turn_complete: true,
                ..Default::default()
            },
        ])),
        real: Arc::clone(&model),
    });
    let session_id = "transfer-invalid-target";
    let team = match team(model, Arc::clone(&scripted) as Arc<dyn Llm>, session_id).await {
        Ok(team) => team,
        Err(error) => return fail("agent setup", error),
    };

    let first =
        run_turn(&team.runner, USER, session_id, "What is the amount due on invoice INV-2002?")
            .await;
    if let Some(error) = &first.error {
        return Verdict::Fail(format!("turn 1 failed: {}", brief(error)));
    }
    if scripted.remaining() != 0 {
        return Verdict::Retry(format!(
            "billing_agent did not hand back (transfers {:?})",
            first.transfers()
        ));
    }

    let second =
        run_turn(&team.runner, USER, session_id, "Thanks. Where is my order ORD-9 right now?")
            .await;
    if let Some(error) = &second.error {
        return Verdict::Fail(format!(
            "turn 2 with the invalid-then-valid calls in history rejected: {}",
            brief(error)
        ));
    }

    let events = match session_events(team.sessions.as_ref(), APP, USER, session_id).await {
        Ok(events) => events,
        Err(error) => return fail("session read", error),
    };
    let (pairings, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Verdict::Fail(format!("history pairing: {}", problems.join("; ")));
    }
    let answers = |id: &str| {
        pairings
            .iter()
            .find(|pairing| pairing.id == id)
            .map(|pairing| pairing.responses.len())
            .unwrap_or_default()
    };
    if answers("call_ghost_1") != 1 || answers("call_billing_1") != 1 {
        return Verdict::Fail(format!(
            "ghost answered {}x, billing answered {}x",
            answers("call_ghost_1"),
            answers("call_billing_1")
        ));
    }
    Verdict::Pass(format!(
        "ghost+billing calls answered once each; real turn 2 accepted; {} calls paired",
        pairings.len()
    ))
}

pub async fn run(cfg: &Config, provider: Provider) -> Verdict {
    let natural = natural(cfg, provider).await;
    let Verdict::Pass(natural_evidence) = natural else { return natural };
    match invalid_then_valid(cfg, provider).await {
        Verdict::Pass(evidence) => {
            Verdict::Pass(format!("{natural_evidence} | invalid target: {evidence}"))
        }
        Verdict::Fail(reason) => Verdict::Fail(format!("invalid target: {reason}")),
        Verdict::Retry(reason) => Verdict::Retry(format!("invalid target: {reason}")),
        Verdict::Skip(reason) => Verdict::Skip(reason),
    }
}
