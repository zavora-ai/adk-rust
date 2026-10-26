- **Wallet decisions and in-flight payments race-free** (`adk-payments`): a held payment
  is decided through the new `WalletStore::update_if` compare-and-set, which the in-memory,
  SQLite, PostgreSQL and Redis stores implement atomically, so of two decisions racing on
  one payment exactly one proceeds and the spend ledger records it once; a settled payment
  leaves the in-flight total in the same step that commits it, so concurrent in-cap
  payments are no longer held for spend counted twice. The wallet store conformance suite
  checks concurrent decisions.
