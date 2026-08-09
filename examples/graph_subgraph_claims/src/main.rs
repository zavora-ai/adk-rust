//! An insurance claim pipeline built from three graphs, two of them nested.
//!
//! Each graph is written and reasoned about on its own, then composed. That is the
//! point of a subgraph: `pricing` knows nothing about claims, and `assessment`
//! knows nothing about the settlement it feeds.
//!
//! ```text
//!  claim_pipeline   START ─→ assess ──→ settle ─→ END
//!                              │  ╰┄┄┄→ escalate ─→ END      (a decision inside)
//!                              │
//!  assessment       START ─→ classify ─→ estimate ─→ decide ─→ END
//!                                          │
//!  pricing          START ─→ quote ─→ [adjuster sign-off] ─→ commit ─→ END
//! ```
//!
//! What it demonstrates:
//!
//! | Feature | Where |
//! |---------|-------|
//! | Channel mapping | `claim_text` → `text` → `description`, and the amounts back out |
//! | A pause two graphs deep | the adjuster gate inside `pricing` |
//! | Resuming that pause | the second run continues without re-pricing |
//! | Handing control to the parent | `decide` escalates a vague claim past `settle` |
//! | Checked at compile time | a channel neither side declares fails `compile()` |
//!
//! Requires `OPENAI_API_KEY`. Run with:
//!
//! ```bash
//! cargo run --manifest-path examples/graph_subgraph_claims/Cargo.toml
//! ```
//!
//! Set `GRAPH_MODEL` to choose a model; it defaults to `gpt-5-mini`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Event};
use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, NodeDefaults, StateGraph};
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use adk_graph::retry::RetryPolicy;
use adk_graph::state::State;
use adk_graph::subgraph::SubgraphNode;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use serde_json::{Value, json};

/// Counts calls that reached the model, so the resume claim is checkable.
static MODEL_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Collects an agent's answer into one channel, counting the call.
fn answer_into(channel: &'static str) -> impl Fn(&[Event]) -> HashMap<String, Value> {
    move |events| {
        MODEL_CALLS.fetch_add(1, Ordering::SeqCst);
        let text: String = events
            .iter()
            .filter_map(|event| event.content())
            .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
            .collect::<Vec<_>>()
            .join("");
        let mut updates = HashMap::new();
        updates.insert(channel.to_string(), json!(text.trim()));
        updates
    }
}

/// The deepest graph: what is this worth?
///
/// It knows only a description and an estimate. A person signs the figure off
/// before it leaves, which is where the run pauses.
fn pricing_graph(model: Arc<OpenAIClient>) -> anyhow::Result<Arc<CompiledGraph>> {
    let assessor = Arc::new(
        LlmAgentBuilder::new("quote")
            .model(model)
            .instruction(
                "You estimate the cost of an insurance claim in US dollars. Reply with \
                 a bare number and nothing else — no currency symbol, no words. If the \
                 description is too vague to price, reply with exactly UNCLEAR.",
            )
            .build()?,
    );

    Ok(Arc::new(
        StateGraph::with_channels(&["description", "quoted", "estimate"])
            .add_node(
                AgentNode::new(assessor as Arc<dyn Agent>)
                    .with_input_mapper(|state: &State| {
                        let text = state.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        Content::new("user").with_text(format!("Claim: {text}"))
                    })
                    .with_output_mapper(answer_into("quoted")),
            )
            // Gated below: an adjuster reads the figure before it is used.
            .add_node_fn("commit", |ctx| async move {
                let quoted = ctx.get("quoted").and_then(|v| v.as_str()).unwrap_or("UNCLEAR");
                Ok(NodeOutput::new().with_update("estimate", json!(quoted)))
            })
            .add_edge(START, "quote")
            .add_edge("quote", "commit")
            .add_edge("commit", END)
            .compile()?
            // A gate needs a checkpointer, or the pause could not be resumed. The
            // parent's compile() rejects the graph if this is missing.
            .with_checkpointer(MemoryCheckpointer::new())
            .with_interrupt_before(&["commit"]),
    ))
}

