//! `SessionService::append_event_returning_stored` defaults to the event it was given.

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{
    AppendEventRequest, CreateRequest, Event, GetRequest, InMemorySessionService, SessionService,
};

#[tokio::test]
async fn the_default_stores_the_event_and_returns_it_unchanged() {
    let service = InMemorySessionService::new();
    service
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut event = Event::new("inv-1");
    event.author = "agent".into();
    event.llm_response.content = Some(adk_core::Content::new("model").with_text("hello"));

    let stored = service
        .append_event_returning_stored(AppendEventRequest {
            identity: AdkIdentity::new(
                AppName::try_from("app").unwrap(),
                UserId::try_from("user").unwrap(),
                SessionId::try_from("s1").unwrap(),
            ),
            event: event.clone(),
        })
        .await
        .unwrap();

    assert_eq!(stored.id, event.id);
    assert_eq!(stored.llm_response.content, event.llm_response.content);
    let session = service
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "s1".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    assert_eq!(session.events().all().len(), 1);
}
