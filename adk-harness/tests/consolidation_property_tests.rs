//! Property tests of the consolidation tick (spec 3.2 §7.5, §11, §9.2 K-I12, K-I22): over
//! random facts, vectors, ages and confidences, sessions the distiller never reached, and a
//! model that answers every distillation and merge by naming `Known` facts, the tick never
//! writes a `Known` fact and never changes one's status; the passes never cross the loss
//! cap; and `undo` right after a tick is the identity on statuses.

#![cfg(all(feature = "runner", feature = "memory"))]

use crate::common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use adk_core::{
    Content, Event, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, UsageMetadata, UserId,
};
use adk_harness::{
    ChangeReason, ConsolidationConfig, ConsolidationRecord, ConsolidationRequest, FactFilter,
    FactKind, FactScope, FactSource, FactStatus, FactStore, Harness, HarnessAgent, HarnessMode,
    InMemoryFactStore, LongTermMemoryConfig, MemoryFact, Provenance, project_slug,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use chrono::{DateTime, Utc};
use proptest::prelude::*;
use serde_json::json;

use common::APP;

/// The ids every answer of the oracle names in `supersedes`, set per case.
static TARGETS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// One case at a time across the properties, since they share the oracle's targets.
static SERIAL: Mutex<()> = Mutex::new(());

/// A model that answers a distillation with one quoted candidate per user line and a merge
/// with every cluster id, each naming the case's targets, `Known` facts among them.
struct Oracle;

fn targets() -> Vec<String> {
    TARGETS.lock().unwrap().clone()
}

#[async_trait::async_trait]
impl Llm for Oracle {
    fn name(&self) -> &str {
        "oracle"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let input: String = request
            .contents
            .iter()
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .collect::<Vec<_>>()
            .join("\n");
        let targets = targets();
        let answer = if input.contains("Session transcript") {
            const KINDS: [&str; 3] = ["preference", "project_fact", "correction"];
            let facts: Vec<_> = input
                .lines()
                .filter_map(|line| line.strip_prefix("[user] "))
                .enumerate()
                .map(|(i, said)| {
                    json!({
                        "text": said, "kind": KINDS[i % KINDS.len()], "source": "user_stated",
                        "quote": said, "confidence": 0.9,
                        "supersedes": targets.get(i % targets.len().max(1)).cloned().unwrap_or_default(),
                    })
                })
                .collect();
            json!({ "facts": facts })
        } else {
            let mut ids: Vec<String> = input
                .lines()
                .filter_map(|line| line.strip_prefix("- "))
                .filter_map(|line| line.split_whitespace().next().map(str::to_string))
                .collect();
            let text = format!("merged {}", ids.join(" "));
            ids.extend(targets);
            json!({ "merged_text": text, "source_ids": ids, "supersedes": ids })
        };
        let mut response = LlmResponse::new(Content {
            role: "model".into(),
            parts: vec![Part::Text { text: answer.to_string() }],
        });
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 5,
            candidates_token_count: 5,
            total_token_count: 10,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

struct Setup {
    dir: tempfile::TempDir,
    store: Arc<InMemoryFactStore>,
    sessions: Arc<InMemorySessionService>,
    harness: Harness,
}

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
});

static SETUP: LazyLock<Setup> = LazyLock::new(|| {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(InMemoryFactStore::new());
    let sessions = Arc::new(InMemorySessionService::new());
    let _entered = RUNTIME.enter();
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(Arc::new(ScriptedLlm::new("m", Vec::<ScriptedResponse>::new())))
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(common::no_user_file())
            .without_compaction()
            .mode(HarnessMode::Auto)
            .long_term_memory(LongTermMemoryConfig {
                distill_model: Some(Arc::new(Oracle)),
                distill_idle: std::time::Duration::from_secs(3600),
                ..LongTermMemoryConfig::default()
            })
            .fact_store(store.clone())
            .build()
            .unwrap(),
    );
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    Setup { dir, store, sessions, harness }
});

static CASE: AtomicU64 = AtomicU64::new(0);

/// One seeded fact.
#[derive(Clone, Debug)]
struct Spec {
    known: bool,
    user_wide: bool,
    kind: usize,
    direction: usize,
    noise: usize,
    age_days: i64,
    confidence: Option<f32>,
    used: bool,
}

const KINDS: [FactKind; 3] = [FactKind::Preference, FactKind::ProjectFact, FactKind::Correction];
/// Off-axis weights: cosines to the axis 1.0, 0.98, 0.91 and 0.82, so pairs fall in every
/// band of the passes.
const NOISE: [f32; 4] = [0.0, 0.2, 0.45, 0.7];
const WORDS: [&str; 8] =
    ["pnpm", "tests", "sqlite", "deploy", "release", "lint", "cache", "docker"];

