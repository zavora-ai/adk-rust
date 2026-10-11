//! `ap2_payment_governed`: an AP2 payment mandate submitted by a live model runs under the
//! same governance as a governed checkout. The first mandate pays and is recorded in the
//! spend ledger and the audit trail; the second would exceed the daily cap and is refused
//! before it reaches the payment backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_auth::{AuditEvent, AuditOutcome, AuditSink, AuthError, ScopeGuard, StaticScopeResolver};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{
    AdkError, InMemorySpendLedger, Llm, Result as AdkResult, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod, async_trait,
};
use adk_payments::auth::{PAYMENT_EXECUTE_SCOPE, PaymentAuditor};
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, ProtocolDescriptor,
    ProtocolExtensions, TransactionId, TransactionRecord, TransactionState,
};
use adk_payments::journal::{ArtifactBackedEvidenceStore, SessionBackedTransactionStore};
use adk_payments::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    EvidenceStore, ExecutePaymentCommand, MerchantCheckoutService, OrderUpdateCommand,
    PaymentExecutionOutcome, PaymentExecutionResult, PaymentExecutionService,
    SyncPaymentOutcomeCommand, TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::protocol::ap2::{Ap2Adapter, CartMandate};
use adk_payments::tools::PaymentToolsetBuilder;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::{Value, json};

use crate::common::{Config, Provider, Verdict, brief, fail, pair_calls, run_turn, session_events};

const APP: &str = "autonomy-ap2";
const USER: &str = "user-ap2";
const SESSION: &str = "ap2-payments";
const AGENT: &str = "shopper";
const MERCHANT: &str = "merchant-ap2";
const TOOL: &str = "payments_ap2_payment_submit";
/// Each cart costs 12 USD; the daily cap at this merchant holds one.
const CART_MICRO_USD: u64 = 12_000_000;
const CAP_MICRO_USD: u64 = 20_000_000;

/// A merchant and payment processor over the `adk-payments` journal stores.
struct Merchant {
    transactions: Arc<dyn TransactionStore>,
    payments: Mutex<Vec<String>>,
}

impl Merchant {
    fn payments(&self) -> Vec<String> {
        self.payments.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    async fn load(&self, transaction_id: TransactionId) -> AdkResult<TransactionRecord> {
        self.transactions
            .get(TransactionLookup { transaction_id, session_identity: Some(identity()) })
            .await?
            .ok_or_else(|| AdkError::tool("unknown transaction"))
    }
}

fn unsupported<T>() -> AdkResult<T> {
    Err(AdkError::tool("not supported by the in-process merchant"))
}

#[async_trait]
impl MerchantCheckoutService for Merchant {
    async fn create_checkout(
        &self,
        command: CreateCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
        let context = command.context;
        let mut record = TransactionRecord::new(
            context.transaction_id,
            context.actor,
            context.merchant_of_record,
            context.mode,
            command.cart,
            chrono::Utc::now(),
        );
        record.session_identity = context.session_identity;
        self.transactions.upsert(record.clone()).await?;
        Ok(record)
    }
    async fn update_checkout(
        &self,
        _command: UpdateCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
        unsupported()
    }
    async fn get_checkout(
        &self,
        lookup: TransactionLookup,
    ) -> AdkResult<Option<TransactionRecord>> {
        self.transactions.get(lookup).await
    }
    async fn complete_checkout(
        &self,
        _command: CompleteCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
        unsupported()
    }
    async fn cancel_checkout(
        &self,
        _command: CancelCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
        unsupported()
    }
    async fn apply_order_update(
        &self,
        _command: OrderUpdateCommand,
    ) -> AdkResult<TransactionRecord> {
        unsupported()
    }
}

#[async_trait]
impl PaymentExecutionService for Merchant {
    async fn execute_payment(
        &self,
        command: ExecutePaymentCommand,
    ) -> AdkResult<PaymentExecutionResult> {
        self.payments
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(command.context.transaction_id.as_str().to_string());
        let mut record = self.load(command.context.transaction_id).await?;
        if record.state.can_transition_to(&TransactionState::Authorized) {
            record
                .transition_to(TransactionState::Authorized, chrono::Utc::now())
                .map_err(AdkError::from)?;
        }
        record.recompute_safe_summary();
        self.transactions.upsert(record.clone()).await?;
        Ok(PaymentExecutionResult {
            outcome: PaymentExecutionOutcome::Completed,
            transaction: record,
            order: None,
            intervention: None,
            generated_evidence_refs: Vec::new(),
        })
    }
    async fn sync_payment_outcome(
        &self,
        _command: SyncPaymentOutcomeCommand,
    ) -> AdkResult<TransactionRecord> {
        unsupported()
    }
}

/// Keeps every payment audit event.
#[derive(Default)]
struct AuditLog(Mutex<Vec<AuditEvent>>);

#[async_trait]
impl AuditSink for AuditLog {
    async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(event);
        Ok(())
    }
}

