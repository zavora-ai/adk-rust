//! The wallet's agent-facing tools: pay within the mandate, read where the wallet stands
//! (spec 3.5 §7.14, A-I16).
//!
//! | Tool | Effect | Scope | What it does |
//! |------|--------|-------|--------------|
//! | `wallet_pay` | `NonIdempotent` | `payments:payment:execute` | Pays within the category's mandate; holds a payment over a limit for the owner through the run's confirmation flow |
//! | `wallet_status` | `ReadOnly` | — | Caps, spend, remaining budget, pauses and held payments |
//!
//! There is no tool to raise a cap, add a payee, add a payment method or open an account,
//! and `wallet_pay` refuses arguments beyond its schema, so no model can ask for one.
//! Hosts classify `wallet_pay` as a financial call (`EffectClass::Financial`,
//! `ToolKind::Payment`): the governed path's policy and confirmation run before it, and the
//! wallet's own checks run inside it.

use std::sync::Arc;

use adk_auth::{ContextScopeResolver, ScopeGuard};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalScope, ErrorCategory, ErrorComponent, PolicyCall, PolicyDecision, Result,
    Tool, ToolConfirmationDecision, ToolConfirmationRequest, ToolContext, ToolEffect, ToolGate,
    ToolOrigin, ToolPolicyRequest,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::engine::{PaymentRequest, Wallet, WalletCaller};
use super::policy::{Payee, SpendCategory, category_name};
use super::store::PaymentRecord;
use crate::auth::PAYMENT_EXECUTE_SCOPES;

/// The name of the tool that pays from the wallet.
pub const WALLET_PAY_TOOL: &str = "wallet_pay";
/// The name of the tool that reads the wallet's state.
pub const WALLET_STATUS_TOOL: &str = "wallet_status";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PayArgs {
    category: SpendCategory,
    payee: Payee,
    amount: u64,
    currency: String,
    description: String,
    #[serde(default)]
    rail_data: Option<Value>,
}

fn parse_pay_args(args: Value) -> Result<PayArgs> {
    serde_json::from_value(args).map_err(|error| {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "wallet.tools.invalid_args",
            format!(
                "invalid arguments for `{WALLET_PAY_TOOL}`: {error}. It takes category, payee, \
                 amount, currency, description and railData only"
            ),
        )
    })
}

fn settled_response(record: &PaymentRecord) -> Value {
    json!({
        "status": "settled",
        "paymentId": record.id,
        "category": category_name(record.category),
        "payee": record.payee.id,
        "amount": record.amount,
        "currency": record.currency,
        "ownerApproved": record.owner_approved(),
        "proof": record.settlement.as_ref().and_then(|settlement| settlement.proof.clone()),
    })
}

struct WalletPayTool {
    wallet: Wallet,
}

