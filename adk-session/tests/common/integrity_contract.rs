//! Session integrity checks shared by every backend: duplicate creates, missing sessions,
//! event order, U+0000 in strings, list order, and rewind.
// Each test binary that declares `mod common` uses a different subset of these checks.
#![allow(dead_code)]

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{Content, ErrorCategory, FunctionResponseData, Part};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session,
    SessionService,
};
use chrono::{DateTime, Duration, SubsecRound, TimeZone, Utc};
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

async fn create(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    initial: HashMap<String, Value>,
) -> adk_core::Result<Box<dyn Session>> {
    service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: Some(session_id.to_string()),
            state: initial,
        })
        .await
}

async fn get(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> Box<dyn Session> {
    service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get session")
}

async fn append(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: Event,
) {
    service
        .append_event_for_identity(AppendEventRequest {
            identity: identity(app_name, user_id, session_id),
            event,
        })
        .await
        .expect("append event");
}

async fn delete(service: &dyn SessionService, app_name: &str, user_id: &str, session_id: &str) {
    service
        .delete(DeleteRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
        })
        .await
        .expect("delete session");
}

fn delta_event(at: DateTime<Utc>, delta: &[(&str, Value)]) -> Event {
    let mut event = Event::new("inv-integrity");
    event.author = "agent".to_string();
    event.timestamp = at;
    event.actions.state_delta = state(delta);
    event
}

fn ids(session: &dyn Session) -> Vec<String> {
    session.events().all().into_iter().map(|event| event.id).collect()
}

/// A `create` for an existing identity fails with `session.already_exists` and changes
/// nothing, and appending to a missing session fails with `session.not_found`.
pub async fn assert_create_and_append_errors_contract(
    service: &dyn SessionService,
    app_name: &str,
) {
    let user = "integrity-user";
    let session = unique("existing");
    let flag = format!("flag_{}", uuid::Uuid::new_v4().simple());
    let app_flag = format!("app:{flag}");
    let user_flag = format!("user:{flag}");

    create(
        service,
        app_name,
        user,
        &session,
        state(&[("plan", json!("gold")), (&app_flag, json!("on"))]),
    )
    .await
    .expect("first create");
    append(service, app_name, user, &session, delta_event(Utc::now(), &[("step", json!(1))])).await;

    let duplicate = create(
        service,
        app_name,
        user,
        &session,
        state(&[("plan", json!("free")), (&app_flag, json!("off")), (&user_flag, json!(true))]),
    )
    .await
    .err()
    .expect("a create for an existing identity must fail");
    assert_eq!(
        (duplicate.category, duplicate.code),
        (ErrorCategory::Conflict, "session.already_exists"),
        "{duplicate}"
    );

    let stored = get(service, app_name, user, &session).await;
    assert_eq!(stored.events().len(), 1, "the conflicting create kept the events");
    assert_eq!(
        [
            stored.state().get("plan"),
            stored.state().get("step"),
            stored.state().get(&app_flag),
            stored.state().get(&user_flag),
        ],
        [Some(json!("gold")), Some(json!(1)), Some(json!("on")), None],
        "the conflicting create wrote no session, app, or user state"
    );

    let missing = unique("missing");
    let mut missing_event = Event::new("inv-missing");
    missing_event.author = "user".to_string();
    let not_found = service
        .append_event_for_identity(AppendEventRequest {
            identity: identity(app_name, user, &missing),
            event: missing_event,
        })
        .await
        .expect_err("appending to a missing session must fail");
    assert_eq!(
        (not_found.category, not_found.code),
        (ErrorCategory::NotFound, "session.not_found"),
        "{not_found}"
    );

    delete(service, app_name, user, &session).await;
}

