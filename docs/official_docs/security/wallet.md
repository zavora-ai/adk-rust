# Wallet

The `wallet` feature of `adk-payments` gives a site an owner-funded wallet it pays its own
costs from — model tokens, hosting, domains, advertising — under the owner's mandates
(spec 3.5 §7.14). Money is boxed: caps are checked before any money moves, a payment over a
limit waits for the owner, a spending burst pauses its category, and the site can never
open accounts, add payment methods, change payees or raise a cap.

> **Note:** the wallet is Experimental. It tracks spec 3.5 and may change without notice.

```toml
[dependencies]
adk-payments = { version = "3.0.0", features = ["wallet"] }
```

Add `ap2` for the AP2 rail: `features = ["wallet", "ap2"]`.

## Parts

| Part | Types | Role |
|------|-------|------|
| Policy | `WalletPolicy`, `Mandate`, `SpendCategory`, `PayeeRule`, `RailRef`, `AnomalyRule`, `Cadence` | The owner's rules: a mandate per category, the anomaly rule, the reconciliation cadence |
| Funding | `WalletProvider`, `CardIssuer`, `PrepaidCardProvider` | Where the owner's money sits: stored value with a payment provider, or a prepaid virtual card from an issuing platform |
| Rails | `PaymentRail`, `UseBasedRail`, `ProviderBillingRail`, `Ap2Rail` | How money reaches a payee |
| Payments | `Wallet`, `PaymentRequest`, `PaymentOutcome`, `WalletStore` | The checks, holds, owner decisions and journal |
| Oversight | `WalletAlarmHook`, `ReconciliationReport`, `WalletKpiSource` | Anomaly and reconciliation alarms, monthly reconciliation, profit and loss |
| Tools | `WalletToolset` | `wallet_pay` and `wallet_status` for agents |
| Commerce receipts | `ReceiptSigner`, `ReceiptVerifier`, `issue_receipt`, `verify_receipt` | The hook a site's delegated receipt key signs commerce receipts through |

Built-in in-memory implementations (`InMemoryWalletProvider`, `InMemoryCardIssuer`,
`InMemoryBillingAccount`, `InMemoryUseBasedScheme`, `InMemoryWalletStore`,
`InMemoryAlarmHook`) exist so tests are hermetic; none is a default over another provider or
rail. `wallet::conformance` checks any `WalletProvider`, `WalletStore` or `PaymentRail`.

## The policy

`WalletPolicy` follows spec 3.5 §7.14. Amounts are minor units of the wallet's currency.

| Field | Meaning | Default |
|-------|---------|---------|
| `currency` | ISO 4217 code of every amount | — |
| `period` | The window caps are summed over: `day`, `month` or `lifetime` | `month` |
| `categories` | A `Mandate` per `SpendCategory` (`hosting`, `models`, `advertising`, `domains`, `other`); a category without one cannot be spent in | empty |
| `anomaly` | The multiple of the trailing daily average that pauses a category | 3× over 14 days, after 7 days of history |
| `reconciliation` | How often the wallet reconciles against its providers | `monthly` |

A `Mandate` has `cap_per_period`, `max_per_payment`, `payees` and `rail`. A mandate with no
payee rules allows no payee; `PayeeRule::Any`, `PayeeRule::Id` and `PayeeRule::Domain`
(the domain and its subdomains) name them.

## What a payment goes through

`Wallet::pay` checks, in order, before any money moves:

| Check | Fails as |
|-------|----------|
| The category has a mandate | Refused, `wallet.no_mandate` |
| The wallet's currency and an amount above zero | Refused |
| A payee rule allows the payee | Refused, `wallet.payee_outside_mandate` |
| The mandate's rail is held and its data describes this payment | Refused |
| The category is not paused | Refused, `wallet.category_paused` |
| The amount is within `max_per_payment` | Held for the owner |
| The category stays within `cap_per_period` | Held for the owner |
| The anomaly rule | The category is paused, a P1 alarm raised, the payment refused |

It then reserves the amount in the spend ledger, debits the provider for a wallet-funded
rail, pays through the rail and commits the reservation. A failed rail voids the debit and
releases the reservation. Every refusal carries `awpError: payment_policy_rejected` in its
details for the AWP error envelope.

The cap is checked by the spend ledger: `WalletPolicy::spend_limits(wallet_id)` gives one
limit per category on the scope `wallet:{id}/{category}`, and a ledger built with them
refuses an over-cap reservation atomically, even across processes. Without a ledger the
wallet builds an `InMemorySpendLedger` with those limits. Amounts in the ledger are
millionths of the currency's major unit, so a USD wallet records micro-USD like model
calls do.

A payment key makes each payment exactly once: a repeated key returns the first outcome,
and the same key for another payment is refused with `wallet.key_reused`.

## Holds and owner decisions

A held payment waits in the journal (`Wallet::held`). Only a person decides it:
`Wallet::decide` takes an `ApprovalDecision` whose approver is a human — not an agent, not
a service — and, when the wallet names owners (`WalletBuilder::owner`), one of them. An
approved payment is paid beyond the mandate as the owner's act: it is recorded in the
category's owner-approved scope, counts toward the category's spend for the period, and
leaves the cap unchanged. `Wallet::resume_category` lifts an anomaly pause the same way.

