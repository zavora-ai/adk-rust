- **The payments pack in `adk-harness`** (feature `payments`, spec 3.1 §7.16, §5 H8):
  `HarnessAgentBuilder::pack_payments(PaymentsPack)` adds the `adk-payments` checkout tools under
  a `PaymentMandate` as tool class `Payment`.
  - A payment is denied in `read_only` and `plan` and held for a one-time approval in `ask` and
    `auto`; no standing rule or `tool_approvals` entry covers it, and team members never get the
    tools (§9.2 I38).
  - Every call runs under the run's `RunBudget`, the mandate's guardrails (its limit as a hard
    stop in its currency, its merchants as an allowlist) and the run's spend ledger, keyed by
    merchant.
  - Each attempt writes `harness.payment` with `mandateId`, amount, currency, decision and a
    `journalReceiptRef` into the transaction journal; every coordinator request names the mandate,
    what has been paid at its merchants and the run budget left; `HarnessInfo::packs` and the
    `<environment>` block list `payments`.
  - `Pack::Payments` alone, and `adk-rust --pack payments`, now refuse with an actionable message
    instead of `not yet implemented`. `examples/harness_agent` gains `payment_requires_approval`
    and the live `live_register_domain`.
