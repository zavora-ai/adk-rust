use adk_core::ErrorCategory;
use adk_core::identity::MAX_ID_LEN;
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, KEY_PREFIX_TEMP, ListRequest, SessionService,
};
use chrono::{Duration, SubsecRound, Utc};
use serde_json::json;
use std::collections::HashMap;

#[allow(dead_code)]
pub async fn assert_session_contract(
    service: &dyn SessionService,
    app_name: &str,
    other_app_name: &str,
) {
    assert_session_contract_with_users(service, app_name, other_app_name, "user1", "user2").await;
}

/// Checks that every method rejects an invalid app name, user ID, or session ID (empty, over
/// [`MAX_ID_LEN`] bytes, or containing NUL) with `session.invalid_identifier`
/// (`InvalidInput`).
///
/// The contract names only identifiers no call can store, so it runs against a live backend
/// without leaving data behind, and against an unreachable one: a backend that validates
/// before its first network call returns `session.invalid_identifier`, one that does not
/// returns the connection error instead.
pub async fn assert_identifier_validation_contract(service: &dyn SessionService) {
    let too_long = "x".repeat(MAX_ID_LEN + 1);
    let mut accepted = Vec::new();
    let mut check = |operation: &str, outcome: adk_core::Result<()>| match outcome {
        Err(error)
            if error.category == ErrorCategory::InvalidInput
                && error.code == "session.invalid_identifier" => {}
        other => accepted.push(format!("{operation}: {other:?}")),
    };

    let create = |app: &str, user: &str, session: Option<&str>| CreateRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        session_id: session.map(str::to_string),
        state: HashMap::new(),
    };
    for (operation, request) in [
        ("create with an empty app_name", create("", "user", Some("session"))),
        ("create with a NUL in user_id", create("app", "us\0er", Some("session"))),
        ("create with an oversized session_id", create("app", "user", Some(too_long.as_str()))),
        ("create with an empty session_id", create("app", "user", Some(""))),
        ("create with an empty user_id and no session_id", create("app", "", None)),
        ("create with an oversized app_name and no session_id", create(&too_long, "user", None)),
    ] {
        check(operation, service.create(request).await.map(|_| ()));
    }

    let get = |app: &str, user: &str, session: &str| GetRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        session_id: session.to_string(),
        num_recent_events: None,
        after: None,
    };
    for (operation, request) in [
        ("get with an empty app_name", get("", "user", "session")),
        ("get with a NUL in session_id", get("app", "user", "sess\0ion")),
    ] {
        check(operation, service.get(request).await.map(|_| ()));
    }

    let list = |app: &str, user: &str| ListRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        limit: None,
        offset: None,
    };
    for (operation, request) in [
        ("list with an empty user_id", list("app", "")),
        ("list with a NUL in app_name", list("a\0pp", "user")),
    ] {
        check(operation, service.list(request).await.map(|_| ()));
    }

    let delete = |app: &str, user: &str, session: &str| DeleteRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        session_id: session.to_string(),
    };
    for (operation, request) in [
        ("delete with an empty app_name", delete("", "user", "session")),
        ("delete with an oversized user_id", delete("app", &too_long, "session")),
        ("delete with a NUL in session_id", delete("app", "user", "sess\0ion")),
    ] {
        check(operation, service.delete(request).await);
    }

    for (operation, app, user) in [
        ("delete_all_sessions with an empty app_name", "", "user"),
        ("delete_all_sessions with a NUL in user_id", "app", "us\0er"),
    ] {
        check(operation, service.delete_all_sessions(app, user).await);
    }

    for (operation, session) in [
        ("append_event with an empty session_id", ""),
        ("append_event with a NUL in session_id", "sess\0ion"),
        ("append_event with an oversized session_id", too_long.as_str()),
    ] {
        check(operation, service.append_event(session, Event::new("inv-invalid")).await);
    }

    assert!(accepted.is_empty(), "invalid identifiers were not rejected:\n{}", accepted.join("\n"));
}

