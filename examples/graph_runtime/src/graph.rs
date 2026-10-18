//! Scenarios that run a `GraphAgent` under a `Runner`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, ErrorCategory, Llm, RunConfig, Tool, ToolApproval, ToolConfirmationDecision, ToolContext,
};
use adk_graph::agent::GraphAgent;
use adk_graph::child::RunNodeOptions;
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::NodeOutput;
use adk_graph::retry::RetryPolicy;
use serde_json::{Value, json};

use crate::scripted::{Reply, ScriptedServer, has_tool_result, last_user_text, system_text};
use crate::{
    Calls, Context, Harness, TimedLlm, Verdict, agent_node, gemini, llm_agent, openai_rejected_key,
    record_session, scripted_model, text, verdict,
};

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

/// Gemini for a live scenario, or the scripted model; `None` when a live key is missing.
fn pick_model(live: bool, server: &ScriptedServer) -> anyhow::Result<Option<Arc<dyn Llm>>> {
    if live { gemini() } else { scripted_model(server).map(Some) }
}

fn skip_without_key() -> Verdict {
    Verdict::Skip("GOOGLE_API_KEY is not set".into())
}

/// WG9: an agent node reads the session's state through its instruction template, with the
/// graph's state layered on top, and sees the session's real id.
pub async fn session_state_templates(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, _| {
        Reply::Text(format!("Greeting written for: {}", system_text(request)))
    }))
    .await?;
    let Some(model) = pick_model(live, &server)? else { return Ok(skip_without_key()) };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let greeter: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("greeter")
            .model(model)
            .instruction(
                "Write one short sentence greeting {customer_name}, a {plan_tier} plan \
                 customer, about {topic}. Use the customer's full name.",
            )
            .before_callback(record_session(Arc::clone(&seen)))
            .build()?,
    );
    let graph = StateGraph::with_channels(&["input", "topic", "output"])
        .add_node_fn("set_topic", |_| async {
            Ok(NodeOutput::new().with_update("topic", json!("a delayed shipment")))
        })
        .add_node(agent_node(greeter, |state| text(state, "input"), "output"))
        .add_edge(START, "set_topic")
        .add_edge("set_topic", "greeter")
        .add_edge("greeter", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&cx.checkpointer));
    let session = cx.id("session-state", live);
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("concierge", graph)),
        &session,
        HashMap::from([
            ("customer_name".to_string(), json!("Ada Lovelace")),
            ("plan_tier".to_string(), json!("gold")),
        ]),
    )
    .await?;

    let turn = harness.turn("Please greet the customer.", None).await?;
    let seen = seen.lock().unwrap().clone();
    let mut checks = vec![
        (turn.error.is_none(), format!("turn error: {:?}", turn.error_text())),
        (
            seen == vec![session.clone()],
            format!("session id the node's agent saw: {seen:?} (the session is {session:?})"),
        ),
    ];
    if live {
        checks.push((turn.text().contains("Ada"), format!("live answer: {:?}", turn.text())));
    } else {
        let instruction = server.requests().first().map(system_text).unwrap_or_default();
        checks.push((
            instruction.contains("Ada Lovelace")
                && instruction.contains("gold")
                && instruction.contains("a delayed shipment"),
            format!("instruction the model received: {instruction:?}"),
        ));
    }
    Ok(verdict(checks))
}

