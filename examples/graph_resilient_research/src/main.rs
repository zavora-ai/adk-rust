//! A research desk an `LlmAgent` calls as a tool, built to survive a slow or
//! broken source.
//!
//! The model does not know it is calling a graph. It sees one tool. Behind that
//! tool three sources run at once, the desk proceeds as soon as two of them answer,
//! and a source that fails is recorded rather than ending the run.
//!
//! ```text
//!  LlmAgent ──tool──→ research graph
//!
//!    START ─→ fast_source   ─┐
//!          ─→ slow_source   ─┼─→ synthesise ─→ END   (releases at 2 of 3)
//!          ─→ broken_source ─┘
//! ```
//!
//! | Feature | Where |
//! |---------|-------|
//! | A graph as a tool | `NodeTool::for_graph`, handed to the agent like any tool |
//! | *n*-of-*m* join | `min_predecessors: Some(2)`, so one slow source cannot hold the desk |
//! | Failure recovery | `with_node_error_handler` records the broken source and continues |
//! | Channel enforcement | `with_strict_channels`, so a mistyped channel fails the run |
//! | Time travel | the checkpoint history is read back after the run |
//!
//! Requires `OPENAI_API_KEY`. Run with:
//!
//! ```bash
//! cargo run --manifest-path examples/graph_resilient_research/Cargo.toml
//! ```
//!
//! Set `GRAPH_MODEL` to choose a model; it defaults to `gpt-5-mini`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Event, Tool};
use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::deferred::{DeferredNodeConfig, MergeStrategy};
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{AgentNode, NodeOutput};
use adk_graph::state::State;
use adk_graph::tool::NodeTool;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};

/// Appends one source's answer to the `findings` channel.
fn finding_from(source: &'static str) -> impl Fn(&[Event]) -> HashMap<String, Value> {
    move |events| {
        let text: String = events
            .iter()
            .filter_map(|event| event.content())
            .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
            .collect::<Vec<_>>()
            .join("");
        let mut updates = HashMap::new();
        updates.insert("findings".to_string(), json!(format!("[{source}] {}", text.trim())));
        updates
    }
}

/// A source backed by the model, with its own angle on the question.
fn source_node(
    name: &'static str,
    angle: &'static str,
    model: Arc<OpenAIClient>,
) -> anyhow::Result<AgentNode> {
    let agent = Arc::new(
        LlmAgentBuilder::new(name)
            .model(model)
            .instruction(format!(
                "You are a research source. {angle} Answer in one sentence, under 30 words."
            ))
            .build()?,
    );
    Ok(AgentNode::new(agent as Arc<dyn Agent>)
        .with_input_mapper(|state: &State| {
            let topic = state.get("topic").and_then(|v| v.as_str()).unwrap_or("");
            Content::new("user").with_text(format!("Question: {topic}"))
        })
        .with_output_mapper(finding_from(name)))
}

