//! AP2 payments run under the same governance as a governed ACP checkout.
//!
//! `Ap2Adapter::submit_payment_mandate` called the payment backend directly, so an AP2
//! payment bypassed payment policies, the spend ledger, approvals, and audit. Payment
//! mandates are now governed before they are consumed, and the agent-facing AP2 tool is
//! non-idempotent so a timed-out payment is answered `outcome_unknown` instead of repeated.

#![cfg(feature = "ap2")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_auth::{AuditEvent, AuditOutcome, AuditSink, AuthError, ScopeGuard, StaticScopeResolver};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{
    AdkError, Agent, CallbackContext, Content, ErrorCategory, ErrorComponent, Event, EventActions,
    InMemoryActionLedger, InMemorySpendLedger, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, MemoryEntry, Part, ReadonlyContext, Result, RunConfig, Session, SpendKey,
    SpendLedger, SpendLimits, SpendPeriod, State, Tool, ToolConfirmationDecision, ToolContext,
    ToolEffect,
};
use adk_payments::auth::{PAYMENT_EXECUTE_SCOPE, PaymentAuditor};
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, ProtocolDescriptor,
    ProtocolExtensions, TransactionId, TransactionRecord, TransactionState,
};
use adk_payments::guardrail::{AmountThresholdGuardrail, PaymentPolicySet};
use adk_payments::journal::{ArtifactBackedEvidenceStore, SessionBackedTransactionStore};
use adk_payments::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    EvidenceStore, ExecutePaymentCommand, MerchantCheckoutService, OrderUpdateCommand,
    PAYMENT_APPROVAL_REQUIRED_CODE, PAYMENT_POLICY_DENIED_CODE, PaymentExecutionOutcome,
    PaymentExecutionResult, PaymentExecutionService, SyncPaymentOutcomeCommand, TransactionLookup,
    TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::protocol::ap2::{
    Ap2Adapter, Ap2Error, AuthorizationArtifact, CartMandate, InMemoryPaymentMandateLedger,
    IntentMandate, MerchantAuthorizationVerifier, PaymentMandate, PaymentMandateLedger,
    UserAuthorizationVerifier, VerifiedAuthorization,
};
use adk_payments::tools::{PaymentToolsetBuilder, ap2_payment_tool};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "payments-app";
const USER: &str = "user-1";
const SESSION: &str = "session-1";
const MERCHANT_ID: &str = "merchant-ap2";
const SHOPPER: &str = "shopper-agent";
/// 120 USD, the total of every cart these tests build.
const CART_MICRO_USD: u64 = 120_000_000;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A merchant checkout and payment backend that records every payment it executes.
struct Kernel {
    transactions: Arc<dyn TransactionStore>,
    executed: Mutex<Vec<ExecutePaymentCommand>>,
    /// Fails this many executions with an unavailable error after recording them.
    fail_first: Mutex<usize>,
    hang: Option<Duration>,
}

fn unused<T>() -> Result<T> {
    Err(AdkError::tool("not used by these tests"))
}

impl Kernel {
    fn executions(&self) -> usize {
        self.executed.lock().unwrap().len()
    }

    fn idempotency_keys(&self) -> Vec<String> {
        self.executed
            .lock()
            .unwrap()
            .iter()
            .map(|command| {
                command
                    .extensions
                    .as_slice()
                    .iter()
                    .find_map(|envelope| envelope.fields.get("idempotency_key"))
                    .and_then(Value::as_str)
                    .expect("every payment carries an idempotency key")
                    .to_string()
            })
            .collect()
    }

    async fn load(&self, lookup: TransactionLookup) -> Result<TransactionRecord> {
        self.transactions.get(lookup).await?.ok_or_else(|| AdkError::tool("unknown transaction"))
    }
}

