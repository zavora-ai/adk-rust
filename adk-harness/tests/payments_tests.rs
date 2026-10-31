//! The payments pack (spec 3.1 §7.16, §5 H8, §9.2 I38, §9.3 `harness.payment`).
//!
//! A scripted model buys a domain through the `adk-payments` checkout tools under a
//! `PaymentMandate`. The pass conditions are on the merchant's counters, the spend ledger,
//! the approval store and the audit records, never on model prose.

#![cfg(feature = "payments")]

use crate::common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::{ScopeGuard, StaticScopeResolver};
use adk_core::{
    ActingContext, AdkError, Event, InMemorySpendLedger, PolicyDecision, Result, RunBudget,
    RunConfig, SpendKey, SpendLedger, SpendLimits, SpendPeriod, SubjectKind, SubjectRef,
    ToolApproval, ToolConfirmationRequest, ToolEffect, ToolOrigin, ToolPolicyRequest,
};
use adk_harness::approvals::{OriginPin, RuleScope, StandingDecision, StandingRule};
use adk_harness::packs::payments::{
    PAYMENT_AUDIT_ACTION, PAYMENT_TOOL_NAMES, PaymentMandate, PaymentsPack,
};
use adk_harness::{HarnessAgent, HarnessDecision, HarnessMode, Pack, ToolKind};
use adk_model::ScriptedResponse;
use adk_payments::auth::ALL_PAYMENT_SCOPES;
use adk_payments::domain::{
    Cart, CartLine, Money, OrderSnapshot, OrderState, ProtocolExtensions, ReceiptState,
    TransactionRecord, TransactionState,
};
use adk_payments::kernel::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand,
    ListUnresolvedTransactionsRequest, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use async_trait::async_trait;
use common::{Fixture, Outcome, fixture, no_user_file, outcome, response_of, responses};
use futures::StreamExt;
use serde_json::{Value, json};

const CREATE: &str = "payments_checkout_create";
const COMPLETE: &str = "payments_checkout_complete";
const MANDATE: &str = "mandate-42";

/// A registrar that counts what reaches it and journals every record it returns.
#[derive(Default)]
struct Merchant {
    created: AtomicUsize,
    completed: AtomicUsize,
    records: Mutex<HashMap<String, TransactionRecord>>,
}

fn unused<T>() -> Result<T> {
    Err(AdkError::tool("not used by this test"))
}

#[async_trait]
impl MerchantCheckoutService for Merchant {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        self.created.fetch_add(1, Ordering::SeqCst);
        let context = command.context;
        let mut record = TransactionRecord::new(
            context.transaction_id,
            context.actor,
            context.merchant_of_record,
            context.mode,
            command.cart,
            chrono::Utc::now(),
        );
        for state in [TransactionState::Negotiating, TransactionState::AwaitingPaymentMethod] {
            record
                .transition_to(state, chrono::Utc::now())
                .map_err(|e| AdkError::tool(e.to_string()))?;
        }
        self.upsert(record.clone()).await?;
        Ok(record)
    }
    async fn update_checkout(&self, _command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        TransactionStore::get(self, lookup).await
    }
    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        let id = command.context.transaction_id.as_str().to_string();
        let Some(mut record) = self.records.lock().unwrap().get(&id).cloned() else {
            return Err(AdkError::tool(format!("no checkout {id}")));
        };
        self.completed.fetch_add(1, Ordering::SeqCst);
        // Before the transitions, which rebuild the masked summary from the record.
        record.order = Some(OrderSnapshot {
            order_id: Some(format!("order-{id}")),
            receipt_id: Some(format!("receipt-{id}")),
            state: OrderState::Completed,
            receipt_state: ReceiptState::Settled,
            extensions: ProtocolExtensions::default(),
        });
        for state in [TransactionState::Authorized, TransactionState::Completed] {
            record
                .transition_to(state, chrono::Utc::now())
                .map_err(|e| AdkError::tool(e.to_string()))?;
        }
        self.upsert(record.clone()).await?;
        Ok(record)
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
    async fn upsert(&self, record: TransactionRecord) -> Result<()> {
        self.records.lock().unwrap().insert(record.transaction_id.as_str().to_string(), record);
        Ok(())
    }
    async fn get(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        Ok(self.records.lock().unwrap().get(lookup.transaction_id.as_str()).cloned())
    }
    async fn list_unresolved(
        &self,
        _request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        Ok(Vec::new())
    }
}

