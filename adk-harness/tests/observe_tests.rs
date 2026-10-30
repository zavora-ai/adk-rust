//! Observability through a real harness run (spec 3.1 §7.13): the loop guard tightens
//! the mode and ends the turn, spend is attributed per agent and subject and matches the
//! ledger, and the harness spans carry the `harness.*` attributes.

#![cfg(all(feature = "runner", feature = "observe"))]

use crate::common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{InMemorySpendLedger, SpendKey, SpendLedger, SpendPeriod, UsageMetadata};
use adk_harness::{HarnessMode, LOOP_DETECTED_CODE, LoopGuard, ModeSource, PromptAnswer};
use adk_model::ScriptedResponse;
use adk_telemetry::{AdkSpanLayer, SpanSink};
use common::{fixture, outcome, response_of};
use futures::StreamExt;
use serde_json::{Value, json};
use tracing_subscriber::layer::SubscriberExt as _;

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn priced(response: ScriptedResponse, tokens: i32, usd: f64) -> ScriptedResponse {
    response.with_usage(UsageMetadata {
        prompt_token_count: tokens / 2,
        candidates_token_count: tokens - tokens / 2,
        total_token_count: tokens,
        cost: Some(usd),
        ..UsageMetadata::default()
    })
}

#[tokio::test]
async fn the_loop_guard_tightens_the_mode_and_ends_the_turn() {
    let read = || call("read_file", json!({ "path": "notes.txt" }));
    let f = fixture(vec![read(), read(), read(), read(), ScriptedResponse::text("done")], |b| {
        b.loop_guard(LoopGuard::default()).mode(HarnessMode::Auto)
    })
    .await;
    std::fs::write(f.path("notes.txt"), "hello").unwrap();
    let stream = f.harness.send(&f.user, &f.session, "read the notes").await.unwrap();
    let results: Vec<_> = stream.collect().await;
    let error = results.iter().find_map(|result| result.as_ref().err()).expect("the turn ends");
    assert_eq!(error.code, LOOP_DETECTED_CODE);
    let stop = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .find(|event| event.llm_response.error_code.as_deref() == Some(LOOP_DETECTED_CODE))
        .expect("the runner records why the turn stopped");
    assert!(stop.llm_response.error_message.as_deref().unwrap_or_default().contains("read-only"));
    assert_eq!(f.model.requests().len(), 3, "no model call after the loop was detected");

    let record = f
        .harness
        .agent()
        .runtime()
        .mode_store()
        .as_ref()
        .get(common::APP, "alice", "s1")
        .await
        .unwrap()
        .expect("a mode record");
    assert_eq!(record.mode, HarnessMode::ReadOnly);
    assert_eq!(record.set_by, ModeSource::SystemTighten);
    let detected = f.records("harness.loop.detected");
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].ext["identicalCalls"], 3);
    assert_eq!(detected[0].ext["action"], "tighten_read_only");
    assert_eq!(f.records("harness.mode.changed").len(), 1);
    assert_eq!(f.records("harness.spend").len(), 1, "the stopped run still reports its spend");
}

#[tokio::test]
async fn the_spend_report_equals_the_ledger_totals_per_agent_and_subject() {
    let ledger = Arc::new(InMemorySpendLedger::default());
    let script = vec![
        priced(call("read_file", json!({ "path": "a.txt" })), 100, 0.002),
        priced(ScriptedResponse::text("alice done"), 40, 0.001),
        priced(ScriptedResponse::text("bob done"), 60, 0.003),
    ];
    let handle = ledger.clone();
    let f = fixture(script, move |b| b.spend_ledger(handle)).await;
    std::fs::write(f.path("a.txt"), "x").unwrap();
    f.send("read a").await;
    let bob = common::user("bob");
    let bob_session = f.harness.open_session(&bob, None).await.unwrap();
    f.send_as(&bob, &bob_session, "hi").await;

    let total = ledger.spent(&SpendKey::org(common::APP), SpendPeriod::Lifetime).await.unwrap();
    let report = f.harness.spend(None);
    assert_eq!(total, 6_000);
    assert_eq!(report.cost_micro_usd, total, "{}", report.render());
    let agent = "urn:adk:agent:harness-tests/harness";
    let by_agent = ledger
        .spent(&SpendKey::org(common::APP).with_agent(agent), SpendPeriod::Lifetime)
        .await
        .unwrap();
    assert_eq!(by_agent, total);
    assert!(report.entries.iter().all(|entry| entry.key.agent.as_deref() == Some(agent)));

    let alice = f.harness.spend(Some(&f.user));
    let bob_report = f.harness.spend(Some(&bob));
    assert_eq!((alice.cost_micro_usd, alice.charges, alice.runs), (3_000, 2, 1));
    assert_eq!((bob_report.cost_micro_usd, bob_report.charges, bob_report.runs), (3_000, 1, 1));
    assert_eq!(alice.total_tokens, 140);
    assert_eq!(alice.model_calls, 2);

    let spend = f.records("harness.spend");
    assert_eq!(spend.len(), 2);
    assert_eq!(spend[0].ext["modelCalls"], 2);
    assert_eq!(spend[0].ext["tokens"], 140);
    assert_eq!(spend[0].ext["cost"], 3_000);
    assert_eq!(spend[0].ext["spendKey"]["agent"], agent);
    assert_eq!(spend[0].ext["spendKey"]["subject"]["id"], "alice");
}

