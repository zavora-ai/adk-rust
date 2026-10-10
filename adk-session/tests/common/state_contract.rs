// Each test binary that declares `mod common` uses a different subset of these checks.
#![allow(dead_code)]

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{
    AppendEventRequest, CreateRequest, Event, GetRequest, ListRequest, Session, SessionService,
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
