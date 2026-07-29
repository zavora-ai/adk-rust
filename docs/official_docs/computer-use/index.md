# Governed Desktop Automation

`adk-computer-use` is a governance layer over the `computer-use-mcp` desktop-automation
server. It performs no actuation itself: it orders observation, approval, mutation, and
verification as a deterministic graph, and checks what the external runtime returns.

## Response binding

The external runtime is authoritative, but its responses are checked locally before entering
graph state. Typed deserialization proves shape, not provenance — `ControlLease`,
`TargetReservation`, and `ExecutionReceipt` have no invariant-enforcing constructor, so a
well-formed object belonging to a different session parses cleanly.

Each response is bound back to the request that produced it:

| Response | Checked against the envelope |
|----------|------------------------------|
| `ControlLease` | `session_id`, `principal_id`, `agent_id`, `execution_mode`, active state, **unexpired** `expires_at`, **remaining** budget (`actions_used < action_budget`), and target within `boundaries` |
| `TargetReservation` | `session_id`, `principal_id`, `agent_id`, `execution_group_id` |
| `ExecutionReceipt` | `session_id`, `action_id`, and `action_digest` against the envelope's `args_digest` |

A mismatch produces `ComputerUseError::IdentityMismatch` naming the field, and the response is
rejected rather than stored.

```rust,ignore
use adk_computer_use::runtime::binding::validate_lease;

// Refuses a lease that is well-formed but belongs to another session.
validate_lease(&lease, &envelope)?;
```

> **Note:** the digest is the strongest binding available. `args_digest` is what approval was
> granted against, so a receipt carrying a different digest describes different work even when
> every identifier matches.

An optional field that comes back absent is treated as under-specification, not contradiction;
a field that is present and different is a mismatch.

## Verification is not commitment

`ComputerUseRuntime::verify` returns a `VerificationOutcome`, not a boolean:

| Outcome | Meaning | `is_verified()` | `is_committed()` | `status()` |
|---------|---------|-----------------|------------------|------------|
| `Verified` | The declared postcondition was observed to hold | `true` | `true` | `completed` |
| `CommittedUnverified` | The runtime performed the action; no postcondition evidence | `false` | `true` | `committed_unverified` |
| `Failed` | Not committed, or the evidence contradicts the postcondition | `false` | `false` | `verification_failed` |

The graph's `verify` node writes `verified`, `committed`, and a `result.verificationDetail`
explaining anything short of `Verified`.

> **Important:** a committed receipt is an acknowledgement that the action was accepted and
> performed. It is not evidence that the intended effect occurred. `verify` previously returned
> `receipt.status == Committed`, which reported a committed-but-ineffective action as
> completed — from a node labelled "verify".

### What counts as evidence

For a postcondition declaring a digest (`valueDigest`, `contentDigest`), verification requires
a `verification.observedDigest` on the receipt result that matches it. For a postcondition
declaring only existence, an explicit `verification.satisfied: true` is required.

| Receipt evidence | Outcome |
|------------------|---------|
| `verification.satisfied: false` | `Failed` — an explicit negative observation |
| `observedDigest` matches the expected digest | `Verified` |
| `observedDigest` differs | `Failed` |
| No `verification` object | `CommittedUnverified` |
| `verification` present, no `observedDigest`, digest expected | `CommittedUnverified` |

Absence of evidence is never treated as evidence of success. If `computer-use-mcp` verifies
before issuing a receipt, that is its contract; this adapter does not assume it.

### Lease limits

A lease is refused unless all of these hold:

| Check | Rejected when |
|-------|---------------|
| Expiry | `expires_at` is in the past, or cannot be parsed as RFC 3339 |
| Remaining budget | `actions_used >= action_budget` |
| Target boundary | the envelope's `target.app_id` is absent from a non-empty `boundaries.app_ids` |
| Window boundary | the envelope's `target.window_id` is absent from a non-empty `boundaries.window_ids` |

Empty boundaries mean "not scoped" and authorize any target — absence of a restriction is not a
restriction to nothing.

> **Important:** the first version of this validator checked `action_budget == 0`, which is the
> *total* budget, so a lease with `action_budget: 1, actions_used: 1` passed while authorizing
> nothing. It also never read `expires_at` or `boundaries`, so an expired lease and a lease scoped
> to a different application were both accepted. An unparseable expiry is rejected rather than
> ignored: a lease whose validity cannot be established is not valid.
