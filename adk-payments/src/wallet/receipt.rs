//! The hook commerce receipts are signed and verified through (spec 3.5 §7.5).
//!
//! When a buyer's payment for an order or booking settles, the commerce payment step
//! issues an AWP [`Receipt`] signed by the site's delegated receipt key, so any agent can
//! verify it against the site's published keys. [`issue_receipt`] and [`verify_receipt`]
//! frame the document — the receipt without its `proof` — and a [`ReceiptSigner`] and
//! [`ReceiptVerifier`] supply the cryptography: a key with scope `receipt` delegated by the
//! site root key, from the AWP signing layer.

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use awp_types::{LineItem, Proof, Receipt, RequesterId, without_proof};
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

/// Signs commerce documents with the site's delegated receipt key (spec 3.5 §7.5).
#[async_trait]
pub trait ReceiptSigner: Send + Sync {
    /// Signs `document` — a receipt without its `proof` — returning a detached JWS over its
    /// JCS form by a key with scope `receipt`. The key never leaves the signer.
    ///
    /// # Errors
    ///
    /// Returns the signer's error; the receipt is not issued.
    async fn sign(&self, document: &Value) -> Result<Proof>;
}

/// Verifies commerce documents against the issuing site's published keys
/// (spec 3.5 §7.5).
#[async_trait]
pub trait ReceiptVerifier: Send + Sync {
    /// Checks `proof` over `document`: the key's delegation from the site root, its
    /// `receipt` scope and validity window, and the signature over the document's JCS
    /// form.
    ///
    /// # Errors
    ///
    /// Returns an error naming the broken link: an unknown or expired key, or a signature
    /// that does not verify, as after any change to the document.
    async fn verify(&self, document: &Value, proof: &Proof) -> Result<()>;
}

/// What a receipt records, before it is signed.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::ReceiptDetails;
///
/// let details = ReceiptDetails::new("book_table", "booking-17", 4_500, "USD").with_rail("ap2");
/// assert_eq!(details.rail.as_deref(), Some("ap2"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptDetails {
    /// The capability the receipt is for: an order, booking or payment.
    pub capability: String,
    /// The order, booking or payment id.
    pub reference: String,
    /// The verified requester who paid.
    pub requester: Option<RequesterId>,
    /// The quote the receipt settles.
    pub quote: Option<Uuid>,
    /// Priced lines.
    pub lines: Vec<LineItem>,
    /// Amount in minor units.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// The rail that settled it.
    pub rail: Option<String>,
    /// The signed release whose prices it uses.
    pub release_seq: Option<u64>,
}

impl ReceiptDetails {
    /// A receipt for `amount` minor units of `currency` paid through `capability` for
    /// `reference`.
    pub fn new(capability: &str, reference: &str, amount: u64, currency: &str) -> Self {
        Self {
            capability: capability.to_string(),
            reference: reference.to_string(),
            requester: None,
            quote: None,
            lines: Vec::new(),
            amount,
            currency: currency.to_ascii_uppercase(),
            rail: None,
            release_seq: None,
        }
    }

    /// Names the rail that settled it.
    #[must_use]
    pub fn with_rail(mut self, rail: &str) -> Self {
        self.rail = Some(rail.to_string());
        self
    }

    /// Adds the priced lines.
    #[must_use]
    pub fn with_lines(mut self, lines: Vec<LineItem>) -> Self {
        self.lines = lines;
        self
    }

    /// Names the quote it settles and the release its prices come from.
    #[must_use]
    pub fn settling_quote(mut self, quote: Uuid, release_seq: u64) -> Self {
        self.quote = Some(quote);
        self.release_seq = Some(release_seq);
        self
    }

    /// Names the verified requester who paid.
    #[must_use]
    pub fn for_requester(mut self, requester: RequesterId) -> Self {
        self.requester = Some(requester);
        self
    }
}

fn framing_error(error: serde_json::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Guardrail,
        ErrorCategory::Internal,
        "wallet.receipt.framing",
        format!("the receipt could not be framed for signing: {error}"),
    )
}

/// Issues a receipt for `details` at `issued_at`, signed by `signer` (spec 3.5 §7.5).
///
/// # Errors
///
/// Returns the signer's error; no receipt is issued unsigned.
pub async fn issue_receipt(
    signer: &dyn ReceiptSigner,
    details: ReceiptDetails,
    issued_at: DateTime<Utc>,
) -> Result<Receipt> {
    let mut receipt = Receipt {
        id: Uuid::new_v4(),
        capability: details.capability,
        reference: details.reference,
        requester: details.requester,
        quote: details.quote,
        lines: details.lines,
        amount: details.amount,
        currency: details.currency,
        rail: details.rail,
        release_seq: details.release_seq,
        issued_at,
        proof: Proof::eddsa("", ""),
    };
    let document = without_proof(&receipt).map_err(framing_error)?;
    receipt.proof = signer.sign(&document).await?;
    Ok(receipt)
}

/// Verifies `receipt` with `verifier`: its proof over the receipt without `proof`
/// (spec 3.5 §7.5).
///
/// # Errors
///
/// Returns the verifier's error for a receipt that was changed after signing or signed by
/// a key the site did not delegate.
pub async fn verify_receipt(verifier: &dyn ReceiptVerifier, receipt: &Receipt) -> Result<()> {
    let document = without_proof(receipt).map_err(framing_error)?;
    verifier.verify(&document, &receipt.proof).await
}
