//! `ultracode`: a native, ultra-review-style coding workflow built on adk-graph.
//!
//! implement → fan out to parallel specialist reviewers → synthesize (fan-in) →
//! revise loop → finalize. Mirrors Claude Code's ultracode/ultrareview pattern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_agent::coding::CodingAgent;
use adk_core::{Agent, Content, Llm, Part, SessionId, UserId};
use adk_devtools::{DevToolset, Workspace};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::{DeferredNodeConfig, MergeStrategy};
use adk_model::ModelProvider;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "adk-rust";

const REVIEWERS: &[(&str, &str)] = &[
    ("correctness", "whether the implementation fully and correctly satisfies the task"),
    ("edge-cases", "edge cases and error handling the implementation might miss"),
    ("style", "readability, naming, structure, and idiomatic style"),
];

/// Run the ultracode workflow on `task` in `dir`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    cli_provider: Option<ModelProvider>,
    cli_model: Option<String>,
    cli_api_key: Option<String>,
    thinking_budget: Option<u32>,
    dir: String,
    task: String,
    max_rounds: i64,
) -> Result<()> {
    let (provider, model_id) = crate::resolve_model_id(cli_provider, cli_model);
    let model = crate::build_model(provider, &model_id, cli_api_key, thinking_budget)?;
    let root = PathBuf::from(&dir);

    println!("ultracode ({model_id}) on {dir}");
    println!("task: {task}");
    println!(
        "reviewers: {} | max rounds: {max_rounds}\n",
        REVIEWERS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
    );

    let graph = build_graph(model, root, max_rounds)?;
    let mut input = State::new();
    input.insert("task".into(), json!(task));
    input.insert("round".into(), json!(0));

    let final_state = graph.invoke(input, ExecutionConfig::new("ultracode")).await?;
    println!("\n══ ultracode finished ══");
    println!("  rounds: {}", final_state.get("round").and_then(Value::as_i64).unwrap_or(0));
    println!("  decision: {}", final_state.get("decision").and_then(Value::as_str).unwrap_or("?"));
    Ok(())
}

