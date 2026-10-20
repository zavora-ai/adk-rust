//! Property tests of long-term facts (spec 3.1 §11 "Long-term memory"): no recall across
//! users or projects; a forgotten fact is never recalled; no fact is `Known` without a
//! verified quote or a human promotion; recall never exceeds its token cap.

#![cfg(all(feature = "runner", feature = "memory"))]

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use adk_core::principal::SubjectRef;
use adk_core::{Content, LlmRequest, Part, ReadonlyContext, Toolset};
use adk_harness::{
    FactFilter, FactKind, FactScope, FactSource, FactStatus, FactStore, Harness, HarnessAgent,
    HarnessToolset, InMemoryFactStore, LongTermMemoryConfig, MemoryFact, Provenance, project_slug,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use proptest::prelude::*;
use serde_json::json;

const WORDS: [&str; 10] =
    ["pnpm", "tests", "sqlite", "deploy", "release", "lint", "cache", "docker", "branch", "build"];

fn words() -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(prop::sample::select(&WORDS[..]), 1..5)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// A harness over one workspace and store, shared by the cases of a property.
struct Setup {
    _dir: tempfile::TempDir,
    store: Arc<InMemoryFactStore>,
    toolset: Arc<HarnessToolset>,
    project: String,
}

fn setup(config: LongTermMemoryConfig) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(InMemoryFactStore::new());
    let model = Arc::new(ScriptedLlm::new("m", Vec::<ScriptedResponse>::new()));
    let agent = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(common::no_user_file())
        .without_compaction()
        .long_term_memory(config)
        .fact_store(store.clone())
        .build()
        .unwrap();
    let toolset = agent.runtime().toolset().clone();
    let project = project_slug(dir.path());
    Setup { _dir: dir, store, toolset, project }
}

static SHARED: LazyLock<Setup> = LazyLock::new(|| {
    setup(LongTermMemoryConfig {
        min_score: 0.01,
        recall_limit: 50,
        recall_tokens: 100_000,
        ..LongTermMemoryConfig::default()
    })
});

static INVOCATION: AtomicU64 = AtomicU64::new(0);

struct Ctx {
    user: String,
    invocation: String,
    content: Content,
}