/// WG10: node agents' events reach the caller while the graph runs, attributed to their node,
/// and the session keeps them.
pub async fn node_events_stream(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, _| {
        if system_text(request).contains("[researcher]") {
            Reply::Text("Finding: checkout returns 503 when the payment queue is full.".into())
        } else {
            Reply::Slow(
                Duration::from_millis(1500),
                Box::new(Reply::Text("Report: scale the payment queue before the sale.".into())),
            )
        }
    }))
    .await?;
    let Some(model) = pick_model(live, &server)? else { return Ok(skip_without_key()) };
    let researcher = llm_agent(
        "researcher",
        Arc::clone(&model),
        "[researcher] State the most likely cause of the reported problem in one sentence.",
        &Calls::default(),
    )?;
    let writer = llm_agent(
        "writer",
        model,
        "[writer] Write a one-sentence incident report from the finding you are given.",
        &Calls::default(),
    )?;
    let graph = StateGraph::with_channels(&["input", "finding", "output"])
        .add_node(agent_node(researcher, |state| text(state, "input"), "finding"))
        .add_node(agent_node(writer, |state| text(state, "finding"), "output"))
        .add_edge(START, "researcher")
        .add_edge("researcher", "writer")
        .add_edge("writer", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&cx.checkpointer));
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("incident", graph)),
        &cx.id("node-events", live),
        HashMap::new(),
    )
    .await?;

    let turn = harness.turn("Checkout fails intermittently during the sale.", None).await?;
    let persisted = harness.persisted().await?;
    let researcher = turn.first_from("researcher");
    let writer = turn.first_from("writer");
    let persisted_authors: Vec<&str> =
        persisted.iter().map(|event| event.author.as_str()).collect();
    let mut checks = vec![
        (turn.error.is_none(), format!("turn error: {:?}", turn.error_text())),
        (
            researcher.is_some() && writer.is_some(),
            format!(
                "the runner stream carried the researcher's answer: {}, the writer's: {}",
                researcher.is_some(),
                writer.is_some()
            ),
        ),
        (
            researcher.is_some_and(|(_, event)| event.branch.ends_with("researcher"))
                && writer.is_some_and(|(_, event)| event.branch.ends_with("writer")),
            format!(
                "branches: researcher {:?}, writer {:?}",
                researcher.map(|(_, event)| event.branch.clone()),
                writer.map(|(_, event)| event.branch.clone())
            ),
        ),
        (
            persisted_authors.contains(&"researcher") && persisted_authors.contains(&"writer"),
            format!("authors the session holds: {persisted_authors:?}"),
        ),
    ];
    match (researcher, writer) {
        (Some((researcher_at, _)), Some((writer_at, _))) if live => checks.push((
            researcher_at < writer_at,
            "the researcher's answer arrived before the writer's".to_string(),
        )),
        (Some((researcher_at, _)), _) if !live => {
            let lead = turn.ended - researcher_at;
            checks.push((
                lead >= Duration::from_secs(1),
                format!("the researcher's answer arrived {lead:?} before the turn ended"),
            ));
        }
        _ => checks.push((false, "no answer to time".to_string())),
    }
    Ok(verdict(checks))
}

