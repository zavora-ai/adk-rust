//! Model posture through real harness runs (spec 3.1 §7.23, §9.2 I56): the request prefix
//! is byte-stable across the model calls of a run while todos, memory and recalled facts
//! change, per-turn text is appended last, `harness.spend` carries the prompt tokens the
//! provider cached and the run's effort, a cache-capable model gets the runner's default
//! context cache unless caching is turned off, and every request carries the effort of the
//! mode it reads, a member's `members`, on the same client (§7.23 "Mechanism", §10 C4).

#![cfg(all(feature = "runner", feature = "observe", feature = "memory"))]

use crate::common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{CacheCapable, LlmRequest, Part, UsageMetadata};
use adk_harness::{
    Effort, EffortPolicy, FactKind, FactScope, FactSource, FactStatus, FactStore, HarnessMode,
    InMemoryFactStore, LongTermMemoryConfig, MemoryConfig, MemoryFact, MemoryLayout, Provenance,
    TurnTokens, project_slug,
};
use adk_model::ScriptedResponse;
use async_trait::async_trait;
use common::{Outcome, fixture, fixture_in, outcome, response_of};
use proptest::prelude::*;
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn text(content: &Content) -> String {
    content.parts.iter().filter_map(Part::text).collect::<Vec<_>>().join("\n")
}

use adk_core::Content;

/// The prefix of a request: its system contents and tool declarations, in order.
fn prefix(request: &LlmRequest) -> (Vec<String>, Vec<(String, Value)>) {
    let system =
        request.contents.iter().filter(|content| content.role == "system").map(text).collect();
    let mut tools: Vec<(String, Value)> = request
        .tools
        .iter()
        .map(|(name, declaration)| (name.clone(), declaration.clone()))
        .collect();
    tools.sort_by(|a, b| a.0.cmp(&b.0));
    (system, tools)
}

/// Whether the request ends with the harness's per-turn text, after the conversation.
fn per_turn_text_is_last(request: &LlmRequest) -> bool {
    request
        .contents
        .last()
        .map(text)
        .is_some_and(|last| last.contains("<system-reminder>") && last.contains("mode"))
}

fn assert_prefix_stable(requests: &[LlmRequest]) {
    let first = prefix(&requests[0]);
    assert!(!first.0.is_empty(), "the request carries a system instruction");
    assert!(!first.1.is_empty(), "the request carries tool declarations");
    for (i, request) in requests.iter().enumerate() {
        assert_eq!(prefix(request), first, "request {i}'s prefix differs from the first");
        assert!(per_turn_text_is_last(request), "request {i} does not end with the per-turn text");
    }
}

#[tokio::test]
async fn the_prefix_is_byte_stable_while_memory_and_todos_change_within_a_run() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let base = tempfile::tempdir().unwrap();
    let f = fixture(
        vec![
            call(
                "write_todos",
                json!({ "todos": [{ "content": "write the note" }, { "content": "read it back" }] }),
            ),
            call(
                "memory_write",
                json!({ "path": "build.md", "summary": "how to build", "content": "cargo build" }),
            ),
            call("read_file", json!({ "path": "README.md" })),
            ScriptedResponse::text("done"),
        ],
        |b| {
            b.mode(HarnessMode::Auto).prompter(prompter.clone()).memory(MemoryConfig {
                layout: MemoryLayout::PerUser { base: base.path().to_path_buf() },
                ..MemoryConfig::default()
            })
        },
    )
    .await;
    std::fs::write(f.path("README.md"), "a project\n").unwrap();

    let events = f.send("write a build note, then read the readme").await;
    assert_eq!(outcome(&response_of(&events, "memory_write")), Outcome::Executed);
    let requests = f.model.requests();
    assert_eq!(requests.len(), 4);
    assert_prefix_stable(&requests);
    let first_instruction = prefix(&requests[0]).0.join("\n");
    assert!(!first_instruction.contains("build.md"), "the run started with no build note");

    // The next run's instruction carries the note written during the first (spec 3.1 §7.7:
    // built once per run).
    f.model.push(ScriptedResponse::text("ok"));
    f.send("anything else?").await;
    let next = common::last_system_text(&f.model);
    assert!(next.contains("build.md"), "the next run lists the new topic:\n{next}");
}

