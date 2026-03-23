//! Protocol-neutral commerce domain types.
//!
//! These types model durable commerce truth shared across ACP stable
//! `2026-01-30` and AP2 `v0.1-alpha` (`2026-03-22`) without forcing direct
//! protocol-to-protocol transcoding.

pub mod actor;
pub mod cart;
pub mod evidence;
pub mod intervention;
pub mod money;
pub mod order;
pub mod transaction;

pub use actor::{CommerceActor, CommerceActorRole, MerchantRef, PaymentProcessorRef};
pub use cart::{
    AffiliateAttribution, Cart, CartLine, FulfillmentDestination, FulfillmentKind,
    FulfillmentSelection, PriceAdjustment, PriceAdjustmentKind,
};
pub use evidence::{
    EvidenceReference, ProtocolDescriptor, ProtocolExtensionEnvelope, ProtocolExtensions,
};
pub use intervention::{InterventionKind, InterventionState, InterventionStatus};
pub use money::Money;
pub use order::{OrderSnapshot, OrderState, ReceiptState};
pub use transaction::{
    CommerceMode, PaymentMethodSelection, ProtocolEnvelopeDigest, ProtocolReference, ProtocolRefs,
    SafeTransactionSummary, TransactionId, TransactionRecord, TransactionState,
    TransactionStateTag,
};
