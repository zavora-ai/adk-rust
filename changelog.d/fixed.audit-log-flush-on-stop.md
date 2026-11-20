- **A signed audit log verifies after a clean stop** (`adk-core`, `adk-auth`, `adk-harness`,
  `adk-kernel`, `adk-cli`): `AuditRecorder::flush` (default: nothing) seals the records written
  so far, and `AuditLog` implements it by signing a checkpoint over the records not yet signed.
  `Harness::close`, and `serve` and `up` of `adk-rust` and of a `HarnessLauncher` app once the
  server and kernel stop, flush the harness's recorder, so a short run's
  `adk-rust audit verify` reports `VALID` instead of `INVALID … [unanchored]`.
  `LedgerAuditTap` and `PseudonymousRecorder` forward the flush.