#[tokio::test]
async fn a_run_dropped_mid_stream_still_counts_its_model_calls() {
    // A launcher stops polling on a person's denial or Ctrl-C and drops the stream.
    let script = vec![
        priced(call("read_file", json!({ "path": "a.txt" })), 100, 0.002),
        priced(ScriptedResponse::text("done"), 40, 0.001),
    ];
    let f = fixture(script, |b| b.mode(HarnessMode::Auto)).await;
    std::fs::write(f.path("a.txt"), "x").unwrap();
    let mut stream = f.harness.send(&f.user, &f.session, "read a").await.unwrap();
    let first = stream.next().await.expect("an event").expect("not an error");
    assert!(first.llm_response.content.is_some(), "the model's first turn");
    drop(stream);

    let report = f.harness.spend(None);
    assert_eq!((report.runs, report.model_calls), (1, 1), "{}", report.render());
    assert_eq!(report.total_tokens, 100);
}

#[derive(Default)]
struct Capture(Mutex<Vec<(String, HashMap<String, String>)>>);

impl SpanSink for Capture {
    fn export_span(&self, span_name: &str, attributes: HashMap<String, String>) {
        self.0.lock().unwrap().push((span_name.to_string(), attributes));
    }
}

#[tokio::test]
async fn harness_spans_carry_the_mode_class_isolation_and_rule() {
    let capture = Arc::new(Capture::default());
    let subscriber = tracing_subscriber::registry().with(AdkSpanLayer::new(capture.clone()));
    let _default = tracing::subscriber::set_default(subscriber);

    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| {
        let suggestion = prompt.suggestions.first().expect("a prefix rule is offered").clone();
        PromptAnswer::ApproveAndRemember(suggestion)
    });
    let handle = prompter.clone();
    let echo = || call("bash", json!({ "command": "echo hi" }));
    let f = fixture(
        vec![echo(), ScriptedResponse::text("done"), echo(), ScriptedResponse::text("done")],
        move |b| b.prompter(handle),
    )
    .await;
    f.send("echo once").await;
    let events = f.send("echo again").await;
    assert_eq!(outcome(&response_of(&events, "bash")), common::Outcome::Executed);
    assert_eq!(prompter.count(), 1, "the second call is allowed by the rule");

    let spans = capture.0.lock().unwrap().clone();
    let run = spans.iter().find(|(name, _)| name == "harness.run").expect("a harness.run span");
    assert_eq!(run.1.get("harness.mode").map(String::as_str), Some("ask"));
    assert_eq!(run.1.get("harness.isolation").map(String::as_str), Some("host"));
    let decisions: Vec<_> = spans.iter().filter(|(name, _)| name == "harness.decide").collect();
    assert_eq!(decisions.len(), 2, "{spans:#?}");
    for (_, attributes) in &decisions {
        assert_eq!(attributes.get("harness.tool_kind").map(String::as_str), Some("shell:host"));
        assert_eq!(attributes.get("harness.mode").map(String::as_str), Some("ask"));
        assert_eq!(attributes.get("harness.isolation").map(String::as_str), Some("host"));
    }
    let rules: Vec<_> =
        decisions.iter().filter_map(|(_, attributes)| attributes.get("harness.rule_id")).collect();
    assert_eq!(rules.len(), 1, "the rule-allowed call names its rule");
    assert!(decisions.iter().all(|(_, attributes)| !attributes.contains_key("harness.member")));
}

#[test]
fn spans_for_members_and_goal_iterations_carry_their_attributes() {
    let capture = Arc::new(Capture::default());
    let subscriber = tracing_subscriber::registry().with(AdkSpanLayer::new(capture.clone()));
    tracing::subscriber::with_default(subscriber, || {
        drop(adk_harness::member_span("explore", "inv-1"));
        drop(adk_harness::goal_iteration_span(2, 10));
    });
    let spans = capture.0.lock().unwrap().clone();
    assert_eq!(spans[0].1.get(adk_harness::ATTR_MEMBER).map(String::as_str), Some("explore"));
    assert_eq!(spans[1].1.get(adk_harness::ATTR_GOAL_ITERATION).map(String::as_str), Some("2"));
}