#[async_trait]
impl MerchantCheckoutService for Kernel {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        let context = command.context;
        let mut record = TransactionRecord::new(
            context.transaction_id,
            context.actor,
            context.merchant_of_record,
            context.mode,
            command.cart,
            Utc::now(),
        );
        record.session_identity = context.session_identity;
        self.transactions.upsert(record.clone()).await?;
        Ok(record)
    }
    async fn update_checkout(&self, command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        let mut record = self
            .load(TransactionLookup {
                transaction_id: command.context.transaction_id,
                session_identity: command.context.session_identity,
            })
            .await?;
        if let Some(cart) = command.cart {
            record.cart = cart;
        }
        record.recompute_safe_summary();
        self.transactions.upsert(record.clone()).await?;
        Ok(record)
    }
    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.transactions.get(lookup).await
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
impl PaymentExecutionService for Kernel {
    async fn execute_payment(
        &self,
        command: ExecutePaymentCommand,
    ) -> Result<PaymentExecutionResult> {
        self.executed.lock().unwrap().push(command.clone());
        {
            let mut fail_first = self.fail_first.lock().unwrap();
            if *fail_first > 0 {
                *fail_first -= 1;
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "payments.test.processor_down",
                    "payment processor unavailable",
                ));
            }
        }
        if let Some(hang) = self.hang {
            tokio::time::sleep(hang).await;
        }
        let mut record = self
            .load(TransactionLookup {
                transaction_id: command.context.transaction_id,
                session_identity: command.context.session_identity,
            })
            .await?;
        if record.state.can_transition_to(&TransactionState::Authorized) {
            record
                .transition_to(TransactionState::Authorized, Utc::now())
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
    ) -> Result<TransactionRecord> {
        unused()
    }
}

/// Stores and backend shared by every adapter a test builds.
struct Shop {
    kernel: Arc<Kernel>,
    transactions: Arc<dyn TransactionStore>,
    evidence: Arc<dyn EvidenceStore>,
    mandates: Arc<dyn PaymentMandateLedger>,
}

impl Shop {
    async fn new() -> Self {
        Self::with_kernel(0, None).await
    }

    async fn with_kernel(fail_first: usize, hang: Option<Duration>) -> Self {
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
        let transactions: Arc<dyn TransactionStore> =
            Arc::new(SessionBackedTransactionStore::new(sessions));
        let evidence: Arc<dyn EvidenceStore> =
            Arc::new(ArtifactBackedEvidenceStore::new(Arc::new(InMemoryArtifactService::new())));
        let kernel = Arc::new(Kernel {
            transactions: transactions.clone(),
            executed: Mutex::new(Vec::new()),
            fail_first: Mutex::new(fail_first),
            hang,
        });
        Self {
            kernel,
            transactions,
            evidence,
            mandates: Arc::new(InMemoryPaymentMandateLedger::new()),
        }
    }

    /// An adapter over the shared stores and mandate ledger, with no policies of its own.
    fn adapter(&self) -> Ap2Adapter {
        Ap2Adapter::new(
            self.kernel.clone(),
            self.kernel.clone(),
            self.transactions.clone(),
            self.evidence.clone(),
        )
        .allow_unverified_authorizations()
        .with_payment_mandate_ledger(self.mandates.clone())
    }

    async fn open_cart(&self, tx: &str) {
        self.adapter()
            .submit_cart_mandate(merchant_context(tx, CommerceMode::HumanPresent), cart_mandate(tx))
            .await
            .expect("the cart mandate is accepted");
    }
}

fn identity() -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from(USER).unwrap(),
        SessionId::try_from(SESSION).unwrap(),
    )
}

fn context(
    tx: &str,
    actor_id: &str,
    role: CommerceActorRole,
    mode: CommerceMode,
) -> CommerceContext {
    CommerceContext {
        transaction_id: TransactionId::from(tx),
        session_identity: Some(identity()),
        actor: CommerceActor {
            actor_id: actor_id.to_string(),
            role,
            display_name: None,
            tenant_id: None,
            extensions: ProtocolExtensions::default(),
        },
        merchant_of_record: MerchantRef {
            merchant_id: MERCHANT_ID.to_string(),
            legal_name: "AP2 Merchant LLC".to_string(),
            display_name: Some("AP2 Merchant".to_string()),
            statement_descriptor: None,
            country_code: None,
            website: None,
            extensions: ProtocolExtensions::default(),
        },
        payment_processor: None,
        mode,
        protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
        extensions: ProtocolExtensions::default(),
    }
}

fn shopper_context(tx: &str, mode: CommerceMode) -> CommerceContext {
    context(tx, SHOPPER, CommerceActorRole::AgentSurface, mode)
}

fn merchant_context(tx: &str, mode: CommerceMode) -> CommerceContext {
    context(tx, MERCHANT_ID, CommerceActorRole::Merchant, mode)
}

