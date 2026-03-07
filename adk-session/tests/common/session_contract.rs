use adk_core::types::{SessionId, UserId};
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, KEY_PREFIX_TEMP, ListRequest, SessionService,
};
use chrono::{Duration, Utc};
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

pub async fn assert_session_contract_with_users(
    service: &dyn SessionService,
    app_name: &str,
    other_app_name: &str,
    user_1: &str,
    user_2: &str,
) {
    let mut initial_state = HashMap::new();
    initial_state.insert("app:locale".to_string(), json!("en-US"));
    initial_state.insert("user:name".to_string(), json!("alice"));
    initial_state.insert("session_key".to_string(), json!("seed"));
    initial_state.insert(format!("{}create", KEY_PREFIX_TEMP), json!("drop-me"));

    let created = service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: None,
            state: initial_state,
        })
        .await
        .expect("create session should succeed");

    let session_id = created.id().clone();
    assert!(!session_id.to_string().is_empty());
    assert_eq!(created.app_name(), app_name);
    assert_eq!(created.user_id().as_str(), user_1);

    let fetched = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: SessionId::new(session_id.clone()).unwrap(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get session should succeed");

    assert_eq!(fetched.id(), &session_id);
    assert_eq!(fetched.state().get("app:locale"), Some(json!("en-US")));
    assert_eq!(fetched.state().get("user:name"), Some(json!("alice")));
    assert_eq!(fetched.state().get("session_key"), Some(json!("seed")));
    assert_eq!(fetched.state().get("temp:create"), None);

    let t1 = Utc::now();
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

    service.append_event(&session_id, event_2).await.expect("append second event should succeed");

    let with_events = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: SessionId::new(session_id.clone()).unwrap(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get after append should succeed");

    assert_eq!(with_events.events().len(), 2);
    assert_eq!(with_events.state().get("result"), Some(json!("ok-2")));
    assert_eq!(with_events.state().get("temp:event"), None);

    let recent = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: SessionId::new(session_id.clone()).unwrap(),
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
            user_id: UserId::from(user_1.to_string()),
            session_id: SessionId::new(session_id.clone()).unwrap(),
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
            user_id: UserId::from(user_1.to_string()),
        })
        .await
        .expect("list for user1 should succeed");

    assert!(sessions_user1.iter().any(|session| session.id() == &session_id));

    let user2 = service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_2.to_string()),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .expect("create session for user2 should succeed");
    let user2_session_id = user2.id().clone();

    let wrong_user_get = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_2.to_string()),
            session_id: SessionId::new(session_id.clone()).unwrap(),
            num_recent_events: None,
            after: None,
        })
        .await;
    assert!(wrong_user_get.is_err());

    let sessions_user2 = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_2.to_string()),
        })
        .await
        .expect("list for user2 should succeed");

    assert!(sessions_user2.iter().any(|session| session.id() == &user2_session_id));
    assert!(!sessions_user2.iter().any(|session| session.id() == &session_id));

    let other_app = service
        .create(CreateRequest {
            app_name: other_app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .expect("create session for second app should succeed");
    let other_app_session_id = other_app.id().clone();

    let sessions_primary_app = service
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
        })
        .await
        .expect("list primary app should succeed");

    assert!(sessions_primary_app.iter().any(|session| session.id() == &session_id));
    assert!(!sessions_primary_app.iter().any(|session| session.id() == &other_app_session_id));

    let sessions_other_app = service
        .list(ListRequest {
            app_name: other_app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
        })
        .await
        .expect("list secondary app should succeed");

    assert!(sessions_other_app.iter().any(|session| session.id() == &other_app_session_id));
    assert!(!sessions_other_app.iter().any(|session| session.id() == &session_id));

    service
        .delete(DeleteRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: SessionId::new(session_id.clone()).unwrap(),
        })
        .await
        .expect("delete should succeed");

    service
        .delete(DeleteRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_2.to_string()),
            session_id: user2_session_id,
        })
        .await
        .expect("delete secondary user session should succeed");

    service
        .delete(DeleteRequest {
            app_name: other_app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id: other_app_session_id,
        })
        .await
        .expect("delete secondary app session should succeed");

    let deleted_get = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: UserId::from(user_1.to_string()),
            session_id,
            num_recent_events: None,
            after: None,
        })
        .await;
    assert!(deleted_get.is_err());
}