fn identity() -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(APP).expect("valid app name"),
        UserId::try_from(USER).expect("valid user id"),
        SessionId::try_from(SESSION).expect("valid session id"),
    )
}

fn merchant_context(tx: &str) -> CommerceContext {
    CommerceContext {
        transaction_id: TransactionId::from(tx),
        session_identity: Some(identity()),
        actor: CommerceActor {
            actor_id: MERCHANT.to_string(),
            role: CommerceActorRole::Merchant,
            display_name: None,
            tenant_id: None,
            extensions: ProtocolExtensions::default(),
        },
        merchant_of_record: MerchantRef {
            merchant_id: MERCHANT.to_string(),
            legal_name: "AP2 Merchant LLC".to_string(),
            display_name: Some("AP2 Merchant".to_string()),
            statement_descriptor: None,
            country_code: None,
            website: None,
            extensions: ProtocolExtensions::default(),
        },
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
        protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
        extensions: ProtocolExtensions::default(),
    }
}

fn expiry() -> String {
    (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
}

fn cart_mandate(tx: &str) -> anyhow::Result<CartMandate> {
    Ok(serde_json::from_value(json!({
        "contents": {
            "id": tx,
            "user_cart_confirmation_required": false,
            "payment_request": {
                "method_data": [{"supported_methods": "CARD"}],
                "details": {
                    "id": tx,
                    "display_items": [{"label": "Trail socks", "amount": {"currency": "USD", "value": 12.0}}],
                    "total": {"label": "Total", "amount": {"currency": "USD", "value": 12.0}}
                }
            },
            "cart_expiry": expiry(),
            "merchant_name": "AP2 Merchant"
        },
        "merchant_authorization": "merchant-signature",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))?)
}

/// The payment mandate a shopper would sign for cart `tx`.
fn payment_mandate(tx: &str) -> Value {
    json!({
        "payment_mandate_contents": {
            "payment_mandate_id": format!("pm-{tx}"),
            "payment_details_id": tx,
            "payment_details_total": {"label": "Total", "amount": {"currency": "USD", "value": 12.0}},
            "payment_response": {"request_id": tx, "method_name": "CARD"},
            "merchant_agent": "AP2 Merchant",
            "timestamp": "2026-10-11T09:00:00Z"
        },
        "user_authorization": "user-signature"
    })
}

pub async fn ap2_payment_governed(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let model = cfg.model(provider)?;

    // The commerce journal lives in its own session store, keyed by the same identity.
    let journal = Arc::new(InMemorySessionService::new());
    journal
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let transactions: Arc<dyn TransactionStore> =
        Arc::new(SessionBackedTransactionStore::new(journal));
    let evidence: Arc<dyn EvidenceStore> =
        Arc::new(ArtifactBackedEvidenceStore::new(Arc::new(InMemoryArtifactService::new())));
    let merchant =
        Arc::new(Merchant { transactions: transactions.clone(), payments: Mutex::new(Vec::new()) });
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(
        SpendKey::org(APP).with_vendor(MERCHANT).per(SpendPeriod::Day),
        CAP_MICRO_USD,
    )));
    let audit = Arc::new(AuditLog::default());
    // Signature checks are out of scope here; the verifier tests cover them.
    let adapter = Arc::new(
        Ap2Adapter::new(merchant.clone(), merchant.clone(), transactions.clone(), evidence)
            .allow_unverified_authorizations()
            .with_spend_ledger(ledger.clone())
            .with_payment_auditor(PaymentAuditor::new(audit.clone())),
    );
    for tx in ["ap2-a", "ap2-b"] {
        adapter.submit_cart_mandate(merchant_context(tx), cart_mandate(tx)?).await?;
    }

    let tools = PaymentToolsetBuilder::new(merchant.clone(), transactions)
        .with_ap2_adapter(adapter)
        .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(vec![PAYMENT_EXECUTE_SCOPE])))
        .build()
        .tools();
    let mut builder = LlmAgentBuilder::new(AGENT)
        .model(model as Arc<dyn Llm>)
        .instruction(format!(
            "You submit AP2 payment mandates. When asked to pay a transaction, call {TOOL} \
             exactly once with the given transactionId and the given paymentMandate object, \
             copied verbatim. Never call {TOOL} a second time for the same transaction; if it \
             returns an error, report the error and stop."
        ));
    for tool in tools.into_iter().filter(|tool| tool.name() == TOOL) {
        builder = builder.tool(tool);
    }
    let agent = builder.build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()?;

    let mut answers = Vec::new();
    for tx in ["ap2-a", "ap2-b"] {
        let prompt = format!(
            "Pay transaction {tx}. Call {TOOL} with transactionId \"{tx}\" and paymentMandate {}",
            payment_mandate(tx)
        );
        let turn = run_turn(&runner, USER, SESSION, &prompt).await;
        if let Some(error) = &turn.error {
            return Ok(Verdict::Fail(format!("turn for {tx} failed: {}", brief(error))));
        }
        let events = session_events(sessions.as_ref(), APP, USER, SESSION).await?;
        let (pairings, problems) = pair_calls(&events);
        if !problems.is_empty() {
            return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
        }
        let calls: Vec<_> = pairings.into_iter().filter(|pairing| pairing.name == TOOL).collect();
        match calls.len().checked_sub(answers.len()) {
            Some(1) => {}
            Some(0) => return Ok(Verdict::Retry(format!("model never called {TOOL} for {tx}"))),
            _ => {
                return Ok(Verdict::Retry(format!(
                    "model called {TOOL} {}x for {tx}",
                    calls.len() - answers.len()
                )));
            }
        }
        answers.push(calls.last().and_then(|call| call.responses.first().cloned()));
    }

    let first = answers[0].clone().unwrap_or_default();
    if first["outcome"] != "completed" {
        return Ok(Verdict::Fail(format!("the payment within the cap was not completed: {first}")));
    }
    let second = answers[1].clone().unwrap_or_default().to_string();
    if !second.contains("payments.policy.denied") && !second.contains("spend limit exceeded") {
        return Ok(Verdict::Fail(format!("the payment over the cap was not refused: {second}")));
    }
    let payments = merchant.payments();
    if payments != ["ap2-a"] {
        return Ok(Verdict::Fail(format!("the payment backend executed {payments:?}")));
    }
    let spent = ledger
        .spent(&SpendKey::org(APP).with_agent(AGENT).with_vendor(MERCHANT), SpendPeriod::Day)
        .await?;
    if spent != CART_MICRO_USD {
        return Ok(Verdict::Fail(format!(
            "the ledger holds {spent} micro-USD for {APP}/{AGENT}/{MERCHANT}, expected {CART_MICRO_USD}"
        )));
    }
    let events = audit.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let outcomes: Vec<_> = events
        .iter()
        .filter(|event| event.resource == "payments.payment.execute")
        .map(|event| event.outcome.clone())
        .collect();
    if outcomes != [AuditOutcome::Allowed, AuditOutcome::Denied] {
        return Ok(Verdict::Fail(format!("payment audit trail was {outcomes:?}")));
    }
    Ok(Verdict::Pass(format!(
        "{TOOL}: ap2-a completed and committed {CART_MICRO_USD} micro-USD under {APP}/{AGENT}/{MERCHANT}; ap2-b refused by spend_limit before the backend (cap {CAP_MICRO_USD}); audit allowed then denied"
    )))
}