impl WalletPayTool {
    /// A held payment: decided now from the run's approval sources, or held as the call.
    async fn on_held(
        &self,
        ctx: &Arc<dyn ToolContext>,
        args: Value,
        key: &str,
        record: PaymentRecord,
        caller: &WalletCaller,
    ) -> Result<Value> {
        let acting = ctx.acting_context();
        let request = ToolConfirmationRequest {
            tool_name: WALLET_PAY_TOOL.to_string(),
            function_call_id: Some(ctx.function_call_id().to_string()),
            args: args.clone(),
            acting: Some(Box::new(acting.clone())),
            origin: Some(ToolOrigin::of(self)),
        };
        let reason = record.hold.as_ref().map(ToString::to_string).unwrap_or_default();
        if let Some(config) = ctx.run_config() {
            let gate = ToolGate::new(
                config,
                ctx.agent_name(),
                ctx.invocation_id(),
                ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()),
            )
            .with_acting_context(acting.clone());
            let why = format!(
                "Tool '{WALLET_PAY_TOOL}' requires confirmation: payment held for the owner ({reason})"
            );
            if let Some(approval) = adk_core::confirm_tool_call(&gate, &request, &why).await? {
                let subject =
                    acting.on_behalf_of.clone().unwrap_or_else(|| SubjectRef::human(ctx.user_id()));
                return match approval.decision {
                    ToolConfirmationDecision::Deny => {
                        let record = self
                            .wallet
                            .decide_governed(&record.id, false, subject, approval.message, caller)
                            .await?;
                        Ok(json!({
                            "status": "rejected",
                            "paymentId": record.id,
                            "message": record.decision.and_then(|decision| decision.message),
                        }))
                    }
                    ToolConfirmationDecision::Approve => match approval.edited_args {
                        Some(edited) if edited != args => {
                            self.wallet
                                .decide_governed(
                                    &record.id,
                                    false,
                                    subject.clone(),
                                    Some("replaced by the approver's edited payment".to_string()),
                                    caller,
                                )
                                .await?;
                            self.pay_edited(ctx, edited, key, subject, approval.message, caller)
                                .await
                        }
                        _ => {
                            let record = self
                                .wallet
                                .decide_governed(
                                    &record.id,
                                    true,
                                    subject,
                                    approval.message,
                                    caller,
                                )
                                .await?;
                            Ok(settled_response(&record))
                        }
                    },
                };
            }
        }
        let mut actions = ctx.actions();
        actions.tool_confirmation = Some(request);
        ctx.set_actions(actions);
        Ok(json!({
            "status": "held",
            "paymentId": record.id,
            "reason": reason,
            "message": "the payment waits for the owner's decision",
        }))
    }

    /// Pays the arguments an approver substituted: checked against the run's policy, then
    /// through the wallet; the approval stands for the owner's decision if it is held again.
    async fn pay_edited(
        &self,
        ctx: &Arc<dyn ToolContext>,
        edited: Value,
        key: &str,
        subject: SubjectRef,
        message: Option<String>,
        caller: &WalletCaller,
    ) -> Result<Value> {
        if let Some(policy) = ctx.run_config().and_then(|config| config.tool_policy.clone()) {
            let request = ToolPolicyRequest::new(WALLET_PAY_TOOL, edited.clone())
                .with_effect(self.effect())
                .with_agent_name(ctx.agent_name())
                .with_identity(ctx.app_name(), ctx.user_id(), ctx.session_id())
                .with_invocation_id(ctx.invocation_id())
                .with_acting_context(ctx.acting_context());
            let origin = ToolOrigin::of(self);
            let call = PolicyCall::new()
                .with_origin(&origin)
                .with_function_call_id(ctx.function_call_id());
            if let PolicyDecision::Deny { reason } = policy.evaluate_call(&request, &call).await {
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Forbidden,
                    "wallet.tools.edited_args_denied",
                    format!("the approver's edited payment is denied by policy: {reason}"),
                ));
            }
        }
        let args = parse_pay_args(edited)?;
        let request = pay_request(format!("{key}/edited"), args);
        match self.wallet.pay(request, caller).await? {
            super::engine::PaymentOutcome::Settled(record) => Ok(settled_response(&record)),
            super::engine::PaymentOutcome::Held(record) => {
                let record =
                    self.wallet.decide_governed(&record.id, true, subject, message, caller).await?;
                Ok(settled_response(&record))
            }
        }
    }
}

fn pay_request(key: String, args: PayArgs) -> PaymentRequest {
    let request = PaymentRequest::new(key, args.category, args.payee, args.amount, &args.currency)
        .with_description(args.description);
    match args.rail_data {
        Some(data) => request.with_rail_data(data),
        None => request,
    }
}

#[async_trait]
impl Tool for WalletPayTool {
    fn name(&self) -> &str {
        WALLET_PAY_TOOL
    }

