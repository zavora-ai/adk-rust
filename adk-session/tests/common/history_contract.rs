//! The fork, rewind, and history-edit contract every session backend runs.
//!
//! Each check uses fresh session IDs under `app_name`, so a backend runs the suite against a
//! shared database. Timestamps are whole milliseconds, the finest every backend stores.
// Each test binary that declares `mod common` uses a different subset of these checks.
#![allow(dead_code)]

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{Content, ErrorCategory, FunctionResponseData, Part, ToolConfirmationRequest};
use adk_session::{
    AppendEventRequest, CreateRequest, Event, FORK_ORIGIN_KEY, ForkOrigin, ForkRequest,
    HistoryEdit, HistoryOperation, HistoryRewrite, ImportSessionRequest, RewriteHistoryRequest,
    Session, SessionService,
};
use chrono::{DateTime, Duration, SubsecRound, Utc};

pub use super::integrity_contract::IdScope;
use serde_json::{Value, json};
use std::collections::HashMap;

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn identity(app_name: &str, user_id: &str, session_id: &str) -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(app_name).expect("valid app name"),
        UserId::try_from(user_id).expect("valid user id"),
        SessionId::try_from(session_id).expect("valid session id"),
    )
}

fn state(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect()
}

fn delta_event(at: DateTime<Utc>, text: &str, delta: &[(&str, Value)]) -> Event {
    let mut event = Event::new("inv-history");
    event.author = "agent".to_string();
    event.timestamp = at;
    event.llm_response.content = Some(Content::new("model").with_text(text));
    event.actions.state_delta = state(delta);
    event
}

fn call_event(at: DateTime<Utc>, call_id: &str) -> Event {
    let mut event = delta_event(at, "", &[]);
    event.llm_response.content = Some(Content {
        role: "model".into(),
        parts: vec![Part::FunctionCall {
            name: "lookup".into(),
            args: json!({ "sku": "A1" }),
            id: Some(call_id.into()),
            thought_signature: None,
        }],
    });
    event
}

fn response_event(at: DateTime<Utc>, call_id: &str) -> Event {
    let mut event = delta_event(at, "", &[]);
    event.author = "user".into();
    event.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new("lookup", json!({ "stock": 3 })),
            id: Some(call_id.into()),
            annotations: None,
        }],
    });
    event
}

fn ids(session: &dyn Session) -> Vec<String> {
    session.events().all().into_iter().map(|event| event.id).collect()
}

fn texts(session: &dyn Session) -> Vec<String> {
    session
        .events()
        .all()
        .into_iter()
        .map(|event| {
            event
                .llm_response
                .content
                .and_then(|content| content.parts.first().and_then(|p| p.text().map(String::from)))
                .unwrap_or_default()
        })
        .collect()
}

async fn get(service: &dyn SessionService, identity: &AdkIdentity) -> Box<dyn Session> {
    service.get_for_identity(identity).await.expect("get session")
}

async fn append(service: &dyn SessionService, identity: &AdkIdentity, event: Event) {
    service
        .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
        .await
        .expect("append event");
}

async fn delete(service: &dyn SessionService, identity: &AdkIdentity) {
    service.delete_for_identity(identity).await.expect("delete session");
}

/// Creates `session_id` for `user` with creation state `owner`/`tier`, then appends three
/// events that write `step`, `tier`, and the `app:` and `user:` tiers. Returns the event IDs.
async fn seed(
    service: &dyn SessionService,
    app_name: &str,
    user: &str,
    session_id: &str,
    at: DateTime<Utc>,
) -> Vec<String> {
    service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user.to_string(),
            session_id: Some(session_id.to_string()),
            state: state(&[("owner", json!(user)), ("tier", json!("gold"))]),
        })
        .await
        .expect("create");
    let target = identity(app_name, user, session_id);
    let mut event_ids = Vec::new();
    for n in 0..3 {
        let event = delta_event(
            at + Duration::seconds(n),
            &format!("turn {n}"),
            &[
                ("step", json!(n)),
                ("tier", json!("silver")),
                ("app:turns", json!(n)),
                ("user:last_step", json!(n)),
            ],
        );
        event_ids.push(event.id.clone());
        append(service, &target, event).await;
    }
    event_ids
}

