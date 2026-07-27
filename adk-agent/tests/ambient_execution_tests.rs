//! An ambient agent must invoke the agent, deliver what it produced, and not serialize triggers.
//!
//! Three defects in `AmbientAgent::start`:
//!
//! 1. **Without a trigger handler the agent was never invoked** — each event was logged and
//!    dropped, so `AmbientAgent::new(..).start()` looked like it was running an agent that never
//!    ran.
//! 2. **Produced events were logged at debug and discarded**, leaving a caller no way to observe
//!    what an ambient run did or whether it failed.
//! 3. **Triggers were strictly serial** — the loop drained a handler's entire event stream before
//!    polling the source again, so one slow trigger blocked every later one.

#![cfg(feature = "ambient")]

use adk_agent::ambient::{AmbientAgent, EventSource, TriggerEvent, TriggerHandler};
use adk_core::{Agent, Content, Event, EventStream, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A leaf agent; these tests assert dispatch, not model behaviour.
#[derive(Debug)]
struct NoopAgent;

#[async_trait]
impl Agent for NoopAgent {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "does nothing"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn adk_core::InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// Emits a fixed number of trigger events, then ends.
struct BurstSource {
    count: usize,
}

#[async_trait]
impl EventSource for BurstSource {
    fn name(&self) -> &str {
        "burst"
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>> {
        let events: Vec<TriggerEvent> = (0..self.count)
            .map(|index| TriggerEvent {
                source: "burst".to_string(),
                payload: serde_json::json!({ "index": index }),
                // A synthetic burst has no authenticated caller.
                principal: None,
            })
            .collect();
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A handler that records invocations and yields one event.
fn counting_handler(invocations: Arc<AtomicUsize>) -> TriggerHandler {
    Arc::new(move |_event, _agent| {
        let invocations = Arc::clone(&invocations);
        Box::pin(async move {
            invocations.fetch_add(1, Ordering::SeqCst);
            let mut event = Event::new("inv");
            event.author = "noop".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
        })
    })
}

#[tokio::test]
async fn starting_without_a_handler_is_refused() {
    let mut ambient = AmbientAgent::new(Arc::new(NoopAgent), Arc::new(BurstSource { count: 1 }));

    let error = ambient
        .start()
        .await
        .expect_err("a configuration that never invokes the agent must not start silently");
    let message = error.to_string();
    assert!(message.contains("trigger handler"), "the error must name what is missing: {message}");
}

#[tokio::test]
async fn produced_events_are_delivered_to_the_caller() {
    let invocations = Arc::new(AtomicUsize::new(0));
    let mut ambient = AmbientAgent::new(Arc::new(NoopAgent), Arc::new(BurstSource { count: 1 }))
        .with_trigger_handler(counting_handler(Arc::clone(&invocations)));

    let mut outputs = ambient.take_output(8);
    ambient.start().await.expect("start");

    let delivered = tokio::time::timeout(std::time::Duration::from_secs(5), outputs.recv())
        .await
        .expect("an ambient run must deliver what it produced")
        .expect("channel must yield");

    let event = delivered.expect("the handler produced a successful event");
    assert_eq!(event.author, "noop");
    assert_eq!(invocations.load(Ordering::SeqCst), 1, "the agent was invoked");
}

#[tokio::test]
async fn a_handler_error_is_delivered_rather_than_only_logged() {
    let failing: TriggerHandler = Arc::new(|_event, _agent| {
        Box::pin(async { Err(adk_core::AdkError::agent("handler exploded")) })
    });

    let mut ambient = AmbientAgent::new(Arc::new(NoopAgent), Arc::new(BurstSource { count: 1 }))
        .with_trigger_handler(failing);

    let mut outputs = ambient.take_output(8);
    ambient.start().await.expect("start");

    let delivered = tokio::time::timeout(std::time::Duration::from_secs(5), outputs.recv())
        .await
        .expect("a failure must reach the caller")
        .expect("channel must yield");

    let error = delivered.expect_err("the handler failed");
    assert!(error.to_string().contains("handler exploded"), "{error}");
}

#[tokio::test]
async fn independent_triggers_do_not_block_each_other() {
    // Every handler waits for all three to arrive. Serial dispatch cannot satisfy that, so this
    // completes only if triggers overlap.
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let handler_barrier = Arc::clone(&barrier);

    let handler: TriggerHandler = Arc::new(move |_event, _agent| {
        let barrier = Arc::clone(&handler_barrier);
        Box::pin(async move {
            barrier.wait().await;
            let mut event = Event::new("inv");
            event.author = "noop".to_string();
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
        })
    });

    let mut ambient = AmbientAgent::new(Arc::new(NoopAgent), Arc::new(BurstSource { count: 3 }))
        .with_trigger_handler(handler)
        .with_max_concurrent_triggers(3);

    let mut outputs = ambient.take_output(8);
    ambient.start().await.expect("start");

    let mut received = 0;
    let collected = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while outputs.recv().await.is_some() {
            received += 1;
            if received == 3 {
                break;
            }
        }
        received
    })
    .await;

    assert_eq!(
        collected.expect("serial dispatch cannot satisfy a three-way barrier"),
        3,
        "all three triggers must complete"
    );
}

#[tokio::test]
async fn the_concurrency_bound_is_respected() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let handler_in_flight = Arc::clone(&in_flight);
    let handler_peak = Arc::clone(&peak);

    let handler: TriggerHandler = Arc::new(move |_event, _agent| {
        let in_flight = Arc::clone(&handler_in_flight);
        let peak = Arc::clone(&handler_peak);
        Box::pin(async move {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::task::yield_now().await;
            in_flight.fetch_sub(1, Ordering::SeqCst);

            let mut event = Event::new("inv");
            event.author = "noop".to_string();
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
        })
    });

    let mut ambient = AmbientAgent::new(Arc::new(NoopAgent), Arc::new(BurstSource { count: 6 }))
        .with_trigger_handler(handler)
        .with_max_concurrent_triggers(2);

    let mut outputs = ambient.take_output(16);
    ambient.start().await.expect("start");

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut seen = 0;
        while outputs.recv().await.is_some() {
            seen += 1;
            if seen == 6 {
                break;
            }
        }
    })
    .await;

    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "the bound was exceeded: peak {}",
        peak.load(Ordering::SeqCst)
    );
}
