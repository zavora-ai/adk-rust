// Each test binary that declares `mod common` uses a different subset of these checks.
#![allow(dead_code)]

use adk_core::ErrorCategory;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session,
    SessionService,
};
use chrono::{DateTime, Duration, SubsecRound, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

fn state(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect()
}

async fn create(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    initial: HashMap<String, Value>,
) -> String {
    service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: None,
            state: initial,
        })
        .await
        .expect("create session")
        .id()
        .to_string()
}

async fn get(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    num_recent_events: Option<usize>,
    after: Option<DateTime<Utc>>,
) -> Box<dyn Session> {
    service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events,
            after,
        })
        .await
        .expect("get session")
}

async fn append_for_identity(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: Event,
) {
    let identity = AdkIdentity::new(
        AppName::try_from(app_name).expect("valid app name"),
        UserId::try_from(user_id).expect("valid user id"),
        SessionId::try_from(session_id).expect("valid session id"),
    );
    service
        .append_event_for_identity(AppendEventRequest { identity, event })
        .await
        .expect("append event for identity");
}

fn event_with_delta(delta: HashMap<String, Value>) -> Event {
    let mut event = Event::new("inv-state");
    event.author = "agent".to_string();
    event.actions.state_delta = delta;
    event
}

/// Checks that `app:` and `user:` state written through one session is current in every
/// other session of the same app or user, on both `get` and `list`.
///
/// The reader session is created before the writer appends, so a backend that serves the
/// tiers from the reader's own stored copy returns the values from creation time.
pub async fn assert_shared_state_contract(
    service: &dyn SessionService,
    app_name: &str,
    user_1: &str,
    user_2: &str,
) {
    let reader = create(
        service,
        app_name,
        user_1,
        state(&[
            ("app:theme", json!("light")),
            ("user:language", json!("en")),
            ("topic", json!("reader")),
        ]),
    )
    .await;
    let writer = create(service, app_name, user_1, HashMap::new()).await;
    let other_user = create(service, app_name, user_2, HashMap::new()).await;

    append_for_identity(
        service,
        app_name,
        user_1,
        &writer,
        event_with_delta(state(&[
            ("app:theme", json!("dark")),
            ("app:flag", json!(true)),
            ("user:language", json!("fr")),
            ("topic", json!("writer")),
        ])),
    )
    .await;

    let reader_expected = state(&[
        ("app:theme", json!("dark")),
        ("app:flag", json!(true)),
        ("user:language", json!("fr")),
        ("topic", json!("reader")),
    ]);
    let fetched = get(service, app_name, user_1, &reader, None, None).await;
    assert_eq!(fetched.state().all(), reader_expected, "get serves the current app and user tiers");

    let listed = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list sessions");
    let listed_reader =
        listed.iter().find(|session| session.id() == reader).expect("reader session is listed");
    assert_eq!(
        listed_reader.state().all(),
        reader_expected,
        "list serves the current app and user tiers"
    );

    let fetched = get(service, app_name, user_2, &other_user, None, None).await;
    assert_eq!(
        fetched.state().all(),
        state(&[("app:theme", json!("dark")), ("app:flag", json!(true))]),
        "another user sees the app tier only"
    );

    // A null delta value overwrites the stored value; no backend drops it.
    append_for_identity(
        service,
        app_name,
        user_1,
        &writer,
        event_with_delta(state(&[("app:flag", Value::Null), ("user:language", Value::Null)])),
    )
    .await;
    let fetched = get(service, app_name, user_1, &reader, None, None).await;
    assert_eq!(
        fetched.state().all(),
        state(&[
            ("app:theme", json!("dark")),
            ("app:flag", Value::Null),
            ("user:language", Value::Null),
            ("topic", json!("reader")),
        ])
    );
}

