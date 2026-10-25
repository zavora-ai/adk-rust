- **Site wallet** (`adk-payments`, `wallet`): an owner-funded wallet a site pays its own
  costs from (spec 3.5 §7.14). `WalletPolicy` gives each `SpendCategory` a `Mandate` (cap
  per period, limit per payment, payees, rail); `Wallet::pay` checks the mandate and the
  spend ledger's category cap before any money moves, holds a payment over a limit or the
  cap for a person's `ApprovalDecision`, and pauses a category with a P1 alarm when spend
  reaches a multiple of its trailing daily average. Funding through `WalletProvider`
  (stored value) or `PrepaidCardProvider` over a `CardIssuer`; rails `UseBasedRail`
  (x402-style `402` challenges for metered APIs and model tokens), `ProviderBillingRail`
  and, with `ap2`, `Ap2Rail` for purchases under AP2 mandates. `Wallet::reconcile_due`
  reconciles against provider statements, `WalletKpiSource` reports spend, profit and cost
  per lead and per sale, `WalletToolset` exposes `wallet_pay` and `wallet_status`, and
  `ReceiptSigner`/`ReceiptVerifier` with `issue_receipt` are the hook commerce receipts are
  signed through. No API opens accounts, adds payment methods, changes payees or raises a
  cap (A-I16). Experimental.
