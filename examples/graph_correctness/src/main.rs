//! Graph joins after branches and loops, agent errors inside a graph, and work a graph re-enters.
//!
//! Every scenario runs a real graph as a `GraphAgent` under a `Runner`, checkpointed to SQLite,
//! and asserts on what the turn answered, what each model received, how often each node ran, and
//! what the checkpoint holds. Agent nodes are `LlmAgent`s on the real `OpenAIClient`, pointed at a
//! scripted OpenAI-compatible server on 127.0.0.1 — one of them answers `429`. The
//! `live_conditional_merge` scenario runs one `LlmAgent` node on Gemini when `GOOGLE_API_KEY` is
//! set.
//!
//! ```bash
//! cargo run --manifest-path examples/graph_correctness/Cargo.toml
//! cargo run --manifest-path examples/graph_correctness/Cargo.toml -- --scenario review_loop
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Event, Llm, Part, SessionId, Tool, UserId};
use adk_graph::agent::{GraphAgent, session_thread_id};
use adk_graph::checkpoint::{Checkpointer, SqliteCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, NodeDefaults, StateGraph};
use adk_graph::node::{AgentNode, NodeOutput};
use adk_graph::retry::RetryPolicy;
use adk_graph::state::State;
use adk_graph::subgraph::SubgraphNode;
use adk_graph::timeout::{OnTimeout, TimeoutPolicy};
use adk_graph::tool::NodeTool;
use adk_model::RetryConfig;
use adk_model::openai::OpenAIClient;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, last_user_text, tool_message};
use serde_json::{Value, json};

const APP: &str = "graph-correctness";
const USER: &str = "user";
const LIVE_MODEL: &str = "gemini-3.5-flash-lite";

const SCENARIOS: &[&str] = &[
    "review_loop",
    "conditional_merge",
    "builder_join",
    "agent_error_stops_the_graph",
    "agent_error_is_retried",
    "graph_tool_called_twice",
    "subgraph_in_a_loop",
    "child_in_a_loop",
    "parent_retry_serves_finished_children",
    "timeout_policy_with_subgraph_and_children",
    "fan_in_state_stays_bounded",
    "live_conditional_merge",
];

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

/// What one turn produced: its final text, or the error that ended it.
struct Turn {
    text: String,
    error: Option<String>,
}

/// Shared by every scenario: the SQLite checkpointer each graph writes to.
struct Context {
    checkpointer: Arc<dyn Checkpointer>,
    _dir: tempfile::TempDir,
}

impl Context {
    async fn new() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("checkpoints.db").display());
        let checkpointer = Arc::new(SqliteCheckpointer::new(&url).await?) as Arc<dyn Checkpointer>;
        Ok(Self { checkpointer, _dir: dir })
    }
}

/// Runs `inputs` as consecutive turns of one session under a `Runner`.
async fn run_turns(
    agent: Arc<dyn Agent>,
    session: &str,
    inputs: &[&str],
) -> anyhow::Result<Vec<Turn>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?;

    let mut turns = Vec::new();
    for input in inputs {
        let mut stream = runner
            .run(
                UserId::new(USER)?,
                SessionId::new(session)?,
                Content::new("user").with_text(*input),
            )
            .await?;
        let mut events = Vec::new();
        let mut error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        turns.push(Turn { text: final_text(&events), error });
    }
    Ok(turns)
}

/// The text of the last complete event, or of the partial ones when none is complete.
fn final_text(events: &[Event]) -> String {
    let text_of = |event: &Event| -> String {
        event
            .llm_response
            .content
            .as_ref()
            .map(|content| content.parts.iter().filter_map(Part::text).collect())
            .unwrap_or_default()
    };
    events
        .iter()
        .rev()
        .filter(|event| !event.llm_response.partial)
        .map(text_of)
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| events.iter().map(text_of).collect())
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    // The client's own retries are off, so each request the server sees is one node attempt.
    Ok(Arc::new(
        OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?
            .with_retry_config(RetryConfig::disabled()),
    ))
}

fn llm_agent(name: &str, model: Arc<dyn Llm>, instruction: &str) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(LlmAgentBuilder::new(name).model(model).instruction(instruction).build()?))
}

