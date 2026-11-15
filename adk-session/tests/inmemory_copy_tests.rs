//! What `InMemorySessionService::get` copies: only the requested window of events, and none of
//! them under the lock every session shares. Bytes are counted per thread, so the counts hold
//! under `cargo test` as well as nextest.

use adk_core::{AdkIdentity, AppName, Content, Event, SessionId, UserId};
use adk_session::{
    AppendEventRequest, CreateRequest, GetRequest, InMemorySessionService, SessionService,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;

/// The system allocator, counting the bytes each thread asks for.
struct ThreadCounting;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

fn count(bytes: usize) {
    // A thread being torn down has no counter left; its bytes are not measured.
    let _ = ALLOCATED.try_with(|allocated| allocated.set(allocated.get() + bytes));
}

// SAFETY: every call forwards to `System` with the caller's layout and pointer unchanged.
unsafe impl GlobalAlloc for ThreadCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: ThreadCounting = ThreadCounting;

fn allocated() -> usize {
    ALLOCATED.with(Cell::get)
}

const EVENTS: usize = 2_000;

/// A session of [`EVENTS`] events, each carrying 1 KiB of text: about 4 MB to copy.
async fn long_session() -> InMemorySessionService {
    let service = InMemorySessionService::new();
    service
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("long".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let identity = AdkIdentity::new(
        AppName::try_from("app").unwrap(),
        UserId::try_from("user").unwrap(),
        SessionId::try_from("long").unwrap(),
    );
    for index in 0..EVENTS {
        let mut event = Event::new("inv");
        event.author = "agent".to_string();
        event.llm_response.content =
            Some(Content::new("model").with_text(format!("{index:04} {}", "x".repeat(1024))));
        service
            .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
            .await
            .unwrap();
    }
    service
}

fn request(num_recent_events: Option<usize>) -> GetRequest {
    GetRequest {
        app_name: "app".to_string(),
        user_id: "user".to_string(),
        session_id: "long".to_string(),
        num_recent_events,
        after: None,
    }
}

#[tokio::test]
async fn a_window_copies_only_the_events_it_returns() {
    let service = long_session().await;

    let before = allocated();
    let mut session = service.get(request(Some(10))).await.unwrap();
    let events = session.take_events();
    let copied = allocated() - before;

    let texts: Vec<String> = events
        .iter()
        .map(|event| event.llm_response.content.as_ref().unwrap().parts[0].text().unwrap().into())
        .collect();
    let expected: Vec<String> =
        (EVENTS - 10..EVENTS).map(|index| format!("{index:04} {}", "x".repeat(1024))).collect();
    assert_eq!(texts, expected);
    // Ten events of about 2 KiB each; a copy of the whole session is about 4 MB.
    assert!(copied < 128 * 1024, "a 10-event window allocated {copied} bytes");
}

#[tokio::test]
async fn a_read_copies_event_pointers_only() {
    let service = long_session().await;

    let before = allocated();
    let mut session = service.get(request(None)).await.unwrap();
    let read = allocated() - before;
    assert_eq!(session.events().len(), EVENTS);
    // `get` holds the store's lock; the events are copied later, by whoever takes them.
    assert!(read < 64 * EVENTS, "a read of {EVENTS} events allocated {read} bytes");

    let before = allocated();
    let events = session.take_events();
    let taken = allocated() - before;
    assert_eq!(events.len(), EVENTS);
    assert!(taken > 1024 * EVENTS, "taking {EVENTS} events allocated only {taken} bytes");
}
