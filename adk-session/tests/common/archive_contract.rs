//! The session archive contract every backend passes: an archived session keeps its events
//! and state, leaves the active listing, refuses new turns, exports as a bundle, and comes
//! back unarchived; the `app:` and `user:` tiers do not change.
// Each test binary that declares `mod common` uses a different subset of these checks.
#![allow(dead_code)]

use adk_core::identity::AdkIdentity;
use adk_core::{Content, ErrorCategory};
use adk_session::{
    ARCHIVE_AUTHOR, ARCHIVE_KEY, AppendEventRequest, ArchiveMark, CreateRequest, Event,
    ListRequest, SESSION_BUNDLE_FORMAT, Session, SessionBundle, SessionService, ensure_writable,
};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};

async fn create(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    state: HashMap<String, Value>,
) -> AdkIdentity {
    service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: None,
            state,
        })
        .await
        .expect("session is created")
        .try_identity()
        .expect("valid identity")
}

async fn say(service: &dyn SessionService, identity: &AdkIdentity, author: &str, text: &str) {
    let mut event = Event::new("inv-archive");
    event.author = author.to_string();
    let role = if author == "user" { "user" } else { "model" };
    event.llm_response.content = Some(Content::new(role).with_text(text));
    service
        .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
        .await
        .expect("event is appended");
}

fn listing(app_name: &str, user_id: &str) -> ListRequest {
    ListRequest {
        app_name: app_name.to_string(),
        user_id: user_id.to_string(),
        limit: None,
        offset: None,
    }
}

fn ids(sessions: &[Box<dyn Session>]) -> BTreeSet<String> {
    sessions.iter().map(|session| session.id().to_string()).collect()
}

