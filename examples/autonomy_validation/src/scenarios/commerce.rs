//! An in-process merchant for the spend-ledger scenario: checkouts go through the real
//! `adk-payments` tools, policies, and spend guardrail, against a kernel held in memory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext,
    Result as AdkResult, RunConfig, Tool, ToolContext, async_trait,
};
use adk_payments::domain::{Cart, CartLine, Money, ProtocolExtensions, TransactionRecord};
use adk_payments::kernel::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand,
    ListUnresolvedTransactionsRequest, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::tools::PaymentToolsetBuilder;
use serde_json::{Value, json};

pub const MERCHANT: &str = "merchant-credits";

/// Holds created checkouts and counts completed ones.
#[derive(Default)]
pub struct MemoryMerchant {
    records: Mutex<HashMap<String, TransactionRecord>>,
    completions: AtomicUsize,
}

impl MemoryMerchant {
    pub fn completions(&self) -> usize {
        self.completions.load(Ordering::SeqCst)
    }
}

fn unsupported<T>() -> AdkResult<T> {
    Err(AdkError::tool("not supported by the in-process merchant"))
}

#[async_trait]
impl MerchantCheckoutService for MemoryMerchant {
    async fn create_checkout(
        &self,
        command: CreateCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
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
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(context.transaction_id.as_str().to_string(), record.clone());
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
        Ok(self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(lookup.transaction_id.as_str())
            .cloned())
    }
    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
        self.completions.fetch_add(1, Ordering::SeqCst);
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(command.context.transaction_id.as_str())
            .cloned()
            .ok_or_else(|| AdkError::tool("unknown checkout"))
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
impl TransactionStore for MemoryMerchant {
    async fn upsert(&self, _record: TransactionRecord) -> AdkResult<()> {
        Ok(())
    }
    async fn get(&self, lookup: TransactionLookup) -> AdkResult<Option<TransactionRecord>> {
        self.get_checkout(lookup).await
    }
    async fn list_unresolved(
        &self,
        _request: ListUnresolvedTransactionsRequest,
    ) -> AdkResult<Vec<TransactionRecord>> {
        Ok(Vec::new())
    }
}

/// The tool context of one checkout made by `agent` in `app`, holding the checkout scopes.
struct Buyer {
    app: String,
    agent: String,
    call_id: String,
    run_config: RunConfig,
    actions: Mutex<EventActions>,
    content: Content,
}

#[async_trait]
impl ReadonlyContext for Buyer {
    fn invocation_id(&self) -> &str {
        "inv-checkout"
    }
    fn agent_name(&self) -> &str {
        &self.agent
    }
    fn user_id(&self) -> &str {
        "user-budget"
    }
    fn app_name(&self) -> &str {
        &self.app
    }
    fn session_id(&self) -> &str {
        "checkout"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for Buyer {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for Buyer {
    fn function_call_id(&self) -> &str {
        &self.call_id
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = actions;
    }
    async fn search_memory(&self, _query: &str) -> AdkResult<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn run_config(&self) -> Option<&RunConfig> {
        Some(&self.run_config)
    }
    fn user_scopes(&self) -> Vec<String> {
        vec!["payments:checkout:create".to_string(), "payments:checkout:complete".to_string()]
    }
}

fn cart(cents: i64) -> Value {
    let total = Money::new("USD", cents, 2);
    json!(Cart {
        cart_id: None,
        lines: vec![CartLine {
            line_id: "credits".to_string(),
            merchant_sku: None,
            title: "API credits".to_string(),
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
}

/// Creates and completes a checkout of `cents` through the payment tools, with the run
/// configuration an agent of `app` would carry.
pub async fn buy(
    merchant: &Arc<MemoryMerchant>,
    run_config: RunConfig,
    app: &str,
    agent: &str,
    call_id: &str,
    cents: i64,
) -> AdkResult<Value> {
    let tools = PaymentToolsetBuilder::new(merchant.clone(), merchant.clone()).build().tools();
    let tool = |name: &str| -> AdkResult<Arc<dyn Tool>> {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .cloned()
            .ok_or_else(|| AdkError::tool(format!("payment tool {name} missing")))
    };
    let buyer = Arc::new(Buyer {
        app: app.to_string(),
        agent: agent.to_string(),
        call_id: call_id.to_string(),
        run_config,
        actions: Mutex::new(EventActions::default()),
        content: Content::new("user"),
    });
    let created = tool("payments_checkout_create")?
        .execute(
            buyer.clone(),
            json!({ "merchantId": MERCHANT, "merchantName": "Credits", "cart": cart(cents) }),
        )
        .await?;
    let transaction_id = created
        .pointer("/summary/transactionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AdkError::tool(format!("checkout created no transaction: {created}")))?;
    tool("payments_checkout_complete")?
        .execute(buyer, json!({ "transactionId": transaction_id }))
        .await
}