/// Events with one timestamp, and call/response pairs 300 µs apart inside one millisecond,
/// read back in append order.
pub async fn assert_event_order_contract(service: &dyn SessionService, app_name: &str) {
    let user = "order-user";

    let tied = unique("tied");
    create(service, app_name, user, &tied, HashMap::new()).await.expect("create");
    let at = Utc::now().trunc_subsecs(6);
    let mut appended = Vec::new();
    for n in 0..12 {
        let event = delta_event(at, &[("n", json!(n))]);
        appended.push(event.id.clone());
        append(service, app_name, user, &tied, event).await;
    }
    let stored = get(service, app_name, user, &tied).await;
    assert_eq!(ids(stored.as_ref()), appended, "events with one timestamp keep append order");
    assert_eq!(stored.state().get("n"), Some(json!(11)), "the last delta wins");

    let pairs = unique("pairs");
    create(service, app_name, user, &pairs, HashMap::new()).await.expect("create");
    let base = Utc.timestamp_millis_opt(Utc::now().timestamp_millis()).unwrap();
    let mut appended = Vec::new();
    for n in 0..20 {
        let millisecond = base + Duration::milliseconds(n);
        let mut call = delta_event(millisecond + Duration::microseconds(100), &[]);
        call.set_content(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: json!({"n": n}),
                id: Some(format!("call-{n}")),
                thought_signature: None,
            }],
        });
        let mut response = delta_event(millisecond + Duration::microseconds(400), &[]);
        response.set_content(Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new("lookup", json!({"n": n})),
                id: Some(format!("call-{n}")),
                annotations: None,
            }],
        });
        for event in [call, response] {
            appended.push(event.id.clone());
            append(service, app_name, user, &pairs, event).await;
        }
    }
    assert_eq!(
        ids(get(service, app_name, user, &pairs).await.as_ref()),
        appended,
        "each function response reads back after its call"
    );

    delete(service, app_name, user, &tied).await;
    delete(service, app_name, user, &pairs).await;
}

/// Strings containing U+0000 in event content and state read back unchanged.
pub async fn assert_nul_round_trip_contract(service: &dyn SessionService, app_name: &str) {
    let user = "nul-user";
    let session = unique("nul");
    let binary = "PK\u{0}\u{3}binary";
    // A string that starts like an escaped one must read back as written too.
    let lookalike = "\u{e000}adk-nul:%00 literal";
    create(service, app_name, user, &session, state(&[("seed", json!("x\u{0}y"))]))
        .await
        .expect("create with U+0000 in state");

    let mut event =
        delta_event(Utc::now(), &[("blob", json!(binary)), ("lookalike", json!(lookalike))]);
    event.set_content(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new(
                "read_file",
                json!({"text": binary, "lines": ["a\u{0}b", lookalike]}),
            ),
            id: Some("call-nul".to_string()),
            annotations: None,
        }],
    });
    let content = serde_json::to_value(&event.llm_response.content).unwrap();
    append(service, app_name, user, &session, event).await;

    let stored = get(service, app_name, user, &session).await;
    assert_eq!(
        serde_json::to_value(stored.events().at(0).map(|event| &event.llm_response.content))
            .unwrap(),
        content,
        "event content with U+0000 reads back unchanged"
    );
    assert_eq!(
        [stored.state().get("seed"), stored.state().get("blob"), stored.state().get("lookalike")],
        [Some(json!("x\u{0}y")), Some(json!(binary)), Some(json!(lookalike))],
        "state with U+0000 reads back unchanged"
    );

    delete(service, app_name, user, &session).await;
}

/// `list` returns the most recently updated session first, equal update times by session ID
/// descending, and `limit`/`offset` pages tile the full list.
pub async fn assert_list_order_contract(service: &dyn SessionService, app_name: &str) {
    let user = unique("lister");
    let base = Utc::now().trunc_subsecs(6) + Duration::hours(1);
    // (session, seconds after `base` of its last update); s2 and s4 tie.
    let updates = [("s0", 3), ("s1", 5), ("s2", 1), ("s3", 4), ("s4", 1), ("s5", 0)];
    for (session, offset) in updates {
        create(service, app_name, &user, session, HashMap::new()).await.expect("create");
        append(
            service,
            app_name,
            &user,
            session,
            delta_event(base + Duration::seconds(offset), &[]),
        )
        .await;
    }
    let expected = ["s1", "s3", "s0", "s4", "s2", "s5"];

    let list = |limit: Option<usize>, offset: Option<usize>| ListRequest {
        app_name: app_name.to_string(),
        user_id: user.clone(),
        limit,
        offset,
    };
    let listed: Vec<String> = service
        .list(list(None, None))
        .await
        .expect("list")
        .iter()
        .map(|session| session.id().to_string())
        .collect();
    assert_eq!(listed, expected, "newest first, ties by session ID descending");

    let mut paged = Vec::new();
    for offset in [0, 2, 4] {
        let page = service.list(list(Some(2), Some(offset))).await.expect("list page");
        paged.extend(page.iter().map(|session| session.id().to_string()));
    }
    assert_eq!(paged, expected, "pages of two tile the list");

    service.delete_all_sessions(app_name, &user).await.expect("delete all");
}