fn build_graph(
    model: Arc<dyn Llm>,
    root: PathBuf,
    max_rounds: i64,
) -> Result<adk_graph::CompiledGraph> {
    let mut graph = StateGraph::with_channels(&[
        "task",
        "round",
        "decision",
        "notes",
        "rev_correctness",
        "rev_edge-cases",
        "rev_style",
    ]);

    graph = graph.add_node_fn("implement", {
        let model = model.clone();
        let root = root.clone();
        move |ctx| {
            let model = model.clone();
            let root = root.clone();
            async move {
                let task = ctx.get("task").and_then(Value::as_str).unwrap_or_default().to_string();
                println!("━━ implement ━━");
                let agent = coding_agent(model, &root);
                run_agent(agent, "implement", &task, true).await.ok();
                Ok(NodeOutput::new())
            }
        }
    });

    for (name, desc) in REVIEWERS {
        let key = format!("rev_{name}");
        graph = graph.add_node_fn(&format!("review_{name}"), {
            let model = model.clone();
            let root = root.clone();
            let name = name.to_string();
            let desc = desc.to_string();
            let key = key.clone();
            move |ctx| {
                let model = model.clone();
                let root = root.clone();
                let name = name.clone();
                let desc = desc.clone();
                let key = key.clone();
                let task = ctx.get("task").and_then(Value::as_str).unwrap_or_default().to_string();
                async move {
                    let agent = review_agent(model, &root, &name, &desc);
                    let prompt = format!(
                        "The task being implemented is:\n{task}\n\nReview the code in the \
                         workspace against this task now."
                    );
                    let text = run_agent(agent, &format!("rev-{name}"), &prompt, false)
                        .await
                        .unwrap_or_default();
                    let (approved, notes) = parse_verdict(&text);
                    println!(
                        "  🔎 review:{name:<11} → {}",
                        if approved { "approve".into() } else { format!("changes — {notes}") }
                    );
                    Ok(NodeOutput::new()
                        .with_update(&key, json!({ "approved": approved, "notes": notes })))
                }
            }
        });
    }

    graph = graph.add_deferred_node_fn(
        "synthesize",
        move |ctx| async move {
            let round = ctx.get("round").and_then(Value::as_i64).unwrap_or(0) + 1;
            let mut notes = Vec::new();
            let mut all_approved = true;
            for key in ["rev_correctness", "rev_edge-cases", "rev_style"] {
                let approved = ctx
                    .get(key)
                    .and_then(|v| v.get("approved"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if !approved {
                    all_approved = false;
                    if let Some(n) =
                        ctx.get(key).and_then(|v| v.get("notes")).and_then(Value::as_str)
                    {
                        notes.push(n.to_string());
                    }
                }
            }
            let decision = if all_approved || round >= max_rounds { "finalize" } else { "revise" };
            println!(
                "━━ synthesize ━━ round {round}: {} → {decision}",
                if all_approved {
                    "all approved".into()
                } else {
                    format!("{} change request(s)", notes.len())
                }
            );
            Ok(NodeOutput::new()
                .with_update("round", json!(round))
                .with_update("decision", json!(decision))
                .with_update("notes", json!(notes.join("\n"))))
        },
        DeferredNodeConfig {
            merge_strategy: MergeStrategy::Collect,
            fan_in_timeout: None,
            ..Default::default()
        },
    );

    graph = graph.add_node_fn("revise", {
        let model = model.clone();
        let root = root.clone();
        move |ctx| {
            let model = model.clone();
            let root = root.clone();
            async move {
                let notes = ctx.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
                println!("━━ revise ━━");
                let prompt = format!(
                    "Apply this review feedback, then verify it still works. Keep all behavior \
                     the task requires:\n{notes}"
                );
                let agent = coding_agent(model, &root);
                run_agent(agent, "revise", &prompt, true).await.ok();
                Ok(NodeOutput::new())
            }
        }
    });

    graph = graph.add_node_fn("finalize", |_| async {
        println!("━━ finalize ━━ approved");
        Ok(NodeOutput::new())
    });

    graph = graph
        .add_edge(START, "implement")
        .add_edge("implement", "review_correctness")
        .add_edge("implement", "review_edge-cases")
        .add_edge("implement", "review_style")
        .add_edge("revise", "review_correctness")
        .add_edge("revise", "review_edge-cases")
        .add_edge("revise", "review_style")
        .add_edge("review_correctness", "synthesize")
        .add_edge("review_edge-cases", "synthesize")
        .add_edge("review_style", "synthesize")
        .add_conditional_edges(
            "synthesize",
            |state| state.get("decision").and_then(Value::as_str).unwrap_or("finalize").to_string(),
            [("revise", "revise"), ("finalize", "finalize")],
        )
        .add_edge("finalize", END);

    Ok(graph.compile()?.with_recursion_limit(16))
}

fn coding_agent(model: Arc<dyn Llm>, root: &Path) -> Arc<dyn Agent> {
    CodingAgent::builder()
        .model(model)
        .workspace(Workspace::new(root))
        .build()
        .expect("coding agent")
        .into_agent()
}

fn review_agent(model: Arc<dyn Llm>, root: &Path, focus: &str, desc: &str) -> Arc<dyn Agent> {
    let instruction = format!(
        "You are a strict but fair senior code reviewer. Focus ONLY on {focus}: {desc}. \
         Use read_file/glob/grep to inspect the code in the workspace. Reply with a single \
         final line 'VERDICT: approve' if it meets the bar, or 'VERDICT: changes' followed by \
         a short bullet list of required changes. Do not request removing behavior the task \
         requires. Do not modify files."
    );
    Arc::new(
        LlmAgentBuilder::new(format!("review-{focus}"))
            .model(model)
            .instruction(instruction)
            .toolset(Arc::new(DevToolset::new(Workspace::read_only(root))))
            .build()
            .expect("review agent"),
    )
}

async fn run_agent(
    agent: Arc<dyn Agent>,
    session_id: &str,
    prompt: &str,
    show_tools: bool,
) -> Result<String> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder().app_name(APP).agent(agent).session_service(sessions).build()?;
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
        )
        .await?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, .. } if show_tools => println!("       🔧 {name}"),
                    Part::Text { text: t } if !t.is_empty() => text.push_str(t),
                    _ => {}
                }
            }
        }
    }
    Ok(text)
}

fn parse_verdict(text: &str) -> (bool, String) {
    let lower = text.to_lowercase();
    if lower.contains("verdict: approve") || lower.contains("verdict:approve") {
        return (true, String::new());
    }
    let notes = text
        .lines()
        .skip_while(|l| !l.to_lowercase().contains("verdict"))
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let notes = if notes.is_empty() { text.trim().to_string() } else { notes };
    (false, notes.chars().take(180).collect())
}