/// A fork copies the events up to the fork point with the state there, records its origin,
/// leaves the source and the shared tiers alone, and diverges from the source afterwards.
pub async fn assert_fork_contract(service: &dyn SessionService, app_name: &str) {
    let at = Utc::now().trunc_subsecs(3);
    let user = unique("forker");
    let source_id = unique("source");
    let source = identity(app_name, &user, &source_id);
    let event_ids = seed(service, app_name, &user, &source_id, at).await;

    let fork_id = unique("fork");
    let outcome = service
        .fork(ForkRequest::new(source.clone()).at_event(&event_ids[1]).with_session_id(&fork_id))
        .await
        .expect("fork at the second event");
    let fork = identity(app_name, &user, &fork_id);
    assert_eq!(outcome.session.id(), fork_id);
    assert_eq!(outcome.change.operation, HistoryOperation::Fork);
    assert_eq!(outcome.change.events_after, 2);

    let source_now = get(service, &source).await;
    let fork_now = get(service, &fork).await;
    for session in [outcome.session.as_ref(), fork_now.as_ref()] {
        assert_eq!(ids(session), event_ids[..2], "the fork holds the events up to the fork point");
        assert_eq!(
            (
                session.state().get("step"),
                session.state().get("tier"),
                session.state().get("owner"),
            ),
            (Some(json!(1)), Some(json!("silver")), Some(json!(user))),
            "the fork's state is the source's state at the fork point"
        );
        assert_eq!(
            (session.state().get("app:turns"), session.state().get("user:last_step")),
            (Some(json!(2)), Some(json!(2))),
            "the app and user tiers are shared and keep their current values"
        );
        let origin = ForkOrigin::of(session).expect("the fork records its origin");
        assert_eq!(
            (origin.session_id.as_str(), origin.event_id.as_deref()),
            (source_id.as_str(), Some(event_ids[1].as_str()))
        );
    }
    let source_events = source_now.events().all();
    let fork_events = fork_now.events().all();
    for (copied, original) in fork_events.iter().zip(&source_events) {
        assert_eq!(
            (copied.timestamp, &copied.invocation_id, &copied.author),
            (original.timestamp, &original.invocation_id, &original.author),
            "copied events keep their timestamps and authors"
        );
    }
    assert_eq!(texts(fork_now.as_ref()), ["turn 0", "turn 1"]);
    assert_eq!(ids(source_now.as_ref()), event_ids, "the source keeps every event");
    assert_eq!(source_now.state().get("step"), Some(json!(2)));
    assert!(ForkOrigin::of(source_now.as_ref()).is_none());

    // The fork and the source diverge.
    append(
        service,
        &fork,
        delta_event(at + Duration::seconds(10), "fork turn", &[("step", json!(10))]),
    )
    .await;
    append(
        service,
        &source,
        delta_event(at + Duration::seconds(11), "source turn", &[("step", json!(20))]),
    )
    .await;
    let (fork_now, source_now) = (get(service, &fork).await, get(service, &source).await);
    assert_eq!(texts(fork_now.as_ref()), ["turn 0", "turn 1", "fork turn"]);
    assert_eq!(texts(source_now.as_ref()), ["turn 0", "turn 1", "turn 2", "source turn"]);
    assert_eq!(
        (fork_now.state().get("step"), source_now.state().get("step")),
        (Some(json!(10)), Some(json!(20)))
    );

    // A fork of every event under a generated ID, then a fork of the fork.
    let whole = service.fork(ForkRequest::new(source.clone())).await.expect("fork every event");
    assert_eq!(ids(whole.session.as_ref()), ids(source_now.as_ref()));
    assert_ne!(whole.session.id(), source_id);
    let grandchild = service.fork(ForkRequest::new(fork.clone())).await.expect("fork the fork");
    assert_eq!(ForkOrigin::of(grandchild.session.as_ref()).unwrap().session_id, fork_id);

    // A rewind of the fork rebuilds from the state it was created with: the source's
    // creation state and the fork origin.
    let emptied = service.rewind_steps(&fork, 3).await.expect("rewind the fork completely");
    assert_eq!(
        (emptied.events().len(), emptied.state().get("tier"), emptied.state().get("step")),
        (0, Some(json!("gold")), None)
    );
    assert!(emptied.state().get(FORK_ORIGIN_KEY).is_some(), "rewinding a fork keeps its origin");

    // Errors.
    let taken = service
        .fork(ForkRequest::new(source.clone()).with_session_id(&fork_id))
        .await
        .expect_err("a fork cannot reuse a taken session ID");
    assert_eq!((taken.category, taken.code), (ErrorCategory::Conflict, "session.already_exists"));
    let missing = service
        .fork(ForkRequest::new(identity(app_name, &user, &unique("missing"))))
        .await
        .expect_err("a missing source cannot be forked");
    assert_eq!((missing.category, missing.code), (ErrorCategory::NotFound, "session.not_found"));
    let foreign = service
        .fork(ForkRequest::new(source.clone()).at_event("not-an-event"))
        .await
        .expect_err("a fork point must be an event of the source");
    assert_eq!(
        (foreign.category, foreign.code),
        (ErrorCategory::NotFound, "session.event_not_found")
    );

    for session in [
        &source,
        &fork,
        &grandchild.session.try_identity().unwrap(),
        &whole.session.try_identity().unwrap(),
    ] {
        delete(service, session).await;
    }
    service.delete_all_sessions(app_name, &user).await.ok();
}

