//! Regression tests for `ParallelAgent` branch concurrency.
//!
//! `Agent::run` only *builds* an `EventStream`; the work happens when the stream
//! is polled. An earlier implementation awaited the `run()` futures together but
//! then drained each returned stream to completion in turn, so nominally
//! parallel branches executed one at a time. These tests pin the behaviour down
//! without depending on wall-clock timing.

use adk_agent::{CustomAgentBuilder, ParallelAgent};
use adk_core::{Agent, Content, Event, InvocationContext, Part, ReadonlyContext, RunConfig};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Barrier;

// ── Minimal context plumbing (mirrors workflow_tests.rs) ───────────────

struct MockState;

impl adk_core::State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

struct MockSession {
    state: MockState,
}

impl adk_core::Session for MockSession {
    fn id(&self) -> &str {
        "test-session"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn state(&self) -> &dyn adk_core::State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        Vec::new()
    }
}

struct TestContext {
    content: Content,
    config: RunConfig,
    session: MockSession,
}

impl TestContext {
    fn new() -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "go".to_string() }],
            },
            config: RunConfig::default(),
            session: MockSession { state: MockState },
        }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "test-invocation"
    }
    fn agent_name(&self) -> &str {
        "parallel"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "test-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl adk_core::CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for TestContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not used by ParallelAgent")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn adk_core::Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

fn event(author: &str) -> Event {
    let mut e = Event::new("test-invocation");
    e.author = author.to_string();
    e
}

// ── Tests ───────────────────────────────────────────────────────────────

/// Branches must execute concurrently, not one after another.
///
/// Both sub-agents wait on a two-party barrier *inside* their event stream
/// before producing anything. A barrier only releases once both parties arrive,
/// so this completes if and only if both streams are polled concurrently. Under
/// the previous drain-one-stream-at-a-time implementation the first branch waits
/// for a party that is never scheduled and the test times out.
#[tokio::test]
async fn parallel_branches_run_concurrently() {
    let barrier = Arc::new(Barrier::new(2));

    let make = |name: &'static str, barrier: Arc<Barrier>| {
        CustomAgentBuilder::new(name)
            .handler(move |_ctx| {
                let barrier = barrier.clone();
                async move {
                    let s = async_stream::stream! {
                        barrier.wait().await;
                        yield Ok(event(name));
                    };
                    Ok(Box::pin(s) as adk_core::EventStream)
                }
            })
            .build()
            .unwrap()
    };

    let parallel = ParallelAgent::new(
        "parallel",
        vec![Arc::new(make("a", barrier.clone())), Arc::new(make("b", barrier.clone()))],
    );

    let stream = parallel.run(Arc::new(TestContext::new())).await.unwrap();
    let collected =
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("branches did not run concurrently: the barrier never released");

    let mut authors: Vec<String> =
        collected.into_iter().map(|r| r.expect("no branch should fail").author).collect();
    authors.sort();
    assert_eq!(authors, vec!["a".to_string(), "b".to_string()]);
}