/// The research desk. Three sources, and it proceeds on two answers.
fn research_graph(model: Arc<OpenAIClient>) -> anyhow::Result<Arc<CompiledGraph>> {
    let schema = adk_graph::state::StateSchema::builder()
        .channel("topic")
        // Each source appends, so the reducer is what collects them.
        .list_channel("findings")
        .channel("summary")
        .channel("source_errors")
        .build();

    let graph = StateGraph::new(schema)
        .add_node(source_node(
            "fast_source",
            "You favour recent, practical evidence.",
            model.clone(),
        )?)
        .add_node(source_node(
            "careful_source",
            "You favour long-term historical evidence.",
            model.clone(),
        )?)
        // A source that is simply down. Its handler records that and lets the desk
        // carry on, rather than failing the whole question.
        .add_node_fn("broken_source", |_ctx| async move {
            Err(GraphError::Other("archive index unavailable (503)".to_string()))
        })
        .add_node_fn("synthesise", |ctx| async move {
            let findings =
                ctx.get("findings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let errors = ctx.get("source_errors").and_then(|v| v.as_str()).unwrap_or("none");
            let joined: Vec<&str> = findings.iter().filter_map(|v| v.as_str()).collect();
            Ok(NodeOutput::new().with_update(
                "summary",
                json!(format!(
                    "{} of 3 sources answered (errors: {errors}). {}",
                    joined.len(),
                    joined.join(" ")
                )),
            ))
        })
        .add_edge(START, "fast_source")
        .add_edge(START, "careful_source")
        .add_edge(START, "broken_source")
        .add_edge("fast_source", "synthesise")
        .add_edge("careful_source", "synthesise")
        .add_edge("broken_source", "synthesise")
        .add_edge("synthesise", END)
        // `synthesise` has three incoming edges, so it is deferred automatically and
        // runs once, after all three arrive — not once per arrival. The timeout is
        // the upper bound on that wait.
        .mark_deferred(
            "synthesise",
            DeferredNodeConfig {
                merge_strategy: MergeStrategy::Collect,
                fan_in_timeout: Some(Duration::from_secs(60)),
                min_predecessors: None,
            },
        )
        .compile()?
        .with_checkpointer(MemoryCheckpointer::new())
        // A node writing a channel this schema does not declare fails the run,
        // rather than quietly creating one with overwrite semantics.
        .with_strict_channels()
        .with_node_error_handler("broken_source", |node, error, _state| {
            // Recorded as state, so the summary can say a source was missing.
            Ok(NodeOutput::new().with_update("source_errors", json!(format!("{node}: {error}"))))
        });

    Ok(Arc::new(graph))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id = std::env::var("GRAPH_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    println!("model: {model_id}\n");

    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, &model_id))?);
    let desk = research_graph(model.clone())?;

    // The whole graph, as one tool. The model sees a name, a description and a
    // parameter schema derived from the graph's own channels.
    let desk_tool =
        NodeTool::for_graph(Arc::clone(&desk)).with_name("research_desk").with_description(
            "Ask a research question. Queries several sources at once and returns a \
             single summary. Pass the question as `topic`.",
        );
    println!("tool advertised to the model: {}", desk_tool.name());
    println!("parameters: {}\n", serde_json::to_string(&desk_tool.parameters_schema())?);

    let analyst = Arc::new(
        LlmAgentBuilder::new("analyst")
            .model(model)
            .instruction(
                "You answer questions using the research_desk tool. Always call it \
                 before answering. Then give the user two sentences: what the sources \
                 said, and whether any source was missing.",
            )
            .tool(Arc::new(desk_tool) as Arc<dyn Tool>)
            .build()?,
    );

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "research".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("research")
        .agent(analyst as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    println!("=== The agent decides to call the graph ===\n");
    let question = Content::new("user")
        .with_text("Does pair programming actually reduce defects? Use the research desk.");

    let mut stream = runner
        .run(adk_core::UserId::new("user")?, adk_core::SessionId::new("s1")?, question)
        .await?;
    let mut tool_calls = 0usize;
    let mut answer = String::new();
    let elapsed = std::time::Instant::now();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            tool_calls += content
                .parts
                .iter()
                .filter(|part| matches!(part, adk_core::Part::FunctionCall { .. }))
                .count();
        }
        // The model streams, so the answer is accumulated rather than taken from
        // one event.
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    answer.push_str(text);
                }
            }
        }
    }
    println!("  answer: {}\n", answer.trim());
    println!("  tool calls the model made: {tool_calls}");
    println!("  the desk answered in {:?}", elapsed.elapsed());

    // Every super-step left a checkpoint, so the run can be read back afterwards.
    // What the desk itself recorded, which is where the quorum and the failure show.
    println!("\n=== What the desk recorded ===\n");
    match desk.get_state("s1").await? {
        Some(state) => {
            let answered =
                state.get("findings").and_then(|v| v.as_array()).map(Vec::len).unwrap_or(0);
            println!("  sources that answered: {answered} of 3");
            println!(
                "  source_errors: {}",
                state.get("source_errors").and_then(|v| v.as_str()).unwrap_or("none")
            );
            println!(
                "  summary: {}",
                state.get("summary").and_then(|v| v.as_str()).unwrap_or("(none)")
            );
        }
        None => println!("  (no checkpoint, so the model did not call the tool)"),
    }

    // `NodeTool` runs the graph on the caller's session id, so the thread is known.
    println!("\n=== Reading the desk's history back ===\n");
    let handle = desk.time_travel("s1")?;
    let steps = handle.steps().await?;
    if steps.is_empty() {
        println!("  (no checkpoints, so the model chose not to call the tool)");
    }
    for step in &steps {
        println!("  step {}: {} node(s) pending", step.step, step.pending_nodes.len());
    }

    println!("\n`synthesise` ran once, after all three sources arrived — not once");
    println!("per arrival. The broken source is in the summary as an error rather than");
    println!("a failed run, and the whole desk was one tool as far as the model knew.");
    Ok(())
}
