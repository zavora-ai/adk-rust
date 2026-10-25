//! An owner-funded wallet with mandates per category, payment rails, anomaly alarms,
//! reconciliation and profit and loss (spec 3.5 §7.14, D-10, A-I16). Feature `wallet`.
//!
//! Money is boxed. The owner funds the wallet through a [`WalletProvider`] — stored value
//! with a payment provider, or a prepaid virtual card through a [`CardIssuer`] — and the
//! site holds no custody of its own. The owner's [`WalletPolicy`] gives each
//! [`SpendCategory`] a [`Mandate`]: a cap per period, a limit per payment, the payees and
//! the rail. Every payment is a financial call on the governed path (the `wallet_pay`
//! tool), and the [`Wallet`] checks the mandate and the spend ledger's category cap before
//! any money moves; a payment over a limit or the cap is held for the owner.
//!
//! | Part | Types |
//! |------|-------|
//! | Policy | [`WalletPolicy`], [`Mandate`], [`SpendCategory`], [`PayeeRule`], [`RailRef`], [`AnomalyRule`], [`Cadence`] |
//! | Funding | [`WalletProvider`], [`InMemoryWalletProvider`], [`CardIssuer`], [`PrepaidCardProvider`], [`InMemoryCardIssuer`] |
//! | Rails | [`PaymentRail`], [`UseBasedRail`] over a [`UseBasedScheme`], [`ProviderBillingRail`] over a [`BillingAccount`], `Ap2Rail` (with `ap2`) |
//! | Payments | [`Wallet`], [`PaymentRequest`], [`PaymentOutcome`], [`PaymentRecord`], [`WalletStore`] |
//! | Durable stores | `SqliteWalletStore` (`sqlite`), `PostgresWalletStore` (`postgres`), `RedisWalletStore` (`redis`) |
//! | Oversight | [`WalletAlarm`] through a [`WalletAlarmHook`], [`ReconciliationReport`], [`WalletKpiSource`] |
//! | Tools | [`WalletToolset`]: `wallet_pay` and `wallet_status` |
//! | Commerce receipts | [`ReceiptSigner`], [`ReceiptVerifier`], [`issue_receipt`], [`verify_receipt`] |
//!
//! The site never opens accounts, adds payment methods, changes payees outside the
//! mandate or raises a cap (A-I16): no type here has a method that would, the policy is
//! read-only once the wallet is built, and approving a held payment or resuming a paused
//! category takes a person's decision.
//!
//! Experimental: the wallet tracks spec 3.5 and may change without notice.

mod anomaly;
#[cfg(feature = "ap2")]
mod ap2;
pub mod conformance;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "redis"))]
mod durable;
mod engine;
mod error;
mod kpi;
mod policy;
mod provider;
mod rail;
mod receipt;
mod reconcile;
mod store;
mod tools;
mod use_based;

pub use anomaly::{InMemoryAlarmHook, WalletAlarm, WalletAlarmHook, WalletAlarmKind};
#[cfg(feature = "ap2")]
#[cfg_attr(docsrs, doc(cfg(feature = "ap2")))]
pub use ap2::{Ap2MerchantEndpoint, Ap2Rail, Ap2RailData, Ap2Settlement, Ap2UserAuthorizer};
#[cfg(feature = "postgres")]
pub use durable::PostgresWalletStore;
#[cfg(feature = "redis")]
pub use durable::RedisWalletStore;
#[cfg(feature = "sqlite")]
pub use durable::SqliteWalletStore;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "redis"))]
pub use durable::{WALLET_STORE_CORRUPT_CODE, WALLET_STORE_UNAVAILABLE_CODE};
pub use engine::{
    CategoryStatus, PaymentOutcome, PaymentRequest, Wallet, WalletBuilder, WalletCaller,
};
pub use error::{
    AWP_ERROR_DETAIL, WALLET_CATEGORY_PAUSED_CODE, WALLET_CURRENCY_MISMATCH_CODE,
    WALLET_INSUFFICIENT_FUNDS_CODE, WALLET_INVALID_AMOUNT_CODE, WALLET_INVALID_POLICY_CODE,
    WALLET_KEY_REUSED_CODE, WALLET_NO_MANDATE_CODE, WALLET_NOT_HELD_CODE,
    WALLET_OWNER_DECISION_REQUIRED_CODE, WALLET_PAYEE_OUTSIDE_MANDATE_CODE,
    WALLET_PAYMENT_FAILED_CODE, WALLET_PAYMENT_IN_PROGRESS_CODE, WALLET_PAYMENT_NOT_FOUND_CODE,
    WALLET_PAYMENT_REJECTED_CODE, WALLET_RAIL_DATA_MISMATCH_CODE, WALLET_RAIL_UNAVAILABLE_CODE,
    WALLET_UNSUPPORTED_DECISION_CODE, WALLET_WRONG_SITE_CODE,
};
pub use kpi::{
    COST_PER_LEAD_METRIC, COST_PER_SALE_METRIC, CommerceOutcomes, InMemoryCommerceOutcomes,
    PROFIT_METRIC, REVENUE_METRIC, SPEND_METRIC, WalletKpiSource,
};
pub use policy::{
    AnomalyRule, Cadence, Mandate, Payee, PayeeRule, Period, RailRef, SpendCategory, WalletPolicy,
    category_name, category_scope, owner_approved_scope, previous_window,
};
pub use provider::{
    CardIssuer, Debit, DebitRequest, Funding, InMemoryCardIssuer, InMemoryWalletProvider,
    PrepaidCard, PrepaidCardProvider, StatementEntry, StatementKind, WalletProvider,
};
pub use rail::{
    BillingAccount, InMemoryBillingAccount, PaymentRail, ProviderBillingRail, RailFunding,
    RailPayment, RailSettlement,
};
pub use receipt::{ReceiptDetails, ReceiptSigner, ReceiptVerifier, issue_receipt, verify_receipt};
pub use reconcile::{Discrepancy, ReconciliationReport};
pub use store::{
    CategoryPause, HoldReason, InMemoryWalletStore, OwnerDecisionRecord, PaymentQuery,
    PaymentRecord, PaymentState, WalletStore,
};
pub use tools::{
    WALLET_PAY_TOOL, WALLET_STATUS_TOOL, WalletToolset, wallet_pay_tool, wallet_status_tool,
};
pub use use_based::{
    InMemoryUseBasedScheme, PaymentChallenge, PaymentProof, UseBasedPayment, UseBasedRail,
    UseBasedScheme,
};