/// Edits replace events in place and remove them, rebuild the session's own state, keep the
/// shared tiers, change only the named session, and honour the expected version.
pub async fn assert_edit_contract(service: &dyn SessionService, app_name: &str, scope: IdScope) {
    let at = Utc::now().trunc_subsecs(3);
    let shared = unique("edited");
    let mut seeded = HashMap::new();
    for user in ["alice", "bob"] {
        seeded.insert(user, seed(service, app_name, user, &scope.session(&shared, user), at).await);
    }
    let alice = identity(app_name, "alice", &scope.session(&shared, "alice"));
    let bob = identity(app_name, "bob", &scope.session(&shared, "bob"));
    let event_ids = &seeded["alice"];

    let history = service.history(&alice).await.expect("read history");
    assert_eq!(history.events.len(), 3);
    let mut corrected = history.events[1].clone();
    corrected.llm_response.content = Some(Content::new("model").with_text("turn 1, corrected"));
    corrected.actions.state_delta = state(&[("step", json!(11)), ("note", json!("corrected"))]);

    let stale = service
        .rewrite_history(
            RewriteHistoryRequest::new(
                alice.clone(),
                HistoryRewrite::Edit {
                    edits: vec![HistoryEdit::Remove { event_id: event_ids[2].clone() }],
                },
            )
            .expect_version("fnv1a128:stale"),
        )
        .await
        .expect_err("a stale version is refused");
    assert_eq!(
        (stale.category, stale.code),
        (ErrorCategory::Conflict, "session.history.version_mismatch")
    );
    assert_eq!(get(service, &alice).await.events().len(), 3, "a refused edit changes nothing");

    let outcome = service
        .rewrite_history(
            RewriteHistoryRequest::new(
                alice.clone(),
                HistoryRewrite::Edit {
                    edits: vec![
                        HistoryEdit::Replace { event: Box::new(corrected) },
                        HistoryEdit::Remove { event_id: event_ids[2].clone() },
                    ],
                },
            )
            .expect_version(history.version()),
        )
        .await
        .expect("edit at the version read");
    assert_eq!(outcome.change.operation, HistoryOperation::Edit);
    assert_eq!(
        (outcome.change.replaced.clone(), outcome.change.removed.clone()),
        (vec![event_ids[1].clone()], vec![event_ids[2].clone()])
    );
    assert!(outcome.change.changed_digest.is_some());

    let reread = get(service, &alice).await;
    for session in [outcome.session.as_ref(), reread.as_ref()] {
        assert_eq!(ids(session), event_ids[..2]);
        assert_eq!(texts(session), ["turn 0", "turn 1, corrected"]);
        assert_eq!(
            (
                session.state().get("step"),
                session.state().get("note"),
                session.state().get("tier"),
                session.state().get("owner"),
            ),
            (
                Some(json!(11)),
                Some(json!("corrected")),
                Some(json!("silver")),
                Some(json!("alice"))
            ),
            "state is rebuilt from the creation state and the kept events"
        );
        assert_eq!(session.state().get("app:turns"), Some(json!(2)), "shared tiers do not change");
    }
    assert_eq!(
        reread.events().all()[1].timestamp,
        at + Duration::seconds(1),
        "a replacement keeps its place"
    );
    let after = service.history(&alice).await.expect("read history again");
    assert_eq!(after.version(), outcome.change.version_after);

    let bob_now = get(service, &bob).await;
    assert_eq!(
        (bob_now.events().len(), bob_now.state().get("step")),
        (3, Some(json!(2))),
        "another user's session with the same ID is untouched"
    );

    // An appended event lands after the edited history.
    append(
        service,
        &alice,
        delta_event(at + Duration::seconds(5), "turn 3", &[("step", json!(3))]),
    )
    .await;
    assert_eq!(
        texts(get(service, &alice).await.as_ref()),
        ["turn 0", "turn 1, corrected", "turn 3"]
    );

    let missing = service
        .rewrite_history(RewriteHistoryRequest::new(
            alice.clone(),
            HistoryRewrite::Edit {
                edits: vec![HistoryEdit::Remove { event_id: event_ids[2].clone() }],
            },
        ))
        .await
        .expect_err("a removed event cannot be edited again");
    assert_eq!(
        (missing.category, missing.code),
        (ErrorCategory::NotFound, "session.event_not_found")
    );

    delete(service, &alice).await;
    delete(service, &bob).await;
}

