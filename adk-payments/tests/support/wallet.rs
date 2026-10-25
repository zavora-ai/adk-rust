//! Fixtures shared by the wallet tests (spec 3.5 §7.14).

#![allow(dead_code)]

use std::sync::Arc;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{ApprovalDecision, InMemorySpendLedger, ManualClock, SpendLedger, SubjectRef};
use adk_payments::wallet::{
    InMemoryAlarmHook, InMemoryBillingAccount, InMemoryUseBasedScheme, InMemoryWalletProvider,
    InMemoryWalletStore, Mandate, PayeeRule, PaymentChallenge, ProviderBillingRail, RailRef,
    SpendCategory, UseBasedRail, Wallet, WalletBuilder, WalletCaller, WalletPolicy,
};
use chrono::{DateTime, TimeZone, Utc};

pub const SITE: &str = "site-1";
pub const OWNER: &str = "owner@example.com";
pub const MODELS_HOST: &str = "https://api.models.example/v1/generate";
pub const MODELS_PAYEE: &str = "acct-models";

pub fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
}

/// The S12 owner's policy, in USD cents: models through the use-based rail, hosting,
/// domains and advertising billed by their providers.
pub fn s12_policy() -> WalletPolicy {
    WalletPolicy::new("USD")
        .with_mandate(
            SpendCategory::Models,
            Mandate::new(32_000, 500, RailRef::use_based())
                .with_payee(PayeeRule::Domain("models.example".into())),
        )
        .with_mandate(
            SpendCategory::Hosting,
            Mandate::new(5_000, 5_000, RailRef::provider_billing("hostco"))
                .with_payee(PayeeRule::Id("hostco".into())),
        )
        .with_mandate(
            SpendCategory::Domains,
            Mandate::new(4_000, 3_000, RailRef::provider_billing("registrar"))
                .with_payee(PayeeRule::Id("registrar".into())),
        )
        .with_mandate(
            SpendCategory::Advertising,
            Mandate::new(20_000, 50_000, RailRef::provider_billing("adsco"))
                .with_payee(PayeeRule::Id("adsco".into())),
        )
}

/// A wallet with its in-memory provider, rails, ledger, store, alarms and audit trail, on
/// a manual clock.
pub struct Fixture {
    pub clock: ManualClock,
    pub provider: InMemoryWalletProvider,
    pub scheme: InMemoryUseBasedScheme,
    pub hostco: InMemoryBillingAccount,
    pub registrar: InMemoryBillingAccount,
    pub adsco: InMemoryBillingAccount,
    pub ledger: Arc<InMemorySpendLedger>,
    pub store: InMemoryWalletStore,
    pub alarms: InMemoryAlarmHook,
    pub audit: InMemoryAuditRecorder,
    pub wallet: Wallet,
}

impl Fixture {
    pub fn new(policy: WalletPolicy, start: DateTime<Utc>) -> Self {
        Self::with(policy, start, |builder| builder)
    }

    pub fn with(
        policy: WalletPolicy,
        start: DateTime<Utc>,
        extra: impl FnOnce(WalletBuilder) -> WalletBuilder,
    ) -> Self {
        let clock = ManualClock::new(start);
        let shared: Arc<dyn adk_core::Clock> = Arc::new(clock.clone());
        let provider = InMemoryWalletProvider::stored_value("treasury", "USD", 1_000_000)
            .with_clock(shared.clone());
        let scheme = InMemoryUseBasedScheme::new();
        let account =
            |name: &str| InMemoryBillingAccount::new(name, "USD").with_clock(shared.clone());
        let (hostco, registrar, adsco) =
            (account("hostco"), account("registrar"), account("adsco"));
        let ledger = Arc::new(
            InMemorySpendLedger::new(policy.spend_limits(SITE).unwrap()).with_clock(shared.clone()),
        );
        let store = InMemoryWalletStore::new();
        let alarms = InMemoryAlarmHook::new();
        let audit = InMemoryAuditRecorder::new();
        let builder = Wallet::builder(SITE, policy, Arc::new(provider.clone()))
            .rail(Arc::new(UseBasedRail::new(Arc::new(scheme.clone()))))
            .rail(Arc::new(ProviderBillingRail::new(Arc::new(hostco.clone()))))
            .rail(Arc::new(ProviderBillingRail::new(Arc::new(registrar.clone()))))
            .rail(Arc::new(ProviderBillingRail::new(Arc::new(adsco.clone()))))
            .spend_ledger(ledger.clone() as Arc<dyn SpendLedger>)
            .store(Arc::new(store.clone()))
            .alarm_hook(Arc::new(alarms.clone()))
            .audit(AuditTrail::new(Arc::new(audit.clone())))
            .owner(SubjectRef::human(OWNER))
            .clock(shared);
        let wallet = extra(builder).build().expect("the wallet builds");
        Self {
            clock,
            provider,
            scheme,
            hostco,
            registrar,
            adsco,
            ledger,
            store,
            alarms,
            audit,
            wallet,
        }
    }

    pub fn advance_hours(&self, hours: u64) {
        self.clock.advance(std::time::Duration::from_secs(hours * 3_600));
    }

    pub fn audit_actions(&self) -> Vec<String> {
        self.audit.records().into_iter().map(|record| record.action).collect()
    }
}

pub fn finance() -> WalletCaller {
    WalletCaller::agent("urn:adk:agent:site-1/finance").with_charter_version(1)
}

pub fn token_challenge(cents: u64) -> PaymentChallenge {
    PaymentChallenge::new(MODELS_HOST, MODELS_PAYEE, cents, "USD")
}

pub fn owner_approves() -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human(OWNER))
}

pub fn owner_rejects() -> ApprovalDecision {
    ApprovalDecision::reject(SubjectRef::human(OWNER), "not this month")
}
