//! Multi-Perspective Analysis — three LLM analysts under one `ParallelAgent`.
//!
//! A technical, a business, and a UX analyst each answer the same question from
//! their own angle. `ParallelAgent` runs all three concurrently and merges their
//! event streams, so the wall-clock cost is roughly the slowest branch rather
//! than the sum of all three.
//!
//! The output makes that visible: each line is tagged with the branch that
//! produced it and the elapsed time since the run started, and the summary at
//! the end compares wall-clock time against the sum of per-branch spans. If the
//! branches were executing one after another, those two numbers would match.
//!
//! Run: cargo run --manifest-path examples/multi_perspective_analysis/Cargo.toml

use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::{SessionId, UserId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cheapest, fastest current model — good enough for three parallel opinions.
const MODEL: &str = "gemini-3.7-flash";

const QUESTION: &str = "Should a startup adopt WebAssembly for their web app?";

fn analyst(name: &str, instruction: &str, api_key: &str) -> anyhow::Result<Arc<dyn Agent>> {
    let agent = LlmAgentBuilder::new(name)
        .instruction(instruction)
        .model(Arc::new(GeminiModel::new(api_key, MODEL)?))
        .build()?;
    Ok(Arc::new(agent) as Arc<dyn Agent>)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY is not set — copy .env.example to .env"))?;

    let parallel = Arc::new(ParallelAgent::new(
        "multi_perspective_analysis",
        vec![
            analyst(
                "technical_analyst",
                "Analyze from a technical perspective. Be specific about implementation. \
                 Answer in at most three sentences.",
                &api_key,
            )?,
            analyst(
                "business_analyst",
                "Analyze from a business and market perspective. Focus on ROI and strategy. \
                 Answer in at most three sentences.",
                &api_key,
            )?,
            analyst(
                "ux_analyst",
                "Analyze from a user experience perspective. Focus on usability. \
                 Answer in at most three sentences.",
                &api_key,
            )?,
        ],
    ));

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "multi_perspective".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    // The typestate builder is used rather than a `RunnerConfig` struct literal:
    // it enforces the required fields at compile time and does not break when
    // optional fields are added.
    let runner = Runner::builder()
        .app_name("multi_perspective")
        .agent(parallel)
        .session_service(sessions)
        .build()?;

    println!("Question: {QUESTION}\n");
    println!("Running 3 analysts concurrently under ParallelAgent...\n");

    let started = Instant::now();
    let mut stream = runner
        .run(UserId::new("user")?, SessionId::new("s1")?, Content::new("user").with_text(QUESTION))
        .await?;

    // Per-branch first/last event times, so the overlap is measurable rather
    // than asserted. Insertion order is kept for a stable summary.
    let mut order: Vec<String> = Vec::new();
    let mut spans: HashMap<String, (Duration, Duration)> = HashMap::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        let Some(content) = &event.llm_response.content else {
            continue;
        };

        let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
        if text.trim().is_empty() {
            continue;
        }

        let at = started.elapsed();
        let branch = event.author.clone();
        spans.entry(branch.clone()).and_modify(|(_, last)| *last = at).or_insert_with(|| {
            order.push(branch.clone());
            (at, at)
        });

        // Tagging each chunk with its branch shows the streams interleaving.
        println!("[{:>6} ms] {branch}: {}", at.as_millis(), text.trim());
    }

    let wall_clock = started.elapsed();
    let branch_total: Duration = spans.values().map(|(first, last)| *last - *first).sum();

    println!("\n─── timing ───");
    for branch in &order {
        let (first, last) = spans[branch];
        println!(
            "  {branch:<20} first event {:>6} ms, last event {:>6} ms",
            first.as_millis(),
            last.as_millis()
        );
    }
    println!("  wall clock            {:>6} ms", wall_clock.as_millis());
    println!("  sum of branch spans   {:>6} ms", branch_total.as_millis());
    println!(
        "\n{} analysts finished in {} ms. Serial execution would cost roughly the sum above.",
        order.len(),
        wall_clock.as_millis()
    );

    Ok(())
}