fn spec() -> impl Strategy<Value = Spec> {
    (
        any::<bool>(),
        prop::bool::weighted(0.25),
        0..KINDS.len(),
        0..3usize,
        0..NOISE.len(),
        0..200i64,
        prop::option::weighted(0.85, 0.0f32..1.0),
        prop::bool::weighted(0.2),
    )
        .prop_map(|(known, user_wide, kind, direction, noise, age_days, confidence, used)| {
            Spec { known, user_wide, kind, direction, noise, age_days, confidence, used }
        })
}

fn said() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(
        prop::collection::vec(prop::sample::select(&WORDS[..]), 2..5)
            .prop_map(|words| format!("please {}", words.join(" "))),
        1..3,
    )
}

/// A case: seeded facts, sessions of user lines, and which facts the oracle names.
#[derive(Clone, Debug)]
struct Case {
    facts: Vec<Spec>,
    sessions: Vec<Vec<String>>,
    targets: Vec<prop::sample::Index>,
    max_loss_fraction: f32,
}

fn case(with_sessions: bool) -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(spec(), 1..14),
        prop::collection::vec(said(), if with_sessions { 0..3 } else { 0..1 }),
        prop::collection::vec(any::<prop::sample::Index>(), 0..4),
        prop::sample::select(vec![0.1f32, 0.25, 0.5, 1.0]),
    )
        .prop_map(|(facts, sessions, targets, max_loss_fraction)| Case {
            facts,
            sessions,
            targets,
            max_loss_fraction,
        })
}

struct Seeded {
    user: UserId,
    facts: Vec<MemoryFact>,
    sessions: Vec<String>,
    now: DateTime<Utc>,
}

