//! An in-process merchant backend: the payment tools, policies, and approvals around it are the
//! real `adk-payments` ones, and it counts the checkouts that reach it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{AdkError, Result};
use adk_payments::domain::{Cart, CartLine, Money, ProtocolExtensions, TransactionRecord};
use adk_payments::kernel::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand,
    ListUnresolvedTransactionsRequest, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Default)]
pub struct Merchant {
    records: Mutex<HashMap<String, TransactionRecord>>,
    created: AtomicUsize,
}

impl Merchant {
    /// Checkouts the backend opened.
    pub fn created(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }
}

fn unsupported<T>() -> Result<T> {
    Err(AdkError::tool("not supported by the in-process merchant"))
}

#[async_trait]
impl MerchantCheckoutService for Merchant {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        self.created.fetch_add(1, Ordering::SeqCst);
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
            .unwrap()
            .insert(context.transaction_id.as_str().to_string(), record.clone());
        Ok(record)
    }

    async fn update_checkout(&self, _command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        unsupported()
    }

    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        Ok(self.records.lock().unwrap().get(lookup.transaction_id.as_str()).cloned())
    }

    async fn complete_checkout(
        &self,
        _command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        unsupported()
    }

    async fn cancel_checkout(&self, _command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        unsupported()
    }

    async fn apply_order_update(&self, _command: OrderUpdateCommand) -> Result<TransactionRecord> {
        unsupported()
    }
}

#[async_trait]
impl TransactionStore for Merchant {
    async fn upsert(&self, _record: TransactionRecord) -> Result<()> {
        Ok(())
    }

    async fn get(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.get_checkout(lookup).await
    }

    async fn list_unresolved(
        &self,
        _request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        Ok(Vec::new())
    }
}

/// The arguments of `payments_checkout_create` for one widget costing `cents` US cents.
pub fn checkout_args(cents: i64) -> Value {
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