/// `payments_checkout_create` arguments for one domain year at `cents`.
fn domain_for(cents: i64) -> Value {
    let total = Money::new("USD", cents, 2);
    let cart = Cart {
        cart_id: None,
        lines: vec![CartLine {
            line_id: "line-1".to_string(),
            merchant_sku: None,
            title: "adk-harness-demo.example (1 year)".to_string(),
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
    json!({ "merchantId": "domainmart", "merchantName": "DomainMart", "cart": cart })
}

/// The key `SpendLimitGuardrail` reserves the registrar's payments under; model calls
/// reserve under their provider, so a cap on this key caps payments only.
fn registrar_key() -> SpendKey {
    SpendKey::org(common::APP).with_vendor("domainmart")
}

fn mandate() -> PaymentMandate {
    PaymentMandate::new(MANDATE, "domain registration", Money::new("USD", 2_000, 2))
        .with_merchants(["domainmart"])
}

fn pack(merchant: &Arc<Merchant>) -> PaymentsPack {
    PaymentsPack::new(merchant.clone(), merchant.clone(), mandate())
        .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(ALL_PAYMENT_SCOPES.to_vec())))
}

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn request(tool: &str, args: Value) -> ToolPolicyRequest {
    ToolPolicyRequest::new(tool, args)
        .with_effect(ToolEffect::NonIdempotent)
        .with_agent_name("harness")
        .with_identity(common::APP, "alice", "s1")
        .with_invocation_id("inv-1")
        .with_acting_context(ActingContext::derived(
            common::APP,
            "harness",
            "alice",
            SubjectKind::Human,
        ))
}

fn static_origin() -> ToolOrigin {
    ToolOrigin::new("static", &json!({ "name": "x" }), None)
}

/// Collects a run that may end in a budget error.
async fn collect_lenient(stream: adk_core::EventStream) -> (Vec<Event>, Vec<String>) {
    let results = tokio::time::timeout(Duration::from_secs(20), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(event) => events.push(event),
            Err(error) => errors.push(error.to_string()),
        }
    }
    (events, errors)
}

