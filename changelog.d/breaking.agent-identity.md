- **Agent identity fields** (`adk-core`, `adk-auth`, `adk-server`): struct literals need the
  new fields — `ToolConfirmationRequest::acting` (or build the request with
  `ToolConfirmationRequest::new`), `AuditEvent::agent_id` and `on_behalf_of`,
  `AuditFilter::agent_id`, and `RemoteA2aConfig` / `RemoteA2aV1Config::credential_broker` and
  `credential_scopes`. Holding an `Arc<dyn CredentialBroker>`, `RemoteA2aConfig`,
  `RemoteA2aV1Config`, `RemoteA2aAgent`, and `RemoteA2aAgentBuilder` are no longer
  `UnwindSafe` or `RefUnwindSafe`. `V1ClientError` gains a `Credential(AdkError)` variant
  (with `From<AdkError>`), so an exhaustive `match` on it needs the arm.
- **Context wrappers forward `acting_context()`** (`adk-core`): `ReadonlyContext` gains
  `acting_context()`, whose default reports the derived identity
  (`urn:adk:agent:{app}/{agent}` acting for the user as a human). A wrapper context that does
  not forward it to the context it wraps hides the configured principal from policy,
  approvals, spend, and audit.
- **`PostgresAuditSink` writes `agent_id` and `on_behalf_of` columns** (`adk-auth`, feature
  `postgres-audit`): call `migrate()` once on a database created by 2.3.0 to add them. Until
  then every insert fails, and a guarded tool whose audit write fails is refused.