/// WG11: the first failure of a step stops the nodes still queued behind it and cuts a
/// sibling's retry backoff short; the node that finished keeps its write for the resume.
pub async fn failed_step_stops_dispatch(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let healthy = Arc::new(AtomicBool::new(false));
    let up = Arc::clone(&healthy);
    let server = ScriptedServer::start(Box::new(move |request, _| {
        let up = up.load(Ordering::SeqCst);
        match (system_text(request).contains("[validate]"), up) {
            (true, false) => Reply::Status(400, "card number failed validation".into()),
            (true, true) => Reply::Text("card valid".into()),
            (false, false) => Reply::Status(503, "enrichment service overloaded".into()),
            (false, true) => Reply::Text("customer enriched".into()),
        }
    }))
    .await?;
    let scripted = scripted_model(&server)?;
    let validate_model = if live { openai_rejected_key()? } else { Arc::clone(&scripted) };
    let (audit_time, first_delay, limit) = if live {
        (Duration::from_millis(2000), Duration::from_secs(3), Duration::from_millis(3500))
    } else {
        (Duration::from_millis(400), Duration::from_secs(1), Duration::from_millis(1500))
    };
    let retry = RetryPolicy::new(3)
        .with_initial_delay(first_delay)
        .with_backoff_factor(2.0)
        .with_jitter(0.0);

    let audits = counter();
    let ships = counter();
    let (audit_runs, ship_runs) = (Arc::clone(&audits), Arc::clone(&ships));
    let validate = llm_agent(
        "b_validate",
        validate_model,
        "[validate] Validate the card.",
        &Calls::default(),
    )?;
    let enrich =
        llm_agent("c_enrich", scripted, "[enrich] Enrich the customer record.", &Calls::default())?;
    let graph = StateGraph::with_channels(&[
        "input",
        "audit",
        "validation",
        "enrichment",
        "shipment",
        "output",
    ])
    .add_node_fn("a_audit", move |_| {
        let runs = Arc::clone(&audit_runs);
        async move {
            tokio::time::sleep(audit_time).await;
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(NodeOutput::new().with_update("audit", json!("recorded")))
        }
    })
    .add_node(agent_node(validate, |state| text(state, "input"), "validation"))
    .add_node(agent_node(enrich, |state| text(state, "input"), "enrichment"))
    .add_node_fn("d_ship", move |_| {
        let runs = Arc::clone(&ship_runs);
        async move {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(NodeOutput::new().with_update("shipment", json!("label printed")))
        }
    })
    .add_node_fn("summarize", |_| async {
        Ok(NodeOutput::new().with_update("output", json!("order shipped")))
    })
    .add_edge(START, "a_audit")
    .add_edge(START, "b_validate")
    .add_edge(START, "c_enrich")
    .add_edge(START, "d_ship")
    .add_edge("a_audit", "summarize")
    .add_edge("b_validate", "summarize")
    .add_edge("c_enrich", "summarize")
    .add_edge("d_ship", "summarize")
    .add_edge("summarize", END)
    .compile()?
    .with_checkpointer_arc(Arc::clone(&cx.checkpointer))
    .with_max_concurrency(3)
    .with_node_retry("c_enrich", retry);
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("fulfilment", graph)),
        &cx.id("failed-step", live),
        HashMap::new(),
    )
    .await?;

    let first = harness.turn("Ship order 7.", None).await?;
    let ships_in_first = ships.load(Ordering::SeqCst);
    let mut checks = vec![
        (
            first.error_text().contains("b_validate"),
            format!("turn 1 error: {:?}", first.error_text()),
        ),
        (
            first.elapsed() < limit,
            format!(
                "turn 1 ended after {:?} (c_enrich's retry backoff starts at {first_delay:?})",
                first.elapsed()
            ),
        ),
        (
            ships_in_first == 0,
            format!("d_ship, queued behind the failure, ran {ships_in_first} time(s) in turn 1"),
        ),
    ];
    if !live {
        healthy.store(true, Ordering::SeqCst);
        let second = harness.turn("Ship order 7.", None).await?;
        let (audits, ships) = (audits.load(Ordering::SeqCst), ships.load(Ordering::SeqCst));
        checks.push((
            second.error.is_none() && second.text() == "order shipped",
            format!(
                "turn 2 resumed the step: {:?} (error {:?})",
                second.text(),
                second.error_text()
            ),
        ));
        checks.push((
            audits == 1,
            format!("a_audit, which finished in turn 1, ran {audits} time(s) over both turns"),
        ));
        checks.push((ships == 1, format!("d_ship ran {ships} time(s) over both turns")));
    }
    Ok(verdict(checks))
}

/// WG13: a node reached from START and from another node runs once, after that node.
pub async fn entry_join_runs_once(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, _| {
        if system_text(request).contains("[lookup]") {
            Reply::Text("Order A-7 shipped on 1 October and arrives on 4 October.".into())
        } else {
            Reply::Text(format!("Reply drafted from: {}", last_user_text(request)))
        }
    }))
    .await?;
    let Some(model) = pick_model(live, &server)? else { return Ok(skip_without_key()) };
    let compose_calls = Calls::default();
    let lookup = llm_agent(
        "lookup",
        Arc::clone(&model),
        "[lookup] Report the order's status in one sentence. Order A-7 shipped on 1 October \
         and arrives on 4 October.",
        &Calls::default(),
    )?;
    let compose = llm_agent(
        "compose",
        model,
        "[compose] Write a one-sentence reply to the customer that states the order status \
         you are given, including its dates.",
        &compose_calls,
    )?;
    let graph = StateGraph::with_channels(&["input", "status", "output"])
        .add_node(agent_node(lookup, |state| text(state, "input"), "status"))
        .add_node(agent_node(
            compose,
            |state| {
                format!(
                    "Customer asked: {}\nOrder status: {}",
                    text(state, "input"),
                    text(state, "status")
                )
            },
            "output",
        ))
        .add_edge(START, "lookup")
        .add_edge(START, "compose")
        .add_edge("lookup", "compose")
        .add_edge("compose", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&cx.checkpointer));
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("support", graph)),
        &cx.id("entry-join", live),
        HashMap::new(),
    )
    .await?;

    let turn = harness.turn("Where is my order A-7?", None).await?;
    let mut checks = vec![
        (turn.error.is_none(), format!("turn error: {:?}", turn.error_text())),
        (
            compose_calls.count() == 1,
            format!("compose called its model {} time(s)", compose_calls.count()),
        ),
    ];
    if live {
        checks.push((turn.text().contains("October"), format!("live answer: {:?}", turn.text())));
    } else {
        let prompts: Vec<String> = server
            .requests()
            .iter()
            .filter(|request| system_text(request).contains("[compose]"))
            .map(last_user_text)
            .collect();
        checks.push((
            prompts.iter().all(|prompt| prompt.contains("shipped")),
            format!("compose prompts: {prompts:?}"),
        ));
    }
    Ok(verdict(checks))
}