Through the `wallet_pay` tool, a held payment is held as the tool call: the run's approval
sources (the approval store, a fingerprint approval or the confirmation handler) decide
it, the owner's card shows it, and the next run resumes the call and pays it.

## Rails

| Rail | Name | Funding | For |
|------|------|---------|-----|
| `UseBasedRail` over a `UseBasedScheme` | `use_based` | The wallet's provider | Metered APIs and model tokens answering `402 Payment Required`, x402-style |
| `ProviderBillingRail` over a `BillingAccount` | `provider_billing:{provider}` | The owner's account at the provider | Hosting, registrars and other services that bill the owner |
| `Ap2Rail` (`ap2`) over an `Ap2MerchantEndpoint` | `ap2` | The wallet's provider | Purchases under AP2 mandates; the owner's credentials provider (`Ap2UserAuthorizer`) authorises each payment mandate |

A metered call reads the challenges with the scheme, pays one through
`Wallet::pay_challenge`, and retries with the proof the settled payment carries:

```rust
use std::sync::Arc;

use adk_payments::wallet::{
    InMemoryUseBasedScheme, InMemoryWalletProvider, Mandate, PayeeRule, PaymentChallenge,
    RailRef, SpendCategory, UseBasedRail, Wallet, WalletCaller, WalletPolicy,
};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let policy = WalletPolicy::new("USD").with_mandate(
        SpendCategory::Models,
        Mandate::new(30_000, 500, RailRef::use_based())
            .with_payee(PayeeRule::Domain("models.example".into())),
    );
    let provider = Arc::new(InMemoryWalletProvider::stored_value("treasury", "USD", 50_000));
    let wallet = Wallet::builder("site-1", policy, provider)
        .rail(Arc::new(UseBasedRail::new(Arc::new(InMemoryUseBasedScheme::new()))))
        .build()?;

    let challenge = PaymentChallenge::new(
        "https://api.models.example/v1/generate",
        "acct-models",
        12,
        "USD",
    );
    let caller = WalletCaller::agent("urn:adk:agent:site-1/growth");
    let outcome = wallet.pay_challenge("call-1", SpendCategory::Models, &challenge, &caller).await?;
    let proof = outcome.record().use_based_proof().expect("a settled payment carries its proof");
    println!("retry with {}: {}", proof.header, proof.value);
    Ok(())
}
```

## Anomalies, reconciliation and profit and loss

- **Anomaly.** A payment that would bring a category's spend today to the rule's multiple
  of its trailing daily average pauses the category, raises a P1 `WalletAlarm` through the
  `WalletAlarmHook` and is refused. Owner-approved payments neither trigger nor count toward
  the average.
- **Reconciliation.** `Wallet::reconcile_due` reconciles the last full cadence window
  against the provider's statement and every rail that keeps its own, and reports
  payments missing from a statement, statement lines the wallet did not make, amount
  mismatches and debits a failed payment left behind. A mismatch raises a P2 alarm.
- **KPI source.** `WalletKpiSource` answers the KPI tracker's reads for the `spend` source:
  `spend`, `spend.{category}`, and, with `CommerceOutcomes`, `revenue`, `profit`,
  `cost_per_lead` and `cost_per_sale`, in major units. A wallet answers only for its own
  site.

## Audit records

With `WalletBuilder::audit`, the wallet records `wallet.payment` (allowed, refused,
settled, failed), `wallet.held` (held, approved, rejected), `wallet.anomaly` (paused,
resumed) and `wallet.reconciled`, each with the acting agent, its run, the category, payee,
amount, rail and, when the caller gives one, the charter version (spec 3.5 §9.3). Under
`AuditFailureMode::Block` a payment whose record cannot be written does not run.

## What the site cannot do

The site never opens accounts, adds payment methods, changes payees outside the mandate or
raises a cap (spec 3.5 A-I16):

- `Wallet::policy` returns the policy by shared reference, and nothing changes it after the
  wallet is built; a new policy is a new wallet the owner builds.
- `WalletProvider`, `CardIssuer`, `BillingAccount` and `PaymentRail` have no method to open
  an account, add a payment method, raise a limit or add funds.
- The agent's tools are `wallet_pay` and `wallet_status`; `wallet_pay` refuses arguments
  beyond its schema.
- Approving beyond a mandate and resuming a paused category take a person's decision.

## Validation

```bash
cargo test -p adk-payments --features wallet --test wallet_tests
cargo test -p adk-payments --features wallet --test wallet_s12_scenario_tests
cargo test -p adk-payments --features wallet --test wallet_property_tests
cargo test -p adk-payments --features wallet,ap2 --test wallet_s9_ap2_tests
cargo test -p adk-payments --features wallet --test wallet_live_tests -- --ignored
```

## Related

- [Payments and Commerce](payments.md) - Protocol adapters and payment policies
- [Spend Ledger](spend-ledger.md) - The ledger the wallet reserves in
- [Tool Effects](../tools/tool-effects.md) - Why `wallet_pay` is `NonIdempotent`