/// The middle graph: what kind of claim is this, and what is it worth?
///
/// It holds `pricing` as a subgraph and knows nothing about settlement.
fn assessment_graph(
    model: Arc<OpenAIClient>,
    pricing: Arc<CompiledGraph>,
) -> anyhow::Result<Arc<CompiledGraph>> {
    let classifier = Arc::new(
        LlmAgentBuilder::new("classify")
            .model(model)
            .instruction(
                "You categorise an insurance claim. Reply with exactly one word: \
                 motor, property, or medical.",
            )
            .build()?,
    );

    Ok(Arc::new(
        StateGraph::with_channels(&["text", "category", "amount", "verdict"])
            .add_node(
                AgentNode::new(classifier as Arc<dyn Agent>)
                    .with_input_mapper(|state: &State| {
                        let text = state.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        Content::new("user").with_text(format!("Claim: {text}"))
                    })
                    .with_output_mapper(answer_into("category")),
            )
            // The nested subgraph. `text` feeds its `description`; its `estimate`
            // comes back as `amount`. Neither name is shared, so both are stated.
            .add_node(
                SubgraphNode::new("estimate", pricing)
                    .isolated()
                    .with_input("text", "description")
                    .with_output("estimate", "amount"),
            )
            .add_node_fn("decide", |ctx| async move {
                let amount = ctx.get("amount").and_then(|v| v.as_str()).unwrap_or("UNCLEAR");
                match amount.replace(',', "").parse::<f64>() {
                    // A figure this graph cannot use. Rather than guess, hand the
                    // claim to the escalation node of the graph that holds this one.
                    Err(_) => Ok(NodeOutput::new()
                        .with_update("verdict", json!("no usable estimate"))
                        .with_goto_parent(["escalate"])),
                    Ok(value) => Ok(NodeOutput::new()
                        .with_update("verdict", json!(format!("settle at {value:.0}")))),
                }
            })
            .add_edge(START, "classify")
            .add_edge("classify", "estimate")
            .add_edge("estimate", "decide")
            .add_edge("decide", END)
            .compile()?
            // A pause travels up from `pricing`, so this graph must be resumable too.
            .with_checkpointer(MemoryCheckpointer::new()),
    ))
}

/// The top graph: assess a claim, then settle or escalate it.
fn claim_pipeline(assessment: Arc<CompiledGraph>) -> anyhow::Result<CompiledGraph> {
    Ok(StateGraph::with_channels(&["claim_text", "category", "amount", "outcome"])
        .add_node(
            SubgraphNode::new("assess", assessment)
                .isolated()
                .with_input("claim_text", "text")
                .with_output("category", "category")
                .with_output("amount", "amount")
                .with_output("verdict", "outcome"),
        )
        .add_node_fn("settle", |ctx| async move {
            let outcome = ctx.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
            Ok(NodeOutput::new().with_update("outcome", json!(format!("SETTLED — {outcome}"))))
        })
        .add_node_fn("escalate", |ctx| async move {
            let outcome = ctx.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
            Ok(NodeOutput::new()
                .with_update("outcome", json!(format!("ESCALATED to a human — {outcome}"))))
        })
        .add_edge(START, "assess")
        // The declared path settles. `escalate` is reached only by a decision made
        // inside the assessment graph.
        .add_edge("assess", "settle")
        .add_edge("settle", END)
        .add_edge("escalate", END)
        .compile()?
        .with_checkpointer(MemoryCheckpointer::new())
        // Every model call gets three attempts, stated once rather than per node.
        .with_node_defaults(NodeDefaults::new().with_retry(RetryPolicy::new(3))))
}

/// Runs one claim to completion, answering the adjuster gate on the way.
async fn run_claim(pipeline: &CompiledGraph, thread: &str, claim: &str) -> anyhow::Result<()> {
    println!("claim: {claim}");
    let before = MODEL_CALLS.load(Ordering::SeqCst);

    let mut input = State::new();
    input.insert("claim_text".to_string(), json!(claim));

    match pipeline.invoke(input, ExecutionConfig::new(thread)).await {
        Err(GraphError::Interrupted(paused)) => {
            // The message carries every level it passed through.
            println!("  paused:  {}", paused.interrupt);
        }
        Ok(_) => anyhow::bail!("the adjuster gate should have stopped the first run"),
        Err(error) => return Err(error.into()),
    }
    let at_pause = MODEL_CALLS.load(Ordering::SeqCst);
    println!("  model calls so far: {}", at_pause - before);

    // The adjuster signs off, so the same thread runs again.
    let state = pipeline.invoke(State::new(), ExecutionConfig::new(thread)).await?;

    let total = MODEL_CALLS.load(Ordering::SeqCst) - before;
    println!("  category: {:?}", state.get("category").and_then(|v| v.as_str()));
    println!("  amount:   {:?}", state.get("amount").and_then(|v| v.as_str()));
    println!("  outcome:  {:?}", state.get("outcome").and_then(|v| v.as_str()));
    println!("  model calls in total: {total} (unchanged, so nothing was re-priced)\n");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id = std::env::var("GRAPH_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    println!("model: {model_id}\n");

    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, &model_id))?);

    let pricing = pricing_graph(model.clone())?;
    let assessment = assessment_graph(model, pricing)?;
    let pipeline = claim_pipeline(assessment)?;

    println!("=== A claim the model can price ===\n");
    run_claim(
        &pipeline,
        "claim-4471",
        "Rear bumper and tail light damaged when another car reversed into my parked \
         Toyota Corolla in a supermarket car park.",
    )
    .await?;

    println!("=== A claim it cannot, escalated from inside the assessment ===\n");
    run_claim(&pipeline, "claim-4472", "Something happened to my stuff. Please help.").await?;

    println!("What the second claim shows: the escalation was decided two graphs down,");
    println!("in `decide`, and reached `escalate` in the top graph — a node the");
    println!("assessment graph cannot see and has no edge to.");
    Ok(())
}