fn expiry() -> String {
    (Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
}

/// A 120 USD cart whose payment details ID is `tx`.
fn cart_mandate(tx: &str) -> CartMandate {
    serde_json::from_value(json!({
        "contents": {
            "id": tx,
            "user_cart_confirmation_required": false,
            "payment_request": {
                "method_data": [{"supported_methods": "CARD"}],
                "details": {
                    "id": tx,
                    "display_items": [{
                        "label": "Running Shoes",
                        "amount": {"currency": "USD", "value": 120.0}
                    }],
                    "total": {"label": "Total", "amount": {"currency": "USD", "value": 120.0}}
                }
            },
            "cart_expiry": expiry(),
            "merchant_name": "AP2 Merchant"
        },
        "merchant_authorization": "signed-by-merchant",
        "timestamp": Utc::now().to_rfc3339()
    }))
    .unwrap()
}

fn payment_mandate_json(tx: &str, user_authorization: Option<&str>) -> Value {
    json!({
        "payment_mandate_contents": {
            "payment_mandate_id": format!("pm-{tx}"),
            "payment_details_id": tx,
            "payment_details_total": {
                "label": "Total",
                "amount": {"currency": "USD", "value": 120.0}
            },
            "payment_response": {"request_id": tx, "method_name": "CARD"},
            "merchant_agent": "AP2 Merchant",
            "timestamp": Utc::now().to_rfc3339()
        },
        "user_authorization": user_authorization
    })
}

fn payment_mandate(tx: &str) -> PaymentMandate {
    serde_json::from_value(payment_mandate_json(tx, Some("user-signed"))).unwrap()
}

fn daily_ledger(max_micro_usd: u64) -> Arc<InMemorySpendLedger> {
    Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org(APP).per(SpendPeriod::Day), max_micro_usd),
    ))
}

async fn spent(ledger: &InMemorySpendLedger, key: SpendKey) -> u64 {
    ledger.spent(&key, SpendPeriod::Day).await.unwrap()
}

/// Refuses payments above `max_usd` dollars.
fn hard_limit_usd(max_usd: i64) -> PaymentPolicySet {
    PaymentPolicySet::new()
        .with(AmountThresholdGuardrail::new(None, Some(max_usd * 100)).with_currency("USD", 2))
}

/// Escalates payments above `review_usd` dollars.
fn review_above_usd(review_usd: i64) -> PaymentPolicySet {
    PaymentPolicySet::new()
        .with(AmountThresholdGuardrail::new(Some(review_usd * 100), None).with_currency("USD", 2))
}

/// Records every audit event, or fails every write when `down` is set.
#[derive(Default)]
struct Sink {
    events: Mutex<Vec<AuditEvent>>,
    down: bool,
}

