//! Properties of the wallet (spec 3.5 §7.14, A-I16): caps and per-payment limits hold for
//! any sequence of payments, nothing outside a mandate settles, money and ledger agree,
//! reconciliation of an untouched month is clean, and policies round-trip.

#![cfg(feature = "wallet")]

#[path = "support/wallet.rs"]
mod wallet_support;

use std::collections::BTreeMap;

use adk_core::{SpendKey, SpendLedger, SpendPeriod};
use adk_payments::wallet::{
    AnomalyRule, Cadence, Mandate, Payee, PayeeRule, PaymentOutcome, PaymentQuery, PaymentRequest,
    PaymentState, RailRef, SpendCategory, WalletPolicy, category_scope, owner_approved_scope,
};
use proptest::prelude::*;
use wallet_support::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// One payment the site asks for: a category, a payee (allowed or not), an amount, and
/// whether the owner approves it if it is held.
#[derive(Debug, Clone)]
struct Ask {
    category: SpendCategory,
    payee: &'static str,
    amount: u64,
    owner_approves: bool,
}

fn ask() -> impl Strategy<Value = Ask> {
    (
        prop_oneof![
            Just(SpendCategory::Hosting),
            Just(SpendCategory::Advertising),
            Just(SpendCategory::Domains)
        ],
        prop_oneof![Just("own"), Just("stranger")],
        1u64..30_000,
        any::<bool>(),
    )
        .prop_map(|(category, payee, amount, owner_approves)| Ask {
            category,
            payee,
            amount,
            owner_approves,
        })
}

fn payee_for(category: SpendCategory, which: &str) -> Payee {
    let own = match category {
        SpendCategory::Hosting => "hostco",
        SpendCategory::Advertising => "adsco",
        _ => "registrar",
    };
    Payee::new(if which == "own" { own } else { "stranger" })
}

