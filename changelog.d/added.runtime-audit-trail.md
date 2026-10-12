- **The runtime writes audit records** (`adk-core`, `adk-runner`, `adk-agent`, `adk-auth`,
  `adk-payments`, `adk-server`): `Runner::builder().audit_log(recorder)` (or
  `RunConfig::audit`) records every governed tool decision — allowed, denied, held for
  approval — with its `policyRef`, every approval decision and its source, kill-switch
  freezes and unfreezes, payment decisions and outcomes from the ACP and AP2 payment tools,
  and every call answered `outcome_unknown`. Each record names the acting agent and the
  subject it acts for and carries an argument digest, never arguments. Under
  `AuditFailureMode::Block` (the default) an action whose record cannot be written does not
  happen (`audit.record_failed`). New: `AuditRecorder`, `AuditTrail`,
  `InMemoryAuditRecorder`, `AuditActor::for_invocation`, `GovernanceControl::attach_audit`,
  `freeze_recorded`, `unfreeze_recorded` (replicated on a mirrored control),
  `StoreApprovalService::with_audit` (each approver decision, refused when unrecordable),
  `ToolPolicy::policy_ref`, `DeclarativePolicyBuilder::name`, and
  `PaymentCaller::with_audit`; `signed_audit::AuditLog` implements `AuditRecorder`. `AuditFailureMode` moves to `adk_core::audit_log` and is
  re-exported from `adk-auth`.
- **Audit log stores on every backend** (`adk-session`): `RedisAuditLogStore`,
  `MongoAuditLogStore`, `Neo4jAuditLogStore`, and `FirestoreAuditLogStore` pass
  `audit_log_store_conformance_tests!`. Each refuses an append that does not start at the
  log's size, so two writers on one `log_id` cannot fork it: a Lua script on Redis, a
  conditional claim on MongoDB, a write lock on Neo4j, and a transaction on Firestore.