/// An `AgentNode` whose prompt is built from state and whose answer lands in `channel`.
fn agent_node(
    agent: Arc<dyn Agent>,
    prompt: impl Fn(&State) -> String + Send + Sync + 'static,
    channel: &'static str,
) -> AgentNode {
    AgentNode::new(agent)
        .with_input_mapper(move |state| Content::new("user").with_text(prompt(state)))
        .with_output_mapper(move |events| {
            HashMap::from([(channel.to_string(), json!(final_text(events)))])
        })
}

fn text(state: &State, key: &str) -> String {
    state.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn int(state: &State, key: &str) -> i64 {
    state.get(key).and_then(Value::as_i64).unwrap_or_default()
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

/// The coding-graph shape: three reviewers re-entered by a revise loop and joined by
/// `synthesize`. Each reviewer has two incoming edges, `implement` and `revise`, and only one
/// arrives per pass, so waiting for both left the reviewers stranded and the run returned `Ok`.
async fn review_loop(context: &Context) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, _| {
        let prompt = last_user_text(request);
        if prompt.starts_with("Implement") || prompt.starts_with("Revise") {
            Reply::Text("slug.py written".into())
        } else if prompt.contains("round 1") {
            Reply::Text("VERDICT: changes - add a docstring".into())
        } else {
            Reply::Text("VERDICT: approve".into())
        }
    }))
    .await?;
    let model = scripted_model(&server)?;
    let syntheses = counter();

    let reviewers = ["review_correctness", "review_edge_cases", "review_style"];
    let mut graph = StateGraph::with_channels(&[
        "input",
        "code",
        "round",
        "decision",
        "notes",
        "output",
        "review_correctness",
        "review_edge_cases",
        "review_style",
    ])
    .add_node(agent_node(
        llm_agent("implement", model.clone(), "Write the code you are asked for.")?,
        |state| format!("Implement {}", text(state, "input")),
        "code",
    ))
    .add_node(agent_node(
        llm_agent("revise", model.clone(), "Apply the review notes.")?,
        |state| format!("Revise: {}", text(state, "notes")),
        "code",
    ));
    for reviewer in reviewers {
        graph = graph
            .add_node(agent_node(
                llm_agent(reviewer, model.clone(), "Review the code. End with a VERDICT line.")?,
                |state| {
                    format!("Review round {}: {}", int(state, "round") + 1, text(state, "code"))
                },
                reviewer,
            ))
            .add_edge("implement", reviewer)
            .add_edge("revise", reviewer)
            .add_edge(reviewer, "synthesize");
    }
    let runs = Arc::clone(&syntheses);
    let graph = graph
        .add_node_fn("synthesize", move |ctx| {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let round = int(&ctx.state, "round") + 1;
                let verdicts: Vec<String> =
                    reviewers.iter().map(|reviewer| text(&ctx.state, reviewer)).collect();
                let approved = verdicts.iter().all(|v| v.contains("VERDICT: approve"));
                let decision = if approved || round >= 3 { "finalize" } else { "revise" };
                Ok(NodeOutput::new()
                    .with_update("round", json!(round))
                    .with_update("decision", json!(decision))
                    .with_update("notes", json!(verdicts.join("; "))))
            }
        })
        .add_node_fn("finalize", |ctx| async move {
            let round = int(&ctx.state, "round");
            Ok(NodeOutput::new().with_update("output", json!(format!("approved in round {round}"))))
        })
        .add_edge(START, "implement")
        .add_conditional_edges(
            "synthesize",
            |state| text(state, "decision"),
            [("revise", "revise"), ("finalize", "finalize")],
        )
        .add_edge("finalize", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer))
        .with_recursion_limit(30);
    let agent = Arc::new(GraphAgent::from_graph("review_loop", graph));

    let turns = run_turns(agent, "review-loop", &["slugify(text)"]).await?;
    let turn = &turns[0];
    let reviews = server.request_count().saturating_sub(2);
    Ok(verdict(vec![
        (turn.error.is_none(), format!("turn error: {:?}", turn.error)),
        (turn.text == "approved in round 2", format!("answer: {:?}", turn.text)),
        (reviews == 6, format!("reviewer model calls: {reviews} (3 reviewers x 2 rounds)")),
        (
            syntheses.load(Ordering::SeqCst) == 2,
            format!("synthesize ran {} time(s), once per round", syntheses.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A merge after a conditional branch receives only the branch that was taken.
async fn conditional_merge(context: &Context) -> anyhow::Result<Verdict> {
    let packages = counter();
    let runs = Arc::clone(&packages);
    let graph = StateGraph::with_channels(&["input", "route", "label", "output"])
        .add_node_fn("router", |ctx| async move {
            let route =
                if text(&ctx.state, "input").contains("express") { "express" } else { "standard" };
            Ok(NodeOutput::new().with_update("route", json!(route)))
        })
        .add_node_fn("express", |_| async {
            Ok(NodeOutput::new().with_update("label", json!("express")))
        })
        .add_node_fn("standard", |_| async {
            Ok(NodeOutput::new().with_update("label", json!("standard")))
        })
        .add_node_fn("package", move |ctx| {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let label = text(&ctx.state, "label");
                Ok(NodeOutput::new().with_update("output", json!(format!("packaged via {label}"))))
            }
        })
        .add_edge(START, "router")
        .add_conditional_edges(
            "router",
            |state| text(state, "route"),
            [("express", "express"), ("standard", "standard")],
        )
        .add_edge("express", "package")
        .add_edge("standard", "package")
        .add_edge("package", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer));
    let agent = Arc::new(GraphAgent::from_graph("conditional_merge", graph));

    let turns = run_turns(agent, "conditional-merge", &["ship express", "ship by post"]).await?;
    let thread = session_thread_id(APP, USER, "conditional-merge");
    let last = context.checkpointer.load(&thread).await?;
    let stranded = last.as_ref().and_then(|cp| cp.metadata.get("adk.graph.fanIn").cloned());
    let answers: Vec<&str> = turns.iter().map(|turn| turn.text.as_str()).collect();
    Ok(verdict(vec![
        (
            answers == ["packaged via express", "packaged via standard"],
            format!("answers: {answers:?}"),
        ),
        (
            packages.load(Ordering::SeqCst) == 2,
            format!("package ran {} time(s), once per turn", packages.load(Ordering::SeqCst)),
        ),
        (stranded.is_none(), format!("join left waiting in the final checkpoint: {stranded:?}")),
    ]))
}

/// A join built with `GraphAgentBuilder` waits for both branches, as one built with
/// `StateGraph` does.
async fn builder_join(context: &Context) -> anyhow::Result<Verdict> {
    let joins = counter();
    let runs = Arc::clone(&joins);
    let agent = GraphAgent::builder("builder_join")
        .channels(&["input", "short", "long", "output"])
        .node_fn("short", |_| async { Ok(NodeOutput::new().with_update("short", json!("s"))) })
        .node_fn("long_a", |_| async { Ok(NodeOutput::new()) })
        .node_fn("long_b", |_| async { Ok(NodeOutput::new().with_update("long", json!("l"))) })
        .node_fn("join", move |ctx| {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let joined = format!("{}+{}", text(&ctx.state, "short"), text(&ctx.state, "long"));
                Ok(NodeOutput::new().with_update("output", json!(joined)))
            }
        })
        .edge(START, "short")
        .edge(START, "long_a")
        .edge("long_a", "long_b")
        .edge("short", "join")
        .edge("long_b", "join")
        .edge("join", END)
        .checkpointer_arc(Arc::clone(&context.checkpointer))
        .build()?;

    let turns = run_turns(Arc::new(agent), "builder-join", &["go"]).await?;
    Ok(verdict(vec![
        (turns[0].text == "s+l", format!("answer: {:?}", turns[0].text)),
        (
            joins.load(Ordering::SeqCst) == 1,
            format!("join ran {} time(s)", joins.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A writer `LlmAgent` node followed by a `publish` node, with a retry policy on the writer.
fn writer_graph(
    model: Arc<dyn Llm>,
    published: Arc<AtomicUsize>,
    context: &Context,
) -> anyhow::Result<CompiledGraph> {
    Ok(StateGraph::with_channels(&["input", "draft", "output"])
        .add_node(agent_node(
            llm_agent("writer", model, "Write the report you are asked for.")?,
            |state| text(state, "input"),
            "draft",
        ))
        .add_node_fn("publish", move |ctx| {
            let published = Arc::clone(&published);
            async move {
                published.fetch_add(1, Ordering::SeqCst);
                let draft = text(&ctx.state, "draft");
                Ok(NodeOutput::new().with_update("output", json!(format!("published: {draft}"))))
            }
        })
        .add_edge(START, "writer")
        .add_edge("writer", "publish")
        .add_edge("publish", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer))
        .with_node_retry(
            "writer",
            RetryPolicy::new(2).with_initial_delay(Duration::from_millis(10)).with_jitter(0.0),
        ))
}

/// A rate-limited model inside an `AgentNode` fails the node: the retry policy runs it again,
/// the turn ends with the error, and `publish` never runs. The error was discarded, so the node
/// succeeded with no answer and `publish` ran.
async fn agent_error_stops_the_graph(context: &Context) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_, _| {
        Reply::Status(429, "Rate limit reached for scripted-model".into())
    }))
    .await?;
    let published = counter();
    let graph = writer_graph(scripted_model(&server)?, Arc::clone(&published), context)?;
    let agent = Arc::new(GraphAgent::from_graph("agent_errors", graph));

    let turns = run_turns(agent, "agent-error", &["write the weekly report"]).await?;
    let error = turns[0].error.clone().unwrap_or_default();
    Ok(verdict(vec![
        (
            error.contains("writer") && error.to_lowercase().contains("rate limit"),
            format!("turn error: {error:?}"),
        ),
        (
            server.request_count() == 2,
            format!(
                "model requests: {} (one per attempt of a 2-attempt policy)",
                server.request_count()
            ),
        ),
        (
            published.load(Ordering::SeqCst) == 0,
            format!("publish ran {} time(s)", published.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A model rate-limited once is retried by the node's policy, and the second answer is published.
async fn agent_error_is_retried(context: &Context) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_, turn| {
        if turn == 0 {
            Reply::Status(429, "Rate limit reached for scripted-model".into())
        } else {
            Reply::Text("revenue grew 4%".into())
        }
    }))
    .await?;
    let published = counter();
    let graph = writer_graph(scripted_model(&server)?, Arc::clone(&published), context)?;
    let agent = Arc::new(GraphAgent::from_graph("agent_retry", graph));

    let turns = run_turns(agent, "agent-retry", &["write the weekly report"]).await?;
    Ok(verdict(vec![
        (turns[0].error.is_none(), format!("turn error: {:?}", turns[0].error)),
        (turns[0].text == "published: revenue grew 4%", format!("answer: {:?}", turns[0].text)),
        (server.request_count() == 2, format!("model requests: {}", server.request_count())),
        (
            published.load(Ordering::SeqCst) == 1,
            format!("publish ran {} time(s)", published.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A model calls a checkpointed graph tool twice in one turn. Each call runs the graph; the
/// second used to be answered with the first call's final state.
async fn graph_tool_called_twice(context: &Context) -> anyhow::Result<Verdict> {
    let doubled_in = |request: &Value, call: &str| -> String {
        tool_message(request, call)
            .and_then(|content| serde_json::from_str::<Value>(&content).ok())
            .and_then(|value| {
                value.pointer("/doubled").or_else(|| value.pointer("/result/doubled")).cloned()
            })
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".into())
    };
    let server = ScriptedServer::start(Box::new(move |request, _| {
        if tool_message(request, "call_2").is_some() {
            Reply::Text(format!(
                "first={} second={}",
                doubled_in(request, "call_1"),
                doubled_in(request, "call_2")
            ))
        } else if tool_message(request, "call_1").is_some() {
            Reply::Call("call_2".into(), "double".into(), r#"{"input":5}"#.into())
        } else {
            Reply::Call("call_1".into(), "double".into(), r#"{"input":21}"#.into())
        }
    }))
    .await?;
    let runs = counter();
    let counter = Arc::clone(&runs);
    let graph = Arc::new(
        StateGraph::with_channels(&["input", "doubled"])
            .add_node_fn("double", move |ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new()
                        .with_update("doubled", json!(int(&ctx.state, "input") * 2)))
                }
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()?
            .with_checkpointer_arc(Arc::clone(&context.checkpointer)),
    );
    let tool = NodeTool::for_graph(graph)
        .with_name("double")
        .with_description("Doubles `input`.")
        .with_parameters_schema(json!({
            "type": "object",
            "properties": { "input": { "type": "integer" } },
            "required": ["input"]
        }));
    let assistant = LlmAgentBuilder::new("assistant")
        .model(scripted_model(&server)?)
        .instruction("Use the double tool.")
        .tool(Arc::new(tool) as Arc<dyn Tool>)
        .build()?;

    let turns = run_turns(Arc::new(assistant), "graph-tool", &["double 21, then 5"]).await?;
    Ok(verdict(vec![
        (turns[0].error.is_none(), format!("turn error: {:?}", turns[0].error)),
        (turns[0].text == "first=42 second=10", format!("answer: {:?}", turns[0].text)),
        (
            runs.load(Ordering::SeqCst) == 2,
            format!("graph node ran {} time(s), once per call", runs.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A checkpointed subgraph runs on every pass of its parent's loop and again on a later turn.
/// It ran on one thread, so every later pass and turn read its first finished state.
async fn subgraph_in_a_loop(context: &Context) -> anyhow::Result<Verdict> {
    let inner_runs = counter();
    let counter = Arc::clone(&inner_runs);
    let inner = Arc::new(
        StateGraph::with_channels(&["n", "doubled"])
            .add_node_fn("double", move |ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("doubled", json!(int(&ctx.state, "n") * 2)))
                }
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()?
            .with_checkpointer_arc(Arc::clone(&context.checkpointer)),
    );
    let graph = StateGraph::with_channels(&["input", "n", "doubled", "pass", "doubles", "output"])
        .add_node_fn("read", |ctx| async move {
            let start = text(&ctx.state, "input").trim().parse::<i64>().unwrap_or(0);
            Ok(NodeOutput::new()
                .with_update("n", json!(start - 1))
                .with_update("pass", json!(0))
                .with_update("doubles", json!([])))
        })
        .add_node_fn("bump", |ctx| async move {
            Ok(NodeOutput::new()
                .with_update("n", json!(int(&ctx.state, "n") + 1))
                .with_update("pass", json!(int(&ctx.state, "pass") + 1)))
        })
        .add_node(SubgraphNode::new("sub", inner))
        .add_node_fn("record", |ctx| async move {
            let mut doubles = ctx.state.get("doubles").cloned().unwrap_or_else(|| json!([]));
            if let Some(list) = doubles.as_array_mut() {
                list.push(json!(int(&ctx.state, "doubled")));
            }
            let output = doubles
                .as_array()
                .map(|list| list.iter().map(Value::to_string).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            Ok(NodeOutput::new()
                .with_update("doubles", doubles)
                .with_update("output", json!(output)))
        })
        .add_edge(START, "read")
        .add_edge("read", "bump")
        .add_edge("bump", "sub")
        .add_edge("sub", "record")
        .add_conditional_edges(
            "record",
            |state| if int(state, "pass") < 3 { "again" } else { "done" }.to_string(),
            [("again", "bump"), ("done", END)],
        )
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer));
    let agent = Arc::new(GraphAgent::from_graph("subgraph_loop", graph));

    let turns = run_turns(agent, "subgraph-loop", &["1", "10"]).await?;
    let answers: Vec<&str> = turns.iter().map(|turn| turn.text.as_str()).collect();
    Ok(verdict(vec![
        (answers == ["2,4,6", "20,22,24"], format!("doubles per turn: {answers:?}")),
        (
            inner_runs.load(Ordering::SeqCst) == 6,
            format!("subgraph ran {} time(s), once per pass", inner_runs.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A node in a loop invokes a child with `run_node` on every pass, and gets this pass's output.
async fn child_in_a_loop(context: &Context) -> anyhow::Result<Verdict> {
    let child_runs = counter();
    let counter = Arc::clone(&child_runs);
    let graph = StateGraph::with_channels(&["input", "pass", "echoes", "output"])
        .add_node_fn("child", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new()
                    .with_update("echo", ctx.state.get("i").cloned().unwrap_or_default()))
            }
        })
        .add_node_fn("parent", |ctx| async move {
            let pass = int(&ctx.state, "pass") + 1;
            let out = ctx.run_node("child", json!({ "i": pass })).await?;
            let mut echoes = ctx.state.get("echoes").cloned().unwrap_or_else(|| json!([]));
            if let Some(list) = echoes.as_array_mut() {
                list.push(out.get("echo").cloned().unwrap_or_default());
            }
            let output = echoes
                .as_array()
                .map(|list| list.iter().map(Value::to_string).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            Ok(NodeOutput::new()
                .with_update("pass", json!(pass))
                .with_update("echoes", echoes)
                .with_update("output", json!(output)))
        })
        .add_edge(START, "parent")
        .add_conditional_edges(
            "parent",
            |state| if int(state, "pass") < 3 { "again" } else { "done" }.to_string(),
            [("again", "parent"), ("done", END)],
        )
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer));
    let agent = Arc::new(GraphAgent::from_graph("child_loop", graph));

    let turns = run_turns(agent, "child-loop", &["go"]).await?;
    Ok(verdict(vec![
        (turns[0].text == "1,2,3", format!("echoes: {:?}", turns[0].text)),
        (
            child_runs.load(Ordering::SeqCst) == 3,
            format!("child ran {} time(s)", child_runs.load(Ordering::SeqCst)),
        ),
    ]))
}

/// A parent retried after its second child fails is served the first child, so a charge is
/// made once; and a child retried under its own policy does not fail its parent.
async fn parent_retry_serves_finished_children(context: &Context) -> anyhow::Result<Verdict> {
    let (charges, notifications, flaky) = (counter(), counter(), counter());
    let (charge, notify, flaky_attempts) =
        (Arc::clone(&charges), Arc::clone(&notifications), Arc::clone(&flaky));
    let transient = |node: &str| adk_graph::GraphError::NodeExecutionFailed {
        node: node.to_string(),
        message: "transient".to_string(),
    };
    let graph = StateGraph::with_channels(&["input", "output"])
        .add_node_fn("charge", move |_| {
            let charge = Arc::clone(&charge);
            async move {
                charge.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("receipt", json!("r-1")))
            }
        })
        .add_node_fn("notify", move |_| {
            let notify = Arc::clone(&notify);
            async move {
                if notify.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(transient("notify"));
                }
                Ok(NodeOutput::new().with_update("sent", json!(true)))
            }
        })
        .add_node_fn("audit", move |_| {
            let flaky_attempts = Arc::clone(&flaky_attempts);
            async move {
                if flaky_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(transient("audit"));
                }
                Ok(NodeOutput::new().with_update("audited", json!(true)))
            }
        })
        .add_node_fn("checkout", |ctx| async move {
            let receipt = ctx.run_node("charge", json!({})).await?;
            ctx.run_node("notify", json!({})).await?;
            ctx.run_node("audit", json!({})).await?;
            let receipt = receipt.get("receipt").and_then(Value::as_str).unwrap_or("?").to_string();
            Ok(NodeOutput::new().with_update("output", json!(format!("paid {receipt}"))))
        })
        .add_edge(START, "checkout")
        .add_edge("checkout", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer))
        .with_node_retry(
            "checkout",
            RetryPolicy::new(2).with_initial_delay(Duration::from_millis(5)).with_jitter(0.0),
        )
        .with_node_retry(
            "audit",
            RetryPolicy::new(2).with_initial_delay(Duration::from_millis(5)).with_jitter(0.0),
        );
    let agent = Arc::new(GraphAgent::from_graph("parent_retry", graph));

    let turns = run_turns(agent, "parent-retry", &["check out"]).await?;
    Ok(verdict(vec![
        (turns[0].error.is_none(), format!("turn error: {:?}", turns[0].error)),
        (turns[0].text == "paid r-1", format!("answer: {:?}", turns[0].text)),
        (
            charges.load(Ordering::SeqCst) == 1,
            format!(
                "charge ran {} time(s) across 2 checkout attempts",
                charges.load(Ordering::SeqCst)
            ),
        ),
        (
            notifications.load(Ordering::SeqCst) == 2,
            format!("notify ran {} time(s)", notifications.load(Ordering::SeqCst)),
        ),
        (
            flaky.load(Ordering::SeqCst) == 2,
            format!(
                "audit ran {} time(s) under its own 2-attempt policy",
                flaky.load(Ordering::SeqCst)
            ),
        ),
    ]))
}

/// A default timeout policy leaves subgraphs and child invocation working. Each failed with an
/// "executor defect" error once any timeout policy applied.
async fn timeout_policy_with_subgraph_and_children(context: &Context) -> anyhow::Result<Verdict> {
    let inner = Arc::new(
        StateGraph::with_channels(&["value"])
            .add_node_fn("inner", |_| async {
                Ok(NodeOutput::new().with_update("value", json!(7)))
            })
            .add_edge(START, "inner")
            .add_edge("inner", END)
            .compile()?,
    );
    let graph = StateGraph::with_channels(&["input", "value", "output"])
        .add_node(SubgraphNode::new("sub", inner))
        .add_node_fn("lookup", |_| async { Ok(NodeOutput::new().with_update("found", json!(1))) })
        .add_node_fn("parent", |ctx| async move {
            let found = ctx.run_node("lookup", json!({})).await?;
            let output = format!("value={} found={}", int(&ctx.state, "value"), found["found"]);
            Ok(NodeOutput::new().with_update("output", json!(output)))
        })
        .add_edge(START, "sub")
        .add_edge("sub", "parent")
        .add_edge("parent", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer))
        .with_node_defaults(NodeDefaults::new().with_timeout(TimeoutPolicy {
            run_timeout: Some(Duration::from_secs(30)),
            idle_timeout: None,
            on_timeout: OnTimeout::Retry { max_attempts: 2 },
        }));
    let agent = Arc::new(GraphAgent::from_graph("timeouts", graph));

    let turns = run_turns(agent, "timeouts", &["go"]).await?;
    Ok(verdict(vec![
        (turns[0].error.is_none(), format!("turn error: {:?}", turns[0].error)),
        (turns[0].text == "value=7 found=1", format!("answer: {:?}", turns[0].text)),
    ]))
}

/// An automatic join in a 12-pass loop records each predecessor's own updates, so the state
/// the SQLite checkpoint holds stays small. Recording the whole state doubled it every pass.
async fn fan_in_state_stays_bounded(context: &Context) -> anyhow::Result<Verdict> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let graph = StateGraph::with_channels(&["input", "a_out", "b_out", "iter", "output"])
        .add_node_fn("start", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("a", |_| async { Ok(NodeOutput::new().with_update("a_out", json!("A"))) })
        .add_node_fn("b", |_| async { Ok(NodeOutput::new().with_update("b_out", json!("B"))) })
        .add_node_fn("join", move |ctx| {
            let record = Arc::clone(&record);
            async move {
                record.lock().unwrap().push(ctx.state.get("join_fan_in").cloned());
                let iter = int(&ctx.state, "iter") + 1;
                Ok(NodeOutput::new()
                    .with_update("iter", json!(iter))
                    .with_update("output", json!(format!("{iter} passes"))))
            }
        })
        .add_edge(START, "start")
        .add_edge("start", "a")
        .add_edge("start", "b")
        .add_edge("a", "join")
        .add_edge("b", "join")
        .add_conditional_edges(
            "join",
            |state| if int(state, "iter") < 12 { "again" } else { "done" }.to_string(),
            [("again", "start"), ("done", END)],
        )
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer))
        .with_recursion_limit(200);
    let agent = Arc::new(GraphAgent::from_graph("fan_in_size", graph));

    let turns = run_turns(agent, "fan-in-size", &["go"]).await?;
    let thread = session_thread_id(APP, USER, "fan-in-size");
    let bytes = match context.checkpointer.load(&thread).await? {
        Some(checkpoint) => serde_json::to_string(&checkpoint.state)?.len(),
        None => 0,
    };
    let seen = seen.lock().unwrap().clone();
    let expected = Some(json!([{"a_out": "A"}, {"b_out": "B"}]));
    let constant = seen.len() == 12 && seen.iter().all(|value| *value == expected);
    Ok(verdict(vec![
        (turns[0].text == "12 passes", format!("answer: {:?}", turns[0].text)),
        (bytes > 0 && bytes < 4096, format!("final checkpoint state: {bytes} bytes")),
        (
            constant,
            format!(
                "join_fan_in on every pass equals {expected:?}; last pass saw {} bytes",
                seen.last().cloned().flatten().map(|v| v.to_string().len()).unwrap_or_default()
            ),
        ),
    ]))
}

/// One live `LlmAgent` node on Gemini sits on a conditional branch; the merge after it runs.
async fn live_conditional_merge(context: &Context) -> anyhow::Result<Verdict> {
    let Some(key) =
        std::env::var("GOOGLE_API_KEY").ok().or_else(|| std::env::var("GEMINI_API_KEY").ok())
    else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let model_id = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| LIVE_MODEL.to_string());
    let model: Arc<dyn Llm> = Arc::new(adk_model::GeminiModel::new(&key, &model_id)?);
    let classifier = llm_agent(
        "classify",
        model,
        "Classify the support ticket as billing, technical, or other. Reply with that one word only.",
    )?;
    let graph = StateGraph::with_channels(&["input", "route", "category", "output"])
        .add_node_fn("triage", |ctx| async move {
            let route = if text(&ctx.state, "input").is_empty() { "skip" } else { "classify" };
            Ok(NodeOutput::new().with_update("route", json!(route)))
        })
        .add_node(agent_node(classifier, |state| text(state, "input"), "category"))
        .add_node_fn("skip", |_| async {
            Ok(NodeOutput::new().with_update("category", json!("none")))
        })
        .add_node_fn("file", |ctx| async move {
            let category = text(&ctx.state, "category").trim().to_lowercase();
            Ok(NodeOutput::new().with_update("output", json!(format!("filed as {category}"))))
        })
        .add_edge(START, "triage")
        .add_conditional_edges(
            "triage",
            |state| text(state, "route"),
            [("classify", "classify"), ("skip", "skip")],
        )
        .add_edge("classify", "file")
        .add_edge("skip", "file")
        .add_edge("file", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&context.checkpointer));
    let agent = Arc::new(GraphAgent::from_graph("live_merge", graph));

    let turns =
        run_turns(agent, "live-merge", &["I was charged twice for my subscription this month."])
            .await?;
    let answer = turns[0].text.clone();
    Ok(verdict(vec![
        (turns[0].error.is_none(), format!("turn error: {:?}", turns[0].error)),
        (answer.contains("billing"), format!("answer from {model_id}: {answer:?}")),
    ]))
}

async fn run_scenario(name: &str, context: &Context) -> anyhow::Result<Verdict> {
    match name {
        "review_loop" => review_loop(context).await,
        "conditional_merge" => conditional_merge(context).await,
        "builder_join" => builder_join(context).await,
        "agent_error_stops_the_graph" => agent_error_stops_the_graph(context).await,
        "agent_error_is_retried" => agent_error_is_retried(context).await,
        "graph_tool_called_twice" => graph_tool_called_twice(context).await,
        "subgraph_in_a_loop" => subgraph_in_a_loop(context).await,
        "child_in_a_loop" => child_in_a_loop(context).await,
        "parent_retry_serves_finished_children" => {
            parent_retry_serves_finished_children(context).await
        }
        "timeout_policy_with_subgraph_and_children" => {
            timeout_policy_with_subgraph_and_children(context).await
        }
        "fan_in_state_stays_bounded" => fan_in_state_stays_bounded(context).await,
        "live_conditional_merge" => live_conditional_merge(context).await,
        other => anyhow::bail!("unknown scenario '{other}'; known: {SCENARIOS:?}"),
    }
}

fn load_env() {
    let env_file = std::env::var("ADK_ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    let _ = dotenvy::from_path_override(&env_file);
    let _ = dotenvy::dotenv();
}

#[tokio::main]
async fn main() -> ExitCode {
    load_env();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .try_init();
    adk_core::ensure_crypto_provider();

    let mut selected: Vec<String> = SCENARIOS.iter().map(ToString::to_string).collect();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scenario"
            && let Some(value) = args.next()
        {
            selected = value.split(',').map(str::to_string).collect();
        }
    }

    let context = match Context::new().await {
        Ok(context) => context,
        Err(error) => {
            println!("could not open the SQLite checkpointer: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    println!("graph correctness — {} scenario(s)\n", selected.len());
    let mut failures = 0;
    for name in &selected {
        let result =
            tokio::time::timeout(Duration::from_secs(120), run_scenario(name, &context)).await;
        let (status, evidence) = match result {
            Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
            Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
            Ok(Ok(Verdict::Fail(evidence))) => {
                failures += 1;
                ("FAIL", evidence)
            }
            Ok(Err(error)) => {
                failures += 1;
                ("FAIL", format!("scenario error: {error:#}"))
            }
            Err(_) => {
                failures += 1;
                ("FAIL", "timed out after 120s".into())
            }
        };
        println!("{status}  {name}\n      {evidence}");
    }
    println!("\n{} scenario(s), {failures} failed", selected.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