/// Checks that concurrent appends never lose a state delta.
///
/// Every session receives two concurrent events, one through `append_event` and one
/// through `append_event_for_identity`, each writing its own app, user, and session keys.
pub async fn assert_concurrent_state_writes(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    sessions: usize,
) {
    let mut session_ids = Vec::with_capacity(sessions);
    for _ in 0..sessions {
        session_ids.push(create(service, app_name, user_id, HashMap::new()).await);
    }

    let mut appends: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = Vec::new();
    for (index, session_id) in session_ids.iter().enumerate() {
        let by_id = event_with_delta(state(&[
            (&format!("app:a{index}"), json!(index)),
            (&format!("user:a{index}"), json!(index)),
            ("by_id", json!(index)),
        ]));
        let by_identity = event_with_delta(state(&[
            (&format!("app:b{index}"), json!(index)),
            (&format!("user:b{index}"), json!(index)),
            ("by_identity", json!(index)),
        ]));
        appends.push(Box::pin(async move {
            service.append_event(session_id, by_id).await.expect("append event by id");
        }));
        appends.push(Box::pin(append_for_identity(
            service,
            app_name,
            user_id,
            session_id,
            by_identity,
        )));
    }
    futures::future::join_all(appends).await;

    let mut shared = HashMap::new();
    for index in 0..sessions {
        for tier in ["app", "user"] {
            for writer in ["a", "b"] {
                shared.insert(format!("{tier}:{writer}{index}"), json!(index));
            }
        }
    }
    for (index, session_id) in session_ids.iter().enumerate() {
        let fetched = get(service, app_name, user_id, session_id, None, None).await;
        let mut expected = shared.clone();
        expected.insert("by_id".to_string(), json!(index));
        expected.insert("by_identity".to_string(), json!(index));
        assert_eq!(fetched.state().all(), expected, "session {index} lost a concurrent delta");
        assert_eq!(fetched.events().len(), 2, "session {index} lost a concurrent event");
    }
}

/// `num_recent_events`, `after`, and the indexes of the events expected back.
type FilterCase = (Option<usize>, Option<DateTime<Utc>>, &'static [usize]);

/// Checks `num_recent_events` and `after`, alone and combined, against five events one
/// second apart.
pub async fn assert_event_filter_contract(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
) {
    let session_id = create(service, app_name, user_id, HashMap::new()).await;
    // Whole seconds, so backends that store milliseconds or microseconds round-trip exactly.
    let base = Utc::now().trunc_subsecs(0) - Duration::seconds(60);
    let at = |index: i64| base + Duration::seconds(index);

    let mut ids = Vec::new();
    for index in 0..5 {
        let mut event = Event::new(format!("inv-{index}"));
        event.author = "agent".to_string();
        event.timestamp = at(index);
        ids.push(event.id.clone());
        append_for_identity(service, app_name, user_id, &session_id, event).await;
    }

    let cases: [FilterCase; 9] = [
        (None, None, &[0, 1, 2, 3, 4]),
        (Some(2), None, &[3, 4]),
        (Some(0), None, &[]),
        (Some(10), None, &[0, 1, 2, 3, 4]),
        (None, Some(at(2)), &[2, 3, 4]),
        (None, Some(at(5)), &[]),
        (Some(2), Some(at(1)), &[3, 4]),
        (Some(4), Some(at(3)), &[3, 4]),
        (Some(1), Some(at(0)), &[4]),
    ];
    for (num_recent_events, after, expected) in cases {
        let fetched = get(service, app_name, user_id, &session_id, num_recent_events, after).await;
        let returned: Vec<(String, DateTime<Utc>)> =
            fetched.events().all().into_iter().map(|event| (event.id, event.timestamp)).collect();
        let expected: Vec<(String, DateTime<Utc>)> =
            expected.iter().map(|&index| (ids[index].clone(), at(index as i64))).collect();
        assert_eq!(returned, expected, "num_recent_events={num_recent_events:?} after={after:?}");
    }
}

async fn create_with_id(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    initial: HashMap<String, Value>,
) -> Box<dyn Session> {
    service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: Some(session_id.to_string()),
            state: initial,
        })
        .await
        .expect("create session")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Checks that `delete_all_sessions` erases what the user holds in the app — every session