fn seeded(scope: &FactScope, n: usize) -> Arc<InMemoryFactStore> {
    let store = Arc::new(InMemoryFactStore::new());
    for i in 0..n {
        let fact = MemoryFact::new(
            scope.clone(),
            FactKind::Procedure,
            format!("Fact {i}: the tests need `--features sqlite`"),
            FactStatus::Known,
            Provenance::new(FactSource::UserStated, "s0", "inv-0"),
        );
        futures::executor::block_on(store.put(fact)).unwrap();
    }
    store
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 100, ..ProptestConfig::default() })]

    /// I56: whatever the mode, the todo list and the recalled facts do, the system
    /// instruction and tool declarations are byte-identical across the model calls of a
    /// run and the per-turn text is appended last.
    #[test]
    fn the_prefix_is_byte_identical_whatever_the_mode_todos_and_facts_do(
        mode in prop::sample::select(HarnessMode::ALL.to_vec()),
        todos in 0usize..5,
        facts in 0usize..4,
        rounds in 1usize..4,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.txt"), "a").unwrap();
            let scope = FactScope::new(common::APP, "alice", Some(project_slug(dir.path())));
            let store = seeded(&scope, facts);
            let mut script = Vec::new();
            if todos > 0 {
                let items: Vec<Value> =
                    (0..todos).map(|i| json!({ "content": format!("step {i}") })).collect();
                script.push(call("write_todos", json!({ "todos": items })));
            }
            for _ in 0..rounds {
                script.push(call("read_file", json!({ "path": "a.txt" })));
            }
            script.push(ScriptedResponse::text("done"));
            let expected = script.len();
            let f = fixture_in(dir, script, |b| {
                b.mode(mode)
                    .long_term_memory(LongTermMemoryConfig::default())
                    .fact_store(store.clone())
            })
            .await;
            f.send("run the tests").await;
            let requests = f.model.requests();
            assert_eq!(requests.len(), expected);
            assert_prefix_stable(&requests);
            let recalled = requests[0].contents.iter().map(text).any(|t| t.contains("<memory-facts>"));
            assert_eq!(recalled, facts > 0, "facts are recalled exactly when the store has some");
        });
    }

    /// The policy is total over modes, `uniform` is constant, and it round-trips through
    /// serde (the `[effort]` table of `harness.toml`).
    #[test]
    fn the_effort_policy_is_total_and_round_trips(
        levels in prop::collection::vec(prop::sample::select(Effort::ALL.to_vec()), 7),
        mode in prop::sample::select(HarnessMode::ALL.to_vec()),
    ) {
        let policy = EffortPolicy {
            read_only: levels[0],
            plan: levels[1],
            ask: levels[2],
            auto: levels[3],
            members: levels[4],
            distiller: levels[5],
            sampling: levels[6],
        };
        let expected = match mode {
            HarnessMode::ReadOnly => policy.read_only,
            HarnessMode::Plan => policy.plan,
            HarnessMode::Ask => policy.ask,
            HarnessMode::Auto => policy.auto,
            other => unreachable!("{other} is not a mode this release knows"),
        };
        prop_assert_eq!(policy.for_mode(mode), expected);
        prop_assert_eq!(EffortPolicy::uniform(levels[0]).for_mode(mode), levels[0]);
        let json = serde_json::to_value(policy).unwrap();
        prop_assert_eq!(serde_json::from_value::<EffortPolicy>(json).unwrap(), policy);
        prop_assert!(Effort::ALL.contains(&policy.for_mode(mode)));
    }
}

#[tokio::test]
async fn harness_spend_carries_prompt_and_cached_tokens_and_the_run_effort() {
    let usage = |prompt: i32, cached: i32| UsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: 10,
        total_token_count: prompt + 10,
        cache_read_input_token_count: Some(cached),
        ..UsageMetadata::default()
    };
    let f = fixture(
        vec![
            call("read_file", json!({ "path": "a.txt" })).with_usage(usage(1000, 800)),
            ScriptedResponse::text("done").with_usage(usage(1200, 1000)),
        ],
        |b| b.effort(EffortPolicy { ask: Effort::High, ..EffortPolicy::default() }),
    )
    .await;
    std::fs::write(f.path("a.txt"), "a").unwrap();
    f.send("read a").await;

    let spend = f.records("harness.spend");
    assert_eq!(spend.len(), 1);
    assert_eq!(spend[0].ext["promptTokens"], 2200);
    assert_eq!(spend[0].ext["cachedTokens"], 1800);
    assert_eq!(spend[0].ext["effort"], "high");
    assert_eq!(spend[0].ext["modelCalls"], 2);

    let report = f.harness.spend(Some(&f.user));
    assert_eq!(
        report.turns,
        vec![TurnTokens { model_calls: 2, prompt_tokens: 2200, cached_prompt_tokens: 1800 }]
    );
    assert_eq!((report.prompt_tokens, report.cached_prompt_tokens), (2200, 1800));
    assert!(report.render().contains("2200 (1800 cached, 400 uncached)"), "{}", report.render());
    assert_eq!(f.harness.agent().effort().ask, Effort::High);
    assert_eq!(f.harness.agent().info().effort.plan, Effort::High);
}

