//! A graph cannot hold a call for a person's decision, so a graph that starts a run refuses
//! the call a person asked for before any node runs (spec 3.3 §7.4 rule 5).

use crate::support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{
    Agent, Content, InvocationContext, Part, RequestedCall, RequestedVia, RunConfig, Session,
    SubjectRef,
};
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::node::NodeOutput;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

/// The test context, started by `agent_name` with `config`.
struct Started {
    inner: Arc<dyn InvocationContext>,
    agent_name: String,
    config: RunConfig,
}

#[async_trait]
impl adk_core::ReadonlyContext for Started {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }
    fn agent_name(&self) -> &str {
        &self.agent_name
    }
    fn user_id(&self) -> &str {
        self.inner.user_id()
    }
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }
    fn branch(&self) -> &str {
        self.inner.branch()
    }
    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl adk_core::CallbackContext for Started {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for Started {
    fn agent(&self) -> Arc<dyn Agent> {
        self.inner.agent()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        self.inner.session()
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn a_graph_that_starts_the_run_refuses_a_requested_call_before_any_node_runs() {
    let ran = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&ran);
    let agent = GraphAgent::builder("invoices")
        .channels(&["value"])
        .node_fn("send", move |_ctx| {
            let counted = Arc::clone(&counted);
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("value", json!(1)))
            }
        })
        .edge(START, "send")
        .edge("send", END)
        .build()
        .unwrap();
    let call = RequestedCall::new(
        "send_invoice",
        json!({ "to": "ana@example.com" }),
        SubjectRef::human("ana"),
        RequestedVia::form("invoice-10", 1, "send"),
    );
    let ctx = Arc::new(Started {
        inner: support::test_context("s1"),
        agent_name: "invoices".to_string(),
        config: RunConfig { requested_call: Some(call.clone()), ..RunConfig::default() },
    });

    let events: Vec<_> = agent.run(ctx).await.unwrap().collect().await;

    let answers: Vec<_> = events
        .iter()
        .filter_map(|event| event.as_ref().ok()?.content())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        answers,
        vec![(
            Some(call.function_call_id.clone()),
            json!({
                "error": "Tool 'send_invoice' was not run: this agent runs a graph and cannot \
                          hold a call for a person's decision"
            })
        )]
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "no node runs");
}
