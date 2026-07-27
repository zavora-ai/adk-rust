//! A functional interrupt must be resumable with a typed value.
//!
//! `TaskContext::interrupt<T>` emitted an event, saved a checkpoint, recorded an `__interrupt__`
//! task, and then *always* returned `InterruptTypeMismatch { message: "workflow interrupted" }`.
//! Its own comment said the runtime would suspend "in a real runtime" and that the macro wrapper
//! handled resumption; nothing outside the method consumed a resume value. So the signature
//! promised typed resumption that could not occur, and callers had to read an error as control
//! flow with no value ever delivered — and no key under which to supply one.

#![cfg(feature = "functional")]

use adk_graph::functional::{FunctionalError, TaskContext};
use adk_graph::{Checkpointer, MemoryCheckpointer, State};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// What an approval interrupt would return.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Approval {
    approved: bool,
    approver: String,
}

/// A context over an in-memory checkpointer.
fn context() -> TaskContext {
    let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
    TaskContext::new(
        "thread-1".to_string(),
        State::new(),
        Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>,
        event_tx,
        Arc::new(tokio::sync::RwLock::new(adk_graph::functional::ExecutionLog::default())),
        tokio_util::sync::CancellationToken::new(),
        None,
    )
}

#[tokio::test]
async fn an_interrupt_without_a_resume_value_suspends_and_reports_its_key() {
    let ctx = context();

    let error = ctx
        .interrupt::<Approval>("approve the refund")
        .await
        .expect_err("with no value supplied the workflow must suspend");

    let message = error.to_string();
    assert!(
        message.contains("interrupt-1"),
        "the caller needs the key to supply a value under: {message}"
    );
    assert!(
        message.contains("approve the refund"),
        "the interrupt's message must survive: {message}"
    );
    assert!(!message.contains("type mismatch"), "needing input is not a type mismatch: {message}");
}

#[tokio::test]
async fn a_supplied_value_reaches_the_call_site_typed() {
    let expected = Approval { approved: true, approver: "alice".to_string() };
    let ctx = context().with_resume_values(HashMap::from([(
        "interrupt-1".to_string(),
        serde_json::to_value(&expected).unwrap(),
    )]));

    let approval: Approval =
        ctx.interrupt("approve the refund").await.expect("a supplied value must resume the call");

    assert_eq!(approval, expected, "the interrupt call must return the value, not an error");
}

#[tokio::test]
async fn each_interrupt_site_gets_its_own_key() {
    // Two interrupts in one run must be independently resumable, so their keys differ and are
    // stable in call order — which is what makes a replayed workflow find the right value.
    let first = Approval { approved: true, approver: "alice".to_string() };
    let second = Approval { approved: false, approver: "bob".to_string() };

    let ctx = context().with_resume_values(HashMap::from([
        ("interrupt-1".to_string(), serde_json::to_value(&first).unwrap()),
        ("interrupt-2".to_string(), serde_json::to_value(&second).unwrap()),
    ]));

    let a: Approval = ctx.interrupt("first").await.expect("first resumes");
    let b: Approval = ctx.interrupt("second").await.expect("second resumes");

    assert_eq!(a, first);
    assert_eq!(b, second, "the second site must not receive the first site's value");
}

#[tokio::test]
async fn a_partially_supplied_run_resumes_then_suspends_again() {
    let first = Approval { approved: true, approver: "alice".to_string() };
    let ctx = context().with_resume_values(HashMap::from([(
        "interrupt-1".to_string(),
        serde_json::to_value(&first).unwrap(),
    )]));

    let a: Approval = ctx.interrupt("first").await.expect("first resumes");
    assert_eq!(a, first);

    let error =
        ctx.interrupt::<Approval>("second").await.expect_err("the unsupplied site must suspend");
    assert!(
        error.to_string().contains("interrupt-2"),
        "the next key must be reported so the caller can supply it: {error}"
    );
}

#[tokio::test]
async fn a_wrong_typed_value_is_a_type_mismatch_not_a_suspension() {
    // `InterruptTypeMismatch` now means what its name says. Previously every interrupt produced
    // it, so it carried no information.
    let ctx = context().with_resume_values(HashMap::from([(
        "interrupt-1".to_string(),
        serde_json::json!("not an approval object"),
    )]));

    let error = ctx
        .interrupt::<Approval>("approve the refund")
        .await
        .expect_err("a value of the wrong shape must be rejected");

    let message = error.to_string();
    assert!(message.contains("type mismatch"), "{message}");
    assert!(message.contains("interrupt-1"), "the failing site must be identified: {message}");
}

#[tokio::test]
async fn suspension_and_type_mismatch_are_distinguishable_variants() {
    let suspended = context().interrupt::<Approval>("needs input").await.expect_err("must suspend");
    let mismatched = context()
        .with_resume_values(HashMap::from([("interrupt-1".to_string(), serde_json::json!(42))]))
        .interrupt::<Approval>("needs input")
        .await
        .expect_err("must reject");

    // A caller deciding whether to prompt a human or fix its payload needs these apart. They are
    // separate `FunctionalError` variants; `From<FunctionalError> for GraphError` still flattens
    // to `GraphError::Other(String)`, so at this level they are distinguished by message.
    let suspended = suspended.to_string();
    let mismatched = mismatched.to_string();

    assert!(suspended.contains("suspended at interrupt"), "{suspended}");
    assert!(!suspended.contains("type mismatch"), "{suspended}");
    assert!(mismatched.contains("type mismatch"), "{mismatched}");
    assert!(!mismatched.contains("suspended at interrupt"), "{mismatched}");

    // The variants themselves are distinct, which is what a caller matching on
    // `FunctionalError` sees before the conversion flattens them.
    let direct = FunctionalError::Suspended {
        continuation_key: "interrupt-1".to_string(),
        message: "needs input".to_string(),
    };
    assert!(matches!(direct, FunctionalError::Suspended { .. }));
}