#[async_trait]
impl AuditSink for Sink {
    async fn log(&self, event: AuditEvent) -> std::result::Result<(), AuthError> {
        if self.down {
            return Err(AuthError::AuditError("disk full".to_string()));
        }
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

/// A tool context for agent `shopper-agent` in the shop's session, call `call-1`.
struct Caller {
    run_config: RunConfig,
    actions: Mutex<EventActions>,
    content: Content,
    scopes: Vec<String>,
}

impl Caller {
    fn new(run_config: RunConfig) -> Arc<Self> {
        Arc::new(Self {
            run_config,
            actions: Mutex::new(EventActions::default()),
            content: Content::new("user"),
            scopes: vec![PAYMENT_EXECUTE_SCOPE.to_string()],
        })
    }
}

#[async_trait]
impl ReadonlyContext for Caller {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        SHOPPER
    }
    fn user_id(&self) -> &str {
        USER
    }
    fn app_name(&self) -> &str {
        APP
    }
    fn session_id(&self) -> &str {
        SESSION
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for Caller {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for Caller {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn run_config(&self) -> Option<&RunConfig> {
        Some(&self.run_config)
    }
    fn user_scopes(&self) -> Vec<String> {
        self.scopes.clone()
    }
}

fn tool_args(tx: &str) -> Value {
    json!({ "transactionId": tx, "paymentMandate": payment_mandate_json(tx, Some("user-signed")) })
}

// ---------------------------------------------------------------------------
// Spend ledger and payment policies on the protocol path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_spend_cap_blocks_an_ap2_payment() {
    let shop = Shop::new().await;
    let ledger = daily_ledger(200_000_000);
    let adapter = shop.adapter().with_spend_ledger(ledger.clone());
    shop.open_cart("tx-1").await;
    shop.open_cart("tx-2").await;

    adapter
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .expect("120 USD fits under the 200 USD cap");
    assert_eq!(
        spent(&ledger, SpendKey::org(APP).with_agent(SHOPPER).with_vendor(MERCHANT_ID)).await,
        CART_MICRO_USD
    );

    let error = adapter
        .submit_payment_mandate(
            shopper_context("tx-2", CommerceMode::HumanPresent),
            payment_mandate("tx-2"),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert!(error.message.contains("spend limit exceeded"), "{error}");
    assert_eq!(shop.kernel.executions(), 1, "the capped payment must not reach the backend");
    assert_eq!(spent(&ledger, SpendKey::org(APP)).await, CART_MICRO_USD);
}

#[tokio::test]
async fn a_policy_denial_blocks_an_ap2_payment_without_consuming_its_mandate() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;

    let strict = shop.adapter().with_payment_policies(hard_limit_usd(100));
    let error = strict
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert!(error.message.contains("hard limit"), "{error}");
    assert_eq!(shop.kernel.executions(), 0);

    // The refused mandate was not consumed, so it still pays once policy allows it.
    let result = shop
        .adapter()
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .expect("the same mandate pays under a lenient policy");
    assert_eq!(result.outcome, PaymentExecutionOutcome::Completed);
    assert_eq!(shop.kernel.executions(), 1);
}

#[tokio::test]
async fn a_policy_denial_blocks_a_cart_mandate_checkout() {
    let shop = Shop::new().await;
    let strict = shop.adapter().with_payment_policies(hard_limit_usd(100));

    let error = strict
        .submit_cart_mandate(
            merchant_context("tx-1", CommerceMode::HumanPresent),
            cart_mandate("tx-1"),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    let lookup = TransactionLookup {
        transaction_id: TransactionId::from("tx-1"),
        session_identity: Some(identity()),
    };
    assert!(shop.transactions.get(lookup).await.unwrap().is_none(), "no checkout was created");
}

#[tokio::test]
async fn an_escalated_protocol_payment_waits_for_approval() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let reviewed = shop.adapter().with_payment_policies(review_above_usd(50));

    let error = reviewed
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, PAYMENT_APPROVAL_REQUIRED_CODE, "{error}");
    assert_eq!(shop.kernel.executions(), 0);
}

// ---------------------------------------------------------------------------
// Mandate verification fails closed
// ---------------------------------------------------------------------------

struct AcceptingMerchant;

#[async_trait]
impl MerchantAuthorizationVerifier for AcceptingMerchant {
    async fn verify_cart_authorization(
        &self,
        _mandate: &CartMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        Ok(VerifiedAuthorization::new(artifact.artifact_type.clone(), serde_json::Map::new()))
    }
}

/// Rejects every user authorization as forged.
struct RejectingUser;

#[async_trait]
impl UserAuthorizationVerifier for RejectingUser {
    async fn verify_intent_authorization(
        &self,
        _mandate: &IntentMandate,
        _artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        Err(AdkError::tool("intent signature does not verify"))
    }
    async fn verify_payment_authorization(
        &self,
        _mandate: &PaymentMandate,
        _artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        Err(AdkError::tool("payment signature does not verify"))
    }
}

#[tokio::test]
async fn a_forged_user_authorization_is_refused_before_spend_is_reserved() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let ledger = daily_ledger(CART_MICRO_USD);
    let adapter = Ap2Adapter::new(
        shop.kernel.clone(),
        shop.kernel.clone(),
        shop.transactions.clone(),
        shop.evidence.clone(),
    )
    .with_merchant_authorization_verifier(Arc::new(AcceptingMerchant))
    .with_user_authorization_verifier(Arc::new(RejectingUser))
    .with_spend_ledger(ledger.clone());

    let error = adapter
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .unwrap_err();

    assert!(error.message.contains("does not verify"), "{error}");
    assert_eq!(shop.kernel.executions(), 0);
    ledger
        .reserve(&SpendKey::org(APP), CART_MICRO_USD)
        .await
        .expect("nothing was reserved for the refused payment");
}

#[tokio::test]
async fn an_unverified_intent_grants_no_autonomous_authority() {
    let shop = Shop::new().await;
    let adapter = shop.adapter();
    // A human-present intent needs no authorization artifact, so none was verified.
    adapter
        .submit_intent_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            IntentMandate {
                user_cart_confirmation_required: false,
                natural_language_description: "Buy running shoes".to_string(),
                merchants: Some(vec!["AP2 Merchant".to_string()]),
                skus: None,
                requires_refundability: false,
                intent_expiry: expiry(),
            },
            None,
        )
        .await
        .unwrap();
    shop.open_cart("tx-1").await;

