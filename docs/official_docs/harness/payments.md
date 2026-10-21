# Harness Payments Pack

The payments pack lets a `HarnessAgent` buy something under a mandate: the `adk-payments`
checkout tools join the harness as tool class `Payment`, every call is put to a person for a
one-time approval, the money is metered in the run's spend ledger, and every attempt is
recorded as `harness.payment` with a link to the receipt in the transaction journal
(spec 3.1 §7.16, §5 H8). The pack writes no payment code: the commerce kernel, the
guardrails, the journal and the spend reservation are `adk-payments`'; the pack is classes
and wiring.

## Quick start

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["payments"] }
adk-payments = "3.0.0"
adk-auth = "3.0.0"
adk-devtools = "3.0.0"
adk-core = "3.0.0"
anyhow = "1"
```

```rust
use std::sync::Arc;

use adk_auth::{ScopeGuard, StaticScopeResolver};
use adk_core::Llm;
use adk_harness::{HarnessAgent, PaymentMandate, PaymentsPack};
use adk_payments::auth::ALL_PAYMENT_SCOPES;
use adk_payments::domain::Money;
use adk_payments::kernel::{MerchantCheckoutService, TransactionStore};

/// A harness that may buy at `registrar` under a 20.00 USD mandate.
fn shopping_harness<R>(model: Arc<dyn Llm>, registrar: Arc<R>) -> anyhow::Result<HarnessAgent>
where
    R: MerchantCheckoutService + TransactionStore + 'static,
{
    let mandate = PaymentMandate::new(
        "mandate-domain-1",
        "register adk-harness-demo.example",
        Money::new("USD", 2_000, 2),
    )
    .with_merchants(["domainmart"]);
    let pack = PaymentsPack::new(registrar.clone(), registrar, mandate)
        .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(ALL_PAYMENT_SCOPES.to_vec())));
    Ok(HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .pack_payments(pack)
        .build()?)
}
```

`HarnessInfo::packs` then lists `payments`, and the `<environment>` block of the prompt reads
`packs: payments`.

## What the pack does

| Concern | Mechanism |
|---|---|
| Tool class | Every name in `PAYMENT_TOOL_NAMES` (`payments_checkout_create`, `_update`, `_complete`, `_cancel`, `payments_status_lookup`, `payments_intervention_continue`, `payments_ap2_payment_submit`) is registered as `ToolKind::Payment`, declared or not; with the pack on, a tool of the same name attached with `HarnessAgentBuilder::tool` is refused at `build` |
| Modes | `Deny` in `read_only` and `plan`; `RequireApproval`, non-waivable, in `ask` and `auto` |
| Standing rules | Never apply: `Payment` is not rule-eligible, so neither an Allow nor a Deny rule changes the decision (I38) |
| Run-wide approvals | `RunConfig::tool_approvals` entries naming a payment tool are removed by `configure_run` |
| Confirmation policy | The payment tools are in the agent's `ToolConfirmationPolicy::PerTool` set, as defence in depth |
| Run budget | Each call is reserved against `RunBudget` (`max_tool_calls`) before it starts; a refused batch is answered `not run` |
| Guardrails | The mandate's limit is the hard stop of an `AmountThresholdGuardrail` in the mandate's currency; its merchants, when set, form a `MerchantAllowlistGuardrail`; `PaymentsPack::with_payment_policies` adds more |
| Spend | `SpendLimitGuardrail` reserves a completion against the run's spend ledger under `SpendKey::org(app).with_agent(agent).with_vendor(merchant)`, commits it once the checkout completes and releases it if it fails |
| Scopes | Every tool runs inside an `adk-auth` `ScopeGuard` (`PaymentsPack::with_scope_guard`; by default the scopes of the run's request context) |
| Audit | Each attempt writes `harness.payment`, linking the journal receipt |
| Prompt | Every coordinator request ends with a reminder naming the mandate and the remaining budget |
| Team | Members never get the payment tools; a purchase is the coordinator's to put to the person |

## The mandate

| Field | Meaning |
|---|---|
| `id` | Cited by every `harness.payment` record as `mandateId` |
| `description` | What the mandate authorizes, in the user's words; named in the prompt |
| `limit` | The most one transaction may total; a larger total, or one in another currency, is refused before the merchant is called |
| `merchants` | Merchant ids the mandate allows; empty allows any merchant |

The mandate's guardrails deny and never escalate: the harness already holds each payment for
one approval before the tool runs, and a review threshold inside the tool would ask the person
a second time.

## Approvals

A purchase is two calls, and each is held:

1. `payments_checkout_create` is held. Nothing reaches the merchant. Without a prompter the
   call parks in the approval store; `Harness::pending_approvals` lists it and `Harness::decide`
   answers it.
2. The next turn runs the approved call once; the merchant opens the checkout and the journal
   records it.
3. `payments_checkout_complete` is held the same way. Once approved it runs once: the spend is
   reserved, the merchant completes the order, the reservation is committed and the receipt is
   journaled.

An approval is spent by the call it authorizes, so a second purchase asks again.

## Capping spend

Model calls reserve under their provider's vendor key and payments under the merchant's, so a
cap on the merchant's key caps payments only:

```rust
use std::sync::Arc;

