//! Payment policies run on every checkout creation and completion.
//!
//! `PaymentPolicySet`, `AmountThresholdGuardrail`, and `MerchantAllowlistGuardrail` were
//! defined but never evaluated by the kernel or the payment tools, so a checkout of any
//! amount at any merchant went through. Completed checkouts were not recorded against a
//! spend budget either.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, CallbackContext, Content, EventActions, InMemorySpendLedger, MemoryEntry,
    ReadonlyContext, ReservationId, Result, RunConfig, SpendError, SpendKey, SpendLedger,
    SpendLimits, SpendPeriod, Tool, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest, ToolContext,
};
use adk_payments::domain::{
    Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
    ProtocolDescriptor, ProtocolExtensions, TransactionId, TransactionRecord,
};
use adk_payments::guardrail::{
    AmountThresholdGuardrail, MerchantAllowlistGuardrail, PaymentPolicySet,
};
use adk_payments::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    GovernedCheckoutService, ListUnresolvedTransactionsRequest, MerchantCheckoutService,
    OrderUpdateCommand, PAYMENT_APPROVAL_DENIED_CODE, PAYMENT_APPROVAL_REQUIRED_CODE,
    PAYMENT_POLICY_DENIED_CODE, TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::tools::PaymentToolsetBuilder;
use async_trait::async_trait;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A tool context for agent `shopper` in app `store`, holding every checkout scope.
struct Caller {
    run_config: RunConfig,
    actions: Mutex<EventActions>,
    content: Content,
}

impl Caller {
    fn new(run_config: RunConfig) -> Arc<Self> {
        Arc::new(Self {
            run_config,
            actions: Mutex::new(EventActions::default()),
            content: Content::new("user"),
        })
    }
}

#[async_trait]
impl ReadonlyContext for Caller {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "shopper"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "store"
    }
    fn session_id(&self) -> &str {
        "session-1"
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
        vec!["payments:checkout:create".to_string(), "payments:checkout:complete".to_string()]
    }
}

/// Stores created checkouts and counts completions; fails completions when `fail` is set.
#[derive(Default)]
struct MemoryKernel {
    records: Mutex<HashMap<String, TransactionRecord>>,
    last_created: Mutex<Option<String>>,
    completions: AtomicUsize,
    fail_completion: bool,
}

fn unused<T>() -> Result<T> {
    Err(AdkError::tool("not used by this test"))
}

#[async_trait]
impl MerchantCheckoutService for MemoryKernel {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        let context = command.context;
        let mut record = TransactionRecord::new(
            context.transaction_id.clone(),
            context.actor,
            context.merchant_of_record,
            context.mode,
            command.cart,
            chrono::Utc::now(),
        );
        record.session_identity = context.session_identity;
        let id = context.transaction_id.as_str().to_string();
        self.records.lock().unwrap().insert(id.clone(), record.clone());
        *self.last_created.lock().unwrap() = Some(id);
        Ok(record)
    }
    async fn update_checkout(&self, _command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        Ok(self.records.lock().unwrap().get(lookup.transaction_id.as_str()).cloned())
    }
    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        self.completions.fetch_add(1, Ordering::SeqCst);
        if self.fail_completion {
            return Err(AdkError::tool("processor declined"));
        }
        self.records
            .lock()
            .unwrap()
            .get(command.context.transaction_id.as_str())
            .cloned()
            .ok_or_else(|| AdkError::tool("unknown checkout"))
    }
    async fn cancel_checkout(&self, _command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn apply_order_update(&self, _command: OrderUpdateCommand) -> Result<TransactionRecord> {
        unused()
    }
}

#[async_trait]
impl TransactionStore for MemoryKernel {
    async fn upsert(&self, _record: TransactionRecord) -> Result<()> {
        unused()
    }
    async fn get(&self, _lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        unused()
    }
    async fn list_unresolved(
        &self,
        _request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        unused()
    }
}

/// A ledger whose backing store is down.
#[derive(Debug)]
struct UnreachableLedger;

#[async_trait]
impl SpendLedger for UnreachableLedger {
    async fn reserve(&self, _key: &SpendKey, _amount: u64) -> Result<ReservationId> {
        Err(SpendError::Unavailable("connection refused".to_string()).into())
    }
    async fn commit(&self, id: ReservationId, _actual: u64) -> Result<()> {
        Err(SpendError::UnknownReservation(id).into())
    }
    async fn release(&self, id: ReservationId) -> Result<()> {
        Err(SpendError::UnknownReservation(id).into())
    }
    async fn spent(&self, _key: &SpendKey, _period: SpendPeriod) -> Result<u64> {
        Err(SpendError::Unavailable("connection refused".to_string()).into())
    }
}

/// Answers every confirmation with `decision` and records the requests.
#[derive(Debug)]
struct FixedHandler {
    decision: ToolConfirmationDecision,
    requests: Mutex<Vec<ToolConfirmationRequest>>,
}

#[async_trait]
impl ToolConfirmationHandler for FixedHandler {
    async fn decide(&self, request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.decision)
    }
}

fn usd(cents: i64) -> Money {
    Money::new("USD", cents, 2)
}