/// WG14: with full jitter, no retry delay exceeds the policy's `max_delay`.
pub async fn retry_delay_within_max(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server =
        ScriptedServer::start(Box::new(|_, _| Reply::Status(503, "upstream overloaded".into())))
            .await?;
    let (attempts, ceiling) =
        if live { (8, Duration::from_millis(300)) } else { (13, Duration::from_millis(200)) };
    let timed = Arc::new(TimedLlm::new(if live {
        openai_rejected_key()?
    } else {
        scripted_model(&server)?
    }));
    let policy = RetryPolicy::new(attempts)
        .with_initial_delay(ceiling)
        .with_max_delay(ceiling)
        .with_backoff_factor(1.0)
        .with_jitter(1.0);
    let flaky = llm_agent(
        "flaky",
        Arc::clone(&timed) as Arc<dyn Llm>,
        "[flaky] Answer briefly.",
        &Calls::default(),
    )?;
    let graph = StateGraph::with_channels(&["input", "output"])
        .add_node(agent_node(flaky, |state| text(state, "input"), "output"))
        .add_edge(START, "flaky")
        .add_edge("flaky", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&cx.checkpointer))
        .with_node_retry("flaky", policy);
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("retries", graph)),
        &cx.id("retry-delay", live),
        HashMap::new(),
    )
    .await?;

    let turn = harness.turn("Hello", None).await?;
    let gaps = timed.gaps();
    let longest = gaps.iter().max().copied().unwrap_or_default();
    // The gap runs from the end of one model call to the start of the next, so it holds
    // the sleep and the executor's own few milliseconds.
    let tolerance = Duration::from_millis(25);
    Ok(verdict(vec![
        (
            turn.error.is_some(),
            format!("the turn failed after the budget: {:?}", turn.error_text()),
        ),
        (
            gaps.len() == attempts as usize - 1,
            format!("{} attempt(s), {} delay(s)", gaps.len() + 1, gaps.len()),
        ),
        (
            longest <= ceiling + tolerance,
            format!(
                "longest delay {longest:?} against max_delay {ceiling:?} (+{tolerance:?}); all: {:?}",
                gaps.iter().map(Duration::as_millis).collect::<Vec<_>>()
            ),
        ),
    ]))
}

/// A tool that counts how often it runs.
struct CountingTool {
    name: &'static str,
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Issues money to the customer for an order."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"order_id": {"type": "string"}, "amount": {"type": "number"}},
            "required": ["order_id", "amount"],
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"status": "issued"}))
    }
}