async fn seed(case: &Case) -> Seeded {
    let setup = &*SETUP;
    let n = CASE.fetch_add(1, Ordering::SeqCst);
    let user = format!("user-{n}");
    let project = project_slug(setup.dir.path());
    let now = Utc::now();
    let mut facts = Vec::new();
    for (i, spec) in case.facts.iter().enumerate() {
        let scope = FactScope::new(APP, user.clone(), (!spec.user_wide).then(|| project.clone()));
        let status = if spec.known { FactStatus::Known } else { FactStatus::Assumed };
        let mut fact = MemoryFact::new(
            scope,
            KINDS[spec.kind],
            format!("seeded fact {i} of {user}"),
            status,
            Provenance::new(FactSource::AgentInferred, "s0", "inv-0"),
        );
        fact.valid_from =
            now - chrono::Duration::days(spec.age_days) + chrono::Duration::milliseconds(i as i64);
        fact.confidence = spec.confidence;
        let fact = setup.store.put(fact).await.unwrap();
        if spec.used {
            setup.store.record_recall(std::slice::from_ref(&fact.id), now).await.unwrap();
        }
        let mut vector = vec![0.0f32; 4];
        vector[spec.direction] = 1.0;
        vector[spec.direction + 1] = NOISE[spec.noise];
        let embedding = adk_harness::FactEmbedding::new("test/seeded", vector);
        setup.store.put_embedding(&fact.id, embedding).await.unwrap();
        facts.push(setup.store.get(&fact.id).await.unwrap().unwrap());
    }
    let mut sessions = Vec::new();
    for (i, lines) in case.sessions.iter().enumerate() {
        let id = format!("{user}-s{i}");
        setup
            .sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: user.clone(),
                session_id: Some(id.clone()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        for line in lines {
            let mut event = Event::new("inv-1");
            event.author = "user".into();
            event.llm_response.content = Some(Content::new("user").with_text(line.clone()));
            setup.sessions.append_event(&id, event).await.unwrap();
        }
        sessions.push(id);
    }
    *TARGETS.lock().unwrap() =
        case.targets.iter().map(|index| index.get(&facts).id.clone()).collect();
    Seeded { user: UserId::new(user).unwrap(), facts, sessions, now }
}

async fn tick(seeded: &Seeded, case: &Case) -> ConsolidationRecord {
    SETUP
        .harness
        .consolidate_memory(
            &seeded.user,
            ConsolidationRequest::new()
                .with_sessions(seeded.sessions.clone())
                .with_config(ConsolidationConfig {
                    max_loss_fraction: case.max_loss_fraction,
                    ..ConsolidationConfig::default()
                })
                .at(seeded.now + chrono::Duration::hours(2)),
        )
        .await
        .unwrap()
}

async fn all(user: &UserId) -> Vec<MemoryFact> {
    SETUP.store.list(FactFilter::user(APP, user.as_ref())).await.unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// K-I12 and K-I22: no fact the tick writes is `Known`, no `Known` fact changes, and
    /// every status change is of a fact that was `Assumed`.
    #[test]
    fn the_tick_never_writes_or_changes_a_known_fact(case in case(true)) {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        RUNTIME.block_on(async {
            let seeded = seed(&case).await;
            let record = tick(&seeded, &case).await;
            let after: HashMap<String, MemoryFact> =
                all(&seeded.user).await.into_iter().map(|fact| (fact.id.clone(), fact)).collect();
            for before in seeded.facts.iter().filter(|fact| fact.status == FactStatus::Known) {
                let now = &after[&before.id];
                prop_assert_eq!((&now.status, now.valid_to), (&FactStatus::Known, None));
            }
            let seeded_ids: Vec<&String> = seeded.facts.iter().map(|fact| &fact.id).collect();
            for fact in after.values() {
                if fact.provenance.tick.as_deref() == Some(record.tick_id.as_str()) {
                    prop_assert_ne!(&fact.status, &FactStatus::Known);
                }
                if fact.status == FactStatus::Known {
                    prop_assert!(seeded_ids.contains(&&fact.id), "a new known fact: {:?}", fact);
                }
            }
            for change in &record.changes {
                let seeded_known = seeded
                    .facts
                    .iter()
                    .any(|fact| fact.id == change.fact && fact.status == FactStatus::Known);
                prop_assert!(!seeded_known, "a change of a known fact: {:?}", change);
            }
            Ok(())
        })?;
    }

    /// The loss cap: the passes lose at most `max(1, ⌊max_loss_fraction × live⌋)` of each
    /// scope's live facts — one per pass-1 supersession or expiry, and per merge the facts
    /// it supersedes less the one it writes — and a refused tick changes nothing.
    #[test]
    fn no_tick_crosses_the_loss_cap(case in case(false)) {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        RUNTIME.block_on(async {
            let seeded = seed(&case).await;
            let record = tick(&seeded, &case).await;
            for scope in &record.scopes {
                let live = seeded.facts.iter().filter(|fact| fact.scope == *scope).count();
                let in_scope: Vec<_> =
                    record.changes.iter().filter(|change| change.scope == *scope).collect();
                let pass_one = in_scope
                    .iter()
                    .filter(|change| {
                        matches!(change.reason, ChangeReason::Duplicate | ChangeReason::Expired)
                    })
                    .count();
                // Per merge target, the facts it superseded; a target the tick wrote kept
                // their content, so that merge lost one fact fewer.
                let mut merges: HashMap<&str, usize> = HashMap::new();
                for change in &in_scope {
                    if let (ChangeReason::Merged, FactStatus::Superseded { by }) =
                        (&change.reason, &change.to)
                    {
                        *merges.entry(by.as_str()).or_default() += 1;
                    }
                }
                let merged: usize = merges
                    .iter()
                    .map(|(by, superseded)| {
                        if record.merged.iter().any(|id| id == by) {
                            superseded - 1
                        } else {
                            *superseded
                        }
                    })
                    .sum();
                let lost = pass_one + merged;
                let cap = ((case.max_loss_fraction * live as f32).floor() as usize).max(1);
                prop_assert!(lost <= cap, "{lost} of {live} under {}", case.max_loss_fraction);
            }
            if record.refused.is_some() {
                prop_assert!(record.changes.is_empty() && record.merged.is_empty());
                let after = all(&seeded.user).await;
                for before in &seeded.facts {
                    let now = after.iter().find(|fact| fact.id == before.id).unwrap();
                    prop_assert_eq!(&now.status, &before.status);
                }
            }
            Ok(())
        })?;
    }

    /// `undo` right after any tick is the identity on statuses: every fact the tick found
    /// has its status and `valid_to` back, and no fact it wrote is live.
    #[test]
    fn undo_after_a_tick_is_the_identity_on_statuses(case in case(true)) {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        RUNTIME.block_on(async {
            let seeded = seed(&case).await;
            let record = tick(&seeded, &case).await;
            SETUP.harness.undo_consolidation(&seeded.user, &record.tick_id).await.unwrap();
            let after: HashMap<String, MemoryFact> =
                all(&seeded.user).await.into_iter().map(|fact| (fact.id.clone(), fact)).collect();
            for before in &seeded.facts {
                let now = &after[&before.id];
                prop_assert_eq!((&now.status, now.valid_to), (&before.status, before.valid_to));
            }
            for id in record.written() {
                prop_assert!(!after[id].status.is_live(), "{:?}", after[id]);
            }
            Ok(())
        })?;
    }
}