fn payment_records(fixture: &Fixture) -> Vec<Value> {
    fixture
        .records(PAYMENT_AUDIT_ACTION)
        .into_iter()
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

// --- Classes and the mode table ---------------------------------------------------------

#[tokio::test]
async fn every_payment_tool_is_class_payment_in_every_mode() {
    for mode in HarnessMode::ALL {
        let merchant = Arc::new(Merchant::default());
        let fixture = fixture(Vec::new(), |b| b.mode(mode).pack_payments(pack(&merchant))).await;
        let policy = fixture.harness.agent().runtime().policy();
        for tool in PAYMENT_TOOL_NAMES {
            let kind = policy.classify(&request(tool, json!({})), Some(&static_origin()));
            assert_eq!(kind, ToolKind::Payment, "{tool} in {mode}");
            let decision = policy
                .decide(&request(tool, domain_for(1_499)), Some(&static_origin()), Some("c"))
                .await;
            match mode {
                HarnessMode::ReadOnly | HarnessMode::Plan => {
                    assert!(
                        matches!(decision, PolicyDecision::Deny { .. }),
                        "{tool} in {mode}: {decision:?}"
                    );
                }
                HarnessMode::Ask | HarnessMode::Auto => match &decision {
                    PolicyDecision::RequireApproval { reason } => {
                        assert!(reason.contains("one-time approval"), "{tool} in {mode}: {reason}");
                    }
                    other => panic!("{tool} in {mode}: {other:?}"),
                },
                _ => unreachable!(),
            }
        }
        let info = fixture.harness.agent().info();
        assert_eq!(info.packs, vec!["payments".to_string()]);
        for tool in [CREATE, COMPLETE, "payments_checkout_cancel", "payments_status_lookup"] {
            assert!(info.tools.contains(&tool.to_string()), "{tool} missing from {:?}", info.tools);
        }
    }
}

#[tokio::test]
async fn without_the_pack_a_payment_tool_name_is_external() {
    let fixture = fixture(Vec::new(), |b| b).await;
    let policy = fixture.harness.agent().runtime().policy();
    let kind = policy.classify(&request(CREATE, json!({})), Some(&static_origin()));
    assert!(matches!(kind, ToolKind::External { .. }), "{kind:?}");
    assert!(fixture.harness.agent().info().packs.is_empty());
}

#[test]
fn pack_payments_without_services_is_refused_at_build() {
    let dir = tempfile::tempdir().unwrap();
    let Err(error) = HarnessAgent::builder("harness")
        .model(Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .pack(Pack::Payments)
        .build()
    else {
        panic!("Pack::Payments alone cannot build");
    };
    assert!(error.to_string().contains("pack_payments"), "{error}");
}

// --- Never waivable (spec 3.1 §9.2 I38) ------------------------------------------------

#[tokio::test]
async fn a_standing_rule_never_covers_a_payment() {
    let merchant = Arc::new(Merchant::default());
    let fixture = fixture(Vec::new(), |b| b.pack_payments(pack(&merchant))).await;
    let rules = fixture.harness.standing_rules().clone();
    let rule = |tool: &str| StandingRule {
        id: format!("rule-{tool}"),
        scope: RuleScope::User {
            app_name: common::APP.to_string(),
            user_id: "alice".to_string(),
            subject_kind: SubjectKind::Human,
        },
        tool: tool.to_string(),
        origin: OriginPin::AnyDeclaration { source: "static".to_string() },
        agent: None,
        predicates: Vec::new(),
        decision: StandingDecision::Allow,
        created_by: SubjectRef::human("alice"),
        created_at: chrono::Utc::now(),
        expires_at: None,
        label: format!("always allow {tool}"),
        source_fingerprint: None,
    };
    rules.grant(rule(CREATE)).await.unwrap();
    rules.grant(rule("write_file")).await.unwrap();
    assert!(!ToolKind::Payment.rule_eligible());

    let policy = fixture.harness.agent().runtime().policy();
    // The same rule machinery turns an edit's approval into Allow …
    let edit = request("write_file", json!({ "path": "src/a.rs", "content": "" }));
    assert_eq!(
        policy.decide(&edit, Some(&static_origin()), Some("c1")).await,
        PolicyDecision::Allow
    );
    // … and leaves a payment held.
    let pay = policy
        .decide(&request(CREATE, domain_for(1_499)), Some(&static_origin()), Some("c2"))
        .await;
    assert!(
        matches!(&pay, PolicyDecision::RequireApproval { reason } if reason.contains("no standing rule applies")),
        "{pay:?}"
    );

    // A run-wide `tool_approvals` entry naming a payment is stripped by the layer (§7.3).
    let mut config = RunConfig::default();
    let fingerprint = ToolConfirmationRequest::new(CREATE, domain_for(1_499)).fingerprint();
    config.tool_approvals.insert(fingerprint.clone(), ToolApproval::approve());
    let kept = ToolConfirmationRequest::new("read_file", json!({ "path": "a" })).fingerprint();
    config.tool_approvals.insert(kept.clone(), ToolApproval::approve());
    fixture.harness.agent().runtime().layer().apply(&mut config);
    assert!(!config.tool_approvals.contains_key(&fingerprint));
    assert!(config.tool_approvals.contains_key(&kept));
}

#[tokio::test]
async fn in_auto_a_payment_is_held_not_allowed() {
    let merchant = Arc::new(Merchant::default());
    let fixture = fixture(vec![call(CREATE, domain_for(1_499))], |b| {
        b.mode(HarnessMode::Auto).pack_payments(pack(&merchant))
    })
    .await;
    let events = fixture.send("Register the domain if it costs under $20.").await;
    assert_eq!(outcome(&response_of(&events, CREATE)), Outcome::Held);
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.harness.pending_approvals(&fixture.user, &fixture.session).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn in_plan_a_payment_is_denied() {
    let merchant = Arc::new(Merchant::default());
    let fixture = fixture(vec![call(CREATE, domain_for(1_499)), done()], |b| {
        b.mode(HarnessMode::Plan).pack_payments(pack(&merchant))
    })
    .await;
    let events = fixture.send("Register the domain if it costs under $20.").await;
    assert_eq!(outcome(&response_of(&events, CREATE)), Outcome::Denied);
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .harness
            .pending_approvals(&fixture.user, &fixture.session)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(payment_records(&fixture).is_empty(), "the tool never ran");
}

// --- Budget and mandate limits ----------------------------------------------------------

#[tokio::test]
async fn the_run_budget_refuses_a_payment_before_it_starts() {
    let merchant = Arc::new(Merchant::default());
    let fixture = fixture(vec![call(CREATE, domain_for(1_499))], |b| {
        b.pack_payments(pack(&merchant)).budget(RunBudget::new().max_tool_calls(0))
    })
    .await;
    let stream =
        fixture.harness.send(&fixture.user, &fixture.session, "Register it.").await.unwrap();
    let (events, errors) = collect_lenient(stream).await;
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    let held = responses(&events)
        .into_iter()
        .any(|(name, response)| name == CREATE && outcome(&response) == Outcome::Held);
    assert!(!held, "a refused batch is not held");
    let refused =
        responses(&events).into_iter().any(|(name, response)| {
            name == CREATE && response["error"].as_str().is_some_and(|e| e.contains("not run"))
        }) || errors.iter().any(|error| error.contains("budget") || error.contains("tool_calls"));
    assert!(refused, "events {:?}, errors {errors:?}", responses(&events));
    assert!(
        fixture
            .harness
            .pending_approvals(&fixture.user, &fixture.session)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_mandate_limit_refuses_a_purchase_over_it() {
    let merchant = Arc::new(Merchant::default());
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let fixture = fixture(vec![call(CREATE, domain_for(2_500)), done()], |b| {
        b.pack_payments(pack(&merchant)).prompter(prompter.clone())
    })
    .await;
    let events = fixture.send("Register the domain if it costs under $20.").await;
    let response = response_of(&events, CREATE);
    let error = response["error"].as_str().unwrap_or_default();
    assert!(error.contains("payments.policy.denied"), "{response}");
    assert_eq!(prompter.count(), 1, "the harness asked once; the mandate refused");
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    let records = payment_records(&fixture);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["mandateId"], MANDATE);
    assert_eq!(records[0]["decision"], "refused");
    assert_eq!(records[0]["amount"], "25.00");
    assert_eq!(records[0]["currency"], "USD");
}

#[tokio::test]
async fn a_merchant_outside_the_mandate_is_refused() {
    let merchant = Arc::new(Merchant::default());
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let mut args = domain_for(1_499);
    args["merchantId"] = json!("other-registrar");
    let fixture = fixture(vec![call(CREATE, args), done()], |b| {
        b.pack_payments(pack(&merchant)).prompter(prompter.clone())
    })
    .await;
    let events = fixture.send("Register it at the other registrar.").await;
    let response = response_of(&events, CREATE);
    assert!(
        response["error"].as_str().unwrap_or_default().contains("payments.policy.denied"),
        "{response}"
    );
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    assert_eq!(payment_records(&fixture)[0]["decision"], "refused");
}

#[tokio::test]
async fn without_scopes_the_tool_refuses_at_execution() {
    let merchant = Arc::new(Merchant::default());
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let fixture = fixture(vec![call(CREATE, domain_for(1_499)), done()], |b| {
        b.pack_payments(PaymentsPack::new(merchant.clone(), merchant.clone(), mandate()))
            .prompter(prompter.clone())
    })
    .await;
    let events = fixture.send("Register it.").await;
    let response = response_of(&events, CREATE);
    assert!(response["error"].is_string(), "the scope guard refuses: {response}");
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    assert_eq!(payment_records(&fixture)[0]["decision"], "failed");
}

// --- H8: paid once, after approval, within the mandate, receipted -----------------------

#[tokio::test]
async fn h8_a_purchase_is_held_approved_paid_once_and_receipted() {
    let merchant = Arc::new(Merchant::default());
    let ledger =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(registrar_key(), 20_000_000)));
    let fixture = fixture(vec![call(CREATE, domain_for(1_499)), done()], |b| {
        b.pack_payments(pack(&merchant))
            .spend_ledger(ledger.clone())
            .budget(RunBudget::new().max_tool_calls(10))
    })
    .await;
    let (user, session) = (fixture.user.clone(), fixture.session.clone());

    // Run 1: the checkout is held; nothing reaches the merchant; the inbox lists it.
    let events = fixture.send("Register adk-harness-demo.example if it costs under $20.").await;
    assert_eq!(outcome(&response_of(&events, CREATE)), Outcome::Held);
    assert_eq!(merchant.created.load(Ordering::SeqCst), 0);
    let pending = fixture.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.tool_name, CREATE);

    // The person approves from another process; the next turn runs the call once.
    fixture
        .harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    let events = fixture.resume().await;
    let created = response_of(&events, CREATE);
    assert_eq!(outcome(&created), Outcome::Executed, "{created}");
    assert_eq!(merchant.created.load(Ordering::SeqCst), 1);
    let transaction =
        created["summary"]["transactionId"].as_str().expect("a transaction id").to_string();
    assert_eq!(created["summary"]["total"]["amountMinor"], 1_499);

    // Run 2: completing the checkout moves the money, after its own approval.
    fixture.model.push(call(COMPLETE, json!({ "transactionId": transaction })));
    fixture.model.push(done());
    let events = fixture.send("Complete the purchase.").await;
    assert_eq!(outcome(&response_of(&events, COMPLETE)), Outcome::Held);
    assert_eq!(merchant.completed.load(Ordering::SeqCst), 0);
    let pending = fixture.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    fixture
        .harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    let events = fixture.resume().await;
    let completed = response_of(&events, COMPLETE);
    assert_eq!(outcome(&completed), Outcome::Executed, "{completed}");
    assert_eq!(merchant.completed.load(Ordering::SeqCst), 1);
    assert_eq!(completed["summary"]["receiptState"], "settled");

    // The ledger shows the spend under the app's key.
    assert_eq!(ledger.spent(&registrar_key(), SpendPeriod::Lifetime).await.unwrap(), 14_990_000);

    // A receipt is in the journal, linked from the audit record.
    let records = payment_records(&fixture);
    assert_eq!(records.len(), 2, "{records:?}");
    for record in &records {
        assert_eq!(record["mandateId"], MANDATE);
        assert_eq!(record["decision"], "executed");
        assert_eq!(record["amount"], "14.99");
        assert_eq!(record["currency"], "USD");
        assert_eq!(record["journalReceiptRef"]["transactionId"], transaction);
        assert!(
            record["journalReceiptRef"]["stateKey"]
                .as_str()
                .unwrap()
                .starts_with("app:payments:tx:")
        );
    }
    assert_eq!(records[1]["tool"], COMPLETE);
    assert_eq!(records[1]["journalReceiptRef"]["receiptId"], format!("receipt-{transaction}"));
    assert_eq!(records[1]["journalReceiptRef"]["orderId"], format!("order-{transaction}"));
    let journal = merchant.records.lock().unwrap().get(&transaction).cloned().unwrap();
    assert_eq!(
        journal.order.as_ref().and_then(|o| o.receipt_id.clone()),
        Some(format!("receipt-{transaction}"))
    );

    // Run 3: nothing is paid again, and the approvals are spent.
    fixture.model.push(done());
    fixture.send("Thanks.").await;
    assert_eq!(merchant.created.load(Ordering::SeqCst), 1);
    assert_eq!(merchant.completed.load(Ordering::SeqCst), 1);
    assert!(fixture.harness.pending_approvals(&user, &session).await.unwrap().is_empty());
    assert_eq!(ledger.spent(&registrar_key(), SpendPeriod::Lifetime).await.unwrap(), 14_990_000);

    // The prompt names the mandate and the remaining budget (spec 3.1 §7.16).
    let text = fixture
        .model
        .last_request()
        .unwrap()
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(&format!("mandate {MANDATE} (domain registration) allows at most 20.00 USD per transaction at merchants domainmart")), "{text}");
    assert!(text.contains("paid so far at these merchants: 14.99 USD"), "{text}");
    // The count is the run's tracker's; `run_budget_text` is unit-tested for exact numbers.
    assert!(text.contains("run budget remaining: ") && text.contains(" tool calls."), "{text}");
    assert!(common::last_system_text(&fixture.model).contains("packs: payments"));
}

