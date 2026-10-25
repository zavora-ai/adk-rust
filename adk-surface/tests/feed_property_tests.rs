//! The feed resumes without a gap or a duplicate (spec 3.3 §6): events are numbered by their
//! position in the session store, and a surface that disconnects at any point and reconnects
//! with the last `seq` it received (`Last-Event-ID`) gets every later event exactly once, in
//! order, while the session keeps growing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{ApprovalDecision, ApprovalRequest, Content, Event};
use adk_surface::{Caller, FeedFrame, Origin, Source, SurfaceEngine, SurfaceKind};
use async_trait::async_trait;
use futures::StreamExt;
use proptest::prelude::*;

/// A session store holding one growing session.
#[derive(Default)]
struct GrowingSession {
    events: Mutex<Vec<Event>>,
}

impl GrowingSession {
    fn append(&self, count: usize) {
        let mut events = self.events.lock().unwrap();
        for _ in 0..count {
            let n = events.len() + 1;
            let mut event = Event::new("inv");
            event.author = if n.is_multiple_of(2) { "user".into() } else { "finance".into() };
            event.llm_response.content =
                Some(Content::new("model").with_text(format!("message {n}")));
            events.push(event);
        }
    }

    fn len(&self) -> u64 {
        self.events.lock().unwrap().len() as u64
    }
}

struct Store(Arc<GrowingSession>);

#[async_trait]
impl Source for Store {
    fn app_name(&self) -> &str {
        "app"
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        Ok((user == "ana" && session == "s1").then(|| self.0.events.lock().unwrap().clone()))
    }

    async fn changed(&self, _max: Duration) {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    async fn requests(
        &self,
        _caller: &Caller,
        _session: Option<&str>,
        _open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }

    async fn request(&self, _id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        Ok(None)
    }

    async fn decide(
        &self,
        id: &str,
        _version: u64,
        _decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        Err(adk_surface::SurfaceError::not_found(id))
    }
}

fn me() -> Caller {
    Caller::human("ana", SurfaceKind::Phone, Origin::Touch)
}

/// One connection: events appended before it opens, how many entries it reads before it
/// drops, and events appended after each entry it reads.
#[derive(Debug, Clone)]
struct Connection {
    before: usize,
    reads: usize,
    during: usize,
}

fn connections() -> impl Strategy<Value = Vec<Connection>> {
    prop::collection::vec(
        (0usize..6, 0usize..8, 0usize..3).prop_map(|(before, reads, during)| Connection {
            before,
            reads,
            during,
        }),
        1..8,
    )
}

/// Reads one connection's worth of entries after `last`.
async fn read(
    engine: &SurfaceEngine,
    store: &Arc<GrowingSession>,
    last: Option<u64>,
    reads: usize,
    during: usize,
    received: &mut Vec<u64>,
) -> Option<u64> {
    let mut last = last;
    let mut feed = engine.feed(me(), "s1".into(), last);
    let mut taken = 0;
    while taken < reads && last.unwrap_or(0) < store.len() {
        let frame = tokio::time::timeout(Duration::from_secs(5), feed.next())
            .await
            .expect("the feed delivers the next stored event")
            .expect("the feed stays open")
            .expect("the feed reads the store");
        if let FeedFrame::Entry { entry } = frame {
            received.push(entry.seq);
            last = Some(entry.seq);
            taken += 1;
            store.append(during);
        }
    }
    last
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn reconnecting_never_skips_or_repeats_an_event(plan in connections()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(async {
            let store = Arc::new(GrowingSession::default());
            let engine = SurfaceEngine::builder(Arc::new(Store(Arc::clone(&store)))).build();
            let mut received = Vec::new();
            let mut last = None;
            for connection in &plan {
                store.append(connection.before);
                last = read(&engine, &store, last, connection.reads, connection.during, &mut received).await;
            }
            // A last reconnect catches up with everything stored.
            last = read(&engine, &store, last, usize::MAX, 0, &mut received).await;
            let expected: Vec<u64> = (1..=store.len()).collect();
            prop_assert_eq!(&received, &expected);
            prop_assert_eq!(last.unwrap_or(0), store.len());
            Ok(())
        })?;
    }
}

#[tokio::test]
async fn a_shorter_history_resets_the_feed() {
    let store = Arc::new(GrowingSession::default());
    store.append(5);
    let engine = SurfaceEngine::builder(Arc::new(Store(Arc::clone(&store)))).build();
    store.events.lock().unwrap().truncate(2);
    let mut feed = engine.feed(me(), "s1".into(), Some(5));
    let first = feed.next().await.unwrap().unwrap();
    assert_eq!(first, FeedFrame::Reset { len: 2 });
    let FeedFrame::Entry { entry } = feed.next().await.unwrap().unwrap() else {
        panic!("entries follow the reset from the start");
    };
    assert_eq!(entry.seq, 1);
}

/// Wave 2 review finding 14: a history replaced while the feed runs — rewound or started
/// again, then grown back past what was sent — resets the feed instead of skipping the new
/// events at positions already sent.
#[tokio::test]
async fn a_history_replaced_while_the_feed_runs_resets_it() {
    let store = Arc::new(GrowingSession::default());
    store.append(3);
    let engine = SurfaceEngine::builder(Arc::new(Store(Arc::clone(&store)))).build();
    let mut feed = engine.feed(me(), "s1".into(), None);
    let mut seen = 0;
    while seen < 3 {
        if let FeedFrame::Entry { .. } = feed.next().await.unwrap().unwrap() {
            seen += 1;
        }
    }
    // Rewound to one event, then a new turn writes four more: five events, all new after
    // the first.
    store.events.lock().unwrap().truncate(1);
    store.append(4);
    let frame = loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), feed.next())
            .await
            .expect("the feed reports the new history")
            .unwrap()
            .unwrap();
        if !matches!(frame, FeedFrame::Decisions { .. }) {
            break frame;
        }
    };
    assert_eq!(frame, FeedFrame::Reset { len: 5 });
    let mut seqs = Vec::new();
    while seqs.len() < 5 {
        if let FeedFrame::Entry { entry } = feed.next().await.unwrap().unwrap() {
            seqs.push(entry.seq);
        }
    }
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn another_persons_session_is_not_found() {
    let store = Arc::new(GrowingSession::default());
    store.append(1);
    let engine = SurfaceEngine::builder(Arc::new(Store(store))).build();
    let stranger = Caller::human("bo", SurfaceKind::Web, Origin::Pointer);
    let error = engine.feed(stranger, "s1".into(), None).next().await.unwrap().unwrap_err();
    assert_eq!(error.code(), "surface.not_found");
}
