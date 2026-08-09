//! # Coding workflow as a graph ("ultra-review" style)
//!
//! Inspired by Claude Code's *ultracode / ultrareview* workflows — implement,
//! then fan out to **parallel specialist reviewers**, **synthesize** their
//! verdicts, and **iterate** until they approve — expressed as an
//! [`adk-graph`] `StateGraph`:
//!
//! ```text
//!   START → implement ──┬─▶ review:correctness ─┐
//!                       ├─▶ review:edge-cases  ─┤
//!                       └─▶ review:style       ─┤   (parallel, real agents)
//!                                                ▼
//!                                          synthesize  (fan-in barrier)
//!                                                │
//!                              decision ─────────┤
//!                          ┌── "revise" ◀────────┘
//!                          ▼                      └──▶ "finalize" → END
//!                       revise ─▶ (back to the three reviewers)
//! ```
//!
//! The reviewers run **concurrently** (same graph super-step), and `synthesize`
//! is a **deferred fan-in node** so it runs exactly once, after all three finish.
//!
//! Requires `GOOGLE_API_KEY` (Gemini 3) — or `CODING_PROVIDER=openai` + `OPENAI_API_KEY`.

use std::path::PathBuf;
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
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};

const APP_NAME: &str = "coding-graph";
const MAX_ROUNDS: i64 = 2;