    let unsigned: PaymentMandate =
        serde_json::from_value(payment_mandate_json("tx-1", None)).unwrap();
    let error = adapter
        .submit_payment_mandate(shopper_context("tx-1", CommerceMode::HumanNotPresent), unsigned)
        .await
        .unwrap_err();

    assert_eq!(
        error.code,
        AdkError::from(Ap2Error::MissingIntentAuthorization { transaction_id: "tx-1".to_string() })
            .code,
        "{error}"
    );
    assert_eq!(shop.kernel.executions(), 0);
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_audit_write_failure_blocks_an_ap2_payment() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let ledger = daily_ledger(CART_MICRO_USD);
    let adapter = shop.adapter().with_spend_ledger(ledger.clone()).with_payment_auditor(
        PaymentAuditor::new(Arc::new(Sink { down: true, ..Sink::default() })),
    );

    let error = adapter
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "payments.auth.audit_failed", "{error}");
    assert_eq!(shop.kernel.executions(), 0);
    ledger.reserve(&SpendKey::org(APP), CART_MICRO_USD).await.expect("the hold was released");
}

#[tokio::test]
async fn an_ap2_payment_is_audited_before_it_executes() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let sink = Arc::new(Sink::default());
    let adapter = shop.adapter().with_payment_auditor(PaymentAuditor::new(sink.clone()));

    adapter
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .unwrap();

    let events = sink.events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].resource, "payments.payment.execute");
    assert_eq!(events[0].user, USER);
    assert_eq!(events[0].outcome, AuditOutcome::Allowed);
    let metadata = events[0].metadata.as_ref().unwrap();
    assert_eq!(metadata["protocol"], "ap2");
    assert_eq!(metadata["transactionId"], "tx-1");
    assert_eq!(metadata["authenticatedRequest"]["metadata"]["agent"], SHOPPER);
}

#[tokio::test]
async fn an_audit_write_failure_blocks_the_ap2_payment_tool() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let tools = PaymentToolsetBuilder::new(shop.kernel.clone(), shop.transactions.clone())
        .with_ap2_adapter(Arc::new(shop.adapter()))
        .with_scope_guard(ScopeGuard::with_audit(
            StaticScopeResolver::new(vec![PAYMENT_EXECUTE_SCOPE]),
            Sink { down: true, ..Sink::default() },
        ))
        .build()
        .tools();
    let tool = tools.iter().find(|tool| tool.name() == "payments_ap2_payment_submit").unwrap();

    let error =
        tool.execute(Caller::new(RunConfig::default()), tool_args("tx-1")).await.unwrap_err();

    assert_eq!(error.code, "auth.audit_failed", "{error}");
    assert_eq!(shop.kernel.executions(), 0);
}

// ---------------------------------------------------------------------------
// Idempotency keys
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_protocol_payment_retried_on_another_instance_keeps_its_idempotency_key() {
    // The processor fails after receiving the first attempt, so the caller retries the
    // mandate on a second adapter instance that has its own mandate ledger.
    let shop = Shop::with_kernel(1, None).await;
    shop.open_cart("tx-1").await;

    shop.adapter()
        .submit_payment_mandate(
            shopper_context("tx-1", CommerceMode::HumanPresent),
            payment_mandate("tx-1"),
        )
        .await
        .unwrap_err();
    Ap2Adapter::new(
        shop.kernel.clone(),
        shop.kernel.clone(),
        shop.transactions.clone(),
        shop.evidence.clone(),
    )
    .allow_unverified_authorizations()
    .submit_payment_mandate(
        shopper_context("tx-1", CommerceMode::HumanPresent),
        payment_mandate("tx-1"),
    )
    .await
    .expect("the retry succeeds");

    assert_eq!(
        shop.kernel.idempotency_keys(),
        vec!["ap2:payment_mandate:pm-tx-1".to_string(), "ap2:payment_mandate:pm-tx-1".to_string()],
        "the backend can deduplicate the retry"
    );
}