fn cart(total: Money) -> Value {
    serde_json::to_value(Cart {
        cart_id: None,
        lines: vec![CartLine {
            line_id: "line-1".to_string(),
            merchant_sku: None,
            title: "Widget".to_string(),
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
    })
    .unwrap()
}

struct Shop {
    kernel: Arc<MemoryKernel>,
    create: Arc<dyn Tool>,
    complete: Arc<dyn Tool>,
}

impl Shop {
    fn new(kernel: MemoryKernel, policies: PaymentPolicySet) -> Self {
        let kernel = Arc::new(kernel);
        let tools = PaymentToolsetBuilder::new(kernel.clone(), kernel.clone())
            .with_payment_policies(policies)
            .build()
            .tools();
        let find = |name: &str| tools.iter().find(|tool| tool.name() == name).unwrap().clone();
        Self {
            create: find("payments_checkout_create"),
            complete: find("payments_checkout_complete"),
            kernel,
        }
    }

    async fn create(&self, caller: &Arc<Caller>, merchant: &str, total: Money) -> Result<Value> {
        self.create
            .execute(
                caller.clone(),
                json!({ "merchantId": merchant, "merchantName": "Merchant", "cart": cart(total) }),
            )
            .await
    }

    async fn complete_last(&self, caller: &Arc<Caller>) -> Result<Value> {
        let id = self.kernel.last_created.lock().unwrap().clone().expect("a checkout exists");
        self.complete.execute(caller.clone(), json!({ "transactionId": id })).await
    }

    /// Creates a checkout at `merchant-1` and completes it.
    async fn buy(&self, caller: &Arc<Caller>, total: Money) -> Result<Value> {
        self.create(caller, "merchant-1", total).await?;
        self.complete_last(caller).await
    }
}

fn daily_ledger(max_micro_usd: u64) -> Arc<InMemorySpendLedger> {
    Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("store").per(SpendPeriod::Day), max_micro_usd),
    ))
}

async fn spent(ledger: &InMemorySpendLedger, key: SpendKey) -> u64 {
    ledger.spent(&key, SpendPeriod::Day).await.unwrap()
}

// ---------------------------------------------------------------------------
// Policy outcomes through the payment tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_payment_over_the_hard_limit_is_denied() {
    let shop = Shop::new(
        MemoryKernel::default(),
        PaymentPolicySet::new()
            .with(AmountThresholdGuardrail::new(None, Some(10_000)).with_currency("USD", 2)),
    );
    let caller = Caller::new(RunConfig::default());

    let error = shop.create(&caller, "merchant-1", usd(15_000)).await.unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert!(error.message.contains("hard limit"), "{error}");
    assert!(shop.kernel.records.lock().unwrap().is_empty(), "the backend must not be reached");
}

#[tokio::test]
async fn a_merchant_outside_the_allowlist_is_denied() {
    let shop = Shop::new(
        MemoryKernel::default(),
        PaymentPolicySet::new().with(MerchantAllowlistGuardrail::new(["merchant-allowed"])),
    );
    let caller = Caller::new(RunConfig::default());

    let error = shop.create(&caller, "merchant-1", usd(500)).await.unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert!(error.message.contains("merchant_allowlist"), "{error}");
    shop.create(&caller, "merchant-allowed", usd(500)).await.expect("listed merchant passes");
}

