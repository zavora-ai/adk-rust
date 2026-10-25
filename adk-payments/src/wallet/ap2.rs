//! The AP2 rail: purchases under AP2 mandates (spec 3.5 §7.14, §13 Q5).
//!
//! The site buys as an AP2 shopper. The merchant's signed cart mandate is the payment's
//! rail data; the rail builds the payment mandate for exactly that cart, has the owner's
//! credentials provider authorise it — never the agent — and submits it to the merchant
//! agent through an [`Ap2MerchantEndpoint`], which on the merchant's side is an
//! [`Ap2Adapter`](crate::protocol::ap2::Ap2Adapter).

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::error::{WALLET_RAIL_DATA_MISMATCH_CODE, refused};
use super::policy::RailRef;
use super::rail::{PaymentRail, RailPayment, RailSettlement};
use crate::domain::Money;
use crate::kernel::PaymentExecutionOutcome;
use crate::protocol::ap2::{
    CartMandate, PaymentMandate, PaymentMandateContents, PaymentReceipt, PaymentResponse,
};

/// What a merchant agent answered to a payment mandate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2Settlement {
    /// The merchant's outcome.
    pub outcome: PaymentExecutionOutcome,
    /// The merchant's reference for the payment.
    pub reference: String,
    /// The merchant's receipt, when it returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<PaymentReceipt>,
}

/// Where the wallet sends AP2 payment mandates: the merchant agent's AP2 endpoint, over A2A
/// in production.
#[async_trait]
pub trait Ap2MerchantEndpoint: Send + Sync {
    /// Submits `mandate` for the cart `cart` describes.
    ///
    /// # Errors
    ///
    /// Returns the merchant's refusal or a transport error.
    async fn submit_payment_mandate(
        &self,
        cart: &CartMandate,
        mandate: PaymentMandate,
    ) -> Result<Ap2Settlement>;
}

/// The owner's credentials provider: authorises a payment mandate on the owner's behalf
/// within the wallet's mandate (spec 3.5 §7.14). The agent never holds this authority.
#[async_trait]
pub trait Ap2UserAuthorizer: Send + Sync {
    /// The user authorization over `contents`, such as an SD-JWT the merchant verifies.
    ///
    /// # Errors
    ///
    /// Returns the provider's refusal.
    async fn authorize(&self, contents: &PaymentMandateContents) -> Result<String>;
}

/// The rail data of an AP2 purchase: the merchant's signed cart mandate.
///
/// # Example
///
/// ```rust,ignore
/// let request = PaymentRequest::new(key, SpendCategory::Domains, payee, 1_500, "USD")
///     .with_rail_data(Ap2RailData { cart_mandate }.into_value());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2RailData {
    /// The merchant's signed cart.
    pub cart_mandate: CartMandate,
}

