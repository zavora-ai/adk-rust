# Fork, Rewind, and Edit History

A session's history is the list of events it holds. `SessionService` forks a session at an
event, rewinds it to an event, and edits its events, on every shipped backend. These are the
basis for exploring alternatives from one point of a conversation, retrying a turn, and removing
content from a transcript.

| Operation | Method | Effect |
|-----------|--------|--------|
| Fork | `fork(ForkRequest)` | Creates a session of the same app and user holding the source's events up to an event, with the state the source had there |
| Rewind | `rewind(&identity, event_id)`, `rewind_steps(&identity, n)` | Removes the events after an event, or the last `n` events |
| Edit | `rewrite_history(RewriteHistoryRequest)` with `HistoryRewrite::Edit` | Replaces events in place and removes events |
| Read | `history(&identity)` | Returns the events, the session's own state, the state it was created with, and a version |
| Import | `import_session(ImportSessionRequest)` | Creates a session that already holds events, without applying their state deltas to the shared tiers |

## State

Every operation rebuilds the session's own state from the state it was created with plus the
state deltas of the events it keeps. The `app:` and `user:` tiers are shared with the user's
other sessions and do not change: a removed event's `app:` delta stays in the app tier.

| Data | After a fork | After a rewind or edit |
|------|--------------|------------------------|
| Events | The source's events up to the fork point, with their IDs, timestamps, and authors | The kept events; a replacement keeps the original's timestamp and position |
| Session state | The source's creation state plus the copied events' deltas | The creation state plus the kept events' deltas |
| `app:` and `user:` state | Shared, current values | Unchanged |
| The source | Unchanged | — |

Every backend records the state a session was created with. A session created before its
backend recorded it rebuilds from its current keys that no event wrote, whose values cannot
have changed since it was created; a key the session was created with and a later event
overwrote is then lost on rewind.

## Rules

Three rules hold on every backend, whoever calls the method:

| Rule | Error |
|------|-------|
| A result never keeps a tool call without its response, or a response without its call, when the history held both | `session.history.splits_tool_call` (`InvalidInput`) |
| A rewind or edit of a session that holds a tool call waiting for a decision fails; a fork that would copy such a call fails; a fork at an earlier event succeeds | `session.history.held_call` (`Conflict`) |
| A rewrite with `expected_version` applies only while the history's `version()` is still that value | `session.history.version_mismatch` (`Conflict`) |

A rewrite reads, checks, and writes the session in one transaction, or in one guarded script
that plans again when an append lands in between, so a concurrent append is never lost.

## Fork

```rust
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{ForkOrigin, ForkRequest, InMemorySessionService, SessionService};

async fn try_another_answer(
    service: &InMemorySessionService,
    event_id: &str,
) -> adk_core::Result<()> {
    let source = AdkIdentity::new(
        AppName::try_from("my_app")?,
        UserId::try_from("user_123")?,
        SessionId::try_from("session_abc")?,
    );
    let fork = service
        .fork(ForkRequest::new(source).at_event(event_id).with_session_id("session_abc_b"))
        .await?;
    let origin = ForkOrigin::of(fork.session.as_ref()).expect("a fork records its origin");
    println!("forked {} at {:?}", origin.session_id, origin.event_id);
    Ok(())
}
```

The fork records a `ForkOrigin` — source session, last copied event, and time — in its state
under `adk:forked_from` (`FORK_ORIGIN_KEY`). The key is part of the fork's creation state, so
rewinding the fork keeps it. `ForkRequest::new(source)` without `at_event` copies every event,
and without `with_session_id` the fork gets a UUID.

## Rewind

```rust
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{InMemorySessionService, SessionService};

async fn drop_last_turn(service: &InMemorySessionService) -> adk_core::Result<()> {
    let identity = AdkIdentity::new(
        AppName::try_from("my_app")?,
        UserId::try_from("user_123")?,
        SessionId::try_from("session_abc")?,
    );
    // The last turn is the user message and the model's reply.
    let session = service.rewind_steps(&identity, 2).await?;
    println!("{} events remain", session.events().len());
    Ok(())
}
```

## Edit

