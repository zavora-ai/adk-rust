//! `ConsentGatedSessionService` keeps a visitor's conversation and profile out of durable
//! storage until the visitor consents, and refuses writes while the visitor is erased.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::subject::{
    CONSENT_REQUIRED_CODE, ConsentGate, ConsentPurpose, ConsentRecord, ConsentStore,
    InMemoryConsentStore, InMemorySubjectRegistry, SUBJECT_ERASING_CODE, SubjectRegistry,
    SubjectState, VisitorId,
};
use adk_session::{
    AppendEventRequest, ConsentGatedSessionService, CreateRequest, Event, GetRequest,
    InMemorySessionService, ListRequest, Session, SessionService,
};
use serde_json::json;

struct Fixture {
    durable: Arc<InMemorySessionService>,
    consent: Arc<InMemoryConsentStore>,
    registry: Arc<InMemorySubjectRegistry>,
    gated: ConsentGatedSessionService,
}

fn fixture() -> Fixture {
    let durable = Arc::new(InMemorySessionService::new());
    let consent = Arc::new(InMemoryConsentStore::new());
    let registry = Arc::new(InMemorySubjectRegistry::new());
    let gate = ConsentGate::new(consent.clone()).with_registry(registry.clone());
    let gated = ConsentGatedSessionService::new(durable.clone(), gate);
    Fixture { durable, consent, registry, gated }
}

impl Fixture {
    async fn grant(&self, visitor: &VisitorId, purpose: ConsentPurpose) {
        self.consent
            .grant(ConsentRecord::new(visitor.subject(), purpose, "2026-10"))
            .await
            .unwrap();
    }

    async fn durable_ids(&self, user: &str) -> Vec<String> {
        self.durable
            .list(ListRequest {
                app_name: "shop".into(),
                user_id: user.into(),
                limit: None,
                offset: None,
            })
            .await
            .unwrap()
            .iter()
            .map(|session| session.id().to_string())
            .collect()
    }
}

fn create(user: &str, session: &str) -> CreateRequest {
    CreateRequest {
        app_name: "shop".into(),
        user_id: user.into(),
        session_id: Some(session.into()),
        state: HashMap::from([
            ("user:name".to_string(), json!("Wanjiru")),
            ("cart".to_string(), json!(["kikoi"])),
        ]),
    }
}

fn append(user: &str, session: &str) -> AppendEventRequest {
    let mut event = Event::new("inv-1");
    event.author = "agent".into();
    event.actions.state_delta = HashMap::from([
        ("user:city".to_string(), json!("Mombasa")),
        ("step".to_string(), json!(2)),
    ]);
    AppendEventRequest {
        identity: AdkIdentity::new(
            AppName::try_from("shop").unwrap(),
            UserId::try_from(user).unwrap(),
            SessionId::try_from(session).unwrap(),
        ),
        event,
    }
}

async fn read(service: &dyn SessionService, user: &str, session: &str) -> Box<dyn Session> {
    service
        .get(GetRequest {
            app_name: "shop".into(),
            user_id: user.into(),
            session_id: session.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn without_consent_nothing_lasting_is_written() {
    let fx = fixture();
    let visitor = VisitorId::generate();
    let user = visitor.as_str();
    let session = fx.gated.create(create(user, "s1")).await.unwrap();
    assert_eq!(session.state().get("user:name"), None, "no profile consent");
    assert_eq!(session.state().get("cart"), Some(json!(["kikoi"])));
    fx.gated.append_event_for_identity(append(user, "s1")).await.unwrap();

    assert!(fx.durable_ids(user).await.is_empty(), "no continuity consent: nothing durable");
    let read_back = read(&fx.gated, user, "s1").await;
    assert_eq!(read_back.events().len(), 1);
    assert_eq!(read_back.state().get("user:city"), None);
    assert_eq!(read_back.state().get("step"), Some(json!(2)));
}

#[tokio::test]
async fn with_consent_sessions_and_profiles_persist() {
    let fx = fixture();
    let visitor = VisitorId::generate();
    let user = visitor.as_str();
    fx.grant(&visitor, ConsentPurpose::SessionContinuity).await;
    fx.grant(&visitor, ConsentPurpose::PersistentProfile).await;

    fx.gated.create(create(user, "s1")).await.unwrap();
    fx.gated.append_event_for_identity(append(user, "s1")).await.unwrap();
    assert_eq!(fx.durable_ids(user).await, vec!["s1".to_string()]);
    let durable = read(fx.durable.as_ref(), user, "s1").await;
    assert_eq!(durable.state().get("user:name"), Some(json!("Wanjiru")));
    assert_eq!(durable.state().get("user:city"), Some(json!("Mombasa")));
}

#[tokio::test]
async fn revoking_continuity_stops_durable_writes_without_erasing() {
    let fx = fixture();
    let visitor = VisitorId::generate();
    let user = visitor.as_str();
    fx.grant(&visitor, ConsentPurpose::SessionContinuity).await;
    fx.gated.create(create(user, "s1")).await.unwrap();
    fx.consent
        .revoke_purpose(
            &visitor.subject(),
            Some(&ConsentPurpose::SessionContinuity),
            chrono::Utc::now(),
        )
        .await
        .unwrap();

    let refused = fx.gated.append_event_for_identity(append(user, "s1")).await.unwrap_err();
    assert_eq!(refused.code, CONSENT_REQUIRED_CODE);
    assert_eq!(fx.durable_ids(user).await, vec!["s1".to_string()], "revoking erases nothing");

    fx.gated.create(create(user, "s2")).await.unwrap();
    assert_eq!(fx.durable_ids(user).await, vec!["s1".to_string()], "new sessions stay ephemeral");
    let listed = fx
        .gated
        .list(ListRequest {
            app_name: "shop".into(),
            user_id: user.into(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 2, "reads reach both stores");

    fx.gated.delete_all_sessions("shop", user).await.unwrap();
    let listed = fx
        .gated
        .list(ListRequest {
            app_name: "shop".into(),
            user_id: user.into(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    assert!(listed.is_empty(), "erasure reaches both stores");
}

#[tokio::test]
async fn a_subject_being_erased_cannot_write() {
    let fx = fixture();
    let visitor = VisitorId::generate();
    let user = visitor.as_str();
    fx.gated.create(create(user, "s1")).await.unwrap();
    fx.registry.set_state(&visitor.subject(), SubjectState::Erasing).await.unwrap();

    let refused = fx.gated.create(create(user, "s2")).await.err().expect("create is refused");
    assert_eq!(refused.code, SUBJECT_ERASING_CODE);
    assert_eq!(
        fx.gated.append_event_for_identity(append(user, "s1")).await.unwrap_err().code,
        SUBJECT_ERASING_CODE
    );
}

#[tokio::test]
async fn ungated_users_pass_straight_through() {
    let fx = fixture();
    let session = fx.gated.create(create("alice", "s1")).await.unwrap();
    assert_eq!(session.state().get("user:name"), Some(json!("Wanjiru")));
    fx.gated.append_event_for_identity(append("alice", "s1")).await.unwrap();
    assert_eq!(fx.durable_ids("alice").await, vec!["s1".to_string()]);
}
