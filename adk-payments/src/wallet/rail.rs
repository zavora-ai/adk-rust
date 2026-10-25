//! Payment rails: how money reaches a payee (spec 3.5 §7.14).
//!
//! | Rail | Name | Funding | For |
//! |------|------|---------|-----|
//! | [`Ap2Rail`](super::Ap2Rail) (`ap2`) | `ap2` | the wallet's provider | purchases under AP2 mandates |
//! | [`UseBasedRail`](super::UseBasedRail) | `use_based` | the wallet's provider | metered APIs and model tokens, answering `402 Payment Required` challenges |
//! | [`ProviderBillingRail`] | `provider_billing:{provider}` | the owner's account at the provider | hosting, registrars and other services that bill the owner, within caps |
//!
//! None is the default: a mandate names its rail, and the wallet holds the adapters the
//! owner configured.

use std::sync::{Arc, Mutex};

use adk_core::{Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{WALLET_RAIL_DATA_MISMATCH_CODE, refused};
use super::policy::{Payee, RailRef, SpendCategory};
use super::provider::{Debit, StatementEntry, StatementKind};

/// Where a rail's money comes from.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::RailFunding;
///
/// assert_ne!(RailFunding::Wallet, RailFunding::ProviderBilling);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RailFunding {
    /// The wallet debits its provider before the rail pays, and voids the debit when the
    /// rail fails.
    Wallet,
    /// The payee bills the owner's own account with it; the wallet authorises the charge
    /// within the cap and reconciles against the provider's invoice.
    ProviderBilling,
}

/// One payment a rail executes, after the wallet's policy allowed it and, for a
/// wallet-funded rail, the provider debited the funds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RailPayment {
    /// The wallet's payment id.
    pub payment_id: String,
    /// The category it is spent in.
    pub category: SpendCategory,
    /// Who receives it.
    pub payee: Payee,
    /// Amount in minor units of `currency`.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// What it is for.
    pub description: String,
    /// The rail's own data: an AP2 cart mandate, a use-based challenge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail_data: Option<Value>,
    /// The provider's debit, for a wallet-funded rail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debit: Option<Debit>,
}

/// What a rail reports once a payment settled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RailSettlement {
    /// The payee's or rail's reference for the settled payment.
    pub reference: String,
    /// What settled, in minor units.
    pub amount: u64,
    /// When it settled.
    pub settled_at: DateTime<Utc>,
    /// What the caller presents to the payee or keeps as evidence: a use-based payment
    /// header, an AP2 payment receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Value>,
}

/// How money reaches a payee (spec 3.5 §7.14).
///
/// The wallet calls [`check`](Self::check) before any money moves, so rail data that does
/// not describe the payment the policy allowed — another merchant, another amount — is
/// refused; then [`execute`](Self::execute) once the policy, the cap and, for a
/// wallet-funded rail, the provider's debit allow it.
///
/// [`conformance::payment_rail_conformance`](super::conformance::payment_rail_conformance)
/// checks an implementation.
#[async_trait]
pub trait PaymentRail: Send + Sync {
    /// The rail's name, as mandates name it.
    fn rail(&self) -> RailRef;

    /// Where the rail's money comes from.
    fn funding(&self) -> RailFunding {
        RailFunding::Wallet
    }

    /// Refuses a payment whose rail data does not describe it.
    ///
    /// # Errors
    ///
    /// Returns `wallet.rail_data_mismatch` naming the disagreement.
    fn check(&self, payment: &RailPayment) -> Result<()> {
        let _ = payment;
        Ok(())
    }

    /// Pays `payment`.
    ///
    /// # Errors
    ///
    /// Returns the rail's error when the payment did not settle; for a wallet-funded rail
    /// the wallet then voids the debit.
    async fn execute(&self, payment: &RailPayment) -> Result<RailSettlement>;

    /// The rail's own statement for `[from, to)`, for rails whose money does not pass
    /// through the wallet's provider; `None` for the others.
    ///
    /// # Errors
    ///
    /// Returns the rail's error when it cannot answer.
    async fn statement(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<Vec<StatementEntry>>> {
        let _ = (from, to);
        Ok(None)
    }
}

/// The owner's account at a provider that bills it directly: a host, a registrar, a model
/// provider (spec 3.5 §7.14).
///
/// The adapter records each charge the wallet allowed, so the provider bills it to the
/// owner, and reads the provider's invoice for reconciliation. It has no method to add a
/// payment method or change the account: those are the owner's acts.
#[async_trait]
pub trait BillingAccount: Send + Sync {
    /// The provider's name; payments through its rail go to the payee with this id.
    fn provider(&self) -> &str;

