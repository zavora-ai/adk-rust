//! `CompiledGraph::shape` reports nodes, edges and entry nodes deterministically
//! (spec 3.3 §12, developer track).

use adk_core::ToolEffect;
use adk_graph::edge::{END, START};
use adk_graph::governed::GovernedNode;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{AgentNode, NodeOutput};
use adk_graph::shape::{EdgeShape, GraphShape, NodeKind, NodeShape};
use std::sync::Arc;

fn node(name: &str, kind: NodeKind) -> NodeShape {
    NodeShape {
        name: name.to_string(),
        description: "Graph workflow node".to_string(),
        kind,
        effect: None,
        interrupt_before: false,
        interrupt_after: false,
        governed: None,
    }
}

fn function(graph: StateGraph, name: &str) -> StateGraph {
    graph.add_node_fn(name, |_ctx| async { Ok(NodeOutput::new()) })
}

#[test]
fn a_linear_graph_lists_its_nodes_edges_and_entry() {
    let graph = function(function(StateGraph::with_channels(&["x"]), "b"), "a")
        .add_edge(START, "a")
        .add_edge("a", "b")
        .add_edge("b", END)
        .compile()
        .unwrap()
        .with_interrupt_before(&["b"])
        .with_interrupt_after(&["a"])
        .with_node_effect("b", ToolEffect::NonIdempotent)
        .with_governed_node("b", GovernedNode::new().with_approval().spending_money());

    let mut b = node("b", NodeKind::Function);
    b.effect = Some(ToolEffect::NonIdempotent);
    b.interrupt_before = true;
    b.governed = Some(GovernedNode::new().with_approval().spending_money());
    let mut a = node("a", NodeKind::Function);
    a.interrupt_after = true;
    assert_eq!(
        graph.shape(),
        GraphShape {
            nodes: vec![a, b],
            edges: vec![EdgeShape::new("a", "b", false), EdgeShape::new("b", END, false)],
            entry: vec!["a".to_string()],
        }
    );
}

#[test]
fn a_conditional_edge_lists_each_target_once_including_end() {
    let graph = function(function(function(StateGraph::with_channels(&["r"]), "triage"), "x"), "y")
        .add_edge(START, "triage")
        .add_conditional_edges(
            "triage",
            |_state| "x".to_string(),
            [("x", "x"), ("y", "y"), ("again", "x"), (END, END)],
        )
        .add_edge("x", END)
        .add_edge("y", END)
        .compile()
        .unwrap();

    assert_eq!(
        graph.shape(),
        GraphShape {
            nodes: vec![
                node("triage", NodeKind::Function),
                node("x", NodeKind::Function),
                node("y", NodeKind::Function),
            ],
            edges: vec![
                EdgeShape::new("triage", END, true),
                EdgeShape::new("triage", "x", true),
                EdgeShape::new("triage", "y", true),
                EdgeShape::new("x", END, false),
                EdgeShape::new("y", END, false),
            ],
            entry: vec!["triage".to_string()],
        }
    );
}

#[test]
fn a_fan_in_node_is_a_join() {
    let graph =
        function(function(function(StateGraph::with_channels(&["x"]), "left"), "right"), "join")
            .add_edge(START, "left")
            .add_edge(START, "right")
            .add_edge("left", "join")
            .add_edge("right", "join")
            .add_edge("join", END)
            .compile()
            .unwrap();

    assert_eq!(
        graph.shape(),
        GraphShape {
            nodes: vec![
                node("join", NodeKind::Join),
                node("left", NodeKind::Function),
                node("right", NodeKind::Function),
            ],
            edges: vec![
                EdgeShape::new("join", END, false),
                EdgeShape::new("left", "join", false),
                EdgeShape::new("right", "join", false),
            ],
            entry: vec!["left".to_string(), "right".to_string()],
        }
    );
}