/// Rewind changes only the session the identity names, keeps the state the session was
/// created with, and reports typed errors.
pub async fn assert_rewind_contract(service: &dyn SessionService, app_name: &str) {
    let shared = unique("shared");
    let at = Utc::now().trunc_subsecs(6);
    let mut event_ids = HashMap::new();
    for user in ["alice", "bob"] {
        create(
            service,
            app_name,
            user,
            &shared,
            state(&[("owner", json!(user)), ("tier", json!("gold"))]),
        )
        .await
        .expect("create");
        let mut ids = Vec::new();
        for n in 0..3 {
            let event = delta_event(
                at + Duration::seconds(n),
                &[("step", json!(n)), ("tier", json!("silver"))],
            );
            ids.push(event.id.clone());
            append(service, app_name, user, &shared, event).await;
        }
        event_ids.insert(user, ids);
    }

    let bob = identity(app_name, "bob", &shared);
    let rewound = service.rewind_steps(&bob, 1).await.expect("rewind bob");
    assert_eq!((rewound.user_id(), rewound.events().len()), ("bob", 2));
    let alice = get(service, app_name, "alice", &shared).await;
    assert_eq!(
        (alice.events().len(), alice.state().get("owner"), alice.state().get("step")),
        (3, Some(json!("alice")), Some(json!(2))),
        "bob's rewind leaves alice's session with the same ID untouched"
    );

    let to_first =
        service.rewind(&bob, &event_ids["bob"][0]).await.expect("rewind bob to his first event");
    assert_eq!(
        (to_first.events().len(), to_first.state().get("step"), to_first.state().get("tier")),
        (1, Some(json!(0)), Some(json!("silver")))
    );
    let emptied = service.rewind_steps(&bob, 1).await.expect("rewind bob's last event");
    let reread = get(service, app_name, "bob", &shared).await;
    for session in [emptied.as_ref(), reread.as_ref()] {
        assert_eq!(
            (
                session.events().len(),
                session.state().get("owner"),
                session.state().get("tier"),
                session.state().get("step"),
            ),
            (0, Some(json!("bob")), Some(json!("gold")), None),
            "rewinding every event leaves the state the session was created with"
        );
    }

    let unchanged = service.rewind_steps(&identity(app_name, "alice", &shared), 0).await.unwrap();
    assert_eq!(unchanged.events().len(), 3, "zero steps changes nothing");

    let too_many = service
        .rewind_steps(&identity(app_name, "alice", &shared), 4)
        .await
        .err()
        .expect("more steps than events must fail");
    assert_eq!(
        (too_many.category, too_many.code),
        (ErrorCategory::InvalidInput, "session.rewind.too_many_steps")
    );
    let foreign_event = service
        .rewind(&identity(app_name, "alice", &shared), &event_ids["bob"][1])
        .await
        .err()
        .expect("another session's event is not a rewind target");
    assert_eq!(
        (foreign_event.category, foreign_event.code),
        (ErrorCategory::NotFound, "session.event_not_found")
    );
    let missing = service
        .rewind_steps(&identity(app_name, "carol", &shared), 1)
        .await
        .err()
        .expect("a missing session cannot be rewound");
    assert_eq!((missing.category, missing.code), (ErrorCategory::NotFound, "session.not_found"));
    assert_eq!(get(service, app_name, "alice", &shared).await.events().len(), 3);

    delete(service, app_name, "alice", &shared).await;
    delete(service, app_name, "bob", &shared).await;
}

/// A backend without rewind reports it as `Unsupported`.
pub async fn assert_rewind_unsupported(service: &dyn SessionService, app_name: &str) {
    let target = identity(app_name, "user", "session");
    for error in
        [service.rewind(&target, "event").await.err(), service.rewind_steps(&target, 1).await.err()]
    {
        let error = error.expect("rewind must fail on this backend");
        assert_eq!(
            (error.category, error.code),
            (ErrorCategory::Unsupported, "session.rewind.unsupported")
        );
    }
}