#[tokio::test]
async fn the_spend_ledger_caps_a_purchase_over_the_remaining_budget() {
    let merchant = Arc::new(Merchant::default());
    // A ten-dollar cap at the registrar: the $14.99 completion is refused by the ledger.
    let ledger =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(registrar_key(), 10_000_000)));
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    prompter.approve_once();
    let fixture = fixture(vec![call(CREATE, domain_for(1_499)), done()], |b| {
        b.pack_payments(pack(&merchant)).spend_ledger(ledger.clone()).prompter(prompter.clone())
    })
    .await;
    let events = fixture.send("Register it.").await;
    let created = response_of(&events, CREATE);
    assert_eq!(outcome(&created), Outcome::Executed, "{created}");
    let transaction = created["summary"]["transactionId"].as_str().unwrap().to_string();
    fixture.model.push(call(COMPLETE, json!({ "transactionId": transaction })));
    fixture.model.push(done());
    let events = fixture.send("Complete it.").await;
    let completed = response_of(&events, COMPLETE);
    assert!(
        completed["error"].as_str().unwrap_or_default().contains("payments.policy.denied"),
        "{completed}"
    );
    assert_eq!(merchant.completed.load(Ordering::SeqCst), 0);
    assert_eq!(ledger.spent(&registrar_key(), SpendPeriod::Lifetime).await.unwrap(), 0);
    let records = payment_records(&fixture);
    assert_eq!(records.last().unwrap()["decision"], "refused");
}