#[tokio::test]
async fn completion_is_judged_on_the_stored_checkout() {
    // The checkout passes at creation; the policy changes before it is completed.
    let kernel = Arc::new(MemoryKernel::default());
    let lenient = GovernedCheckoutService::new(kernel.clone(), PaymentPolicySet::new());
    let strict = GovernedCheckoutService::new(
        kernel.clone(),
        PaymentPolicySet::new().with(AmountThresholdGuardrail::new(None, Some(1_000))),
    );
    let context = CommerceContext {
        transaction_id: TransactionId::from("tx-1"),
        session_identity: None,
        actor: CommerceActor {
            actor_id: "shopper".to_string(),
            role: CommerceActorRole::AgentSurface,
            display_name: None,
            tenant_id: None,
            extensions: ProtocolExtensions::default(),
        },
        merchant_of_record: MerchantRef {
            merchant_id: "merchant-1".to_string(),
            legal_name: "Merchant".to_string(),
            display_name: None,
            statement_descriptor: None,
            country_code: None,
            website: None,
            extensions: ProtocolExtensions::default(),
        },
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
        protocol: ProtocolDescriptor::acp("2026-01-30"),
        extensions: ProtocolExtensions::default(),
    };
    lenient
        .create_checkout(CreateCheckoutCommand {
            context: context.clone(),
            cart: serde_json::from_value(cart(usd(5_000))).unwrap(),
            fulfillment: None,
        })
        .await
        .unwrap();

    let error = strict
        .complete_checkout(CompleteCheckoutCommand {
            context,
            selected_payment_method: None,
            extensions: ProtocolExtensions::default(),
        })
        .await
        .unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert_eq!(kernel.completions.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// Escalations reach the tool confirmation flow
// ---------------------------------------------------------------------------

fn review_above_fifty_dollars() -> PaymentPolicySet {
    PaymentPolicySet::new()
        .with(AmountThresholdGuardrail::new(Some(5_000), None).with_currency("USD", 2))
}

#[tokio::test]
async fn an_escalation_without_a_decision_requests_confirmation() {
    let shop = Shop::new(MemoryKernel::default(), review_above_fifty_dollars());
    let caller = Caller::new(RunConfig::default());

    let error = shop.create(&caller, "merchant-1", usd(7_500)).await.unwrap_err();

    assert_eq!(error.code, PAYMENT_APPROVAL_REQUIRED_CODE, "{error}");
    let request = caller.actions().tool_confirmation.expect("the event asks for confirmation");
    assert_eq!(request.tool_name, "payments_checkout_create");
    assert_eq!(request.function_call_id.as_deref(), Some("call-1"));
    assert!(shop.kernel.records.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_confirmation_handler_decides_an_escalation() {
    let approving = Arc::new(FixedHandler {
        decision: ToolConfirmationDecision::Approve,
        requests: Mutex::new(Vec::new()),
    });
    let shop = Shop::new(MemoryKernel::default(), review_above_fifty_dollars());
    let caller =
        Caller::new(RunConfig::builder().tool_confirmation_handler(approving.clone()).build());
    shop.create(&caller, "merchant-1", usd(7_500)).await.expect("the handler approves");
    assert_eq!(approving.requests.lock().unwrap().len(), 1);

    let denying = Arc::new(FixedHandler {
        decision: ToolConfirmationDecision::Deny,
        requests: Mutex::new(Vec::new()),
    });
    let caller = Caller::new(RunConfig::builder().tool_confirmation_handler(denying).build());
    let error = shop.create(&caller, "merchant-1", usd(7_500)).await.unwrap_err();
    assert_eq!(error.code, PAYMENT_APPROVAL_DENIED_CODE, "{error}");
}

#[tokio::test]
async fn a_static_decision_for_the_call_approves_an_escalation() {
    let shop = Shop::new(MemoryKernel::default(), review_above_fifty_dollars());
    let caller = Caller::new(
        RunConfig::builder()
            .tool_confirmation_decisions(HashMap::from([(
                "call-1".to_string(),
                ToolConfirmationDecision::Approve,
            )]))
            .build(),
    );
    shop.create(&caller, "merchant-1", usd(7_500)).await.expect("the call was approved");
}

// ---------------------------------------------------------------------------
// Spend ledger
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_daily_spend_cap_spans_two_checkouts() {
    let ledger = daily_ledger(100_000_000);
    let shop = Shop::new(MemoryKernel::default(), PaymentPolicySet::new());
    let caller = Caller::new(RunConfig::builder().spend_ledger(ledger.clone()).build());

    shop.buy(&caller, usd(6_000)).await.expect("60 USD fits under the 100 USD cap");
    assert_eq!(
        spent(&ledger, SpendKey::org("store").with_agent("shopper").with_vendor("merchant-1"))
            .await,
        60_000_000
    );

    let error = shop.buy(&caller, usd(5_000)).await.unwrap_err();
    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert!(error.message.contains("spend limit exceeded"), "{error}");
    assert_eq!(shop.kernel.completions.load(Ordering::SeqCst), 1);
    assert_eq!(spent(&ledger, SpendKey::org("store")).await, 60_000_000);
}

#[tokio::test]
async fn a_failed_completion_releases_its_hold() {
    let ledger = daily_ledger(100_000_000);
    let shop = Shop::new(
        MemoryKernel { fail_completion: true, ..MemoryKernel::default() },
        PaymentPolicySet::new(),
    );
    let caller = Caller::new(RunConfig::builder().spend_ledger(ledger.clone()).build());

    let error = shop.buy(&caller, usd(9_000)).await.unwrap_err();

    assert!(error.message.contains("processor declined"), "{error}");
    assert_eq!(spent(&ledger, SpendKey::org("store")).await, 0);
    ledger
        .reserve(&SpendKey::org("store"), 100_000_000)
        .await
        .expect("the released hold leaves the whole cap");
}

#[tokio::test]
async fn an_unreachable_ledger_fails_closed() {
    let shop = Shop::new(MemoryKernel::default(), PaymentPolicySet::new());
    let caller =
        Caller::new(RunConfig::builder().spend_ledger(Arc::new(UnreachableLedger)).build());

    let error = shop.buy(&caller, usd(100)).await.unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert!(error.message.contains("unreachable"), "{error}");
    assert_eq!(shop.kernel.completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_explicit_spend_guardrail_without_a_ledger_fails_closed() {
    let shop = Shop::new(
        MemoryKernel::default(),
        PaymentPolicySet::new().with(adk_payments::guardrail::SpendLimitGuardrail::new()),
    );
    let caller = Caller::new(RunConfig::default());

    let error = shop.buy(&caller, usd(100)).await.unwrap_err();

    assert_eq!(error.code, PAYMENT_POLICY_DENIED_CODE, "{error}");
    assert_eq!(shop.kernel.completions.load(Ordering::SeqCst), 0);
}
