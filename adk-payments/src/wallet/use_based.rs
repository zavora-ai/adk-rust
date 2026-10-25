//! The use-based rail: metered APIs and model tokens paid per use, x402-style
//! (spec 3.5 §7.14, §13 Q5).
//!
//! A metered resource answers `402 Payment Required` with what it accepts. The caller
//! reads the [`PaymentChallenge`]s with its [`UseBasedScheme`], asks the wallet to pay one
//! ([`Wallet::pay_challenge`](super::Wallet::pay_challenge)) — the category's mandate,
//! payee rules and cap apply before any money moves — and retries the request with the
//! [`PaymentProof`] the settled payment carries. The scheme hides the wire: x402 with a
//! facilitator, or any other use-based protocol.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::error::{WALLET_RAIL_DATA_MISMATCH_CODE, refused};
use super::policy::{Payee, RailRef};
use super::rail::{PaymentRail, RailPayment, RailSettlement};

/// What a metered resource asked for in its `402 Payment Required` answer.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::PaymentChallenge;
///
/// let challenge = PaymentChallenge::new("https://api.models.example/v1/generate", "acct-models", 12, "USD");
/// assert_eq!(challenge.payee().domain.as_deref(), Some("api.models.example"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentChallenge {
    /// The payment scheme, such as x402's `exact`.
    pub scheme: String,
    /// The settlement network, when the scheme names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// The URL of the metered resource.
    pub resource: String,
    /// Who is paid: the resource's payee id on the scheme.
    pub pay_to: String,
    /// The price, in minor units of `currency`.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// What the payment buys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How long the resource honours a payment for this challenge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timeout_seconds: Option<u64>,
    /// Scheme-specific fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl PaymentChallenge {
    /// A challenge from `resource` asking `amount` minor units of `currency` for `pay_to`,
    /// under the `exact` scheme.
    pub fn new(resource: &str, pay_to: &str, amount: u64, currency: &str) -> Self {
        Self {
            scheme: "exact".to_string(),
            network: None,
            resource: resource.to_string(),
            pay_to: pay_to.to_string(),
            amount,
            currency: currency.to_ascii_uppercase(),
            description: None,
            max_timeout_seconds: None,
            extra: None,
        }
    }

    /// The payee the wallet's mandate is checked against: `pay_to` on the resource's host.
    pub fn payee(&self) -> Payee {
        let payee = Payee::new(&self.pay_to);
        match host_of(&self.resource) {
            Some(host) => payee.with_domain(host),
            None => payee,
        }
    }
}

/// The host of `url`, lowercased, without user info or port.
pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if authority.starts_with('[') {
        authority.split_once(']').map(|(host, _)| &host[1..])?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// What the caller attaches when it retries a metered request: x402's `X-PAYMENT` header,
/// or the scheme's own.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::PaymentProof;
///
/// let proof = PaymentProof { header: "X-PAYMENT".into(), value: "...".into() };
/// assert_eq!(proof.header, "X-PAYMENT");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentProof {
    /// The request header to set.
    pub header: String,
    /// Its value.
    pub value: String,
}

/// What a scheme reports for a paid challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UseBasedPayment {
    /// The scheme's or facilitator's reference for the payment.
    pub reference: String,
    /// What the caller attaches when it retries the request.
    pub proof: PaymentProof,
}

/// A use-based payment protocol (spec 3.5 §7.14): x402 or another 402 challenge and
/// response.
///
/// The wallet has already checked the mandate, reserved the cap and debited its provider
/// when it calls [`pay`](Self::pay); the scheme turns the funds into the proof the resource
/// accepts.
#[async_trait]
pub trait UseBasedScheme: Send + Sync {
    /// The scheme's name, such as `x402`.
    fn name(&self) -> &str;

    /// Reads the challenges in a `402 Payment Required` answer's body.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidInput` error when the body is not this scheme's challenge.
    fn challenges(&self, body: &Value) -> Result<Vec<PaymentChallenge>>;

