//! The payment toolset enforces each tool's declared scopes and reports the caller's identity.
//!
//! `PaymentToolsetBuilder::build` returned the bare tools, so a model could create, complete,
//! or cancel a checkout for a caller holding no payment scope. Every call also reported the
//! hardcoded actor `agent-tool` and no session identity.

use std::sync::{Arc, Mutex};

use adk_core::identity::AdkIdentity;
use adk_core::{
    AdkError, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool,
    ToolContext, ToolEffect,
};
use adk_payments::domain::{
    Cart, CommerceActor, CommerceActorRole, CommerceMode, Money, ProtocolExtensions,
    TransactionRecord,
};
use adk_payments::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    ListUnresolvedTransactionsRequest, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::tools::PaymentToolsetBuilder;
use async_trait::async_trait;
use serde_json::json;

/// A tool context for agent `shopper`, user `alice`, granted `scopes`.
struct Caller {
    scopes: Vec<String>,
    content: Content,
}

impl Caller {
    fn with_scopes(scopes: &[&str]) -> Arc<dyn ToolContext> {
        Arc::new(Self {
            scopes: scopes.iter().map(ToString::to_string).collect(),
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
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn user_scopes(&self) -> Vec<String> {
        self.scopes.clone()
    }
}

/// Records the context of each create, complete, and cancel, and the lookup of each status query.
#[derive(Default)]
struct RecordingKernel {
    creates: Mutex<Vec<CommerceContext>>,
    completes: Mutex<Vec<CommerceContext>>,
    cancels: Mutex<Vec<CommerceContext>>,
    lookups: Mutex<Vec<TransactionLookup>>,
}

fn unused<T>() -> Result<T> {
    Err(AdkError::tool("not used by this test"))
}

fn empty_cart() -> Cart {
    Cart {
        cart_id: None,
        lines: Vec::new(),
        subtotal: None,
        adjustments: Vec::new(),
        total: Money::new("USD", 0, 2),
        affiliate_attribution: None,
        extensions: ProtocolExtensions::default(),
    }
}

fn record_for(context: CommerceContext) -> TransactionRecord {
    TransactionRecord::new(
        context.transaction_id,
        context.actor,
        context.merchant_of_record,
        CommerceMode::HumanPresent,
        empty_cart(),
        chrono::Utc::now(),
    )
}

#[async_trait]
impl MerchantCheckoutService for RecordingKernel {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        self.creates.lock().unwrap().push(command.context.clone());
        Ok(record_for(command.context))
    }
    async fn update_checkout(&self, _command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn get_checkout(&self, _lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        unused()
    }
    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        self.completes.lock().unwrap().push(command.context.clone());
        Ok(record_for(command.context))
    }
    async fn cancel_checkout(&self, command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        self.cancels.lock().unwrap().push(command.context.clone());
        Ok(record_for(command.context))
    }
    async fn apply_order_update(&self, _command: OrderUpdateCommand) -> Result<TransactionRecord> {
        unused()
    }
}

#[async_trait]
impl TransactionStore for RecordingKernel {
    async fn upsert(&self, _record: TransactionRecord) -> Result<()> {
        unused()
    }
    async fn get(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.lookups.lock().unwrap().push(lookup);
        Ok(None)
    }
    async fn list_unresolved(
        &self,
        _request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        unused()
    }
}

fn toolset_tool(kernel: &Arc<RecordingKernel>, name: &str) -> Arc<dyn Tool> {
    PaymentToolsetBuilder::new(kernel.clone(), kernel.clone())
        .build()
        .tools()
        .into_iter()
        .find(|tool| tool.name() == name)
        .expect("the toolset exposes the tool")
}

fn alice_session() -> AdkIdentity {
    AdkIdentity::new(
        "store".try_into().unwrap(),
        "alice".try_into().unwrap(),
        "session-1".try_into().unwrap(),
    )
}

#[tokio::test]
async fn a_caller_without_the_declared_scope_is_refused() {
    let kernel = Arc::new(RecordingKernel::default());
    let cancel = toolset_tool(&kernel, "payments_checkout_cancel");

    let error = cancel
        .execute(
            Caller::with_scopes(&["payments:checkout:create"]),
            json!({"transactionId": "tx-1"}),
        )
        .await
        .expect_err("a caller without payments:checkout:cancel must be refused");

    assert!(error.to_string().contains("payments:checkout:cancel"), "{error}");
    assert!(kernel.cancels.lock().unwrap().is_empty(), "the kernel must not be reached");
}

#[tokio::test]
async fn every_toolset_tool_is_scope_protected() {
    let kernel = Arc::new(RecordingKernel::default());
    let tools = PaymentToolsetBuilder::new(kernel.clone(), kernel.clone()).build().tools();

    for tool in tools {
        assert!(!tool.required_scopes().is_empty(), "{} declares no scope", tool.name());
        let refused = tool.execute(Caller::with_scopes(&[]), json!({})).await;
        assert!(
            refused.is_err_and(|error| error.to_string().contains("missing required scopes")),
            "{} ran for a caller with no scopes",
            tool.name()
        );
    }
}

#[tokio::test]
async fn a_scoped_call_reports_the_calling_agent_and_session() {
    let kernel = Arc::new(RecordingKernel::default());
    let cancel = toolset_tool(&kernel, "payments_checkout_cancel");

    cancel
        .execute(
            Caller::with_scopes(&["payments:checkout:cancel"]),
            json!({"transactionId": "tx-1"}),
        )
        .await
        .expect("a caller holding the scope may cancel");

    let context = kernel.cancels.lock().unwrap().pop().expect("the kernel was called");
    assert_eq!(context.session_identity, Some(alice_session()));
    assert_eq!(
        context.actor,
        CommerceActor {
            actor_id: "shopper".to_string(),
            role: CommerceActorRole::AgentSurface,
            display_name: Some("shopper".to_string()),
            tenant_id: None,
            extensions: ProtocolExtensions::default(),
        }
    );
}

#[tokio::test]
async fn a_status_lookup_is_bound_to_the_callers_session() {
    let kernel = Arc::new(RecordingKernel::default());
    let status = toolset_tool(&kernel, "payments_status_lookup");

    status
        .execute(
            Caller::with_scopes(&["payments:checkout:create"]),
            json!({"transactionId": "tx-1"}),
        )
        .await
        .expect("a caller holding the scope may look up a transaction");

    let lookup = kernel.lookups.lock().unwrap().pop().expect("the store was queried");
    assert_eq!(lookup.session_identity, Some(alice_session()));
}

fn idempotency_key_of(context: &CommerceContext) -> Option<String> {
    context.extensions.as_slice().iter().find_map(|envelope| {
        envelope.fields.get("idempotency_key").and_then(|key| key.as_str()).map(str::to_string)
    })
}

#[tokio::test]
async fn checkout_create_and_complete_are_non_idempotent_and_carry_the_idempotency_key() {
    let kernel = Arc::new(RecordingKernel::default());
    let create = toolset_tool(&kernel, "payments_checkout_create");
    let complete = toolset_tool(&kernel, "payments_checkout_complete");
    // The scope guard reports the wrapped tool's declared effect.
    assert_eq!(create.effect(), ToolEffect::NonIdempotent);
    assert_eq!(complete.effect(), ToolEffect::NonIdempotent);

    let create_args = json!({
        "merchantId": "m-1",
        "merchantName": "Merchant",
        "cart": serde_json::to_value(empty_cart()).unwrap(),
    });
    for _ in 0..2 {
        create
            .execute(Caller::with_scopes(&["payments:checkout:create"]), create_args.clone())
            .await
            .expect("a caller holding the scope may create a checkout");
    }
    complete
        .execute(
            Caller::with_scopes(&["payments:checkout:complete"]),
            json!({"transactionId": "tx-1"}),
        )
        .await
        .expect("a caller holding the scope may complete a checkout");

    let expected_key = Some("store/alice/session-1/inv-1/call-1".to_string());
    let creates = kernel.creates.lock().unwrap().clone();
    assert_eq!(creates.len(), 2);
    assert_eq!(idempotency_key_of(&creates[0]), expected_key);
    // A replay of the same call names the same transaction rather than a new one.
    assert_eq!(creates[0].transaction_id, creates[1].transaction_id);
    let completed = kernel.completes.lock().unwrap().pop().expect("the kernel was called");
    assert_eq!(idempotency_key_of(&completed), expected_key);
}
