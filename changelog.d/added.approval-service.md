- **Approval service** (`adk-core`, `adk-session`, `adk-server`): `ApprovalService` and
  `StoreApprovalService` turn calls held for confirmation into approval requests with an
  `ApprovalPolicy` — approver selectors, quorum, self-approval refusal, escalation steps,
  and expiry — plus delegation windows, an inbox, compare-and-swap decisions
  (`approval.version_conflict` on a stale version), `tick` for expiry and escalation, and
  the approve, edit, reject, and respond actions. `RunConfig::approval_service` submits held
  calls through an `ApprovalRouter`; `RunConfig::durable_run_id` lets a resolution wake its
  `DurableRunStore` run, and `adk-server`'s `DurableWorker` sets it for every agent run it
  executes. `ApprovalStore` gains `take_approval`, `create_request`,
  `get_request`, `replace_request`, `list_requests`, `put_delegation`, `delegations_to`, and
  `delete_delegation`, implemented by the in-memory, SQLite, PostgreSQL, Redis, MongoDB,
  Neo4j, and Firestore stores and checked by `approval_store_conformance_tests!` and the new
  `approval_service_conformance_tests!`. The `approvals` feature of `adk-server` adds the
  `/api/approvals` endpoints, `spawn_approval_ticker`, and `WebhookApprovalNotifier`
  (Standard Webhooks signatures). See `docs/official_docs/security/approval-service.md`.