    /// Records a charge the provider bills to the owner's account, returning the
    /// provider's reference for it.
    ///
    /// # Errors
    ///
    /// Returns the provider's error when it refuses the charge.
    async fn charge(&self, payment: &RailPayment) -> Result<String>;

    /// The provider's invoice lines in `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns the provider's error when it cannot answer.
    async fn invoice(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<StatementEntry>>;
}

/// Provider billing under the owner's accounts within caps (spec 3.5 §7.14).
///
/// Named `provider_billing:{provider}`, so a wallet holds one per provider. A payment to a
/// payee other than the provider is refused.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_payments::wallet::{InMemoryBillingAccount, PaymentRail, ProviderBillingRail, RailRef};
///
/// let rail = ProviderBillingRail::new(Arc::new(InMemoryBillingAccount::new("hostco", "USD")));
/// assert_eq!(rail.rail(), RailRef::provider_billing("hostco"));
/// ```
pub struct ProviderBillingRail {
    account: Arc<dyn BillingAccount>,
}

impl ProviderBillingRail {
    /// A rail billing `account`.
    pub fn new(account: Arc<dyn BillingAccount>) -> Self {
        Self { account }
    }
}

#[async_trait]
impl PaymentRail for ProviderBillingRail {
    fn rail(&self) -> RailRef {
        RailRef::provider_billing(self.account.provider())
    }

    fn funding(&self) -> RailFunding {
        RailFunding::ProviderBilling
    }

    fn check(&self, payment: &RailPayment) -> Result<()> {
        if payment.payee.id != self.account.provider() {
            return Err(refused(
                WALLET_RAIL_DATA_MISMATCH_CODE,
                format!(
                    "the {} billing rail bills only {}, not payee {}",
                    self.account.provider(),
                    self.account.provider(),
                    payment.payee.id
                ),
            ));
        }
        Ok(())
    }

    async fn execute(&self, payment: &RailPayment) -> Result<RailSettlement> {
        let reference = self.account.charge(payment).await?;
        Ok(RailSettlement {
            reference,
            amount: payment.amount,
            settled_at: Utc::now(),
            proof: None,
        })
    }

    async fn statement(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<Vec<StatementEntry>>> {
        Ok(Some(self.account.invoice(from, to).await?))
    }
}

/// A [`BillingAccount`] kept in memory, so tests are hermetic.
///
/// [`push_invoice_line`](Self::push_invoice_line) stands for a charge on the provider's
/// invoice the wallet did not make.
#[derive(Debug, Clone)]
pub struct InMemoryBillingAccount {
    provider: String,
    currency: String,
    lines: Arc<Mutex<Vec<StatementEntry>>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryBillingAccount {
    /// The owner's account at `provider`, billed in `currency`.
    pub fn new(provider: &str, currency: &str) -> Self {
        Self {
            provider: provider.to_string(),
            currency: currency.to_ascii_uppercase(),
            lines: Arc::new(Mutex::new(Vec::new())),
            clock: Arc::new(SystemClock),
        }
    }

    /// Sets the clock that stamps invoice lines.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Adds an invoice line the wallet did not make.
    pub fn push_invoice_line(&self, line: StatementEntry) {
        self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(line);
    }

    /// Every invoice line so far, for tests.
    pub fn lines(&self) -> Vec<StatementEntry> {
        self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl BillingAccount for InMemoryBillingAccount {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn charge(&self, payment: &RailPayment) -> Result<String> {
        let mut lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(line) =
            lines.iter().find(|line| line.reference.as_deref() == Some(&payment.payment_id))
        {
            return Ok(line.id.clone());
        }
        let id = format!("{}-charge-{}", self.provider, lines.len() + 1);
        lines.push(StatementEntry {
            id: id.clone(),
            reference: Some(payment.payment_id.clone()),
            kind: StatementKind::Debit,
            amount: payment.amount,
            currency: self.currency.clone(),
            payee: Some(self.provider.clone()),
            at: self.clock.now(),
        });
        Ok(id)
    }

    async fn invoice(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<StatementEntry>> {
        let lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(lines.iter().filter(|line| line.at >= from && line.at < to).cloned().collect())
    }
}