    fn description(&self) -> &str {
        "Pay from the site's owner-funded wallet within the owner's mandate for one spending \
         category. A payment over the mandate's per-payment limit or the category's cap waits \
         for the owner. Limits, payees and payment methods are the owner's; no tool changes them."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "category": {
                    "type": "string",
                    "enum": ["hosting", "models", "advertising", "domains", "other"]
                },
                "payee": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string", "description": "The payee's id on the rail" },
                        "name": { "type": "string" },
                        "domain": { "type": "string" }
                    },
                    "required": ["id"]
                },
                "amount": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Amount in minor units of the currency, such as cents"
                },
                "currency": { "type": "string", "description": "ISO 4217 code, the wallet's" },
                "description": { "type": "string", "description": "What the payment is for" },
                "railData": {
                    "type": "object",
                    "description": "The rail's data: the merchant's AP2 cart mandate, or the metered resource's payment challenge"
                }
            },
            "required": ["category", "payee", "amount", "currency", "description"]
        }))
    }

    fn required_scopes(&self) -> &[&str] {
        PAYMENT_EXECUTE_SCOPES
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let parsed = parse_pay_args(args.clone())?;
        // The call id without the invocation, so a held call resumed by a later run is the
        // same payment.
        let key = format!(
            "{}/{}/{}/{}",
            ctx.app_name(),
            ctx.user_id(),
            ctx.session_id(),
            ctx.function_call_id()
        );
        let caller = WalletCaller::from_context(ctx.as_ref());
        match self.wallet.pay(pay_request(key.clone(), parsed), &caller).await? {
            super::engine::PaymentOutcome::Settled(record) => Ok(settled_response(&record)),
            super::engine::PaymentOutcome::Held(record) => {
                self.on_held(&ctx, args, &key, record, &caller).await
            }
        }
    }
}

struct WalletStatusTool {
    wallet: Wallet,
}

#[async_trait]
impl Tool for WalletStatusTool {
    fn name(&self) -> &str {
        WALLET_STATUS_TOOL
    }

    fn description(&self) -> &str {
        "Read the site wallet: each category's cap, spend this period, remaining budget, \
         rail, allowed payees and pause, the payments waiting for the owner, and the balance."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {}, "additionalProperties": false }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let held: Vec<Value> = self
            .wallet
            .held()
            .await?
            .iter()
            .map(|record| {
                json!({
                    "paymentId": record.id,
                    "category": category_name(record.category),
                    "payee": record.payee.id,
                    "amount": record.amount,
                    "reason": record.hold.as_ref().map(ToString::to_string),
                })
            })
            .collect();
        Ok(json!({
            "currency": self.wallet.policy().currency,
            "period": self.wallet.policy().period,
            "balance": self.wallet.balance().await?,
            "categories": self.wallet.status().await?,
            "held": held,
        }))
    }
}

/// The bare `wallet_pay` tool. It declares its scope but does not enforce it; wrap it with
/// `adk_auth::ScopeGuard`, or use [`WalletToolset`].
pub fn wallet_pay_tool(wallet: Wallet) -> impl Tool {
    WalletPayTool { wallet }
}

/// The bare `wallet_status` tool.
pub fn wallet_status_tool(wallet: Wallet) -> impl Tool {
    WalletStatusTool { wallet }
}

/// The wallet's tools, scope-guarded (spec 3.5 §7.14).
///
/// # Example
///
/// ```rust,ignore
/// let tools = WalletToolset::new(wallet).tools();
/// assert_eq!(tools.len(), 2);
/// ```
pub struct WalletToolset {
    wallet: Wallet,
    scope_guard: Option<ScopeGuard>,
}

impl WalletToolset {
    /// The tools over `wallet`, guarded by the run's scopes.
    pub fn new(wallet: Wallet) -> Self {
        Self { wallet, scope_guard: None }
    }

    /// Enforces scopes with `guard` instead of the run's request context.
    #[must_use]
    pub fn with_scope_guard(mut self, guard: ScopeGuard) -> Self {
        self.scope_guard = Some(guard);
        self
    }

    /// `wallet_pay` and `wallet_status`, each wrapped in the scope guard.
    pub fn tools(self) -> Vec<Arc<dyn Tool>> {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(wallet_pay_tool(self.wallet.clone())),
            Arc::new(wallet_status_tool(self.wallet)),
        ];
        let guard = self.scope_guard.unwrap_or_else(|| ScopeGuard::new(ContextScopeResolver));
        guard.protect_all(tools)
    }
}