#[tokio::test]
async fn an_approved_tool_retry_pays_once_under_the_calls_idempotency_key() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let tool =
        ap2_payment_tool(Arc::new(shop.adapter().with_payment_policies(review_above_usd(50))));

    // The first attempt is escalated and deferred without reaching the backend.
    let caller = Caller::new(RunConfig::default());
    let error = tool.execute(caller.clone(), tool_args("tx-1")).await.unwrap_err();
    assert_eq!(error.code, PAYMENT_APPROVAL_REQUIRED_CODE, "{error}");
    assert!(caller.actions().tool_confirmation.is_some());
    assert_eq!(shop.kernel.executions(), 0);

    // The approved retry of the same call pays the same mandate once.
    let approved = Caller::new(
        RunConfig::builder()
            .tool_confirmation_decisions(HashMap::from([(
                "call-1".to_string(),
                ToolConfirmationDecision::Approve,
            )]))
            .build(),
    );
    let response = tool.execute(approved.clone(), tool_args("tx-1")).await.unwrap();

    assert_eq!(response["outcome"], "completed");
    assert_eq!(shop.kernel.idempotency_keys(), vec![approved.idempotency_key()]);
    assert_eq!(approved.idempotency_key(), caller.idempotency_key());
}

// ---------------------------------------------------------------------------
// Timeouts: the action ledger answers instead of paying twice
// ---------------------------------------------------------------------------

struct EmptyState;

impl State for EmptyState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct ShopSession;

