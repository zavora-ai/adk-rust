//! The company: an accounts-payable clerk agent, its tools, its policy, and the bank it
//! pays through.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_auth::{ScopeGuard, StaticScopeResolver};
use adk_core::{
    AdkError, AgentId, AgentPrincipal, ArgPredicate, DeclarativePolicy, Llm, PolicyRule,
    Result as AdkResult, Tool, ToolContext, ToolEffect,
};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_payments::domain::{Cart, CartLine, Money, ProtocolExtensions, TransactionRecord};
use adk_payments::kernel::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand,
    ListUnresolvedTransactionsRequest, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::tools::PaymentToolsetBuilder;
use async_trait::async_trait;
use serde_json::{Value, json};

/// The application every run, session, spend record, and audit record belongs to.
pub const APP: &str = "durable-company";
/// The queue the accounts-payable runs go through.
pub const QUEUE: &str = "accounts-payable";
/// The agent the worker runs.
pub const TARGET: &str = "ap_clerk";
/// The controller the scheduled runs act for; they request approvals.
pub const CONTROLLER: &str = "controller@acme.example";
/// The agent's identity, stable across processes and redeploys.
pub const AGENT_ID: &str = "spiffe://acme.example/agent/ap-clerk";
/// The tenant the agent belongs to.
pub const TENANT: &str = "acme";
/// Payments above this amount need approval.
pub const APPROVAL_THRESHOLD_USD: f64 = 1_000.0;
/// Approvals a held payment needs.
pub const QUORUM: u32 = 2;
/// The daily cap on model spend and payments together.
pub const DAILY_CAP_USD: u64 = 2_000;

/// An invoice due today.
pub struct Invoice {
    pub id: &'static str,
    pub vendor: &'static str,
    pub amount_usd: i64,
}

/// What `list_due_invoices` returns until each is paid.
pub const DUE_TODAY: [Invoice; 2] = [
    Invoice { id: "INV-1041", vendor: "Northwind Paper", amount_usd: 300 },
    Invoice { id: "INV-1042", vendor: "Contoso Logistics", amount_usd: 1_200 },
];
/// The rush invoice a second run is asked to pay; it fits the policy but not the cap.
pub const RUSH: Invoice = Invoice { id: "INV-1043", vendor: "Fabrikam Freight", amount_usd: 800 };

pub const CLERK_INSTRUCTION: &str = "You are the accounts-payable clerk of Acme Supplies. \
When asked to pay the invoices due today, call list_due_invoices, then call pay_invoice once \
for each invoice it returns, with the invoice id, vendor, and amount exactly as listed. When \
asked to pay one named invoice, call pay_invoice once for it. Never pay an invoice twice. If \
list_due_invoices answers with an unknown outcome, call it again; it only reads. If a payment \
is held for approval, refused, or fails, do not retry it. Finish with one line per invoice: \
its id, its amount, and what happened.";

/// The tool policy: lookups are free, payments up to the threshold are allowed for this
/// agent only, larger ones need approval, and everything else is denied.
pub fn policy() -> DeclarativePolicy {
    DeclarativePolicy::builder()
        .name("acme-ap-v1")
        .rule(PolicyRule::allow("list_due_invoices").for_agent(AGENT_ID).for_tenant(TENANT))
        .rule(
            PolicyRule::allow("pay_invoice")
                .for_agent(AGENT_ID)
                .for_tenant(TENANT)
                .when(ArgPredicate::at_most("/amount_usd", APPROVAL_THRESHOLD_USD)),
        )
        .rule(
            PolicyRule::require_approval(
                "pay_invoice",
                "payments over 1,000 USD need two finance approvers",
            )
            .for_agent(AGENT_ID)
            .for_tenant(TENANT),
        )
        .build()
}

/// The principal every worker process runs the clerk as; only the instance differs.
pub fn principal(instance: &str) -> anyhow::Result<AgentPrincipal> {
    Ok(AgentPrincipal::new(AgentId::new(AGENT_ID)?)
        .with_tenant(TENANT)
        .with_class("ap-clerk")
        .with_instance(instance))
}

/// The model provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    Anthropic,
}

impl Provider {
    pub fn parse(name: &str) -> anyhow::Result<Self> {
        match name {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            other => anyhow::bail!("unknown provider '{other}'; use openai or anthropic"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    /// The model id, overridable with `OPENAI_MODEL` or `ANTHROPIC_MODEL`.
    pub fn model_id(self) -> String {
        match self {
            Self::OpenAi => std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.6-luna".into()),
            Self::Anthropic => {
                std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into())
            }
        }
    }

    pub fn model(self) -> anyhow::Result<Arc<dyn Llm>> {
        let key = |name: &str| {
            std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is not set; see .env.example"))
        };
        Ok(match self {
            Self::OpenAi => Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(
                key("OPENAI_API_KEY")?,
                self.model_id(),
            ))?),
            Self::Anthropic => Arc::new(AnthropicClient::new(AnthropicConfig::new(
                key("ANTHROPIC_API_KEY")?,
                self.model_id(),
            ))?),
        })
    }
}

/// The bank: checkouts live in the process that makes them, completed payments in a
/// file every process appends to, so the operator counts each payment across crashes.
pub struct Bank {
    payments: PathBuf,
    checkouts: Mutex<HashMap<String, TransactionRecord>>,
}

impl Bank {
    pub fn new(dir: &Path) -> Self {
        Self { payments: dir.join("payments.jsonl"), checkouts: Mutex::new(HashMap::new()) }
    }

    /// Every completed payment, in order.
    pub fn payments(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.payments)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The payment of `invoice_id`, if one completed.
    pub fn paid(&self, invoice_id: &str) -> Option<Value> {
        self.payments().into_iter().find(|payment| payment["invoiceId"] == invoice_id)
    }
}