impl Ap2RailData {
    /// The data as a payment request's `rail_data`.
    pub fn into_value(self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// AP2 mandates for purchases (spec 3.5 §7.14).
///
/// A cart from another merchant than the payee, or at another total than the payment the
/// wallet allowed, is refused before any money moves. A merchant answer other than
/// authorised or completed fails the payment, and the wallet voids its debit.
pub struct Ap2Rail {
    endpoint: Arc<dyn Ap2MerchantEndpoint>,
    authorizer: Arc<dyn Ap2UserAuthorizer>,
}

impl Ap2Rail {
    /// A rail submitting to `endpoint`, authorised by `authorizer`.
    pub fn new(
        endpoint: Arc<dyn Ap2MerchantEndpoint>,
        authorizer: Arc<dyn Ap2UserAuthorizer>,
    ) -> Self {
        Self { endpoint, authorizer }
    }
}

fn cart_of(payment: &RailPayment) -> Result<CartMandate> {
    let data = payment.rail_data.clone().ok_or_else(|| {
        refused(
            WALLET_RAIL_DATA_MISMATCH_CODE,
            "an AP2 purchase carries the merchant's cart mandate as its rail data",
        )
    })?;
    let data: Ap2RailData = serde_json::from_value(data).map_err(|error| {
        refused(
            WALLET_RAIL_DATA_MISMATCH_CODE,
            format!("the AP2 rail data is not a cart mandate: {error}"),
        )
    })?;
    Ok(data.cart_mandate)
}

/// The cart's total in minor units of `currency`, or why it cannot be paid as `currency`.
fn cart_total(cart: &CartMandate, currency: &str) -> std::result::Result<u64, String> {
    let total = &cart.contents.payment_request.details.total.amount;
    if !total.currency.eq_ignore_ascii_case(currency) {
        return Err(format!("the cart is in {}, not {currency}", total.currency));
    }
    let scale = Money::iso_minor_unit_scale(currency).unwrap_or(2);
    let money = total.to_money().map_err(|error| error.to_string())?;
    let money = money.rescaled(scale).map_err(|error| error.to_string())?;
    u64::try_from(money.amount_minor).map_err(|_| "the cart total is negative".to_string())
}

#[async_trait]
impl PaymentRail for Ap2Rail {
    fn rail(&self) -> RailRef {
        RailRef::ap2()
    }

    fn check(&self, payment: &RailPayment) -> Result<()> {
        let cart = cart_of(payment)?;
        let merchant = &cart.contents.merchant_name;
        let names_payee = merchant.eq_ignore_ascii_case(&payment.payee.id)
            || payment
                .payee
                .name
                .as_deref()
                .is_some_and(|name| merchant.eq_ignore_ascii_case(name));
        if !names_payee {
            return Err(refused(
                WALLET_RAIL_DATA_MISMATCH_CODE,
                format!("the cart is from {merchant}, not payee {}", payment.payee.id),
            ));
        }
        let total = cart_total(&cart, &payment.currency)
            .map_err(|reason| refused(WALLET_RAIL_DATA_MISMATCH_CODE, reason))?;
        if total != payment.amount {
            return Err(refused(
                WALLET_RAIL_DATA_MISMATCH_CODE,
                format!("the cart totals {total}, not the {} the wallet allowed", payment.amount),
            ));
        }
        Ok(())
    }

    async fn execute(&self, payment: &RailPayment) -> Result<RailSettlement> {
        let cart = cart_of(payment)?;
        let details = &cart.contents.payment_request.details;
        let method = cart
            .contents
            .payment_request
            .method_data
            .first()
            .map_or_else(|| "CARD".to_string(), |method| method.supported_methods.clone());
        let mut funding = Map::new();
        if let Some(debit) = &payment.debit {
            funding.insert("debitId".to_string(), Value::String(debit.id.clone()));
            if let Some(token) = &debit.funding_token {
                funding.insert("fundingToken".to_string(), Value::String(token.clone()));
            }
        }
        let contents = PaymentMandateContents {
            payment_mandate_id: payment.payment_id.clone(),
            payment_details_id: details.id.clone(),
            payment_details_total: details.total.clone(),
            payment_response: PaymentResponse {
                request_id: details.id.clone(),
                method_name: method,
                details: Some(funding),
                shipping_address: None,
                shipping_option: None,
                payer_name: None,
                payer_email: None,
                payer_phone: None,
            },
            merchant_agent: cart.contents.merchant_name.clone(),
            timestamp: Utc::now().to_rfc3339(),
        };
        let user_authorization = self.authorizer.authorize(&contents).await?;
        let mandate = PaymentMandate {
            payment_mandate_contents: contents,
            user_authorization: Some(user_authorization),
        };
        let settled = self.endpoint.submit_payment_mandate(&cart, mandate).await?;
        match settled.outcome {
            PaymentExecutionOutcome::Authorized | PaymentExecutionOutcome::Completed => {
                Ok(RailSettlement {
                    reference: settled.reference.clone(),
                    amount: payment.amount,
                    settled_at: Utc::now(),
                    proof: Some(json!({
                        "merchant": cart.contents.merchant_name,
                        "outcome": settled.outcome,
                        "receipt": settled.receipt,
                    })),
                })
            }
            PaymentExecutionOutcome::InterventionRequired | PaymentExecutionOutcome::Failed => {
                Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "wallet.ap2.not_settled",
                    format!(
                        "{} answered the payment mandate with {:?}; nothing was paid",
                        cart.contents.merchant_name, settled.outcome
                    ),
                ))
            }
        }
    }
}