impl Session for ShopSession {
    fn id(&self) -> &str {
        SESSION
    }
    fn app_name(&self) -> &str {
        APP
    }
    fn user_id(&self) -> &str {
        USER
    }
    fn state(&self) -> &dyn State {
        &EmptyState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

/// Invocation `inv-1` of the shop's session.
struct Invocation {
    user_content: Content,
    run_config: RunConfig,
}

#[async_trait]
impl ReadonlyContext for Invocation {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        SHOPPER
    }
    fn user_id(&self) -> &str {
        USER
    }
    fn app_name(&self) -> &str {
        APP
    }
    fn session_id(&self) -> &str {
        SESSION
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for Invocation {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for Invocation {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not used by these tests")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &ShopSession
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

/// Replays scripted responses, then answers "done".
struct ScriptedModel(Mutex<std::collections::VecDeque<LlmResponse>>);

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

/// A model that asks to pay `tx` with call ID `pay-1`, `times` times.
fn paying_model(tx: &str, times: usize) -> Arc<dyn Llm> {
    let call = LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "payments_ap2_payment_submit".to_string(),
                args: tool_args(tx),
                id: Some("pay-1".to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    };
    Arc::new(ScriptedModel(Mutex::new(std::iter::repeat_n(call, times).collect())))
}

async fn run(agent: &dyn Agent, run_config: RunConfig) -> Vec<Event> {
    let ctx: Arc<dyn InvocationContext> =
        Arc::new(Invocation { user_content: Content::new("user").with_text("pay"), run_config });
    let mut stream = agent.run(ctx).await.expect("agent starts");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("agent event"));
    }
    events
}

fn function_responses(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_ap2_payment_tool_is_non_idempotent() {
    let shop = Shop::new().await;
    assert_eq!(ap2_payment_tool(Arc::new(shop.adapter())).effect(), ToolEffect::NonIdempotent);
}

#[tokio::test]
async fn a_timed_out_ap2_payment_executes_once_and_is_answered_outcome_unknown() {
    // The processor takes the payment, then never answers.
    let shop = Shop::with_kernel(0, Some(Duration::from_secs(3600))).await;
    shop.open_cart("tx-1").await;
    let adapter = Arc::new(shop.adapter());
    let actions = Arc::new(InMemoryActionLedger::new());
    let agent = LlmAgentBuilder::new(SHOPPER)
        // The model asks to pay again with the same call ID after the timeout.
        .model(paying_model("tx-1", 2))
        .tool(Arc::new(ap2_payment_tool(adapter.clone())))
        .tool_timeout(Duration::from_millis(300))
        .default_retry_budget(adk_core::RetryBudget::new(3, Duration::from_millis(10)))
        .build()
        .unwrap();

    let events = run(&agent, RunConfig::builder().action_ledger(actions.clone()).build()).await;

    assert_eq!(shop.kernel.executions(), 1, "the payment must execute exactly once");
    let responses = function_responses(&events);
    assert_eq!(responses.len(), 2);
    for response in &responses {
        assert!(adk_core::is_outcome_unknown(response), "{response}");
    }

    // A resumed invocation replays the same call and is answered from the ledger.
    let resumed = LlmAgentBuilder::new(SHOPPER)
        .model(paying_model("tx-1", 1))
        .tool(Arc::new(ap2_payment_tool(adapter)))
        .build()
        .unwrap();
    let events = run(&resumed, RunConfig::builder().action_ledger(actions).build()).await;

    assert_eq!(shop.kernel.executions(), 1, "the resumed run must not pay again");
    let responses = function_responses(&events);
    assert_eq!(responses.len(), 1);
    assert!(adk_core::is_outcome_unknown(&responses[0]), "{}", responses[0]);
}

// ---------------------------------------------------------------------------
// A2A and MCP exposures
// ---------------------------------------------------------------------------

#[cfg(feature = "ap2-a2a")]
#[tokio::test]
async fn a_payment_mandate_received_over_a2a_is_governed() {
    use adk_payments::protocol::ap2::Ap2A2aMessage;

    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let ledger = daily_ledger(100_000_000);
    let adapter = shop.adapter().with_spend_ledger(ledger.clone());
    let wire = serde_json::to_string(&Ap2A2aMessage::payment_mandate(
        "msg-1",
        "user",
        Some("ctx-1".to_string()),
        None,
        &payment_mandate("tx-1"),
    ))
    .unwrap();

    let message: Ap2A2aMessage = serde_json::from_str(&wire).unwrap();
    let mandate =
        message.extract_payment_mandate().unwrap().expect("the message carries a mandate");
    let error = adapter
        .submit_payment_mandate(shopper_context("tx-1", CommerceMode::HumanPresent), mandate)
        .await
        .unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert_eq!(shop.kernel.executions(), 0);
    assert_eq!(spent(&ledger, SpendKey::org(APP)).await, 0);
}

#[cfg(feature = "ap2-mcp")]
#[tokio::test]
async fn the_mcp_payment_tool_is_governed_by_the_runs_spend_ledger() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    shop.open_cart("tx-2").await;
    let tools = PaymentToolsetBuilder::new(shop.kernel.clone(), shop.transactions.clone())
        .with_ap2_adapter(Arc::new(shop.adapter()))
        .build()
        .tools();
    let tool = tools.iter().find(|tool| tool.name() == "payments_ap2_payment_submit").unwrap();
    let ledger = daily_ledger(200_000_000);
    let caller = Caller::new(RunConfig::builder().spend_ledger(ledger.clone()).build());

    let response = tool.execute(caller.clone(), tool_args("tx-1")).await.unwrap();
    assert_eq!(response["outcome"], "completed");
    assert_eq!(response["continuation"]["transactionId"], "tx-1");
    assert_eq!(
        spent(&ledger, SpendKey::org(APP).with_agent(SHOPPER).with_vendor(MERCHANT_ID)).await,
        CART_MICRO_USD
    );

    let error = tool.execute(caller, tool_args("tx-2")).await.unwrap_err();
    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert_eq!(shop.kernel.executions(), 1);
}

#[tokio::test]
async fn the_ap2_payment_tool_requires_the_execute_scope() {
    let shop = Shop::new().await;
    shop.open_cart("tx-1").await;
    let tools = PaymentToolsetBuilder::new(shop.kernel.clone(), shop.transactions.clone())
        .with_ap2_adapter(Arc::new(shop.adapter()))
        .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(vec![
            "payments:checkout:complete",
        ])))
        .build()
        .tools();
    let tool = tools.iter().find(|tool| tool.name() == "payments_ap2_payment_submit").unwrap();

    let error =
        tool.execute(Caller::new(RunConfig::default()), tool_args("tx-1")).await.unwrap_err();

    assert!(error.message.contains(PAYMENT_EXECUTE_SCOPE), "{error}");
    assert_eq!(shop.kernel.executions(), 0);
}