    /// Pays `challenge` with the funds `payment` debited.
    ///
    /// # Errors
    ///
    /// Returns the scheme's error when the payment did not settle; the wallet then voids
    /// the debit.
    async fn pay(
        &self,
        challenge: &PaymentChallenge,
        payment: &RailPayment,
    ) -> Result<UseBasedPayment>;
}

/// The use-based rail adapter for metered APIs and model tokens (spec 3.5 §7.14).
///
/// A payment's rail data is the [`PaymentChallenge`] it answers; a challenge whose payee,
/// host, amount or currency differs from the payment the wallet allowed is refused before
/// any money moves.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_payments::wallet::{InMemoryUseBasedScheme, PaymentRail, RailRef, UseBasedRail};
///
/// let rail = UseBasedRail::new(Arc::new(InMemoryUseBasedScheme::new()));
/// assert_eq!(rail.rail(), RailRef::use_based());
/// ```
pub struct UseBasedRail {
    scheme: Arc<dyn UseBasedScheme>,
}

impl UseBasedRail {
    /// A rail paying through `scheme`.
    pub fn new(scheme: Arc<dyn UseBasedScheme>) -> Self {
        Self { scheme }
    }

    /// The scheme, to read challenges with.
    pub fn scheme(&self) -> &Arc<dyn UseBasedScheme> {
        &self.scheme
    }
}

fn challenge_of(payment: &RailPayment) -> Result<PaymentChallenge> {
    let data = payment.rail_data.clone().ok_or_else(|| {
        refused(
            WALLET_RAIL_DATA_MISMATCH_CODE,
            "a use-based payment carries the challenge it answers as its rail data",
        )
    })?;
    serde_json::from_value(data).map_err(|error| {
        refused(
            WALLET_RAIL_DATA_MISMATCH_CODE,
            format!("the use-based rail data is not a payment challenge: {error}"),
        )
    })
}

#[async_trait]
impl PaymentRail for UseBasedRail {
    fn rail(&self) -> RailRef {
        RailRef::use_based()
    }

    fn check(&self, payment: &RailPayment) -> Result<()> {
        let challenge = challenge_of(payment)?;
        let mismatch = |what: String| Err(refused(WALLET_RAIL_DATA_MISMATCH_CODE, what));
        if challenge.pay_to != payment.payee.id {
            return mismatch(format!(
                "the challenge pays {}, not payee {}",
                challenge.pay_to, payment.payee.id
            ));
        }
        if let Some(domain) = &payment.payee.domain
            && challenge.payee().domain.as_deref() != Some(domain.to_ascii_lowercase().as_str())
        {
            return mismatch(format!(
                "the challenge comes from {}, not the payee's domain {domain}",
                challenge.resource
            ));
        }
        if challenge.amount != payment.amount
            || !challenge.currency.eq_ignore_ascii_case(&payment.currency)
        {
            return mismatch(format!(
                "the challenge asks {} {}, not the {} {} the wallet allowed",
                challenge.amount, challenge.currency, payment.amount, payment.currency
            ));
        }
        Ok(())
    }

    async fn execute(&self, payment: &RailPayment) -> Result<RailSettlement> {
        let challenge = challenge_of(payment)?;
        let paid = self.scheme.pay(&challenge, payment).await?;
        Ok(RailSettlement {
            reference: paid.reference,
            amount: payment.amount,
            settled_at: Utc::now(),
            proof: Some(serde_json::to_value(&paid.proof).unwrap_or(Value::Null)),
        })
    }
}