/// Archive, list, refuse, export and unarchive one session among three on `service`.
pub async fn assert_archive_contract(service: &dyn SessionService, app_name: &str) {
    let user = "archivist";
    let tiers: HashMap<String, Value> = [
        ("topic".to_string(), json!("trip")),
        ("user:tone".to_string(), json!("brief")),
        ("app:region".to_string(), json!("eu")),
    ]
    .into();
    let kept_one = create(service, app_name, user, HashMap::new()).await;
    let archived = create(service, app_name, user, tiers).await;
    let kept_two = create(service, app_name, user, HashMap::new()).await;
    say(service, &archived, "user", "Book the train to Lyon.").await;
    say(service, &archived, "agent", "Booked: the 09:12 to Lyon.").await;

    // Archiving keeps every event and the state, and adds the mark.
    let mark = service
        .archive(&archived, Some("trip booked".into()))
        .await
        .expect("the session is archived");
    assert_eq!(mark.reason.as_deref(), Some("trip booked"));
    let again = service.archive(&archived, Some("other".into())).await.expect("archive again");
    assert_eq!(again, mark, "archiving an archived session returns its mark");
    let session = service.get_for_identity(&archived).await.expect("an archived session reads");
    let events = session.events().all();
    assert_eq!(events.len(), 3, "two turns and one marker, the repeated archive wrote nothing");
    assert_eq!(events[2].author, ARCHIVE_AUTHOR);
    assert!(events[2].llm_response.content.is_none(), "a marker carries no content");
    assert_eq!(ArchiveMark::of(session.as_ref()), Some(mark.clone()));
    assert_eq!(session.state().get("topic"), Some(json!("trip")));
    assert_eq!(session.state().get("user:tone"), Some(json!("brief")));
    assert_eq!(session.state().get("app:region"), Some(json!("eu")));

    // Read-only: nothing is added to it.
    let refused = ensure_writable(session.as_ref()).expect_err("an archived session is refused");
    assert_eq!(refused.code, "session.archived");
    assert_eq!(refused.category, ErrorCategory::Conflict);
    let kept = service.get_for_identity(&kept_one).await.expect("a kept session reads");
    ensure_writable(kept.as_ref()).expect("a kept session is writable");

    // Hidden from the active listing, found in the archived one; `list` lists all three.
    let everyone = service.list(listing(app_name, user)).await.expect("list");
    assert_eq!(everyone.len(), 3);
    let active = service.list_active(listing(app_name, user)).await.expect("list_active");
    assert_eq!(
        ids(&active),
        [kept_one.session_id.to_string(), kept_two.session_id.to_string()].into()
    );
    let shelf = service.list_archived(listing(app_name, user)).await.expect("list_archived");
    assert_eq!(ids(&shelf), [archived.session_id.to_string()].into());
    // Offset and limit count only the sessions the listing keeps.
    let paged = service
        .list_active(ListRequest { limit: Some(1), offset: Some(1), ..listing(app_name, user) })
        .await
        .expect("a page of list_active");
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].id(), active[1].id());
    let none = service
        .list_active(ListRequest { limit: Some(0), ..listing(app_name, user) })
        .await
        .expect("an empty page");
    assert!(none.is_empty());

    // The bundle holds the conversation, the session's own state and the mark, and reads back.
    let bundle = service.export_session(&archived).await.expect("export");
    assert_eq!(bundle.format, SESSION_BUNDLE_FORMAT);
    assert_eq!(bundle.identity().expect("bundle identity"), archived);
    assert_eq!(bundle.archived, Some(mark.clone()));
    assert_eq!(bundle.events.len(), 3);
    assert_eq!(bundle.state.get("topic"), Some(&json!("trip")));
    assert!(bundle.state.contains_key(ARCHIVE_KEY));
    assert!(
        !bundle.state.keys().any(|key| key.starts_with("user:") || key.starts_with("app:")),
        "the shared tiers stay out of a bundle: {:?}",
        bundle.state.keys()
    );
    let json = serde_json::to_string(&bundle).expect("bundle serializes");
    let read: SessionBundle = serde_json::from_str(&json).expect("bundle reads back");
    assert_eq!(read.events.len(), 3);
    assert_eq!(
        read.events[1].llm_response.content.as_ref().and_then(|content| content.parts[0].text()),
        Some("Booked: the 09:12 to Lyon.")
    );

    // A fork of the archived session is a new, active session: it keeps the conversation but
    // neither the mark nor the events that set it.
    let fork = service
        .fork(adk_session::ForkRequest::new(archived.clone()))
        .await
        .expect("an archived session forks");
    assert!(ArchiveMark::of(fork.session.as_ref()).is_none(), "the fork is not archived");
    ensure_writable(fork.session.as_ref()).expect("the fork takes turns");
    assert!(fork.session.events().all().iter().all(|event| event.author != ARCHIVE_AUTHOR));
    assert_eq!(fork.session.events().len(), 2, "the conversation without the marker");
    service
        .delete(adk_session::DeleteRequest {
            app_name: app_name.to_string(),
            user_id: user.to_string(),
            session_id: fork.session.id().to_string(),
        })
        .await
        .expect("the fork is deleted");

    // Unarchived, it lists and takes turns again; unarchiving again writes nothing.
    service.unarchive(&archived).await.expect("unarchive");
    service.unarchive(&archived).await.expect("unarchive again");
    let session = service.get_for_identity(&archived).await.expect("read");
    assert_eq!(ArchiveMark::of(session.as_ref()), None);
    ensure_writable(session.as_ref()).expect("an unarchived session is writable");
    assert_eq!(session.events().len(), 4, "one marker to archive, one to unarchive");
    let active = service.list_active(listing(app_name, user)).await.expect("list_active");
    assert_eq!(active.len(), 3);
    assert!(service.list_archived(listing(app_name, user)).await.expect("archived").is_empty());
    say(service, &archived, "user", "And the return?").await;
    // The shared tiers never changed.
    let session = service.get_for_identity(&archived).await.expect("read");
    assert_eq!(session.state().get("user:tone"), Some(json!("brief")));
    assert_eq!(session.state().get("app:region"), Some(json!("eu")));

    // A session that does not exist is not found.
    let missing = AdkIdentity::new(
        archived.app_name.clone(),
        archived.user_id.clone(),
        adk_core::SessionId::try_from("no-such-session").expect("valid id"),
    );
    let error = service.archive(&missing, None).await.expect_err("missing session");
    assert_eq!(error.category, ErrorCategory::NotFound);
    let error = service.export_session(&missing).await.expect_err("missing session");
    assert_eq!(error.category, ErrorCategory::NotFound);
}