use adk_core::{InMemorySpendLedger, SpendKey, SpendLimits, SpendPeriod};

/// At most 50.00 USD a month at `domainmart` for app `my-app`; pass it to
/// `HarnessAgentBuilder::spend_ledger`.
fn payments_ledger() -> Arc<InMemorySpendLedger> {
    let monthly = SpendKey::org("my-app").with_vendor("domainmart").per(SpendPeriod::Month);
    Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(monthly, 50_000_000)))
}
```

## The `harness.payment` record

Kind `Other("harness")`, written through the run's audit trail after each payment tool call
returns (spec 3.1 §9.3).

| Field (`ext`) | Value |
|---|---|
| `mandateId` | The mandate's `id` |
| `tool` | The payment tool |
| `functionCallId` | The call |
| `amount`, `amountMinor`, `currency` | The transaction total, from the masked summary, the call's cart or the journal |
| `journalReceiptRef` | `transactionId`, `stateKey` (the journal's session-state key, `app:payments:tx:…`), `orderId`, `receiptId`, `receiptState`, `state` |
| `decision` | `executed`, `refused` (a payment policy denied it), `pending` or `denied` (a guardrail's escalation), `failed` |
| `error` | The kernel's error code, when the call failed |
| `sessionId` | The session |

A call the harness denies or holds never runs the tool, so it writes no `harness.payment`; the
governed `ToolCall` record covers it.

## The prompt reminder

Appended to every coordinator request, after the cached prefix:

```text
<system-reminder>
Payments: mandate mandate-domain-1 (register adk-harness-demo.example) allows at most 20.00 USD
per transaction at merchants domainmart; paid so far at these merchants: 14.99 USD; run budget
remaining: 8 tool calls. Every payment needs the user's approval, which the harness asks for
when you call the tool.
</system-reminder>
```

"Paid so far" is what the run's spend ledger has committed at the mandate's merchants; the run
budget names each `RunBudget` limit the run sets.

## In the CLI

`HarnessAgentBuilder::pack(Pack::Payments)`, which `adk-rust --pack payments` calls, refuses at
`build`: the pack needs a merchant checkout service, a transaction store and a mandate, which
only the library API takes (`HarnessAgentBuilder::pack_payments`).

## Known limits

- The `adk-payments` checkout, status and intervention tools declare no parameter schema, so a
  provider is sent an empty object schema and a live model cannot fill their arguments. Scripted
  models and direct callers are unaffected.
- `payments_ap2_payment_submit` needs an `Ap2Adapter`; `PaymentsPack` builds the checkout
  tools only.

## Scenarios

`examples/harness_agent`:

| Scenario | What passes |
|---|---|
| `payment_requires_approval` (H8) | The checkout and the completion are each held before the registrar sees them; after approval each runs once; the ledger shows 14.99 USD at the registrar; the receipt is in the journal and `harness.payment` links it; a 25.00 USD domain is refused by the 20.00 USD mandate; in Plan the purchase is denied. The 3.0 `CodingAgent` with the same tools pays without asking |
| `live_register_domain` | The same purchase by a live model, approved at the prompt |