/// WG15: two nodes that pause in the same step both reach the caller, and one answer resumes
/// both.
pub async fn parallel_pauses_surface(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, _| {
        let system = system_text(request);
        let (id, tool, amount) = if system.contains("[refund]") {
            ("call-refund", "issue_refund", 20)
        } else {
            ("call-credit", "issue_credit", 5)
        };
        if has_tool_result(request) {
            Reply::Text(format!("{tool} done"))
        } else {
            Reply::Call(
                id.into(),
                tool.into(),
                format!(r#"{{"order_id":"A-1","amount":{amount}}}"#),
            )
        }
    }))
    .await?;
    let Some(model) = pick_model(live, &server)? else { return Ok(skip_without_key()) };
    let refunds = counter();
    let credits = counter();
    let desk = |name: &str, tool: &'static str, runs: &Arc<AtomicUsize>, instruction: &str| {
        LlmAgentBuilder::new(name)
            .model(Arc::clone(&model))
            .instruction(instruction)
            .tool(Arc::new(CountingTool { name: tool, runs: Arc::clone(runs) }))
            .require_tool_confirmation(tool)
            .build()
            .map(|agent| Arc::new(agent) as Arc<dyn Agent>)
    };
    let refund_desk = desk(
        "refund_desk",
        "issue_refund",
        &refunds,
        "[refund] Call issue_refund exactly once with order_id \"A-1\" and amount 20, then \
         confirm in one short sentence.",
    )?;
    let credit_desk = desk(
        "credit_desk",
        "issue_credit",
        &credits,
        "[credit] Call issue_credit exactly once with order_id \"A-1\" and amount 5, then \
         confirm in one short sentence.",
    )?;
    let graph = StateGraph::with_channels(&["input", "refund", "credit", "output"])
        .add_node(agent_node(refund_desk, |state| text(state, "input"), "refund"))
        .add_node(agent_node(credit_desk, |state| text(state, "input"), "credit"))
        .add_node_fn("close", |_| async {
            Ok(NodeOutput::new().with_update("output", json!("ticket closed")))
        })
        .add_edge(START, "refund_desk")
        .add_edge(START, "credit_desk")
        .add_edge("refund_desk", "close")
        .add_edge("credit_desk", "close")
        .add_edge("close", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&cx.checkpointer));
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("desk", graph)),
        &cx.id("parallel-pauses", live),
        HashMap::new(),
    )
    .await?;

    let first = harness.turn("Process ticket 9 for order A-1.", None).await?;
    let requests = first.confirmations();
    let mut tools: Vec<&str> = requests.iter().map(|request| request.tool_name.as_str()).collect();
    tools.sort_unstable();
    // The caller answers every request the turn surfaced.
    let mut answer = RunConfig::builder();
    if live {
        for request in &requests {
            answer = answer.tool_approval(request.fingerprint(), ToolApproval::approve());
        }
    } else {
        answer = answer.tool_confirmation_decisions(
            requests
                .iter()
                .filter_map(|request| request.function_call_id.clone())
                .map(|id| (id, ToolConfirmationDecision::Approve))
                .collect(),
        );
    }
    let second = harness.turn("Approved.", Some(answer.build())).await?;
    let (refunds, credits) = (refunds.load(Ordering::SeqCst), credits.load(Ordering::SeqCst));
    Ok(verdict(vec![
        (first.error.is_none(), format!("turn 1 error: {:?}", first.error_text())),
        (tools == ["issue_credit", "issue_refund"], format!("turn 1 asked to confirm: {tools:?}")),
        (
            second.error.is_none() && second.confirmations().is_empty(),
            format!(
                "turn 2 error {:?}, confirmations still asked: {:?}",
                second.error_text(),
                second.confirmations().iter().map(|r| r.tool_name.clone()).collect::<Vec<_>>()
            ),
        ),
        (second.text() == "ticket closed", format!("turn 2 answer: {:?}", second.text())),
        (
            refunds == 1 && credits == 1,
            format!("issue_refund ran {refunds} time(s), issue_credit {credits}"),
        ),
    ]))
}

/// WG16: the answer to one pause does not answer the next.
pub async fn resume_answer_is_scoped(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, _| {
        Reply::Text(format!("Plan: {}", last_user_text(request)))
    }))
    .await?;
    let Some(model) = pick_model(live, &server)? else { return Ok(skip_without_key()) };
    let planner = llm_agent(
        "planner",
        model,
        "[planner] Restate the refund request as a one-line plan.",
        &Calls::default(),
    )?;
    let executions = counter();
    let runs = Arc::clone(&executions);
    let agent = GraphAgent::builder("refunds")
        .channels(&["input", "plan", "decision", "output"])
        .node(agent_node(planner, |state| text(state, "input"), "plan"))
        // Pauses until a person answers. The turn that resumes the pause is its
        // answer, read with `resume_value`; it is not written to state.
        .node_fn("review", |ctx| async move {
            match ctx.resume_value().and_then(Value::as_str) {
                Some("approve") => Ok(NodeOutput::new().with_update("decision", json!("approved"))),
                Some(_) => Ok(NodeOutput::new()
                    .with_update("output", json!("refund rejected"))
                    .with_goto([END])),
                None => Ok(NodeOutput::interrupt("approve this refund?")),
            }
        })
        .node_fn("execute", move |ctx| {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let plan = text(&ctx.state, "plan");
                Ok(NodeOutput::new().with_update("output", json!(format!("refunded: {plan}"))))
            }
        })
        .edge(START, "planner")
        .edge("planner", "review")
        .edge("review", "execute")
        .edge("execute", END)
        .checkpointer_arc(Arc::clone(&cx.checkpointer))
        .build()?;
    let harness =
        Harness::new(Arc::new(agent), &cx.id("resume-answer", live), HashMap::new()).await?;

    let first = harness.turn("Refund order A-1 for $900.", None).await?;
    let second = harness.turn("approve", None).await?;
    let after_second = executions.load(Ordering::SeqCst);
    let third = harness.turn("Refund order B-2 for $950.", None).await?;
    let after_third = executions.load(Ordering::SeqCst);
    Ok(verdict(vec![
        (first.paused(), format!("turn 1 paused for approval: {}", first.paused())),
        (
            after_second == 1 && second.text().starts_with("refunded"),
            format!(
                "turn 2 approved and executed: {:?} ({after_second} execution(s))",
                second.text()
            ),
        ),
        (
            third.paused() && after_third == 1,
            format!(
                "turn 3, a new refund, paused for its own approval: {} ({after_third} execution(s) in total)",
                third.paused()
            ),
        ),
    ]))
}