#[async_trait::async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        &self.invocation
    }
    fn agent_name(&self) -> &str {
        "harness"
    }
    fn user_id(&self) -> &str {
        &self.user
    }
    fn app_name(&self) -> &str {
        common::APP
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

/// The ids and lines of the `<memory-facts>` block one request receives.
async fn recall(
    toolset: &HarnessToolset,
    user: &str,
    invocation: &str,
    query: &str,
) -> Vec<(String, String)> {
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(Ctx {
        user: user.to_string(),
        invocation: invocation.to_string(),
        content: Content::new("user").with_text(query),
    });
    let mut request = LlmRequest::new("m", vec![Content::new("user").with_text(query)]);
    toolset.process_llm_request(ctx, &mut request).await.unwrap();
    request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .filter(|text| text.starts_with("<memory-facts>"))
        .flat_map(|block| block.lines())
        .filter(|line| line.starts_with("- ["))
        .map(|line| {
            let id =
                line.rsplit_once('(').map(|(_, id)| id.trim_end_matches(')')).unwrap_or_default();
            (id.to_string(), line.to_string())
        })
        .collect()
}

fn fresh_invocation() -> String {
    format!("inv-{}", INVOCATION.fetch_add(1, Ordering::SeqCst))
}

#[derive(Clone, Debug)]
struct Planted {
    app: bool,
    user: bool,
    project: u8,
    known: bool,
    words: Vec<&'static str>,
}

fn planted() -> impl Strategy<Value = Planted> {
    (any::<bool>(), any::<bool>(), 0u8..3, any::<bool>(), words())
        .prop_map(|(app, user, project, known, words)| Planted { app, user, project, known, words })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Recall for a user in a project returns only that user's facts of that project and
    /// their user-wide facts, never another user's, project's or app's (I42).
    #[test]
    fn recall_never_crosses_users_projects_or_apps(facts in prop::collection::vec(planted(), 1..16), query in words()) {
        let shared = &*SHARED;
        let case = fresh_invocation();
        let me = format!("alice-{case}");
        let them = format!("bob-{case}");
        runtime().block_on(async {
            let mut mine = Vec::new();
            for fact in &facts {
                let project = match fact.project {
                    0 => Some(shared.project.clone()),
                    1 => Some("elsewhere-000000000000".to_string()),
                    _ => None,
                };
                let scope = FactScope::new(
                    if fact.app { common::APP } else { "other-app" },
                    if fact.user { me.clone() } else { them.clone() },
                    project.clone(),
                );
                let stored = shared.store.put(MemoryFact::new(
                    scope,
                    FactKind::ProjectFact,
                    fact.words.join(" "),
                    if fact.known { FactStatus::Known } else { FactStatus::Assumed },
                    Provenance::new(FactSource::AgentInferred, "s", "i"),
                )).await.unwrap();
                if fact.app && fact.user && fact.project != 1 {
                    mine.push(stored.id);
                }
            }
            for (id, line) in recall(&shared.toolset, &me, &case, &query.join(" ")).await {
                prop_assert!(mine.contains(&id), "recalled a fact outside the scope: {line}");
            }
            Ok(())
        })?;
    }

    /// A forgotten fact is never recalled, also within the same turn after it was shown
    /// (I44).
    #[test]
    fn a_forgotten_fact_is_never_recalled(
        texts in prop::collection::vec(words(), 1..12),
        forget in prop::collection::vec(any::<bool>(), 12),
        query in words(),
    ) {
        let shared = &*SHARED;
        let case = fresh_invocation();
        let me = format!("carol-{case}");
        runtime().block_on(async {
            let mut ids = Vec::new();
            for words in &texts {
                let stored = shared.store.put(MemoryFact::new(
                    FactScope::new(common::APP, me.clone(), Some(shared.project.clone())),
                    FactKind::Procedure,
                    words.join(" "),
                    FactStatus::Assumed,
                    Provenance::new(FactSource::AgentInferred, "s", "i"),
                )).await.unwrap();
                ids.push(stored.id);
            }
            let query = query.join(" ");
            recall(&shared.toolset, &me, &case, &query).await;
            let mut forgotten = Vec::new();
            for (id, forget) in ids.iter().zip(&forget) {
                if *forget && shared.store.get(id).await.unwrap().unwrap().status.is_live() {
                    shared.store.set_status(id, FactStatus::Forgotten, &SubjectRef::human(me.clone())).await.unwrap();
                    forgotten.push(id.clone());
                }
            }
            // The same turn's next model call, and a later turn.
            for invocation in [case.clone(), fresh_invocation()] {
                for (id, line) in recall(&shared.toolset, &me, &invocation, &query).await {
                    prop_assert!(!forgotten.contains(&id), "recalled a forgotten fact: {line}");
                }
            }
            Ok(())
        })?;
    }

    /// Recall never exceeds `recall_tokens` (about four characters a token per line) or
    /// `recall_limit`.
    #[test]
    fn recall_never_exceeds_its_token_cap(
        lengths in prop::collection::vec(1usize..480, 1..20),
        cap in 10usize..400,
        limit in 1usize..10,
    ) {
        let setup = setup(LongTermMemoryConfig {
            recall_tokens: cap,
            recall_limit: limit,
            min_score: 0.01,
            ..LongTermMemoryConfig::default()
        });
        runtime().block_on(async {
            for length in &lengths {
                let mut text = "sqlite ".repeat(length / 7 + 1);
                text.truncate(*length);
                setup.store.put(MemoryFact::new(
                    FactScope::new(common::APP, "dave", Some(setup.project.clone())),
                    FactKind::ProjectFact,
                    format!("sqlite {text}"),
                    FactStatus::Known,
                    Provenance::new(FactSource::UserStated, "s", "i"),
                )).await.unwrap();
            }
            let lines = recall(&setup.toolset, "dave", &fresh_invocation(), "sqlite").await;
            let tokens: usize = lines.iter().map(|(_, line)| line.chars().count().div_ceil(4)).sum();
            prop_assert!(tokens <= cap, "{tokens} > {cap}");
            prop_assert!(lines.len() <= limit);
            Ok(())
        })?;
    }
}

#[derive(Clone, Debug)]
struct Proposed {
    text: Vec<&'static str>,
    source: &'static str,
    quote: Option<(usize, usize, bool)>,
}

fn proposed() -> impl Strategy<Value = Proposed> {
    (
        words(),
        // The distiller's structured output allows these three sources.
        prop::sample::select(&["user_stated", "agent_inferred", "tool_observed"][..]),
        prop::option::of((0usize..40, 1usize..40, any::<bool>())),
    )
        .prop_map(|(text, source, quote)| Proposed { text, source, quote })
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Whatever the distiller claims, a stored fact is `Known` only when its quote appears
    /// verbatim in one of the user's own messages of the session; everything else is
    /// `Assumed` (I41).
    #[test]
    fn no_fact_is_known_without_a_verified_quote(
        message in prop::collection::vec(prop::sample::select(&WORDS[..]), 3..12),
        tool_text in prop::collection::vec(prop::sample::select(&WORDS[..]), 3..12),
        candidates in prop::collection::vec(proposed(), 1..6),
    ) {
        let message = message.join(" ");
        let tool_text = tool_text.join(" ");
        let facts: Vec<serde_json::Value> = candidates
            .iter()
            .map(|candidate| {
                let mut fact = json!({
                    "text": candidate.text.join(" "),
                    "kind": "project_fact",
                    "source": candidate.source,
                    "quote": "",
                    "confidence": 0.9,
                    "supersedes": "",
                });
                if let Some((start, len, from_user)) = candidate.quote {
                    let source: Vec<char> = if from_user { message.chars().collect() } else { tool_text.chars().collect() };
                    let start = start.min(source.len().saturating_sub(1));
                    let end = (start + len).min(source.len());
                    fact["quote"] = json!(source[start..end].iter().collect::<String>());
                }
                fact
            })
            .collect();
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("notes.txt"), &tool_text).unwrap();
            let store = Arc::new(InMemoryFactStore::new());
            let model = Arc::new(ScriptedLlm::new("m", vec![
                ScriptedResponse::function_call("read_file", json!({ "path": "notes.txt" })),
                ScriptedResponse::text("ok"),
            ]));
            let distiller = Arc::new(ScriptedLlm::new("d", vec![ScriptedResponse::text(json!({ "facts": facts }).to_string())]));
            let probe = distiller.clone();
            let agent = Arc::new(HarnessAgent::builder("harness")
                .model(model)
                .workspace(adk_devtools::Workspace::new(dir.path()))
                .project_instructions(common::no_user_file())
                .without_compaction()
                .long_term_memory(LongTermMemoryConfig { distill_model: Some(distiller), ..LongTermMemoryConfig::default() })
                .fact_store(store.clone())
                .build()
                .unwrap());
            let runner = agent
                .configure_runner(Runner::builder().app_name(common::APP).agent(agent.clone()).session_service(Arc::new(InMemorySessionService::new())))
                .build()
                .unwrap();
            let harness = Harness::with_runner(agent, runner).unwrap();
            let user = common::user("erin");
            let session = harness.open_session(&user, None).await.unwrap();
            common::collect(harness.send(&user, &session, message.clone()).await.unwrap()).await;
            harness.close().await;
            let all = store.list(FactFilter::user(common::APP, "erin")).await.unwrap();
            prop_assert_eq!(probe.requests().len(), 1, "the session was distilled");
            prop_assert!(!all.is_empty(), "at least one candidate is stored");
            for fact in all {
                if fact.status == FactStatus::Known {
                    prop_assert_eq!(fact.provenance.source, FactSource::UserStated);
                    let quote = fact.provenance.quote.clone().unwrap_or_default();
                    prop_assert!(!quote.is_empty() && collapse(&message).contains(&collapse(&quote)),
                        "known without a verified quote: {:?}", fact);
                } else {
                    prop_assert_eq!(fact.provenance.quote.clone(), None);
                    prop_assert!(matches!(fact.provenance.source, FactSource::AgentInferred | FactSource::ToolObserved));
                }
            }
            Ok(())
        })?;
    }
}