/// A branch that is still producing must not be blocked by a slow sibling.
///
/// The fast branch emits three events while the slow branch waits on a barrier
/// that the fast branch itself releases. Serial draining cannot satisfy this:
/// whichever branch is drained first would block forever.
#[tokio::test]
async fn a_slow_branch_does_not_block_a_fast_one() {
    let gate = Arc::new(Barrier::new(2));

    let fast_gate = gate.clone();
    let fast = CustomAgentBuilder::new("fast")
        .handler(move |_ctx| {
            let gate = fast_gate.clone();
            async move {
                let s = async_stream::stream! {
                    yield Ok(event("fast"));
                    yield Ok(event("fast"));
                    // Release the slow branch only after emitting events, so the
                    // slow branch cannot have been drained first.
                    gate.wait().await;
                    yield Ok(event("fast"));
                };
                Ok(Box::pin(s) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let slow_gate = gate.clone();
    let slow = CustomAgentBuilder::new("slow")
        .handler(move |_ctx| {
            let gate = slow_gate.clone();
            async move {
                let s = async_stream::stream! {
                    gate.wait().await;
                    yield Ok(event("slow"));
                };
                Ok(Box::pin(s) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new("parallel", vec![Arc::new(fast), Arc::new(slow)]);
    let stream = parallel.run(Arc::new(TestContext::new())).await.unwrap();
    let collected =
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("a slow branch blocked a fast one");

    assert_eq!(collected.len(), 4);
    let slow_count = collected
        .iter()
        .filter(|r| r.as_ref().map(|e| e.author == "slow").unwrap_or(false))
        .count();
    assert_eq!(slow_count, 1);
}

/// The reported error is the lowest-indexed failing branch, not whichever branch
/// happened to fail first. With branches running concurrently "first" is a race,
/// so selection is pinned to the declared sub-agent order.
#[tokio::test]
async fn failure_reported_is_deterministic_by_sub_agent_order() {
    // Branch 1 fails first in wall-clock order; branch 0 waits until it observes
    // that, then fails. Declaration order and failure order therefore disagree.
    // A shared flag rather than a barrier: a branch stops after failing, so it
    // would never reach a second barrier party.
    let branch_one_failed = Arc::new(AtomicUsize::new(0));

    let zero_flag = branch_one_failed.clone();
    let first = CustomAgentBuilder::new("first")
        .handler(move |_ctx| {
            let flag = zero_flag.clone();
            async move {
                let s = async_stream::stream! {
                    while flag.load(Ordering::SeqCst) == 0 {
                        tokio::task::yield_now().await;
                    }
                    yield Err(adk_core::AdkError::agent("failure from branch 0"));
                };
                Ok(Box::pin(s) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let one_flag = branch_one_failed.clone();
    let second = CustomAgentBuilder::new("second")
        .handler(move |_ctx| {
            let flag = one_flag.clone();
            async move {
                let s = async_stream::stream! {
                    flag.store(1, Ordering::SeqCst);
                    yield Err(adk_core::AdkError::agent("failure from branch 1"));
                };
                Ok(Box::pin(s) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new("parallel", vec![Arc::new(first), Arc::new(second)]);
    let stream = parallel.run(Arc::new(TestContext::new())).await.unwrap();
    let collected =
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("run did not terminate");

    let errors: Vec<String> =
        collected.iter().filter_map(|r| r.as_ref().err().map(|e| e.to_string())).collect();
    assert_eq!(errors.len(), 1, "exactly one terminal error is surfaced");
    assert!(
        errors[0].contains("branch 0"),
        "expected the lowest-indexed branch's error, got: {}",
        errors[0]
    );
}

/// A failing branch must not suppress its siblings' events.
#[tokio::test]
async fn sibling_events_survive_a_failing_branch() {
    let failing = CustomAgentBuilder::new("failing")
        .handler(|_ctx| async move {
            Ok(Box::pin(stream::iter(vec![Err(adk_core::AdkError::agent("boom"))]))
                as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let healthy = CustomAgentBuilder::new("healthy")
        .handler(|_ctx| async move {
            Ok(Box::pin(stream::iter(vec![Ok(event("healthy")), Ok(event("healthy"))]))
                as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new("parallel", vec![Arc::new(failing), Arc::new(healthy)]);
    let stream = parallel.run(Arc::new(TestContext::new())).await.unwrap();
    let collected = stream.collect::<Vec<_>>().await;

    let healthy_events = collected.iter().filter(|r| r.is_ok()).count();
    let errors = collected.iter().filter(|r| r.is_err()).count();
    assert_eq!(healthy_events, 2, "the healthy branch's events must still arrive");
    assert_eq!(errors, 1);
}

/// Dropping the merged stream early must tear down in-flight branches rather
/// than leaving them running.
#[tokio::test]
async fn dropping_the_stream_tears_down_branches() {
    struct DropFlag(Arc<AtomicUsize>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));

    let make = |name: &'static str, drops: Arc<AtomicUsize>| {
        CustomAgentBuilder::new(name)
            .handler(move |_ctx| {
                let drops = drops.clone();
                async move {
                    let s = async_stream::stream! {
                        // Held by the generator; dropped when the stream is dropped.
                        let _guard = DropFlag(drops.clone());
                        yield Ok(event(name));
                        // Never completes, so the branch is mid-flight when dropped.
                        futures::future::pending::<()>().await;
                        yield Ok(event(name));
                    };
                    Ok(Box::pin(s) as adk_core::EventStream)
                }
            })
            .build()
            .unwrap()
    };

    let parallel = ParallelAgent::new(
        "parallel",
        vec![Arc::new(make("a", drops.clone())), Arc::new(make("b", drops.clone()))],
    );

    let mut stream = parallel.run(Arc::new(TestContext::new())).await.unwrap();
    // One event from each branch, so both generators have started and both hold a
    // guard. `select_all` can return a ready item without polling every branch, so
    // taking a single event would not guarantee both have run.
    for _ in 0..2 {
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("no event produced");
        assert!(next.is_some());
    }
    // Both branches are now suspended on `pending()`. Dropping the merged stream
    // must drop them rather than leave them in flight.
    drop(stream);

    assert_eq!(
        drops.load(Ordering::SeqCst),
        2,
        "dropping the merged stream must drop every branch's state"
    );
}

/// Each branch stamps its own conversation branch onto the events it emits, so a
/// branch-scoped history read can later exclude siblings. The shape matches ADK
/// Python and ADK Go: `{parent}.{parallel_agent}.{sub_agent}`.
#[tokio::test]
async fn branches_stamp_their_own_branch_on_events() {
    let make = |name: &'static str| {
        CustomAgentBuilder::new(name)
            .handler(move |_ctx| async move {
                Ok(Box::pin(stream::iter(vec![Ok(event(name))])) as adk_core::EventStream)
            })
            .build()
            .unwrap()
    };

    let parallel =
        ParallelAgent::new("analysis", vec![Arc::new(make("alpha")), Arc::new(make("beta"))]);
    let collected =
        parallel.run(Arc::new(TestContext::new())).await.unwrap().collect::<Vec<_>>().await;

    let mut stamped: Vec<(String, String)> = collected
        .into_iter()
        .map(|r| {
            let e = r.expect("no branch should fail");
            (e.author, e.branch)
        })
        .collect();
    stamped.sort();

    assert_eq!(
        stamped,
        vec![
            ("alpha".to_string(), "analysis.alpha".to_string()),
            ("beta".to_string(), "analysis.beta".to_string()),
        ]
    );
}

/// A branch already carrying a deeper branch (a nested workflow stamped it) keeps
/// it, so nesting composes instead of the outer agent overwriting the inner one.
#[tokio::test]
async fn an_existing_deeper_branch_is_preserved() {
    let nested = CustomAgentBuilder::new("outer")
        .handler(|_ctx| async move {
            let mut e = event("inner");
            e.branch = "analysis.outer.inner_parallel.leaf".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(e)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new("analysis", vec![Arc::new(nested)]);
    let collected =
        parallel.run(Arc::new(TestContext::new())).await.unwrap().collect::<Vec<_>>().await;

    assert_eq!(collected.len(), 1);
    assert_eq!(
        collected[0].as_ref().unwrap().branch,
        "analysis.outer.inner_parallel.leaf",
        "an inner workflow's branch must not be overwritten"
    );
}