fn unsupported<T>() -> AdkResult<T> {
    Err(AdkError::tool("not supported by the bank"))
}

#[async_trait]
impl MerchantCheckoutService for Bank {
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
        self.checkouts
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
            .checkouts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(lookup.transaction_id.as_str())
            .cloned())
    }

    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> AdkResult<TransactionRecord> {
        let record = self
            .checkouts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(command.context.transaction_id.as_str())
            .cloned()
            .ok_or_else(|| AdkError::tool("unknown checkout"))?;
        let line = record.cart.lines.first();
        let payment = json!({
            "transactionId": record.transaction_id.as_str(),
            "invoiceId": line.map(|line| line.line_id.clone()),
            "vendor": record.merchant_of_record.legal_name,
            "amountMinor": record.cart.total.amount_minor,
            "pid": std::process::id(),
            "at": chrono::Utc::now().to_rfc3339(),
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.payments)
            .map_err(|error| AdkError::tool(format!("bank ledger: {error}")))?;
        writeln!(file, "{payment}")
            .map_err(|error| AdkError::tool(format!("bank ledger: {error}")))?;
        file.sync_all().map_err(|error| AdkError::tool(format!("bank ledger: {error}")))?;
        Ok(record)
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
impl TransactionStore for Bank {
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

/// Lists the invoices due today that are not yet paid. When `crash_marker` is set, the
/// first call writes it and never returns: the process is killed inside the call.
pub struct ListDueInvoices {
    pub bank: Arc<Bank>,
    pub crash_marker: Option<PathBuf>,
}

#[async_trait]
impl Tool for ListDueInvoices {
    fn name(&self) -> &str {
        "list_due_invoices"
    }

    fn description(&self) -> &str {
        "Lists the vendor invoices due today that are not yet paid."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> AdkResult<Value> {
        if let Some(marker) = &self.crash_marker {
            std::fs::write(marker, std::process::id().to_string())
                .map_err(|error| AdkError::tool(format!("crash marker: {error}")))?;
            std::future::pending::<()>().await;
        }
        let due: Vec<Value> = DUE_TODAY
            .iter()
            .filter(|invoice| self.bank.paid(invoice.id).is_none())
            .map(|invoice| {
                json!({ "invoice_id": invoice.id, "vendor": invoice.vendor, "amount_usd": invoice.amount_usd })
            })
            .collect();
        Ok(json!({ "due": due }))
    }
}

/// Pays one invoice through the governed checkout: the payment tools of `adk-payments`,
/// whose spend guardrail reserves the amount against the run's spend ledger and whose
/// records go to the run's audit trail.
pub struct PayInvoice {
    pub bank: Arc<Bank>,
}

#[async_trait]
impl Tool for PayInvoice {
    fn name(&self) -> &str {
        "pay_invoice"
    }

    fn description(&self) -> &str {
        "Pays a vendor invoice from the company account. Each call moves money."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "invoice_id": { "type": "string" },
                "vendor": { "type": "string" },
                "amount_usd": { "type": "integer", "description": "Whole US dollars" }
            },
            "required": ["invoice_id", "vendor", "amount_usd"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        let invoice_id = args["invoice_id"].as_str().unwrap_or_default().to_string();
        let vendor = args["vendor"].as_str().unwrap_or_default().to_string();
        let amount_usd = args["amount_usd"].as_i64().unwrap_or_default();
        if invoice_id.is_empty() || vendor.is_empty() || amount_usd <= 0 {
            return Err(AdkError::tool(
                "invoice_id, vendor, and a positive amount_usd are required",
            ));
        }
        // The invoice id is the business idempotency key: a paid invoice is never paid again.
        if let Some(payment) = self.bank.paid(&invoice_id) {
            return Ok(
                json!({ "status": "already_paid", "transaction_id": payment["transactionId"] }),
            );
        }
        let tools = PaymentToolsetBuilder::new(self.bank.clone(), self.bank.clone())
            .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(vec![
                "payments:checkout:create",
                "payments:checkout:complete",
            ])))
            .build()
            .tools();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name() == name)
                .cloned()
                .ok_or_else(|| AdkError::tool(format!("payment tool {name} missing")))
        };
        let total = Money::new("USD", amount_usd * 100, 2);
        let cart = Cart {
            cart_id: None,
            lines: vec![CartLine {
                line_id: invoice_id.clone(),
                merchant_sku: None,
                title: format!("Invoice {invoice_id}"),
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
        let created = tool("payments_checkout_create")?
            .execute(
                ctx.clone(),
                json!({ "merchantId": vendor.to_lowercase().replace(' ', "-"), "merchantName": vendor, "cart": cart }),
            )
            .await?;
        let transaction_id = created
            .pointer("/summary/transactionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AdkError::tool(format!("checkout created no transaction: {created}")))?;
        tool("payments_checkout_complete")?
            .execute(ctx, json!({ "transactionId": transaction_id }))
            .await?;
        Ok(json!({ "status": "paid", "invoice_id": invoice_id, "transaction_id": transaction_id }))
    }
}

/// The clerk agent.
pub fn clerk(
    model: Arc<dyn Llm>,
    bank: Arc<Bank>,
    crash_marker: Option<PathBuf>,
) -> anyhow::Result<adk_agent::LlmAgent> {
    Ok(LlmAgentBuilder::new(TARGET)
        .description("Pays the vendor invoices due today")
        .model(model)
        .instruction(CLERK_INSTRUCTION)
        .tool(Arc::new(ListDueInvoices { bank: bank.clone(), crash_marker }) as Arc<dyn Tool>)
        .tool(Arc::new(PayInvoice { bank }) as Arc<dyn Tool>)
        .build()?)
}
