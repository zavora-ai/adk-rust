//! Interrupt ids, typed answers and strict resume (autonomy parity item 17): each dynamic
//! pause has an id bound to its thread, step, node and question; an answer is checked
//! against the pause's `response_schema` before any node runs; and under strict resume
//! only the id of a pending pause answers it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::stream::{StreamEvent, StreamMode};
use futures::StreamExt;
use serde_json::{Value, json};

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "approved": { "type": "boolean" }, "note": { "type": "string" } },
        "required": ["approved"],
        "additionalProperties": false
    })
}

/// `review` asks about the refund in state, typed by `schema()`, until answered; `execute`
/// counts the refunds issued. `loops` makes `execute` send the next refund back to review.
fn refund_graph(executions: Arc<AtomicUsize>, loops: bool) -> CompiledGraph {
    StateGraph::with_channels(&["amount", "decision"])
        .add_node_fn("review", |ctx| async move {
            let amount = ctx.state.get("amount").cloned().unwrap_or(json!(0));
            match ctx.resume_value() {
                Some(answer) if answer["approved"] == true => {
                    Ok(NodeOutput::new().with_update("decision", json!("approved")))
                }
                Some(_) => Ok(NodeOutput::new()
                    .with_update("decision", json!("rejected"))
                    .with_goto([END])),
                None => Ok(NodeOutput::interrupt_with_data(
                    "approve this refund?",
                    json!({ "amount": amount }),
                )
                .with_response_schema(schema())),
            }
        })
        .add_node_fn("execute", move |ctx| {
            let executions = Arc::clone(&executions);
            async move {
                let issued = executions.fetch_add(1, Ordering::SeqCst) + 1;
                let amount = ctx.state.get("amount").and_then(Value::as_u64).unwrap_or(0);
                Ok(if loops && issued < 2 {
                    NodeOutput::new()
                        .with_update("amount", json!(amount + 100))
                        .with_goto(["review"])
                } else {
                    NodeOutput::new().with_goto([END])
                })
            }
        })
        .add_edge(START, "review")
        .add_edge("review", "execute")
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
}

fn start(amount: u64) -> State {
    let mut state = State::new();
    state.insert("amount".into(), json!(amount));
    state
}

/// The pause the run stopped at: its id and schema.
async fn pause(graph: &CompiledGraph, config: ExecutionConfig, input: State) -> (String, Value) {
    match graph.invoke(input, config).await {
        Err(GraphError::Interrupted(paused)) => {
            let id = paused.interrupt.id().expect("a dynamic pause has an id").to_string();
            let schema = paused.interrupt.response_schema().cloned().expect("the schema travels");
            (id, schema)
        }
        other => panic!("expected a pause, got {other:?}"),
    }
}

#[tokio::test]
async fn a_pause_has_a_stable_id_and_is_answered_by_it() {
    let executions = Arc::new(AtomicUsize::new(0));
    let graph = refund_graph(Arc::clone(&executions), false);

    let (id, schema) = pause(&graph, ExecutionConfig::new("t1"), start(40)).await;
    assert!(id.starts_with("int_"), "{id}");
    assert_eq!(schema, self::schema());
    // Resuming without an answer asks the same question again, under the same id.
    let (again, _) = pause(&graph, ExecutionConfig::new("t1"), State::new()).await;
    assert_eq!(again, id);

    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t1").with_resume_value(&id, json!({ "approved": true })),
        )
        .await
        .unwrap();
    assert_eq!(state.get("decision"), Some(&json!("approved")));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_answer_that_breaks_the_schema_is_refused_before_anything_runs() {
    let executions = Arc::new(AtomicUsize::new(0));
    let graph = refund_graph(Arc::clone(&executions), false);
    let (id, _) = pause(&graph, ExecutionConfig::new("t2"), start(40)).await;

    for (key, answer) in [
        (id.as_str(), json!({ "approved": "yes" })),
        (id.as_str(), json!({ "note": "fine" })),
        // A node-name answer is checked against the same schema.
        ("review", json!("approve")),
    ] {
        let refused = graph
            .invoke(State::new(), ExecutionConfig::new("t2").with_resume_value(key, answer.clone()))
            .await;
        let Err(GraphError::ResumeRejected { key: refused_key, reason }) = refused else {
            panic!("{answer}: expected a refusal, got {refused:?}");
        };
        assert_eq!(refused_key, key);
        assert!(reason.contains("response_schema"), "{reason}");
        let error: adk_core::AdkError =
            GraphError::ResumeRejected { key: refused_key, reason }.into();
        assert_eq!(error.code, "graph.resume_rejected");
    }
    assert_eq!(executions.load(Ordering::SeqCst), 0, "nothing ran");

    // The checkpoint is unchanged: a valid answer still resumes the same pause.
    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t2").with_resume_value(&id, json!({ "approved": false })),
        )
        .await
        .unwrap();
    assert_eq!(state.get("decision"), Some(&json!("rejected")));
}

#[tokio::test]
async fn strict_resume_rejects_stale_and_name_keyed_answers() {
    let executions = Arc::new(AtomicUsize::new(0));
    let graph = refund_graph(Arc::clone(&executions), true);
    let (first, _) = pause(&graph, ExecutionConfig::new("t3"), start(40)).await;

    // A node name does not answer under strict resume.
    let by_name = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t3")
                .strict_resume()
                .with_resume_value("review", json!({ "approved": true })),
        )
        .await;
    assert!(
        matches!(&by_name, Err(GraphError::ResumeRejected { key, .. }) if key == "review"),
        "{by_name:?}"
    );

    // The first refund is approved by id; the loop brings the second one back to review,
    // a new pause about another amount, with another id.
    let second = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t3")
                .strict_resume()
                .with_resume_value(&first, json!({ "approved": true })),
        )
        .await;
    let Err(GraphError::Interrupted(paused)) = second else { panic!("{second:?}") };
    let second = paused.interrupt.id().unwrap().to_string();
    assert_ne!(second, first);
    assert_eq!(executions.load(Ordering::SeqCst), 1);

    // Replaying the first answer is stale: it would approve the second refund.
    let stale = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t3")
                .strict_resume()
                .with_resume_value(&first, json!({ "approved": true })),
        )
        .await;
    assert!(
        matches!(&stale, Err(GraphError::ResumeRejected { key, reason }) if *key == first && reason.contains("no pause with this id")),
        "{stale:?}"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1, "the second refund was not issued");

    let done = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t3")
                .strict_resume()
                .with_resume_value(&second, json!({ "approved": true })),
        )
        .await
        .unwrap();
    assert_eq!(done.get("decision"), Some(&json!("approved")));
    assert_eq!(executions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_stream_reports_the_pause_id_and_keeps_its_schema() {
    for (thread, mode) in [("t4", StreamMode::Updates), ("t5", StreamMode::Messages)] {
        let graph = refund_graph(Arc::new(AtomicUsize::new(0)), false);
        let events: Vec<_> =
            graph.stream(start(40), ExecutionConfig::new(thread), mode).collect().await;
        let id = events
            .iter()
            .find_map(|event| match event {
                Ok(StreamEvent::Interrupted { interrupt_id, .. }) => interrupt_id.clone(),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{mode:?}: the interrupted event carries the id"));
        assert!(id.starts_with("int_"));
        // The schema reached the checkpoint on this path too.
        let refused = graph
            .invoke(
                State::new(),
                ExecutionConfig::new(thread).with_resume_value(&id, json!({ "approved": 1 })),
            )
            .await;
        assert!(matches!(refused, Err(GraphError::ResumeRejected { .. })), "{mode:?}: {refused:?}");
    }
}