#[test]
fn a_cycle_keeps_its_back_edge() {
    let graph = function(StateGraph::with_channels(&["n"]), "step")
        .add_edge(START, "step")
        .add_conditional_edges("step", |_state| END.to_string(), [("step", "step"), (END, END)])
        .compile()
        .unwrap();

    assert_eq!(
        graph.shape(),
        GraphShape {
            nodes: vec![node("step", NodeKind::Function)],
            edges: vec![EdgeShape::new("step", END, true), EdgeShape::new("step", "step", true)],
            entry: vec!["step".to_string()],
        }
    );
}

#[test]
fn an_agent_node_is_a_model_step_with_its_agent_description() {
    struct Drafter;
    #[async_trait::async_trait]
    impl adk_core::Agent for Drafter {
        fn name(&self) -> &str {
            "draft"
        }
        fn description(&self) -> &str {
            "Drafts the invoice"
        }
        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }
        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }
    let graph = StateGraph::with_channels(&["messages"])
        .add_node(AgentNode::new(Arc::new(Drafter)))
        .add_edge(START, "draft")
        .add_edge("draft", END)
        .compile()
        .unwrap();

    let mut draft = node("draft", NodeKind::ModelStep);
    draft.description = "Drafts the invoice".to_string();
    assert_eq!(graph.shape().nodes, vec![draft]);
}

#[test]
fn a_subgraph_node_is_a_subgraph() {
    let inner: Arc<CompiledGraph> = Arc::new(
        function(StateGraph::with_channels(&["x"]), "inner")
            .add_edge(START, "inner")
            .add_edge("inner", END)
            .compile()
            .unwrap(),
    );
    let graph = StateGraph::with_channels(&["x"])
        .add_node(adk_graph::subgraph::SubgraphNode::new("review", inner))
        .add_edge(START, "review")
        .add_edge("review", END)
        .compile()
        .unwrap();

    assert_eq!(graph.shape().nodes[0].kind, NodeKind::Subgraph);
}

#[test]
fn the_shape_round_trips_through_camel_case_json() {
    let graph = function(function(StateGraph::with_channels(&["x"]), "a"), "b")
        .add_edge(START, "a")
        .add_edge("a", "b")
        .add_edge("b", END)
        .compile()
        .unwrap()
        .with_governed_node("b", GovernedNode::new().with_approval());
    let shape = graph.shape();

    let json = serde_json::to_value(&shape).unwrap();
    assert_eq!(
        json["nodes"][1],
        serde_json::json!({
            "name": "b",
            "description": "Graph workflow node",
            "kind": "function",
            "interruptBefore": false,
            "interruptAfter": false,
            "governed": { "requiresApproval": true, "spendsMoney": false },
        })
    );
    assert_eq!(serde_json::from_value::<GraphShape>(json).unwrap(), shape);
    // Deterministic: the same graph serializes to the same bytes every time.
    assert_eq!(
        serde_json::to_string(&graph.shape()).unwrap(),
        serde_json::to_string(&shape).unwrap()
    );
}

#[cfg(feature = "action")]
#[test]
fn an_action_node_reports_its_action_kind() {
    use adk_action::{
        ActionNodeConfig, Callbacks, ErrorHandling, ErrorMode, ExecutionControl,
        InputOutputMapping, LogLevel, SetMode, SetNodeConfig, StandardProperties, Tracing,
    };
    let standard = StandardProperties {
        id: "prepare".to_string(),
        name: "prepare".to_string(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: false, log_level: LogLevel::None },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "result".to_string() },
    };
    let set = ActionNodeConfig::Set(SetNodeConfig {
        standard,
        mode: SetMode::Set,
        variables: vec![],
        env_vars: None,
    });
    let graph = StateGraph::with_channels(&["x"])
        .add_node(adk_graph::action::ActionNodeExecutor::new(set))
        .add_edge(START, "prepare")
        .add_edge("prepare", END)
        .compile()
        .unwrap();

    assert_eq!(graph.shape().nodes[0].kind, NodeKind::Set);
}
