//! OpenAI planning-and-review graph served through the runtime UI.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Event, Part};
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::node::AgentNode;
use adk_graph::state::State;
use runtime_ui_showcase_example::{openai_model, serve};
use serde_json::json;

fn text_update(key: &'static str, events: &[Event]) -> HashMap<String, serde_json::Value> {
    let text = events
        .iter()
        .filter_map(Event::content)
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect::<String>();
    HashMap::from([(key.to_string(), json!(text))])
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = openai_model()?;
    let planner = Arc::new(
        LlmAgentBuilder::new("plan")
            .description("Creates a bounded migration plan")
            .instruction(
                "Create a numbered, three-step implementation plan. Include one measurable \
                 success criterion per step. Return only the plan.",
            )
            .model(model.clone())
            .build()?,
    );
    let reviewer = Arc::new(
        LlmAgentBuilder::new("review")
            .description("Reviews the plan and publishes the final workflow report")
            .instruction(
                "Review the proposed plan for operational risk. Return a Markdown report with \
                 a heading, a risk table, the improved plan, and a final go/no-go verdict.",
            )
            .model(model)
            .build()?,
    );

    let graph = GraphAgent::builder("migration_workflow")
        .description("A deterministic planning then review workflow")
        .channels(&["input", "plan", "output"])
        .node(
            AgentNode::new(planner as Arc<dyn Agent>)
                .with_input_mapper(|state: &State| {
                    Content::new("user").with_text(
                        state.get("input").and_then(|value| value.as_str()).unwrap_or_default(),
                    )
                })
                .with_output_mapper(|events| text_update("plan", events)),
        )
        .node(
            AgentNode::new(reviewer as Arc<dyn Agent>)
                .with_input_mapper(|state: &State| {
                    let request =
                        state.get("input").and_then(|value| value.as_str()).unwrap_or_default();
                    let plan =
                        state.get("plan").and_then(|value| value.as_str()).unwrap_or_default();
                    Content::new("user").with_text(format!(
                        "Original request:\n{request}\n\nProposed plan:\n{plan}"
                    ))
                })
                .with_output_mapper(|events| text_update("output", events)),
        )
        .edge(START, "plan")
        .edge("plan", "review")
        .edge("review", END)
        .build()?;
    serve(Arc::new(graph) as Arc<dyn Agent>, "runtime-ui-graph").await
}
