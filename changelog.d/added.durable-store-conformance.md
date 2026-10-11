- **Conformance suites for durable stores** (`adk-core`): the `conformance` feature adds
  `spend_ledger_conformance_tests!`, `action_ledger_conformance_tests!`, and
  `approval_store_conformance_tests!`, which run the `SpendLedger`, `ActionLedger`, and
  `ApprovalStore` contracts — accounting, UTC period rollover, concurrency across two
  handles, and restart survival — against any backend through a small harness trait. The
  in-memory stores and every `adk-session` backend run the same suites.
- **`Clock`, `SystemClock`, and `ManualClock`** (`adk-core`): spend ledgers read the time
  from a clock set with `with_clock`, so tests cross day and month boundaries without
  waiting.