// --- Property: no standing rule, mode or amount turns a payment into Allow (I38) --------

const CASES: u32 = 256;

fn harness_in(mode: HarnessMode, merchant: &Arc<Merchant>) -> (tempfile::TempDir, HarnessAgent) {
    let dir = tempfile::tempdir().unwrap();
    let agent = HarnessAgent::builder("harness")
        .model(Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(no_user_file())
        .without_compaction()
        .mode(mode)
        .pack_payments(pack(merchant))
        .build()
        .unwrap();
    (dir, agent)
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(CASES))]

    /// I38: whatever rule the subject holds — any glob, either pin, either scope, Allow or
    /// Deny — a payment is denied in ReadOnly and Plan and held for a one-time approval in
    /// Ask and Auto.
    #[test]
    fn no_rule_or_mode_ever_allows_a_payment(
        mode_index in 0usize..4,
        tool_index in 0usize..PAYMENT_TOOL_NAMES.len(),
        glob_index in 0usize..4,
        exact_pin in proptest::bool::ANY,
        session_scope in proptest::bool::ANY,
        allow in proptest::bool::ANY,
        cents in 0i64..1_000_000,
    ) {
        let mode = HarnessMode::ALL[mode_index];
        let tool = PAYMENT_TOOL_NAMES[tool_index];
        let merchant = Arc::new(Merchant::default());
        let (_dir, agent) = harness_in(mode, &merchant);
        let origin = ToolOrigin::new("static", &json!({ "name": tool }), None);
        let glob = ["*", "payments_*", "payments_checkout_*", tool][glob_index].to_string();
        let rule = StandingRule {
            id: "rule".to_string(),
            scope: if session_scope {
                RuleScope::Session {
                    app_name: common::APP.to_string(),
                    user_id: "alice".to_string(),
                    subject_kind: SubjectKind::Human,
                    session_id: "s1".to_string(),
                }
            } else {
                RuleScope::User {
                    app_name: common::APP.to_string(),
                    user_id: "alice".to_string(),
                    subject_kind: SubjectKind::Human,
                }
            },
            tool: glob,
            origin: if exact_pin {
                OriginPin::exact(&origin)
            } else {
                OriginPin::AnyDeclaration { source: "static".to_string() }
            },
            agent: None,
            predicates: Vec::new(),
            decision: if allow {
                StandingDecision::Allow
            } else {
                StandingDecision::Deny { reason: "no".to_string() }
            },
            created_by: SubjectRef::human("alice"),
            created_at: chrono::Utc::now(),
            expires_at: None,
            label: "generated".to_string(),
            source_fingerprint: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let decision = runtime.block_on(async {
            agent.runtime().standing_rules().grant(rule).await.unwrap();
            agent
                .runtime()
                .policy()
                .decide(&request(tool, domain_for(cents)), Some(&origin), Some("c"))
                .await
        });
        match mode {
            HarnessMode::ReadOnly | HarnessMode::Plan => {
                proptest::prop_assert!(matches!(decision, PolicyDecision::Deny { .. }), "{decision:?}");
            }
            _ => {
                proptest::prop_assert!(
                    matches!(&decision, PolicyDecision::RequireApproval { reason } if reason.contains("one-time approval")),
                    "{decision:?}"
                );
            }
        }
    }
}
