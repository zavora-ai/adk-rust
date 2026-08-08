//! An interrupt must survive the `Agent` boundary with its data intact.
//!
//! `GraphAgent::run` flattened every interrupt into a text event named
//! `graph_interrupted`, formatted with `{:?}`. A caller under a `Runner` could
//! read the prose but not the node name, the payload a node attached with
//! `interrupt_with_data`, or the checkpoint to resume from — so a graph interrupt
//! was unusable as an approval request unless the caller drove the executor
//! directly and bypassed the `Agent` trait.

use adk_core::Agent;
use adk_graph::agent::GraphAgent;
use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::interrupt::GraphInterruptPayload;
use adk_graph::node::NodeOutput;
use futures::StreamExt;
use serde_json::json;

mod support;
use support::test_context;

/// A dynamic interrupt's message and data reach the caller.
#[tokio::test]
async fn a_dynamic_interrupt_carries_its_data_to_the_caller() {
    let agent = GraphAgent::builder("approver")
        .channels(&["draft"])
        .node_fn("draft", |_ctx| async move {
            Ok(NodeOutput::new().with_update("draft", json!("refund 250")))
        })
        .node_fn("approve", |_ctx| async move {
            Ok(NodeOutput::interrupt_with_data(
                "approve this refund?",
                json!({ "amount": 250, "currency": "EUR" }),
            ))
        })
        .edge(START, "draft")
        .edge("draft", "approve")
        .edge("approve", END)
        .checkpointer(MemoryCheckpointer::new())
        .build()
        .expect("build the agent");

    let mut events = agent.run(test_context("approval-1")).await.expect("run");
    let event = events.next().await.expect("one event").expect("not an error");

    let payload =
        GraphInterruptPayload::from_event(&event).expect("the event must carry the interrupt");

    assert_eq!(payload.kind, "dynamic");
    assert_eq!(payload.message.as_deref(), Some("approve this refund?"));
    assert_eq!(payload.data.as_ref().and_then(|d| d.get("amount")), Some(&json!(250)));
    assert!(!payload.checkpoint_id.is_empty(), "a resume needs the checkpoint id");
}

/// A static interrupt reports which node is gated.
#[tokio::test]
async fn a_static_interrupt_reports_its_node() {
    let agent = GraphAgent::builder("gated")
        .channels(&["value"])
        .node_fn("open", |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) })
        .node_fn("gated", |_ctx| async move { Ok(NodeOutput::new()) })
        .edge(START, "open")
        .edge("open", "gated")
        .edge("gated", END)
        .checkpointer(MemoryCheckpointer::new())
        .interrupt_before(&["gated"])
        .build()
        .expect("build the agent");

    let mut events = agent.run(test_context("approval-2")).await.expect("run");
    let event = events.next().await.expect("one event").expect("not an error");

    let payload =
        GraphInterruptPayload::from_event(&event).expect("the event must carry the interrupt");

    assert_eq!(payload.kind, "before");
    assert_eq!(payload.node.as_deref(), Some("gated"));
}

/// A caller resumes the run and the decision reaches the gated node.
///
/// `GraphAgent` maps the invocation to an input state and merges it over the
/// restored checkpoint, so a decision travels the same path as any other input.
/// The gated node reads it from state on the second invocation.
#[tokio::test]
async fn a_resumed_run_sees_the_callers_decision() {
    use adk_graph::state::State;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let decisions = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&decisions);

    // The input mapper supplies the decision. On the first invocation there is
    // none; on the second the caller has approved.
    let approved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let approved_for_mapper = Arc::clone(&approved);

    let agent = GraphAgent::builder("approver")
        .channels(&["approved", "outcome"])
        .input_mapper(move |_ctx| {
            let mut state = State::new();
            if approved_for_mapper.load(Ordering::SeqCst) {
                state.insert("approved".to_string(), json!(true));
            }
            state
        })
        .node_fn("gated", move |ctx| {
            let seen = Arc::clone(&seen);
            async move {
                if ctx.get("approved").and_then(|v| v.as_bool()) == Some(true) {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("outcome", json!("done")))
                } else {
                    Ok(NodeOutput::interrupt("approve?"))
                }
            }
        })
        .edge(START, "gated")
        .edge("gated", END)
        .checkpointer(MemoryCheckpointer::new())
        .build()
        .expect("build the agent");

    // First invocation pauses.
    let mut events = agent.run(test_context("resume-1")).await.expect("run");
    let event = events.next().await.expect("one event").expect("not an error");
    let payload = GraphInterruptPayload::from_event(&event).expect("an interrupt");
    assert_eq!(payload.kind, "dynamic");
    assert_eq!(decisions.load(Ordering::SeqCst), 0);

    // The caller approves, then invokes the same thread again.
    approved.store(true, Ordering::SeqCst);
    let mut events = agent.run(test_context("resume-1")).await.expect("resume");
    let mut saw_interrupt = false;
    while let Some(event) = events.next().await {
        let event = event.expect("not an error");
        if GraphInterruptPayload::from_event(&event).is_some() {
            saw_interrupt = true;
        }
    }

    assert!(!saw_interrupt, "the resumed run must not pause again");
    assert_eq!(decisions.load(Ordering::SeqCst), 1, "the gated node must have seen the approval");
}