/// A rewrite or fork never separates a tool call from its response, and a call held for a
/// decision blocks rewrites and forks that would copy it.
pub async fn assert_tool_call_rules(service: &dyn SessionService, app_name: &str) {
    let at = Utc::now().trunc_subsecs(3);
    let session_id = unique("tools");
    let target = identity(app_name, "carol", &session_id);
    service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: "carol".to_string(),
            session_id: Some(session_id.clone()),
            state: HashMap::new(),
        })
        .await
        .expect("create");
    let ask = delta_event(at, "is A1 in stock?", &[]);
    let call = call_event(at + Duration::seconds(1), "call-1");
    let result = response_event(at + Duration::seconds(2), "call-1");
    let reply = delta_event(at + Duration::seconds(3), "3 in stock", &[]);
    let (ask_id, call_id, result_id) = (ask.id.clone(), call.id.clone(), result.id.clone());
    for event in [ask, call, result, reply] {
        append(service, &target, event).await;
    }

    let splits = [
        RewriteHistoryRequest::new(
            target.clone(),
            HistoryRewrite::RewindTo { event_id: call_id.clone() },
        ),
        RewriteHistoryRequest::new(
            target.clone(),
            HistoryRewrite::Edit { edits: vec![HistoryEdit::Remove { event_id: call_id.clone() }] },
        ),
        RewriteHistoryRequest::new(
            target.clone(),
            HistoryRewrite::Edit {
                edits: vec![HistoryEdit::Remove { event_id: result_id.clone() }],
            },
        ),
    ];
    for request in splits {
        let error = service.rewrite_history(request).await.expect_err("a split is refused");
        assert_eq!(
            (error.category, error.code),
            (ErrorCategory::InvalidInput, "session.history.splits_tool_call")
        );
    }
    let error = service
        .fork(ForkRequest::new(target.clone()).at_event(&call_id))
        .await
        .expect_err("a fork point between a call and its response is refused");
    assert_eq!(error.code, "session.history.splits_tool_call");
    assert_eq!(get(service, &target).await.events().len(), 4, "refused rewrites change nothing");

    let both = service
        .rewrite_history(RewriteHistoryRequest::new(
            target.clone(),
            HistoryRewrite::Edit {
                edits: vec![
                    HistoryEdit::Remove { event_id: call_id.clone() },
                    HistoryEdit::Remove { event_id: result_id.clone() },
                ],
            },
        ))
        .await
        .expect("removing a call with its response is allowed");
    assert_eq!(both.session.events().len(), 2);

    let mut hold = call_event(at + Duration::seconds(4), "call-2");
    hold.actions.tool_confirmation = Some(ToolConfirmationRequest {
        tool_name: "lookup".into(),
        function_call_id: Some("call-2".into()),
        args: json!({ "sku": "A1" }),
        acting: None,
        origin: None,
    });
    append(service, &target, hold).await;
    let held = service.rewind_steps(&target, 1).await.err().expect("a held call blocks a rewind");
    assert_eq!((held.category, held.code), (ErrorCategory::Conflict, "session.history.held_call"));
    let held_fork = service
        .fork(ForkRequest::new(target.clone()))
        .await
        .expect_err("a fork that would copy a held call is refused");
    assert_eq!(held_fork.code, "session.history.held_call");
    let before_hold = service
        .fork(ForkRequest::new(target.clone()).at_event(&ask_id))
        .await
        .expect("a fork before the held call is allowed");
    assert_eq!(before_hold.session.events().len(), 1);

    delete(service, &before_hold.session.try_identity().unwrap()).await;
    delete(service, &target).await;
}

