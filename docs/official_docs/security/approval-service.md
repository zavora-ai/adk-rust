# Approval Service

The approval service turns calls held for confirmation into an inbox of approval requests
with a policy: who may approve, how many distinct approvers a call needs, whether the
requester may approve their own call, when the request escalates, and when it expires. It
runs over any `ApprovalStore`, so requests are as durable as the store — in memory, SQLite,
PostgreSQL, Redis, MongoDB, Neo4j, or Firestore.

| Capability | Behaviour |
|------------|-----------|
| **Inbox** | Lists the requests an approver may decide now — by name, by group, or through a live delegation — oldest first, paged by cursor |
| **Quorum** | A request resolves once `quorum` distinct approvers approve; a delegate's approval counts for the delegator |
| **Self-approval** | Refused by default for the requester, the acting agent, and anyone acting through their delegation |
| **Actions** | Approve, edit (approve with substituted arguments), reject, respond (answer the model instead of running the call) |
| **Concurrency** | Every change is a compare-and-swap on the request's `version`; of two approvers deciding one version, one wins and the other receives `409 approval.version_conflict` |
| **Expiry** | `tick` denies requests past `expires_at`, once |
| **Escalation** | `tick` fires each escalation step once its delay has passed, adding its approvers and notifying |
| **Delegation** | A subject lets another decide in their place within a time window and an application or tool scope |
| **Notification** | `ApprovalNotifier` hears submission, escalation, decisions, resolution, expiry, and cancellation; `adk-server` ships a signed webhook notifier |
| **Resume** | A resolution is written to the store under the call's fingerprint, wakes the durable run waiting for it, and is otherwise taken by the next run, which resumes the held call |

## How a Held Call Becomes a Request

1. The governed path holds a call that needs confirmation and records it in the run's
   `ApprovalStore`.
2. With `RunConfig::approval_service` set, it also offers the call to the service's
   `ApprovalRouter`, which returns the `ApprovalPolicy` (or `None` to leave the call in the
   store only). The request ID is derived from the session, the call's fingerprint, and the
   time it was first held, so a call held again before its decision maps to the same request.
3. Approvers decide through `ApprovalService::decide` or the server endpoints.
4. The decision that resolves the request is written to the store: an approval (with any
   edited arguments) or a denial carrying the reason. The request's `run_id` is woken with
   `DurableRunStore::wake` when the service has a run store
   (`StoreApprovalService::with_run_store`). `adk-server`'s `DurableWorker` sets
   `RunConfig::durable_run_id` to the claimed run's ID, so a run it parks on a held call is
   the run its approval wakes.
5. The next run resumes the held call under its original call ID, and the governed path takes
   the decision. An edited call passes the `ToolPolicy` and the agent's tool screen again before
   it executes; a policy denial of the edit refuses the call.

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApproverSelector, RunConfig,
    StoreApprovalService,
};
use adk_session::SqliteApprovalStore;

async fn configure() -> adk_core::Result<RunConfig> {
    let store = SqliteApprovalStore::new("sqlite://approvals.db?mode=rwc").await?;
    store.migrate().await?;

    // Large transfers need two people from finance and escalate to the CFO office.
    let router = |call: &ApprovalCall| {
        let amount = call.request.args["amount"].as_f64().unwrap_or(0.0);
        let finance = ApprovalPolicy::new([ApproverSelector::group("finance")])
            .expires_after(Duration::from_secs(4 * 3600));
        Some(if amount > 10_000.0 {
            ApprovalRoute::new(
                finance
                    .with_quorum(2)
                    .escalate_after(Duration::from_secs(1800), [ApproverSelector::group("cfo-office")]),
            )
            .with_risk(ApprovalRisk::High)
        } else {
            ApprovalRoute::new(finance)
        })
    };
    let service = StoreApprovalService::new(Arc::new(store)).with_router(router);
    Ok(RunConfig::builder().approval_service(Arc::new(service)).build())
}
```

`RunConfigBuilder::approval_service` also installs the service's store as the run's
`approval_store`, so decisions reach the governed path.

## Policy

| Field | Default | Meaning |
|-------|---------|---------|
| `approvers` | — | `ApproverSelector::Subject(SubjectRef)` or `ApproverSelector::Group(name)` |
| `quorum` | 1 | Distinct approvals needed |
| `forbid_self_approval` | `true` | Refuse the requester and the acting agent; `allow_self_approval()` clears it |
| `escalation` | none | Steps `{after, to}`, ordered by `after` |
| `on_expiry` | `Deny` | What expiry does |
| `expires_after` | none | Request lifetime when the request carries no `expires_at` |

The requester is the acting context's `on_behalf_of` subject, or the session's user when the
call carries no acting context. Group membership is asserted by the caller deciding
(`ApprovalDecision::approver_groups`); `adk-server` takes it from the authenticated request.

## Decisions

| Action | Requires | Effect |
|--------|----------|--------|
| Approve | — | Counts toward the quorum |
| Edit | `edited_args` | Approves the substituted arguments; approvals of earlier arguments stop counting |
| Reject | optional reason | Resolves at once; the call is refused with the reason |
| Respond | response text | Resolves at once; the call does not run and the model receives the response |

| Error code | HTTP | When |
|------------|------|------|
| `approval.version_conflict` | 409 | The request changed since the version read; re-read and decide again |
| `approval.already_resolved` | 409 | The request is no longer pending |
| `approval.expired` | 409 | The request is past its expiry |
| `approval.already_decided` | 409 | The approver's authority already counts toward the quorum |
| `approval.not_an_approver` | 403 | Not eligible directly or by delegation |
| `approval.delegation_inactive` | 403 | The only matching delegation is outside its window |
| `approval.self_approval` | 403 | Self-approval is forbidden |
| `approval.action_not_allowed` | 400 | The request does not allow the action, or an edit or response lacks its content |

## Expiry and Escalation

`ApprovalService::tick(now)` expires overdue requests and fires due escalation steps. The
service reads `now` from its `Clock` everywhere else, so tests drive it with `ManualClock`.
Run it periodically — `adk_server::approvals::spawn_approval_ticker` does — on any number of
replicas: each expiry and each escalation step is a compare-and-swap, so it happens once.

`tick` also applies resolutions whose application an earlier failure interrupted, after
`APPROVAL_REAPPLY_GRACE`, and only while the call is still pending in the store, so a decision
is never written twice.

## Delegation

```rust
use adk_core::principal::SubjectRef;
use adk_core::{ApprovalDelegation, ApprovalService, DelegationScope, DelegationWindow};
use chrono::{Duration, Utc};

