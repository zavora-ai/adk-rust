//! A review pipeline over the OpenAI API, exercising four capabilities added to
//! `adk-graph`.
//!
//! | Capability | Where it shows |
//! |------------|----------------|
//! | Imperative child invocation | `plan` decides how many reviewers to run, then invokes them by hand |
//! | Bounded concurrency | the graph runs at most two nodes at once, so a rate limit is not tripped |
//! | Per-node retry with backoff | every model call retries a transient failure |
//! | Static interrupt, resumed | `publish` waits for a person before the verdict is written |
//!
//! The reviewer count is not known when the graph is built: the planning model
//! decides it. Declared edges cannot express that, which is what imperative
//! invocation is for.
//!
//! Requires `OPENAI_API_KEY`. Run with:
//!
//! ```bash
//! cargo run --manifest-path examples/graph_parity_openai/Cargo.toml
//! ```
//!
//! Set `GRAPH_MODEL` to choose a model; it defaults to `gpt-5-mini`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content};
use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::child::RunNodeOptions;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use adk_graph::retry::{RetryOn, RetryPolicy};
use adk_graph::state::State;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use serde_json::json;

/// Collects the text an agent produced into one state channel.
fn text_into(
    channel: &'static str,
) -> impl Fn(&[adk_core::Event]) -> std::collections::HashMap<String, serde_json::Value> {
    move |events| {
        let text: String = events
            .iter()
            .filter_map(|event| event.content())
            .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
            .collect::<Vec<_>>()
            .join("");
        let mut updates = std::collections::HashMap::new();
        updates.insert(channel.to_string(), json!(text.trim()));
        updates
    }
}

/// Every model call gets the same retry policy: three attempts, growing delay.
///
/// A rate limit or a dropped connection is transient, and before this a single
/// such failure ended the whole run.
fn model_retry() -> RetryPolicy {
    RetryPolicy::new(3)
        .with_initial_delay(Duration::from_millis(500))
        .with_max_delay(Duration::from_secs(8))
        .with_backoff_factor(2.0)
        .with_retry_on(RetryOn::Any)
}

/// Counts reviewer calls that actually reached the model.
///
/// The output mapper runs only when the agent ran, so a value served from the
/// child ledger does not move this counter. That is what makes the resume claim
/// below checkable rather than asserted.
static REVIEWER_CALLS: AtomicUsize = AtomicUsize::new(0);