fn quiet_policy() -> WalletPolicy {
    s12_policy().with_anomaly(AnomalyRule { min_history_days: 100_000, ..AnomalyRule::default() })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn caps_limits_and_mandates_hold_for_any_sequence(asks in prop::collection::vec(ask(), 1..25)) {
        runtime().block_on(async {
            let f = Fixture::new(quiet_policy(), at(2026, 10, 2, 9));
            for (index, ask) in asks.iter().enumerate() {
                let request = PaymentRequest::new(
                    format!("k-{index}"),
                    ask.category,
                    payee_for(ask.category, ask.payee),
                    ask.amount,
                    "USD",
                );
                match f.wallet.pay(request, &finance()).await {
                    Ok(PaymentOutcome::Held(record)) if ask.owner_approves => {
                        let decided = f.wallet.decide(&record.id, &owner_approves()).await.unwrap();
                        assert_eq!(decided.state, PaymentState::Settled);
                    }
                    Ok(_) => {}
                    Err(error) => assert!(error.code.starts_with("wallet."), "{error}"),
                }
            }

            let policy = f.wallet.policy().clone();
            let payments = f.wallet.payments(PaymentQuery::new(SITE)).await.unwrap();
            for (category, mandate) in &policy.categories {
                let settled: Vec<_> = payments
                    .iter()
                    .filter(|p| p.category == *category && p.state == PaymentState::Settled)
                    .collect();
                let autonomous: u64 =
                    settled.iter().filter(|p| !p.owner_approved()).map(|p| p.amount).sum();
                let approved: u64 =
                    settled.iter().filter(|p| p.owner_approved()).map(|p| p.amount).sum();
                assert!(autonomous <= mandate.cap_per_period, "autonomous spend stays under the cap");
                for payment in settled.iter().filter(|p| !p.owner_approved()) {
                    assert!(payment.amount <= mandate.max_per_payment, "no autonomous payment is over the limit");
                }
                for payment in &settled {
                    assert!(mandate.allows_payee(&payment.payee), "nothing outside the mandate settles");
                }
                let own = f.ledger.spent(&SpendKey::org(category_scope(SITE, *category)), SpendPeriod::Month).await.unwrap();
                let owner = f.ledger.spent(&SpendKey::org(owner_approved_scope(SITE, *category)), SpendPeriod::Month).await.unwrap();
                assert_eq!(own, autonomous * 10_000, "the ledger records the site's own spend");
                assert_eq!(owner, approved * 10_000, "and the owner's approvals apart");
            }
            assert_eq!(f.wallet.policy(), &quiet_policy(), "no sequence changes the policy");
        });
    }

    #[test]
    fn an_untouched_period_always_reconciles_clean(amounts in prop::collection::vec((0u32..3, 1u64..400), 1..30)) {
        runtime().block_on(async {
            let f = Fixture::new(quiet_policy(), at(2026, 9, 1, 9));
            for (index, (kind, amount)) in amounts.iter().enumerate() {
                let key = format!("r-{index}");
                match kind {
                    0 => { f.wallet.pay_challenge(key, SpendCategory::Models, &token_challenge(*amount), &finance()).await.unwrap(); }
                    1 => { f.wallet.pay(PaymentRequest::new(key, SpendCategory::Hosting, Payee::new("hostco"), *amount, "USD"), &finance()).await.unwrap(); }
                    _ => { f.wallet.pay(PaymentRequest::new(key, SpendCategory::Domains, Payee::new("registrar"), *amount, "USD"), &finance()).await.unwrap(); }
                }
                f.advance_hours(7);
            }
            let report = f.wallet.reconcile(at(2026, 9, 1, 0), at(2026, 11, 1, 0)).await.unwrap();
            let settled = f.wallet.payments(PaymentQuery::new(SITE).state(PaymentState::Settled)).await.unwrap();
            assert!(report.is_clean(), "{:?}", report.discrepancies);
            assert_eq!(report.matched, settled.len());
            assert_eq!(report.statement_total, i128::from(report.wallet_total));
        });
    }

    #[test]
    fn a_domain_rule_allows_its_subdomains_and_nothing_else(
        label in "[a-z]{1,10}",
        domain in "[a-z]{1,10}\\.[a-z]{2,4}",
        prefix in "[a-z]{1,5}",
    ) {
        let rule = PayeeRule::Domain(domain.clone());
        let subdomain = format!("{label}.{domain}");
        let lookalike = format!("{prefix}{domain}");
        prop_assert!(rule.allows(&Payee::new("x").with_domain(domain.clone())));
        prop_assert!(rule.allows(&Payee::new("x").with_domain(subdomain)));
        prop_assert!(!rule.allows(&Payee::new("x").with_domain(lookalike)));
        prop_assert!(!rule.allows(&Payee::new("x")));
    }

    #[test]
    fn policies_round_trip_through_json(
        mandates in prop::collection::btree_map(
            prop_oneof![
                Just(SpendCategory::Hosting), Just(SpendCategory::Models), Just(SpendCategory::Advertising),
                Just(SpendCategory::Domains), Just(SpendCategory::Other)
            ],
            (any::<u32>(), any::<u32>(), prop::collection::vec(prop_oneof![
                Just(PayeeRule::Any),
                "[a-z]{1,8}".prop_map(PayeeRule::Id),
                "[a-z]{1,8}\\.com".prop_map(PayeeRule::Domain),
            ], 0..4), prop_oneof![Just(RailRef::ap2()), Just(RailRef::use_based()), "[a-z]{1,8}".prop_map(|p| RailRef::provider_billing(&p))]),
            0..5,
        ),
        multiple in (5u32..400).prop_map(|quarters| f64::from(quarters) / 4.0),
        trailing in 1u32..60,
        cadence in prop_oneof![Just(Cadence::Daily), Just(Cadence::Weekly), Just(Cadence::Monthly), Just(Cadence::Quarterly)],
    ) {
        let categories: BTreeMap<SpendCategory, Mandate> = mandates
            .into_iter()
            .map(|(category, (cap, max, payees, rail))| {
                (category, Mandate { cap_per_period: u64::from(cap), max_per_payment: u64::from(max), payees, rail })
            })
            .collect();
        let policy = WalletPolicy {
            categories,
            anomaly: AnomalyRule { multiple, trailing_days: trailing, min_history_days: trailing, min_amount: 0 },
            reconciliation: cadence,
            ..WalletPolicy::new("KES")
        };
        prop_assert!(policy.validate().is_ok());
        let json = serde_json::to_value(&policy).unwrap();
        let back: WalletPolicy = serde_json::from_value(json).unwrap();
        prop_assert_eq!(back, policy);
    }
}