pub async fn assert_session_contract_with_users(
    service: &dyn SessionService,
    app_name: &str,
    other_app_name: &str,
    user_1: &str,
    user_2: &str,
) {
    assert_identifier_validation_contract(service).await;

    let missing = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: "contract-missing".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("a missing session must return an error");
    assert!(missing.is_not_found());
    assert_eq!(missing.code, "session.not_found");

    let invalid = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: String::new(),
            session_id: "contract-invalid".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("an invalid session identity must return an error");
    assert_eq!(invalid.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(invalid.code, "session.invalid_identifier");

    let mut initial_state = HashMap::new();
    initial_state.insert("app:locale".to_string(), json!("en-US"));
    initial_state.insert("user:name".to_string(), json!("alice"));
    initial_state.insert("session_key".to_string(), json!("seed"));
    initial_state.insert(format!("{}create", KEY_PREFIX_TEMP), json!("drop-me"));

    let created = service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: None,
            state: initial_state,
        })
        .await
        .expect("create session should succeed");

    let session_id = created.id().to_string();
    assert!(!session_id.is_empty());
    assert_eq!(created.app_name(), app_name);
    assert_eq!(created.user_id(), user_1);

    let fetched = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get session should succeed");

    assert_eq!(fetched.id(), session_id);
    assert_eq!(fetched.state().get("app:locale"), Some(json!("en-US")));
    assert_eq!(fetched.state().get("user:name"), Some(json!("alice")));
    assert_eq!(fetched.state().get("session_key"), Some(json!("seed")));
    assert_eq!(fetched.state().get("temp:create"), None);

    // Microseconds: the finest precision every backend stores (PostgreSQL TIMESTAMPTZ).
    let t1 = Utc::now().trunc_subsecs(6);
    let t2 = t1 + Duration::seconds(1);

    let mut event_1 = Event::new("inv-1");
    event_1.author = "agent".to_string();
    event_1.timestamp = t1;
    event_1.actions.state_delta.insert("result".to_string(), json!("ok-1"));
    event_1.actions.state_delta.insert(format!("{}event", KEY_PREFIX_TEMP), json!("skip"));

    service.append_event(&session_id, event_1).await.expect("append first event should succeed");

    let mut event_2 = Event::new("inv-2");
    event_2.author = "agent".to_string();
    event_2.timestamp = t2;
    event_2.actions.state_delta.insert("result".to_string(), json!("ok-2"));
    // The runner's screening marker decides whether a saved message is screened again.
    event_2.actions.input_screened = true;

    service.append_event(&session_id, event_2).await.expect("append second event should succeed");

    let with_events = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get after append should succeed");

    assert_eq!(with_events.events().len(), 2);
    assert!(!with_events.events().at(0).expect("event 0").actions.input_screened);
    assert!(with_events.events().at(1).expect("event 1").actions.input_screened);
    assert_eq!(with_events.state().get("result"), Some(json!("ok-2")));
    assert_eq!(with_events.state().get("temp:event"), None);

    let recent = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: session_id.clone(),
            num_recent_events: Some(1),
            after: None,
        })
        .await
        .expect("get recent events should succeed");

    assert_eq!(recent.events().len(), 1);
    assert_eq!(recent.events().at(0).expect("event 0").timestamp, t2);

    let after = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: Some(t2),
        })
        .await
        .expect("get with after filter should succeed");

    assert_eq!(after.events().len(), 1);
    assert_eq!(after.events().at(0).expect("event 0").timestamp, t2);

    let sessions_user1 = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list for user1 should succeed");

    assert!(sessions_user1.iter().any(|session| session.id() == session_id));

    let user2 = service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_2.to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .expect("create session for user2 should succeed");
    let user2_session_id = user2.id().to_string();

    let wrong_user_get = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_2.to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("a session owned by another user must not be returned");
    assert!(wrong_user_get.is_not_found());
    assert_eq!(wrong_user_get.code, "session.not_found");

    let sessions_user2 = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: user_2.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list for user2 should succeed");

    assert!(sessions_user2.iter().any(|session| session.id() == user2_session_id));
    assert!(!sessions_user2.iter().any(|session| session.id() == session_id));

    let other_app = service
        .create(CreateRequest {
            app_name: other_app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .expect("create session for second app should succeed");
    let other_app_session_id = other_app.id().to_string();

    let sessions_primary_app = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list primary app should succeed");

    assert!(sessions_primary_app.iter().any(|session| session.id() == session_id));
    assert!(!sessions_primary_app.iter().any(|session| session.id() == other_app_session_id));

    let sessions_other_app = service
        .list(ListRequest {
            app_name: other_app_name.to_string(),
            user_id: user_1.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list secondary app should succeed");

    assert!(sessions_other_app.iter().any(|session| session.id() == other_app_session_id));
    assert!(!sessions_other_app.iter().any(|session| session.id() == session_id));

    service
        .delete(DeleteRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: session_id.clone(),
        })
        .await
        .expect("delete should succeed");

    service
        .delete(DeleteRequest {
            app_name: app_name.to_string(),
            user_id: user_2.to_string(),
            session_id: user2_session_id,
        })
        .await
        .expect("delete secondary user session should succeed");

    service
        .delete(DeleteRequest {
            app_name: other_app_name.to_string(),
            user_id: user_1.to_string(),
            session_id: other_app_session_id,
        })
        .await
        .expect("delete secondary app session should succeed");

    let deleted_get = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_1.to_string(),
            session_id,
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("a deleted session must return an error");
    assert!(deleted_get.is_not_found());
    assert_eq!(deleted_get.code, "session.not_found");
}
