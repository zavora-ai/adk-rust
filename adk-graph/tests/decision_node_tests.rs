//! `DecisionNode` routes a graph by a model's typed decision, falls back below its confidence
//! floor, and meters its call against the run's budget.

use std::sync::{Arc, Mutex};

use adk_core::{
    BudgetTracker, Content, ErrorCategory, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    RunBudget, RunConfig,
};
use adk_graph::node::FunctionNode;
use adk_graph::state::State;
use adk_graph::{
    CompiledGraph, DecisionNode, END, ExecutionConfig, GraphError, NodeOutput, Router, START,
    StateGraph, StateSchema,
};
use async_trait::async_trait;
use serde_json::json;

struct Replying {
    reply: String,
    seen: Mutex<Vec<LlmRequest>>,
}

#[async_trait]
impl Llm for Replying {
    fn name(&self) -> &str {
        "claude-sonnet-5-5"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.seen.lock().unwrap().push(request);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(self.reply.clone())),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

fn model(choice: &str, confidence: f64) -> Arc<Replying> {
    Arc::new(Replying {
        reply: json!({"choice": choice, "rationale": "because", "confidence": confidence})
            .to_string(),
        seen: Mutex::new(Vec::new()),
    })
}

fn graph(triage: DecisionNode) -> CompiledGraph {
    let mark = |name: &'static str| {
        FunctionNode::new(name, move |_| async move {
            Ok(NodeOutput::new().with_update("handled_by", json!(name)))
        })
    };
    StateGraph::new(StateSchema::simple(&["ticket", "route", "route_decision", "handled_by"]))
        .add_node(triage)
        .add_node(mark("billing"))
        .add_node(mark("technical"))
        .add_node(mark("general"))
        .add_edge(START, "triage")
        .add_conditional_edges(
            "triage",
            Router::by_field("route"),
            [("billing", "billing"), ("technical", "technical"), ("general", "general")],
        )
        .add_edge("billing", END)
        .add_edge("technical", END)
        .add_edge("general", END)
        .compile()
        .unwrap()
}

fn triage(model: Arc<Replying>) -> DecisionNode {
    DecisionNode::new("triage", model)
        .instruction("Route the support ticket.")
        .option("billing", "Invoices, refunds and payment problems")
        .option("technical", "Errors, outages and how-to questions")
        .input_key("ticket")
        .output_key("route")
}

fn ticket() -> State {
    let mut state = State::new();
    state.insert("ticket".to_string(), json!("I was charged twice"));
    state
}

#[tokio::test]
async fn the_graph_follows_the_models_decision() {
    let model = model("billing", 0.9);
    let state =
        graph(triage(model.clone())).invoke(ticket(), ExecutionConfig::new("t1")).await.unwrap();
    assert_eq!(state["handled_by"], json!("billing"));
    assert_eq!(
        state["route_decision"],
        json!({"option": "billing", "rationale": "because", "confidence": 0.9, "followed": true})
    );
    let request = &model.seen.lock().unwrap()[0];
    assert!(request.tools.is_empty());
    let schema = request.config.as_ref().unwrap().response_schema.clone().unwrap();
    assert_eq!(schema["properties"]["choice"]["enum"], json!(["billing", "technical"]));
}

#[tokio::test]
async fn a_decision_below_the_floor_takes_the_fallback() {
    let node = triage(model("technical", 0.3)).min_confidence(0.6).fallback("general");
    let state = graph(node).invoke(ticket(), ExecutionConfig::new("t2")).await.unwrap();
    assert_eq!(state["handled_by"], json!("general"));
    assert_eq!(state["route_decision"]["followed"], json!(false));

    let strict = triage(model("technical", 0.3)).min_confidence(0.6);
    let error = graph(strict).invoke(ticket(), ExecutionConfig::new("t3")).await.unwrap_err();
    assert!(matches!(error, GraphError::NodeExecutionFailed { .. }), "{error}");
}

#[tokio::test]
async fn the_decision_counts_against_the_run_budget() {
    let model = model("billing", 0.9);
    let run_config = RunConfig {
        budget_tracker: Some(Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(0)))),
        ..RunConfig::default()
    };
    let error = graph(triage(model.clone()))
        .invoke_with_run_config(ticket(), ExecutionConfig::new("t4"), run_config)
        .await
        .unwrap_err();
    let error = adk_core::AdkError::from(error);
    assert_eq!(error.category, ErrorCategory::ResourceExhausted, "{error}");
    assert!(model.seen.lock().unwrap().is_empty(), "the refused call was not sent");
}