/// A model that records every cache it is asked to create.
#[derive(Default)]
struct RecordingCache {
    created: Mutex<Vec<(String, u32)>>,
}

#[async_trait]
impl CacheCapable for RecordingCache {
    async fn create_cache(
        &self,
        system_instruction: &str,
        _tools: &HashMap<String, Value>,
        ttl_seconds: u32,
    ) -> adk_core::Result<String> {
        self.created.lock().unwrap().push((system_instruction.to_string(), ttl_seconds));
        Ok("cachedContents/test".into())
    }

    async fn delete_cache(&self, _name: &str) -> adk_core::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_cache_capable_model_gets_the_default_context_cache_unless_caching_is_off() {
    let cache = Arc::new(RecordingCache::default());
    let f = fixture(vec![ScriptedResponse::text("done")], |b| b.cache_capable(cache.clone())).await;
    f.send("hello").await;
    let created = cache.created.lock().unwrap().clone();
    assert_eq!(created.len(), 1, "the runner created one cache at the start of the run");
    assert_eq!(created[0].1, adk_core::ContextCacheConfig::default().ttl_seconds);
    assert!(f.harness.agent().info().context_caching);

    let off = Arc::new(RecordingCache::default());
    let g = fixture(vec![ScriptedResponse::text("done")], |b| {
        b.cache_capable(off.clone()).without_context_caching()
    })
    .await;
    g.send("hello").await;
    assert!(off.created.lock().unwrap().is_empty(), "no cache without context caching");
    assert!(!g.harness.agent().info().context_caching);
}

/// The effort of each request the model received.
fn efforts(requests: &[LlmRequest]) -> Vec<Option<Effort>> {
    requests.iter().map(|request| request.config.as_ref().and_then(|c| c.effort)).collect()
}

#[tokio::test]
async fn an_approved_plan_moves_the_next_request_to_the_ask_effort_on_the_same_client() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(
        vec![
            call(
                "exit_plan_mode",
                json!({ "plan": "1. read it", "steps": [{ "id": "1", "title": "read", "instruction": "read it" }] }),
            ),
            ScriptedResponse::text("reading it"),
        ],
        |b| {
            b.mode(HarnessMode::Plan)
                .prompter(prompter.clone())
                .effort(EffortPolicy { plan: Effort::High, ask: Effort::Low, ..EffortPolicy::default() })
        },
    )
    .await;
    f.send("plan it").await;
    assert_eq!(f.harness.mode(&f.user, &f.session).await.unwrap(), HarnessMode::Ask);
    f.send("go").await;

    let requests = f.model.requests();
    assert_eq!(efforts(&requests), vec![Some(Effort::High), Some(Effort::Low)]);
    // I56: the effort travels in the config, so the prefix is the one the plan turn sent.
    assert_eq!(prefix(&requests[1]), prefix(&requests[0]));
}

#[tokio::test]
async fn an_operator_mode_change_reaches_the_next_request_without_a_rebuild() {
    let f = fixture(vec![ScriptedResponse::text("one"), ScriptedResponse::text("two")], |b| {
        b.mode(HarnessMode::ReadOnly)
            .effort(EffortPolicy { auto: Effort::High, ..EffortPolicy::uniform(Effort::Low) })
    })
    .await;
    f.send("first").await;
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();
    f.send("second").await;
    assert_eq!(efforts(&f.model.requests()), vec![Some(Effort::Low), Some(Effort::High)]);
}

#[cfg(feature = "team")]
#[tokio::test]
async fn a_member_runs_at_the_members_effort_and_the_coordinator_at_its_modes() {
    let members = Arc::new(adk_model::ScriptedLlm::new(
        "member",
        vec![ScriptedResponse::text("found the readme")],
    ));
    let member_model = members.clone();
    let f = fixture(
        vec![
            call("explore", json!({ "request": "find the readme" })),
            ScriptedResponse::text("ok"),
        ],
        move |b| {
            b.mode(HarnessMode::Auto).member_model(member_model).effort(EffortPolicy {
                auto: Effort::High,
                members: Effort::Low,
                ..EffortPolicy::default()
            })
        },
    )
    .await;
    f.send("look around").await;
    assert_eq!(efforts(&f.model.requests()), vec![Some(Effort::High); 2]);
    assert_eq!(efforts(&members.requests()), vec![Some(Effort::Low)]);
}