```rust
use adk_core::Content;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{
    HistoryEdit, HistoryRewrite, InMemorySessionService, RewriteHistoryRequest, SessionService,
};

async fn redact(service: &InMemorySessionService, event_id: &str) -> adk_core::Result<()> {
    let identity = AdkIdentity::new(
        AppName::try_from("my_app")?,
        UserId::try_from("user_123")?,
        SessionId::try_from("session_abc")?,
    );
    let history = service.history(&identity).await?;
    let mut event = history
        .events
        .iter()
        .find(|event| event.id == event_id)
        .cloned()
        .expect("the event is in the session");
    event.llm_response.content = Some(Content::new("user").with_text("[redacted]"));

    let outcome = service
        .rewrite_history(
            RewriteHistoryRequest::new(
                identity,
                HistoryRewrite::Edit { edits: vec![HistoryEdit::Replace { event: Box::new(event) }] },
            )
            .expect_version(history.version()),
        )
        .await?;
    println!("replaced {:?}", outcome.change.replaced);
    Ok(())
}
```

`HistoryEdit::Remove { event_id }` removes an event. Edits apply in order, and a `Replace`
names its target by the replacement's `id`.

## What Changed

`fork` and `rewrite_history` return a `HistoryOutcome`: the session afterwards and a
`HistoryChange`. The change names the operation, the versions before and after, the removed
and replaced event IDs, the fork origin, and a digest of the removed and replaced events. It
never carries their content, so a record of a redaction does not repeat what was redacted. The
runner and server write it to the audit log.

## Backends

| Backend | Fork | Rewind and edit | How a rewrite is atomic |
|---------|------|-----------------|--------------------------|
| In-memory | Yes | Yes | One write lock |
| SQLite | Yes | Yes | One `BEGIN IMMEDIATE` transaction |
| PostgreSQL | Yes | Yes | One transaction holding the session row's lock |
| Redis, standalone and cluster | Yes | Yes | One script, guarded on the session's state, update time, and event count; plans again on contention |
| MongoDB | Yes | Yes | A transaction where the deployment supports one; the session document's update is guarded on its append count and update time either way |
| Neo4j | Yes | Yes | One transaction holding the session node's lock |
| Firestore | Yes | Yes | One transaction, guarded on the session document; plans again on contention |
| Vertex AI | Yes | No: `session.history.unsupported` | The Session API cannot delete events; fork at the event instead |
| `EncryptedSession` | Yes, re-sealed under the fork's identity | Yes, when the inner backend has them | The inner backend's, at the version checked against the decrypted history |
| `ConsentGatedSessionService` | Yes, routed as `create` is | Yes, on the store that holds the session | The holding store's |

PostgreSQL migration 3 adds `sessions.initial_state`; run `migrate()` after upgrading. The
other backends record the creation state as a field of the session record. On Vertex AI, the
Session API applies the copied events' state deltas itself and keeps `app:` and `user:` keys in
the session, so a fork's state includes them; a key the source was created with and only later
events overwrote is not in a fork taken before those events.

A `SessionService` outside this crate gets `fork`, `rewind`, and `rewind_steps` by implementing
`history`, `rewrite_history`, and `import_session`; the defaults of the last two return
`session.history.unsupported`. Plan a rewrite with `SessionHistory::rewrite` inside the
backend's transaction and a fork with `SessionHistory::fork`, so the rules above hold.

## Errors

| Error | Category | When |
|-------|----------|------|
| `session.not_found` | `NotFound` | The session, or the fork's source, does not exist |
| `session.event_not_found` | `NotFound` | The target event is not in the session |
| `session.already_exists` | `Conflict` | The fork's or import's session ID is taken |
| `session.history.held_call` | `Conflict` | A tool call waits for a decision |
| `session.history.version_mismatch` | `Conflict` | The history changed since the expected version |
| `session.history.splits_tool_call` | `InvalidInput` | The result would separate a tool call from its response |
| `session.history.ambiguous_event` | `InvalidInput` | Two stored events share the target ID |
| `session.rewind.too_many_steps` | `InvalidInput` | `rewind_steps` asks for more events than the session holds |
| `session.history.unsupported` | `Unsupported` | The backend cannot rewrite history in place |

## Related

- [Sessions](sessions.md) — the `SessionService` trait and its backends
- [Harness snapshots and rewind](../harness/snapshots-and-rewind.md) — rewinding files, the
  plan, and the transcript together
- [Tool authorization](../security/tool-authorization.md) — held calls and how they resume