/// with its events, and the `user:` tier — and nothing else: the `app:` tier, the user's
/// sessions in another app, and other users' sessions and `user:` values stay. A user whose
/// sessions were deleted one by one has the tier erased too, and a repeated call succeeds.
///
/// `shared_tiers` is `false` for a backend that keeps `app:` and `user:` keys in each
/// session instead of shared tiers (Vertex AI), where no value is read through another
/// session.
pub async fn assert_delete_all_sessions_contract(
    service: &dyn SessionService,
    app_name: &str,
    other_app_name: &str,
    shared_tiers: bool,
) {
    let (alice, bob, carol) = (unique("alice"), unique("bob"), unique("carol"));
    let session = |user: &str, n: u8| format!("{user}-s{n}");
    // Bob's session exists before Alice writes the app tier, so it reads her value through
    // the tier.
    create_with_id(
        service,
        app_name,
        &bob,
        &session(&bob, 1),
        state(&[("user:language", json!("sw")), ("app:motd", json!("karibu"))]),
    )
    .await;
    create_with_id(
        service,
        app_name,
        &alice,
        &session(&alice, 1),
        state(&[("user:language", json!("en")), ("app:theme", json!("dark"))]),
    )
    .await;
    create_with_id(service, app_name, &alice, &session(&alice, 2), HashMap::new()).await;
    append_for_identity(
        service,
        app_name,
        &alice,
        &session(&alice, 2),
        event_with_delta(state(&[("user:plan", json!("pro"))])),
    )
    .await;
    create_with_id(
        service,
        other_app_name,
        &alice,
        &session(&alice, 1),
        state(&[("user:language", json!("fr"))]),
    )
    .await;
    // Carol's only session is deleted on its own, which leaves her user tier behind.
    create_with_id(
        service,
        app_name,
        &carol,
        &session(&carol, 1),
        state(&[("user:language", json!("de"))]),
    )
    .await;
    service
        .delete(DeleteRequest {
            app_name: app_name.to_string(),
            user_id: carol.clone(),
            session_id: session(&carol, 1),
        })
        .await
        .expect("delete carol's session");

    service.delete_all_sessions(app_name, &alice).await.expect("delete all of alice's");
    service.delete_all_sessions(app_name, &carol).await.expect("delete all of carol's");

    let listed = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: alice.clone(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list alice's sessions");
    assert!(listed.is_empty(), "alice has no session left in the app");
    for n in [1, 2] {
        let missing = service
            .get(GetRequest {
                app_name: app_name.to_string(),
                user_id: alice.clone(),
                session_id: session(&alice, n),
                num_recent_events: None,
                after: None,
            })
            .await
            .err()
            .expect("a deleted session is not found");
        assert_eq!(missing.category, ErrorCategory::NotFound, "{missing}");
    }

    let fresh = create_with_id(service, app_name, &alice, &session(&alice, 3), HashMap::new())
        .await
        .state()
        .all();
    let fresh_carol =
        create_with_id(service, app_name, &carol, &session(&carol, 2), HashMap::new())
            .await
            .state()
            .all();
    let erased = |state: &HashMap<String, Value>| {
        state.keys().filter(|key| key.starts_with("user:")).cloned().collect::<Vec<_>>()
    };
    assert_eq!(erased(&fresh), Vec::<String>::new(), "alice's user tier is erased");
    assert_eq!(erased(&fresh_carol), Vec::<String>::new(), "carol's user tier is erased");

    let bob_state = get(service, app_name, &bob, &session(&bob, 1), None, None).await.state().all();
    assert_eq!(
        [bob_state.get("user:language"), bob_state.get("app:motd")],
        [Some(&json!("sw")), Some(&json!("karibu"))],
        "another user's session and user tier stay"
    );
    let elsewhere =
        get(service, other_app_name, &alice, &session(&alice, 1), None, None).await.state().all();
    assert_eq!(elsewhere.get("user:language"), Some(&json!("fr")), "another app's tier stays");
    if shared_tiers {
        assert_eq!(bob_state.get("app:theme"), Some(&json!("dark")), "the app tier stays");
        assert_eq!(
            fresh.get("app:theme"),
            Some(&json!("dark")),
            "a new session reads the app tier"
        );
    }

    service.delete_all_sessions(app_name, &alice).await.expect("delete all again");
    service.delete_all_sessions(app_name, &alice).await.expect("a repeated call succeeds");
    for (app, user) in [(app_name, &bob), (app_name, &carol), (other_app_name, &alice)] {
        service.delete_all_sessions(app, user).await.expect("clean up");
    }
}
