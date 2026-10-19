- **Approval service** (`adk-core`, `adk-session`, `adk-server`): `ApprovalService` and
  `StoreApprovalService` turn calls held for confirmation into approval requests with an
  `ApprovalPolicy` — approver selectors, quorum, self-approval refusal, escalation steps,
  and expiry — plus delegation windows, an inbox, compare-and-swap decisions
  (`approval.version_conflict` on a stale version), `tick` for expiry and escalation, and
  the approve, edit, reject, and respond actions. `RunConfig::approval_service` submits held
  calls through an `ApprovalRouter`; `RunConfig::durable_run_id` lets a resolution wake its
  `DurableRunStore` run, and `adk-server`'s `DurableWorker` sets it for every agent run it
  executes. `ApprovalStore`'s request and delegation methods (`take_approval`,
  `create_request`, `get_request`, `replace_request`, `list_requests`, `put_delegation`,
  `delegations_to`, `delete_delegation`) are implemented by the in-memory, SQLite,
  PostgreSQL, Redis, MongoDB, Neo4j, and Firestore stores and checked by
  `approval_store_conformance_tests!` and `approval_service_conformance_tests!`. The
  `approvals` feature of `adk-server` adds the `/api/approvals` endpoints,
  `spawn_approval_ticker`, and `WebhookApprovalNotifier`, which sends Standard Webhooks
  notifications in the background through `adk_core::webhook_delivery::WebhookDelivery` —
  retried with backoff and jitter, dead-lettered after the last attempt, and refused for
  loopback, private, and link-local destinations; `with_delivery` with
  `EgressPolicy::allow_private_networks()` reaches in-cluster receivers. See
  `docs/official_docs/security/approval-service.md`.
- **Approvals can edit arguments and carry a message** (`adk-core`): `ToolApproval` has
  `edited_args` and `message`, built with `ToolApproval::approve()` or `deny()` and the
  `with_edited_args`, `with_message`, `expires_at`, and `expires_in` setters. An approval
  with edited arguments executes the call with them after the `ToolPolicy` and tool screen
  check them again; a denial's message is reported to the model.