/// An import creates a session that holds the given events and state without applying the
/// events' deltas to the shared tiers, and refuses an existing session.
pub async fn assert_import_contract(service: &dyn SessionService, app_name: &str) {
    let at = Utc::now().trunc_subsecs(3);
    let user = unique("importer");
    let session_id = unique("imported");
    let target = identity(app_name, &user, &session_id);
    let events = vec![
        delta_event(at, "first", &[("step", json!(1)), ("app:imported", json!(true))]),
        delta_event(at + Duration::seconds(1), "second", &[("step", json!(2))]),
    ];
    let session = service
        .import_session(ImportSessionRequest::new(
            target.clone(),
            state(&[("step", json!(2)), ("origin", json!("archive"))]),
            events.clone(),
        ))
        .await
        .expect("import");
    let reread = get(service, &target).await;
    for session in [session.as_ref(), reread.as_ref()] {
        assert_eq!(texts(session), ["first", "second"]);
        assert_eq!(
            (session.state().get("step"), session.state().get("origin")),
            (Some(json!(2)), Some(json!("archive")))
        );
        assert_eq!(
            session.state().get("app:imported"),
            None,
            "imported deltas are not applied to the app tier"
        );
    }
    let conflict = service
        .import_session(ImportSessionRequest::new(target.clone(), HashMap::new(), Vec::new()))
        .await
        .err()
        .expect("an import cannot overwrite a session");
    assert_eq!(
        (conflict.category, conflict.code),
        (ErrorCategory::Conflict, "session.already_exists")
    );
    assert_eq!(get(service, &target).await.events().len(), 2);
    delete(service, &target).await;
    service.delete_all_sessions(app_name, &user).await.ok();
}

/// Every history check a backend that can delete events runs.
pub async fn assert_history_contract(service: &dyn SessionService, app_name: &str) {
    assert_history_contract_scoped(service, app_name, IdScope::PerUser).await;
}

/// [`assert_history_contract`] on a backend whose session IDs have `scope`.
pub async fn assert_history_contract_scoped(
    service: &dyn SessionService,
    app_name: &str,
    scope: IdScope,
) {
    assert_fork_contract(service, app_name).await;
    assert_edit_contract(service, app_name, scope).await;
    assert_tool_call_rules(service, app_name).await;
    assert_import_contract(service, app_name).await;
}

/// A backend that cannot delete events reports in-place rewrites as `Unsupported`.
pub async fn assert_rewrite_unsupported(service: &dyn SessionService, app_name: &str) {
    let target = identity(app_name, "user", "session");
    for error in [
        service.rewind(&target, "event").await.err(),
        service.rewind_steps(&target, 1).await.err(),
        service
            .rewrite_history(RewriteHistoryRequest::new(
                target.clone(),
                HistoryRewrite::Edit { edits: Vec::new() },
            ))
            .await
            .err(),
    ] {
        let error = error.expect("an in-place rewrite must fail on this backend");
        assert_eq!(
            (error.category, error.code),
            (ErrorCategory::Unsupported, "session.history.unsupported")
        );
    }
}
