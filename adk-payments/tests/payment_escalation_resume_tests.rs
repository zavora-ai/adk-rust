//! An escalated payment held for approval resumes once an approver decides it in the
//! approval store, and pays exactly once.
//!
//! The payment tool runs under `LlmAgent` and `Runner` with an in-memory session service, so
//! the held call is found in the session and dispatched again by the next run.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::identity::{SessionId, UserId};
use adk_core::{
    AdkError, Agent, ApprovalScope, ApprovalStore, Content, Event, InMemoryApprovalStore, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunConfig, ToolApproval,
};
use adk_payments::auth::ALL_PAYMENT_SCOPES;
use adk_payments::domain::{Cart, CartLine, Money, ProtocolExtensions, TransactionRecord};
use adk_payments::guardrail::{AmountThresholdGuardrail, PaymentPolicySet};
use adk_payments::kernel::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand,
    ListUnresolvedTransactionsRequest, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::tools::PaymentToolsetBuilder;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "store";
const USER: &str = "alice";
const SESSION: &str = "session-1";
const CHECKOUT: &str = "payments_checkout_create";

/// Counts the checkouts that reach the merchant, and records their totals in cents.
#[derive(Default)]
struct Merchant {
    created: AtomicUsize,
    totals: Mutex<Vec<i64>>,
}

fn unused<T>() -> Result<T> {
    Err(AdkError::tool("not used by this test"))
}

#[async_trait]
impl MerchantCheckoutService for Merchant {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        self.created.fetch_add(1, Ordering::SeqCst);
        self.totals.lock().unwrap().push(command.cart.total.amount_minor);
        let context = command.context;
        Ok(TransactionRecord::new(
            context.transaction_id,
            context.actor,
            context.merchant_of_record,
            context.mode,
            command.cart,
            chrono::Utc::now(),
        ))
    }
    async fn update_checkout(&self, _command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn get_checkout(&self, _lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        Ok(None)
    }
    async fn complete_checkout(
        &self,
        _command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        unused()
    }
    async fn cancel_checkout(&self, _command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn apply_order_update(&self, _command: OrderUpdateCommand) -> Result<TransactionRecord> {
        unused()
    }
}

#[async_trait]
impl TransactionStore for Merchant {
    async fn upsert(&self, _record: TransactionRecord) -> Result<()> {
        Ok(())
    }
    async fn get(&self, _lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        Ok(None)
    }
    async fn list_unresolved(
        &self,
        _request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        Ok(Vec::new())
    }
}

/// Replays scripted responses, then answers "done".
struct ScriptedModel(Mutex<VecDeque<LlmResponse>>);

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self.0.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn desk_for(cents: i64) -> Value {
    let total = Money::new("USD", cents, 2);
    let cart = Cart {
        cart_id: None,
        lines: vec![CartLine {
            line_id: "line-1".to_string(),
            merchant_sku: None,
            title: "Standing desk".to_string(),
            quantity: 1,
            unit_price: total.clone(),
            total_price: total.clone(),
            product_class: None,
            extensions: ProtocolExtensions::default(),
        }],
        subtotal: Some(total.clone()),
        adjustments: Vec::new(),
        total,
        affiliate_attribution: None,
        extensions: ProtocolExtensions::default(),
    };
    json!({ "merchantId": "desk-co", "merchantName": "Desk Co", "cart": cart })
}

/// A model that asks once to buy a $750 desk under call ID `call-desk`.
fn buying_model() -> Arc<dyn Llm> {
    let call = LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: CHECKOUT.to_string(),
                args: desk_for(75_000),
                id: Some("call-desk".to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    };
    Arc::new(ScriptedModel(Mutex::new(VecDeque::from([call]))))
}

async fn run(runner: &Runner, text: &str) -> Vec<Event> {
    let mut stream = runner
        .run(
            UserId::try_from(USER).unwrap(),
            SessionId::try_from(SESSION).unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .expect("the run starts");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("agent event"));
    }
    events
}

fn answer_to(events: &[Event], call_id: &str) -> Option<Value> {
    events
        .iter()
        .flat_map(|event| event.llm_response.content.iter().flat_map(|content| &content.parts))
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. }
                if id.as_deref() == Some(call_id) =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .next_back()
}

