//! A support desk where the model chooses the branch, with no edge declaring it.
//!
//! The graph has four nodes and **no edge out of the classifier**. The classifier
//! is an `LlmAgent`; its answer becomes the route. This is the counterpart to
//! LangGraph's `Command(update=..., goto=...)`, whose docs make the same point:
//! the graph carries no conditional edge for the routing, because the node
//! decides.
//!
//! ```text
//!   START ─→ classify ┄┄┄→ refund_desk  ─→ END
//!                     ┄┄┄→ tech_desk    ─→ END
//!                     ┄┄┄→ billing_desk ─→ END
//!            (dotted: chosen at run time, not declared)
//! ```
//!
//! Why this needs `goto` rather than a conditional edge: a conditional edge maps
//! a route key to a target chosen when the graph is built. Here the classifier
//! writes its category and names the desk in the same step, so the decision and
//! the state update cannot disagree.
//!
//! Requires `OPENAI_API_KEY`. Run with:
//!
//! ```bash
//! cargo run --manifest-path examples/graph_goto_routing/Cargo.toml
//! ```
//!
//! Set `GRAPH_MODEL` to choose a model; it defaults to `gpt-5-mini`.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Event};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use serde_json::{Value, json};

/// The desks the classifier may choose. A name outside this set fails the run
/// with `GraphError::UnknownRouteTarget` rather than routing anywhere.
const DESKS: [(&str, &str); 3] =
    [("refund", "refund_desk"), ("technical", "tech_desk"), ("billing", "billing_desk")];

/// Reads the classifier's answer into the `category` channel.
fn category_from_events(events: &[Event]) -> HashMap<String, Value> {
    let text: String = events
        .iter()
        .filter_map(|event| event.content())
        .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
        .collect::<Vec<_>>()
        .join("");
    let category = text.trim().to_lowercase();
    let mut updates = HashMap::new();
    updates.insert("category".to_string(), json!(category));
    updates
}

/// Turns the category the model produced into the desk to run next.
///
/// Returning `None` would leave the declared edges in charge. The classifier has
/// none, so an unrecognised answer ends the run with no desk reached — which the
/// `handled` channel then shows as absent.
fn desk_for(updates: &HashMap<String, Value>) -> Option<Vec<String>> {
    let category = updates.get("category")?.as_str()?;
    DESKS
        .iter()
        .find(|(answer, _)| category.contains(answer))
        .map(|(_, desk)| vec![(*desk).to_string()])
}

fn desk_node(name: &'static str, note: &'static str) -> impl Fn() -> (&'static str, &'static str) {
    move || (name, note)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id = std::env::var("GRAPH_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    println!("model: {model_id}\n");

    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, &model_id))?);

    let classifier = Arc::new(
        LlmAgentBuilder::new("classify")
            .model(model)
            .instruction(
                "You route a support ticket. Reply with exactly one word: refund, \
                 technical, or billing. No punctuation, no explanation.",
            )
            .build()?,
    );

    let mut builder = StateGraph::with_channels(&["ticket", "category", "handled"]).add_node(
        AgentNode::new(classifier as Arc<dyn Agent>)
            .with_input_mapper(|state: &State| {
                let ticket = state.get("ticket").and_then(|v| v.as_str()).unwrap_or("");
                Content::new("user").with_text(format!("Ticket: {ticket}"))
            })
            .with_output_mapper(category_from_events)
            // The model's answer is the route. Nothing declares these targets.
            .with_goto_mapper(desk_for),
    );

    for (_, desk) in DESKS {
        let named = desk_node(desk, desk);
        builder = builder.add_node_fn(desk, move |_ctx| {
            let (name, _) = named();
            async move { Ok(NodeOutput::new().with_update("handled", json!(name))) }
        });
    }

    let mut graph = builder.add_edge(START, "classify");
    for (_, desk) in DESKS {
        graph = graph.add_edge(desk, END);
    }
    // Note what is absent: no edge leaves `classify`.
    let graph = graph.compile()?;

    let tickets = [
        "I was charged twice for the same order and want my money back.",
        "The app crashes every time I open the settings screen.",
        "Can you explain the line items on my October statement?",
    ];

    for (index, ticket) in tickets.iter().enumerate() {
        let mut input = State::new();
        input.insert("ticket".to_string(), json!(ticket));

        let state = graph.invoke(input, ExecutionConfig::new(&format!("ticket-{index}"))).await?;

        let category = state.get("category").and_then(|v| v.as_str()).unwrap_or("(none)");
        let handled = state.get("handled").and_then(|v| v.as_str()).unwrap_or("(no desk ran)");
        println!("ticket {}: {ticket}", index + 1);
        println!("  model answered: {category}");
        println!("  desk reached:   {handled}\n");
    }

    println!("Each desk was reached through a route the model chose. The graph");
    println!("declares no edge out of `classify`, so without the goto no desk");
    println!("could run at all.");
    Ok(())
}