/// WG17: an agent node's failure reaches the caller with the model error's category.
pub async fn agent_error_category(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_, _| {
        Reply::Status(429, "rate limit reached for requests".into())
    }))
    .await?;
    let model = if live { openai_rejected_key()? } else { scripted_model(&server)? };
    let writer = llm_agent("writer", model, "[writer] Answer briefly.", &Calls::default())?;
    let graph = StateGraph::with_channels(&["input", "output"])
        .add_node(agent_node(writer, |state| text(state, "input"), "output"))
        .add_edge(START, "writer")
        .add_edge("writer", END)
        .compile()?
        .with_checkpointer_arc(Arc::clone(&cx.checkpointer));
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("drafts", graph)),
        &cx.id("error-category", live),
        HashMap::new(),
    )
    .await?;

    let turn = harness.turn("Hello", None).await?;
    let (category, status) =
        if live { (ErrorCategory::Unauthorized, 401) } else { (ErrorCategory::RateLimited, 429) };
    let Some(error) = turn.error else {
        return Ok(verdict(vec![(false, "the turn did not fail".to_string())]));
    };
    Ok(verdict(vec![
        (
            error.category == category,
            format!("category {:?}, code {:?} (want {category:?})", error.category, error.code),
        ),
        (
            error.http_status_code() == status,
            format!("HTTP status {} (want {status})", error.http_status_code()),
        ),
        (
            error.is_retryable() != live,
            format!("retryable: {} — {}", error.is_retryable(), error.message),
        ),
    ]))
}

/// WG19: two concurrent invocations of a child under one run id run it once.
pub async fn child_single_flight(cx: &Context, live: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_, _| {
        Reply::Slow(Duration::from_millis(200), Box::new(Reply::Text("Charged order A-7.".into())))
    }))
    .await?;
    let Some(model) = pick_model(live, &server)? else { return Ok(skip_without_key()) };
    let charge_calls = Calls::default();
    let charge = llm_agent(
        "charge",
        model,
        "[charge] Confirm, in one short sentence, that the order was charged.",
        &charge_calls,
    )?;
    let graph =
        StateGraph::with_channels(&["input", "order", "receipt", "first", "second", "output"])
            .add_node(agent_node(
                charge,
                |state| format!("Charge order {}", text(state, "order")),
                "receipt",
            ))
            .add_node_fn("checkout", |ctx| async move {
                let order = json!({"order": "A-7"});
                // A retried webhook delivers the same order twice; the order id is the run id.
                let (first, second) = futures::join!(
                    ctx.run_node_with("charge", order.clone(), RunNodeOptions::with_run_id("A-7")),
                    ctx.run_node_with("charge", order, RunNodeOptions::with_run_id("A-7")),
                );
                Ok(NodeOutput::new()
                    .with_update("first", first?)
                    .with_update("second", second?)
                    .with_update("output", json!("checked out")))
            })
            .add_edge(START, "checkout")
            .add_edge("checkout", END)
            .compile()?
            .with_checkpointer_arc(Arc::clone(&cx.checkpointer));
    let harness = Harness::new(
        Arc::new(GraphAgent::from_graph("checkout", graph)),
        &cx.id("single-flight", live),
        HashMap::new(),
    )
    .await?;

    let turn = harness.turn("Check out order A-7.", None).await?;
    Ok(verdict(vec![
        (turn.error.is_none(), format!("turn error: {:?}", turn.error_text())),
        (
            charge_calls.count() == 1,
            format!(
                "the charge agent called its model {} time(s) for one order",
                charge_calls.count()
            ),
        ),
    ]))
}