#[tokio::test]
async fn an_escalated_payment_resumes_from_the_approval_store_and_pays_once() {
    let merchant = Arc::new(Merchant::default());
    let review_above_fifty = PaymentPolicySet::new()
        .with(AmountThresholdGuardrail::new(Some(5_000), None).with_currency("USD", 2));
    let checkout = PaymentToolsetBuilder::new(merchant.clone(), merchant.clone())
        .with_payment_policies(review_above_fifty)
        .with_scope_guard(adk_auth::ScopeGuard::new(adk_auth::StaticScopeResolver::new(
            ALL_PAYMENT_SCOPES.to_vec(),
        )))
        .build()
        .tools()
        .into_iter()
        .find(|tool| tool.name() == CHECKOUT)
        .unwrap();
    let agent =
        LlmAgentBuilder::new("shopper").model(buying_model()).tool(checkout).build().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let store = Arc::new(InMemoryApprovalStore::new());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .run_config(RunConfig::builder().approval_store(store.clone()).build())
        .build()
        .unwrap();
    let scope = ApprovalScope::new(APP, USER, SESSION);

    // Run 1: the escalation holds the call, and the approver's inbox lists it.
    let events = run(&runner, "Buy the desk.").await;
    let held: Vec<_> =
        events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].tool_name, CHECKOUT);
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request, held[0]);

    // The approver decides it; run 2 resumes the held call and the payment goes through.
    store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();
    let events = run(&runner, "Approved.").await;
    assert_eq!(merchant.created.load(Ordering::SeqCst), 1);
    let answer = answer_to(&events, "call-desk").expect("the held call is answered");
    assert_eq!(answer["status"], "ok", "{answer}");
    assert!(events.iter().all(|event| event.actions.tool_confirmation.is_none()));
    assert!(!store.has_decision(&scope, &pending[0].fingerprint).await.unwrap());

    // Run 3: nothing is paid again.
    run(&runner, "Thanks.").await;
    assert_eq!(merchant.created.load(Ordering::SeqCst), 1);
    assert!(store.pending(&scope).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_approval_that_edits_the_cart_resumes_the_held_call_and_pays_the_edited_cart() {
    // spec 3.1 §10 C3: the approver's edited arguments reach the payment.
    let merchant = Arc::new(Merchant::default());
    let review_above_fifty = PaymentPolicySet::new()
        .with(AmountThresholdGuardrail::new(Some(5_000), None).with_currency("USD", 2));
    let checkout = PaymentToolsetBuilder::new(merchant.clone(), merchant.clone())
        .with_payment_policies(review_above_fifty)
        .with_scope_guard(adk_auth::ScopeGuard::new(adk_auth::StaticScopeResolver::new(
            ALL_PAYMENT_SCOPES.to_vec(),
        )))
        .build()
        .tools()
        .into_iter()
        .find(|tool| tool.name() == CHECKOUT)
        .unwrap();
    let agent =
        LlmAgentBuilder::new("shopper").model(buying_model()).tool(checkout).build().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let store = Arc::new(InMemoryApprovalStore::new());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .run_config(RunConfig::builder().approval_store(store.clone()).build())
        .build()
        .unwrap();
    let scope = ApprovalScope::new(APP, USER, SESSION);

    run(&runner, "Buy the desk.").await;
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);

    // The approver brings the desk down to 40 USD.
    let edited = ToolApproval::approve()
        .with_edited_args(desk_for(4_000))
        .with_message("approved at 40 USD");
    store.decide(&scope, &pending[0].fingerprint, edited).await.unwrap();
    let events = run(&runner, "Approved.").await;

    assert_eq!(merchant.created.load(Ordering::SeqCst), 1, "paid once");
    assert_eq!(*merchant.totals.lock().unwrap(), vec![4_000], "paid as edited");
    let answer = answer_to(&events, "call-desk").expect("the held call is answered");
    assert_eq!(answer["status"], "ok", "{answer}");
    assert!(store.pending(&scope).await.unwrap().is_empty());
}