/// The task the workflow implements and reviews.
const TASK: &str = "Implement `slug.py` with a function `slugify(text)` that returns a URL slug: \
    lowercase the text, replace any run of non-alphanumeric characters with a single hyphen, \
    and strip leading/trailing hyphens. Add a `__main__` block that prints \
    slugify('  Hello, World! 123  ').";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let model = build_model()?;
    let dir = tempfile::tempdir()?;
    let root: PathBuf = dir.path().to_path_buf();

    println!("ADK-Rust — coding workflow graph (ultra-review style)");
    println!("workspace: {}", root.display());
    println!("task: {TASK}\n");

    let graph = build_graph(model, root.clone())?;

    let mut input = State::new();
    input.insert("task".into(), json!(TASK));
    input.insert("round".into(), json!(0));

    let final_state = graph.invoke(input, ExecutionConfig::new("ultra")).await?;

    println!("\n══ workflow finished ══");
    println!("  rounds: {}", final_state.get("round").and_then(Value::as_i64).unwrap_or(0));
    println!("  decision: {}", final_state.get("decision").and_then(Value::as_str).unwrap_or("?"));

    // Independent verification: import the function and check it against the
    // spec + a couple of edge cases (robust to whether a __main__ block exists).
    println!("\n══ verification ══");
    for f in std::fs::read_dir(&root)? {
        println!("  file: {}", f?.file_name().to_string_lossy());
    }
    let check = "import slug\n\
        assert slug.slugify('  Hello, World! 123  ') == 'hello-world-123', repr(slug.slugify('  Hello, World! 123  '))\n\
        assert slug.slugify('') == '', 'empty'\n\
        assert slug.slugify('a---b__c') == 'a-b-c', repr(slug.slugify('a---b__c'))\n\
        print('ok')\n";
    let out =
        std::process::Command::new("python3").arg("-c").arg(check).current_dir(&root).output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let ok = o.status.success() && stdout.trim() == "ok";
            println!("  slugify spec + edge cases: {}", if ok { "✅ PASS" } else { "❌ FAIL" });
            if !ok {
                println!("  {}", stderr.trim());
                std::process::exit(1);
            }
        }
        Err(e) => {
            println!("  ❌ could not run python3: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Build the workflow graph.
fn build_graph(model: Arc<dyn Llm>, root: PathBuf) -> anyhow::Result<adk_graph::CompiledGraph> {
    let reviewers = [
        ("correctness", "whether slugify matches the spec exactly for normal input"),
        (
            "edge-cases",
            "edge cases: empty string, leading/trailing separators, multiple separators in a row, digits",
        ),
        (
            "style",
            "readability: a clear docstring, sensible names, and a single clean implementation",
        ),
    ];

    let mut graph = StateGraph::with_channels(&[
        "task",
        "round",
        "decision",
        "notes",
        "rev_correctness",
        "rev_edge-cases",
        "rev_style",
    ]);

    // implement: a coding agent writes the first version.
    graph = graph.add_node_fn("implement", {
        let model = model.clone();
        let root = root.clone();
        move |ctx| {
            let model = model.clone();
            let root = root.clone();
            async move {
                let task = ctx.get("task").and_then(Value::as_str).unwrap_or(TASK).to_string();
                banner("implement", "writing the first version");
                let agent = coding_agent(model, &root);
                run_agent(agent, "implement", &task, true).await.ok();
                Ok(NodeOutput::new())
            }
        }
    });

    // Three reviewers — these run concurrently (fan-out from implement / revise).
    for (name, desc) in reviewers {
        let node = format!("review_{name}");
        let key = format!("rev_{name}");
        graph = graph.add_node_fn(&node.clone(), {
            let model = model.clone();
            let root = root.clone();
            let name = name.to_string();
            let desc = desc.to_string();
            let key = key.clone();
            move |_ctx| {
                let model = model.clone();
                let root = root.clone();
                let name = name.clone();
                let desc = desc.clone();
                let key = key.clone();
                async move {
                    let agent = review_agent(model, &root, &name, &desc);
                    let prompt = "Review the code in the workspace now.";
                    let text = run_agent(agent, &format!("rev-{name}"), prompt, false)
                        .await
                        .unwrap_or_default();
                    let (approved, notes) = parse_verdict(&text);
                    println!(
                        "  🔎 review:{name:<11} → {}",
                        if approved {
                            "approve".to_string()
                        } else {
                            format!("changes — {notes}")
                        }
                    );
                    Ok(NodeOutput::new()
                        .with_update(&key, json!({ "approved": approved, "notes": notes })))
                }
            }
        });
    }

    // synthesize: fan-in. Runs once, after all three reviewers complete.
    graph = graph.add_deferred_node_fn(
        "synthesize",
        |ctx| async move {
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
            let decision = if all_approved || round >= MAX_ROUNDS { "finalize" } else { "revise" };
            banner(
                "synthesize",
                &format!(
                    "round {round}: {} → {decision}",
                    if all_approved {
                        "all approved".into()
                    } else {
                        format!("{} change request(s)", notes.len())
                    }
                ),
            );
            Ok(NodeOutput::new()
                .with_update("round", json!(round))
                .with_update("decision", json!(decision))
                .with_update("notes", json!(notes.join("\n"))))
        },
        DeferredNodeConfig {
            merge_strategy: MergeStrategy::Collect,
            fan_in_timeout: None,
            // All reviewers must arrive; a quorum would revise on partial feedback.
            min_predecessors: None,
        },
    );

    // revise: a coding agent applies the synthesized feedback.
    graph = graph.add_node_fn("revise", {
        let model = model.clone();
        let root = root.clone();
        move |ctx| {
            let model = model.clone();
            let root = root.clone();
            async move {
                let notes = ctx.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
                banner("revise", "applying review feedback");
                let prompt = format!(
                    "Apply this review feedback to slug.py, then run it to confirm it still \
                     works. Keep all behavior the original task requires (including the \
                     slugify function and the __main__ demo):\n{notes}"
                );
                let agent = coding_agent(model, &root);
                run_agent(agent, "revise", &prompt, true).await.ok();
                Ok(NodeOutput::new())
            }
        }
    });

    graph = graph.add_node_fn("finalize", |_| async {
        banner("finalize", "approved — done");
        Ok(NodeOutput::new())
    });

    // Wiring: fan-out → fan-in → conditional loop.
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

// ── Agents ───────────────────────────────────────────────────────────────────

fn coding_agent(model: Arc<dyn Llm>, root: &std::path::Path) -> Arc<dyn Agent> {
    CodingAgent::builder()
        .model(model)
        .workspace(Workspace::new(root))
        .build()
        .expect("coding agent")
        .into_agent()
}

fn review_agent(
    model: Arc<dyn Llm>,
    root: &std::path::Path,
    focus: &str,
    desc: &str,
) -> Arc<dyn Agent> {
    let instruction = format!(
        "You are a strict but fair senior code reviewer. Focus ONLY on {focus}: {desc}. \
         Use read_file/glob/grep to inspect the code in the workspace. Then reply with a \
         single final line 'VERDICT: approve' if it meets the bar, or 'VERDICT: changes' \
         followed by a short, specific bullet list of required changes. Do not request removing \
         any behavior the task requires (e.g. the __main__ demo). Do not modify files."
    );
    let agent = LlmAgentBuilder::new(format!("review-{focus}"))
        .model(model)
        .instruction(instruction)
        .toolset(Arc::new(DevToolset::new(Workspace::read_only(root))))
        .build()
        .expect("review agent");
    Arc::new(agent)
}

/// Run one agent turn; optionally print its tool calls. Returns the final text.
async fn run_agent(
    agent: Arc<dyn Agent>,
    session_id: &str,
    prompt: &str,
    show_tools: bool,
) -> anyhow::Result<String> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

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
                    Part::FunctionCall { name, .. } if show_tools => {
                        println!("       🔧 {name}");
                    }
                    Part::Text { text: t } if !t.is_empty() => text.push_str(t),
                    _ => {}
                }
            }
        }
    }
    Ok(text)
}

/// Parse a reviewer's `VERDICT:` line. Returns (approved, notes).
fn parse_verdict(text: &str) -> (bool, String) {
    let lower = text.to_lowercase();
    let approved = lower.contains("verdict: approve") || lower.contains("verdict:approve");
    if approved {
        return (true, String::new());
    }
    // Notes: everything after the verdict line, trimmed to a compact summary.
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

fn banner(node: &str, detail: &str) {
    println!("━━ {node} ━━ {detail}");
}

fn build_model() -> anyhow::Result<Arc<dyn Llm>> {
    let provider = std::env::var("CODING_PROVIDER").unwrap_or_else(|_| "gemini".into());
    match provider.as_str() {
        "openai" => {
            use adk_model::openai::{OpenAIClient, OpenAIConfig};
            let key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY is not set"))?;
            let model = std::env::var("CODING_MODEL").unwrap_or_else(|_| "gpt-5-mini".into());
            Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model))?))
        }
        _ => {
            let key = std::env::var("GOOGLE_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
                .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY / GEMINI_API_KEY is not set"))?;
            let model =
                std::env::var("CODING_MODEL").unwrap_or_else(|_| "gemini-3.1-flash-lite".into());
            Ok(Arc::new(GeminiModel::new(&key, &model)?))
        }
    }
}