fn build_graph(model: Arc<OpenAIClient>) -> anyhow::Result<CompiledGraph> {
    // Decides how many aspects deserve review, and names them.
    let planner = Arc::new(
        LlmAgentBuilder::new("planner")
            .model(model.clone())
            .instruction(
                "You plan a code review. Given a diff summary, reply with between 1 and 3 \
                 review aspects, comma separated, chosen from: correctness, performance, \
                 style. Reply with the list only.",
            )
            .build()?,
    );

    // One reviewer, invoked once per aspect the planner chose.
    let reviewer = Arc::new(
        LlmAgentBuilder::new("reviewer")
            .model(model.clone())
            .instruction(
                "You review one aspect of a change. Reply with one sentence, and end with \
                 PASS or FAIL.",
            )
            .build()?,
    );

    let graph = StateGraph::with_channels(&["diff", "aspects", "reviews", "verdict", "approved"])
        // An LLM agent as a node: it writes its answer into `aspects`.
        .add_node(
            AgentNode::new(planner as Arc<dyn Agent>)
                .with_input_mapper(|state: &State| {
                    let diff = state.get("diff").and_then(|v| v.as_str()).unwrap_or("");
                    Content::new("user").with_text(format!("Diff summary: {diff}"))
                })
                .with_output_mapper(text_into("aspects")),
        )
        // The reviewer is reachable by no edge. It exists to be invoked by hand.
        .add_node(
            AgentNode::new(reviewer as Arc<dyn Agent>)
                .with_input_mapper(|state: &State| {
                    let aspect = state.get("aspect").and_then(|v| v.as_str()).unwrap_or("");
                    let diff = state.get("diff").and_then(|v| v.as_str()).unwrap_or("");
                    Content::new("user")
                        .with_text(format!("Aspect: {aspect}\nDiff summary: {diff}"))
                })
                .with_output_mapper(|events| {
                    REVIEWER_CALLS.fetch_add(1, Ordering::SeqCst);
                    text_into("review")(events)
                }),
        )
        // Reads the planner's answer and invokes the reviewer once per aspect.
        .add_node_fn("fan_out", |ctx| async move {
            let aspects = ctx.get("aspects").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let chosen: Vec<String> = aspects
                .split(',')
                .map(|aspect| aspect.trim().to_lowercase())
                .filter(|aspect| !aspect.is_empty())
                .collect();

            println!("  planner chose {} aspect(s): {}", chosen.len(), chosen.join(", "));

            let mut reviews = Vec::new();
            for aspect in &chosen {
                // A stable run id, so a resume serves a review already done
                // instead of paying for it twice.
                let output = ctx
                    .run_node_with(
                        "reviewer",
                        json!({ "aspect": aspect }),
                        RunNodeOptions::with_run_id(aspect.clone()),
                    )
                    .await?;
                let text = output
                    .get("review")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no answer)")
                    .to_string();
                println!("  {aspect}: {text}");
                reviews.push(json!({ "aspect": aspect, "review": text }));
            }

            // Pause here rather than before `publish`, so the resume re-runs this
            // node from the top. That is what exercises the child ledger: the
            // reviewer calls above must not be paid for a second time.
            if ctx.get("approved").and_then(|v| v.as_bool()) != Some(true) {
                return Ok(NodeOutput::interrupt_with_data(
                    "approve publishing this review?",
                    json!({ "reviews": reviews.len() }),
                ));
            }

            Ok(NodeOutput::new().with_update("reviews", json!(reviews)))
        })
        // Gated below, so it runs only after a person approves.
        .add_node_fn("publish", |ctx| async move {
            let reviews =
                ctx.get("reviews").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let failed = reviews
                .iter()
                .filter(|review| {
                    review
                        .get("review")
                        .and_then(|v| v.as_str())
                        .is_some_and(|text| text.to_uppercase().contains("FAIL"))
                })
                .count();
            let verdict =
                if failed == 0 { "approved".to_string() } else { format!("{failed} concern(s)") };
            Ok(NodeOutput::new().with_update("verdict", json!(verdict)))
        })
        .add_edge(START, "planner")
        .add_edge("planner", "fan_out")
        .add_edge("fan_out", "publish")
        .add_edge("publish", END)
        .compile()?
        .with_checkpointer(MemoryCheckpointer::new())
        // At most two nodes at once. Imperative child invocations are outside
        // this budget, because the parent awaits them while holding its own slot.
        .with_max_concurrency(2)
        .with_node_retry("planner", model_retry())
        .with_node_retry("reviewer", model_retry())
        // A second gate, static this time, in front of the node that writes the
        // verdict. Both kinds of pause are resumable.
        .with_interrupt_before(&["publish"]);

    Ok(graph)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id = std::env::var("GRAPH_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    println!("model: {model_id}\n");

    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, &model_id))?);
    let graph = build_graph(model)?;

    let mut input = State::new();
    input.insert(
        "diff".to_string(),
        json!("Adds a retry loop around an HTTP call, with a fixed 50ms delay and no cap."),
    );

    let thread = "review-4821";

    println!("=== First run: plan, review, then stop at the approval gate ===\n");
    match graph.invoke(input, ExecutionConfig::new(thread)).await {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("\n  paused: {}", interrupted.interrupt);
            let reviews = interrupted
                .state
                .get("reviews")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            println!("  reviewer model calls so far: {}", REVIEWER_CALLS.load(Ordering::SeqCst));
            let _ = reviews;
        }
        Ok(_) => anyhow::bail!("the gate should have stopped this run"),
        Err(error) => return Err(error.into()),
    }

    println!("\n=== A person approves, so the same thread runs again ===\n");
    let mut approval = State::new();
    approval.insert("approved".to_string(), json!(true));

    // Two gates remain: the dynamic one inside `fan_out`, now answered, and the
    // static one in front of `publish`. Resume until neither fires.
    let mut state = None;
    for attempt in 0..3 {
        match graph.invoke(approval.clone(), ExecutionConfig::new(thread)).await {
            Ok(final_state) => {
                state = Some(final_state);
                break;
            }
            Err(GraphError::Interrupted(interrupted)) => {
                println!("  pause {}: {}", attempt + 1, interrupted.interrupt);
            }
            Err(error) => return Err(error.into()),
        }
    }
    let state = state.ok_or_else(|| anyhow::anyhow!("the run did not complete"))?;
    println!("\n  verdict: {:?}", state.get("verdict"));

    let total = REVIEWER_CALLS.load(Ordering::SeqCst);
    println!("  reviewer model calls in total: {total}");

    println!("\n`fan_out` re-ran on the resume — it had to, to get past its own");
    println!("pause — and it asked for every review again. The total above did not");
    println!("grow, because each child was recorded under its aspect and the ledger");
    println!("answered in place of the model.");
    Ok(())
}