async fn cover(service: &dyn ApprovalService) -> adk_core::Result<()> {
    let now = Utc::now();
    let delegation = ApprovalDelegation::new(
        SubjectRef::human("ana@example.com"),
        SubjectRef::human("bo@example.com"),
        DelegationWindow::new(now, now + Duration::days(7)),
    )
    .with_from_groups(["finance"])
    .with_scope(DelegationScope { app_name: Some("payments".into()), tool_names: vec![] });
    service.delegate(delegation).await?;
    Ok(())
}
```

A decision through a delegation records `via_delegation` and counts for the delegator.
Outside the window the delegate is refused with `approval.delegation_inactive`.

## Server Endpoints

Enable the `approvals` feature of `adk-server` and mount the service:

```rust,ignore
use std::sync::Arc;
use std::time::Duration;

let app = ServerBuilder::new(config) // with a RequestContextExtractor
    .with_approval_service(service.clone())
    .build();
let ticker = adk_server::approvals::spawn_approval_ticker(service, Duration::from_secs(30));
```

| Route | Effect |
|-------|--------|
| `GET /api/approvals` | The caller's inbox (`state`, `group`, `app`, `cursor`, `limit`) |
| `GET /api/approvals/{id}` | One request the caller may decide or asked for |
| `POST /api/approvals/{id}/decide` | `{"expectedVersion", "action", "editedArgs", "reason", "via"}` |
| `POST /api/approvals/{id}/cancel` | `{"reason"}`; the requester or an approver |
| `POST /api/approvals/delegations` | Delegates the caller's authority: `{"to", "notBefore", "notAfter", "scope"}` |
| `DELETE /api/approvals/delegations/{id}?toKind=&toId=` | Revokes a delegation the caller granted or holds |

Every route requires an authenticated caller and answers `401` otherwise. The caller is
`human(user_id)` in the groups named by its scopes; `ApprovalEndpoints::with_identity` changes
the mapping. Responses omit the call's fingerprint, which carries the unredacted arguments;
approvers see `argsRedacted`.

## Webhook Notifications

`WebhookApprovalNotifier` posts each `ApprovalNotification` as JSON in the
[Standard Webhooks](https://www.standardwebhooks.com/) format: `webhook-id` (the
notification ID, `{request id}:{version}`, stable across retries), `webhook-timestamp`, and
`webhook-signature` (`v1,` and the base64 HMAC-SHA256 of `{id}.{timestamp}.{body}`).

```rust,ignore
let notifier = WebhookApprovalNotifier::new("https://hooks.example.com/approvals", "whsec_...")?;
let service = StoreApprovalService::new(store).with_notifier(Arc::new(notifier));
```

Notification is best effort: a failed delivery is logged and the decision stands, because the
store holds the request's state.

## Backends

The store methods behind the service — `create_request`, `get_request`, `replace_request`,
`list_requests`, `put_delegation`, `delegations_to`, `delete_delegation`, and
`take_approval` — are implemented by every backend and checked by the
`approval_store_conformance_tests!` and `approval_service_conformance_tests!` suites. See
[Durable Stores](durable-stores.md) for the primitive each backend uses.

## Related

- [Tool Authorization](tool-authorization.md) — the governed path and held calls
- [Durable Stores](durable-stores.md) — backends and the conformance suites
- [Durable Runtime](../core/durable-runtime.md) — `DurableRunStore::wake`