/// A hermetic [`UseBasedScheme`] for tests and local fixtures, shaped like x402.
///
/// It reads x402's `{"accepts": [...]}` body, with `asset` as the ISO 4217 currency and
/// `maxAmountRequired` in its minor units, and answers with an `X-PAYMENT` token that
/// [`verify`](Self::verify) accepts once. It moves no money of its own: the wallet's
/// provider debit is the payment.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{InMemoryUseBasedScheme, UseBasedScheme};
/// use serde_json::json;
///
/// let scheme = InMemoryUseBasedScheme::new();
/// let challenges = scheme.challenges(&json!({
///     "x402Version": 1,
///     "accepts": [{
///         "scheme": "exact", "resource": "https://api.models.example/v1/generate",
///         "payTo": "acct-models", "maxAmountRequired": "12", "asset": "USD"
///     }]
/// }))?;
/// assert_eq!(challenges[0].amount, 12);
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryUseBasedScheme {
    issued: Arc<Mutex<HashMap<String, (PaymentChallenge, bool)>>>,
}

impl InMemoryUseBasedScheme {
    /// A scheme with no payments yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `value` is an unspent payment for `challenge`; a verified payment is spent,
    /// so a replayed header is refused.
    pub fn verify(&self, challenge: &PaymentChallenge, value: &str) -> bool {
        let mut issued = self.issued.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match issued.get_mut(value) {
            Some((paid, spent)) if paid == challenge && !*spent => {
                *spent = true;
                true
            }
            _ => false,
        }
    }

    /// How many challenges were paid.
    pub fn paid(&self) -> usize {
        self.issued.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }
}

#[async_trait]
impl UseBasedScheme for InMemoryUseBasedScheme {
    fn name(&self) -> &str {
        "x402-local"
    }

    fn challenges(&self, body: &Value) -> Result<Vec<PaymentChallenge>> {
        let invalid = |reason: &str| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "wallet.use_based.invalid_challenge",
                format!("not an x402 payment requirement: {reason}"),
            )
        };
        let accepts =
            body.get("accepts").and_then(Value::as_array).ok_or_else(|| invalid("no accepts"))?;
        accepts
            .iter()
            .map(|item| {
                let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
                let amount = text("maxAmountRequired")
                    .and_then(|amount| amount.parse::<u64>().ok())
                    .ok_or_else(|| invalid("maxAmountRequired is not an amount"))?;
                Ok(PaymentChallenge {
                    scheme: text("scheme").unwrap_or_else(|| "exact".to_string()),
                    network: text("network"),
                    resource: text("resource").ok_or_else(|| invalid("no resource"))?,
                    pay_to: text("payTo").ok_or_else(|| invalid("no payTo"))?,
                    amount,
                    currency: text("asset")
                        .ok_or_else(|| invalid("no asset"))?
                        .to_ascii_uppercase(),
                    description: text("description"),
                    max_timeout_seconds: item.get("maxTimeoutSeconds").and_then(Value::as_u64),
                    extra: item.get("extra").cloned(),
                })
            })
            .collect()
    }

    async fn pay(
        &self,
        challenge: &PaymentChallenge,
        payment: &RailPayment,
    ) -> Result<UseBasedPayment> {
        let token = json!({
            "scheme": challenge.scheme,
            "payTo": challenge.pay_to,
            "amount": challenge.amount,
            "currency": challenge.currency,
            "payment": payment.payment_id,
        });
        let value = hex::encode(token.to_string());
        self.issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(value.clone())
            .or_insert_with(|| (challenge.clone(), false));
        Ok(UseBasedPayment {
            reference: format!("x402-local:{}", payment.payment_id),
            proof: PaymentProof { header: "X-PAYMENT".to_string(), value },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_drop_scheme_user_info_port_and_path() {
        assert_eq!(
            host_of("https://API.example.com:8443/v1?x=1").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(host_of("http://user:pw@example.com/").as_deref(), Some("example.com"));
        assert_eq!(host_of("https://[::1]:8080/x").as_deref(), Some("::1"));
        assert_eq!(host_of("example.com/path").as_deref(), Some("example.com"));
        assert_eq!(host_of("https:///nohost"), None);
    }
}
