//! The consolidation tick's work on long-term facts through real harness runs (spec 3.2
//! §7.5, §10 HK2, §9.2 K-I12, K-I22): the distillation entry with verified-quote promotion
//! off, pass 1's supersede and expiry beside an untouched `Known` twin, pass 2's merge with
//! provenance, the loss cap, the token cap, `undo`, and a store without records.
//!
//! On `main` the distiller makes a quoted candidate `Known` and nothing supersedes a
//! near-duplicate or expires a stale assumption; the cases below show both.

#![cfg(all(feature = "runner", feature = "memory"))]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{SessionId, UsageMetadata, UserId};
use adk_harness::{
    BesideKnown, ChangeReason, ConsolidationConfig, ConsolidationRequest, FactFilter, FactKind,
    FactScope, FactSource, FactStatus, FactStore, Harness, HarnessAgent, HarnessAgentBuilder,
    HarnessMode, InMemoryFactStore, LongTermMemoryConfig, MemoryFact, Provenance, project_slug,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use chrono::Utc;
use serde_json::{Value, json};

use common::{APP, collect};

struct World {
    dir: tempfile::TempDir,
    store: Arc<dyn FactStore>,
    sessions: Arc<dyn SessionService>,
    audit: InMemoryAuditRecorder,
}

impl World {
    fn new() -> Self {
        Self::with_store(Arc::new(InMemoryFactStore::new()))
    }

    fn with_store(store: Arc<dyn FactStore>) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            store,
            sessions: Arc::new(InMemorySessionService::new()),
            audit: InMemoryAuditRecorder::new(),
        }
    }

    /// A harness over this world: a new process over the same stores.
    fn harness(&self, model: &Arc<ScriptedLlm>, distiller: &Arc<ScriptedLlm>) -> Harness {
        self.harness_with(model, distiller, |builder| builder)
    }

    /// As [`harness`](Self::harness), its agent builder passed through `configure`.
    fn harness_with(
        &self,
        model: &Arc<ScriptedLlm>,
        distiller: &Arc<ScriptedLlm>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Harness {
        let agent = Arc::new(
            configure(HarnessAgent::builder("harness"))
                .model(model.clone())
                .workspace(adk_devtools::Workspace::new(self.dir.path()))
                .project_instructions(common::no_user_file())
                .without_compaction()
                .mode(HarnessMode::Auto)
                .long_term_memory(LongTermMemoryConfig {
                    distill_model: Some(distiller.clone()),
                    distill_idle: Duration::from_secs(3600),
                    ..LongTermMemoryConfig::default()
                })
                .fact_store(self.store.clone())
                .build()
                .unwrap(),
        );
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(self.sessions.clone()),
            )
            .audit_log(Arc::new(self.audit.clone()))
            .build()
            .unwrap();
        Harness::with_runner(agent, runner).unwrap()
    }

    fn scope(&self) -> FactScope {
        FactScope::new(APP, "alice", Some(project_slug(self.dir.path())))
    }

    /// Stores a fact `age_days` old with `vector` as its embedding.
    async fn seed(
        &self,
        text: &str,
        status: FactStatus,
        age_days: i64,
        confidence: Option<f32>,
        vector: Vec<f32>,
    ) -> MemoryFact {
        let mut fact = MemoryFact::new(
            self.scope(),
            FactKind::ProjectFact,
            text,
            status,
            Provenance::new(FactSource::AgentInferred, "s0", "inv-0"),
        );
        fact.valid_from = Utc::now() - chrono::Duration::days(age_days);
        fact.confidence = confidence;
        let stored = self.store.put(fact).await.unwrap();
        let embedding = adk_harness::FactEmbedding::new("test/seeded", vector);
        self.store.put_embedding(&stored.id, embedding).await.unwrap();
        stored
    }

    /// Known facts that no pass touches, so a small change stays within the loss cap.
    async fn fillers(&self, count: usize) {
        for i in 0..count {
            self.seed(&format!("filler fact {i}"), FactStatus::Known, 1, None, vec![0.0, 1.0, 0.0])
                .await;
        }
    }

    async fn get(&self, fact: &MemoryFact) -> MemoryFact {
        self.store.get(&fact.id).await.unwrap().unwrap()
    }

    fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|record| record.action == action).collect()
    }
}

fn alice() -> UserId {
    UserId::new("alice").unwrap()
}

fn usage(tokens: i32) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: tokens - 10,
        candidates_token_count: 10,
        total_token_count: tokens,
        ..Default::default()
    }
}

/// The distiller's answer, every field present as its structured output requires.
fn distilled(mut facts: Value) -> ScriptedResponse {
    for fact in facts.as_array_mut().into_iter().flatten() {
        for field in ["quote", "supersedes"] {
            fact.as_object_mut().unwrap().entry(field).or_insert(json!(""));
        }
    }
    ScriptedResponse::text(json!({ "facts": facts }).to_string()).with_usage(usage(100))
}

/// Pass 2's answer for one cluster.
fn merge(text: &str, sources: &[&str], supersedes: &[&str]) -> ScriptedResponse {
    ScriptedResponse::text(
        json!({ "merged_text": text, "source_ids": sources, "supersedes": supersedes }).to_string(),
    )
    .with_usage(usage(100))
}

fn idle() -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new("m", Vec::<ScriptedResponse>::new()))
}

async fn send(harness: &Harness, session: &str, text: &str) {
    let session =
        harness.open_session(&alice(), Some(&SessionId::new(session).unwrap())).await.unwrap();
    collect(harness.send(&alice(), &session, text).await.unwrap()).await;
}

/// Two unit vectors at cosine `score`, in the first two dimensions of three.
fn at_cosine(score: f32) -> Vec<f32> {
    vec![score, (1.0 - score * score).sqrt(), 0.0]
}

/// HK2 and K-I12: a session cut off before the per-session distiller reached it is
/// distilled by the entry with promotion off, so the user's quoted words are `Assumed`;
/// a session the distiller already handled is skipped by its cursor. On `main` the
/// distiller makes the same quoted candidate `Known`, as session s1 shows.
#[tokio::test]
async fn the_distillation_entry_stores_a_quoted_fact_as_assumed() {
    let world = World::new();
    let quoted = |text: &str, quote: &str| {
        json!([{ "text": text, "kind": "preference", "source": "user_stated", "quote": quote,
                 "confidence": 0.9 }])
    };

    // Process 1 closes: the per-session distiller verifies the quote and the fact is Known.
    let first = world.harness(
        &Arc::new(ScriptedLlm::new("m", vec![ScriptedResponse::text("ok")])),
        &Arc::new(ScriptedLlm::new(
            "distiller",
            vec![distilled(quoted("Use pnpm, not npm", "use pnpm, not npm"))],
        )),
    );
    send(&first, "s1", "Please use pnpm, not npm.").await;
    first.close().await;

    // Process 2 is cut off before idle: its session never reaches the distiller.
    let second = world
        .harness(&Arc::new(ScriptedLlm::new("m", vec![ScriptedResponse::text("ok")])), &idle());
    send(&second, "s2", "Always run cargo fmt before you commit.").await;
    drop(second);

    // Process 3 runs the entry over both sessions.
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![distilled(quoted(
            "Run cargo fmt before committing",
            "run cargo fmt before you commit",
        ))],
    ));
    let third = world.harness(&idle(), &distiller);
    let report = third
        .distill_sessions(
            &alice(),
            &["s1".to_string(), "s2".to_string()],
            Utc::now() + chrono::Duration::hours(2),
            None,
        )
        .await
        .unwrap();
    assert_eq!(report.sessions, vec!["s2".to_string()]);
    assert_eq!(report.skipped, vec!["s1".to_string()], "s1's cursor says it was distilled");
    assert_eq!(distiller.requests().len(), 1);

    let facts = world.store.list(FactFilter::user(APP, "alice")).await.unwrap();
    let text = |needle: &str| facts.iter().find(|fact| fact.text.contains(needle)).unwrap();
    let on_main = text("pnpm");
    assert_eq!(on_main.status, FactStatus::Known, "the per-session distiller verifies quotes");
    let entry = text("cargo fmt");
    assert_eq!(report.stored, vec![entry.id.clone()]);
    assert_eq!(entry.status, FactStatus::Assumed, "promotion is off (K-I12)");
    assert_eq!(entry.provenance.source, FactSource::AgentInferred);
    assert_eq!((entry.provenance.quote.as_ref(), entry.provenance.tick.as_ref()), (None, None));
    assert_eq!(entry.provenance.session_id, "s2");

    // The audit tells a catch-up from the per-session distiller (spec 3.1 §7.33 fix 9).
    let caught_up: Vec<(Value, Value)> = world
        .records("harness.memory.distilled")
        .iter()
        .map(|record| (record.ext["sessionId"].clone(), record.ext["catchUp"].clone()))
        .collect();
    assert_eq!(caught_up, vec![(json!("s1"), json!(false)), (json!("s2"), json!(true))]);
    // Both cursors reach the sessions' last events, so a host finds nothing left unsaved.
    let mut cursors = third.distilled_sessions(&alice()).await.unwrap();
    cursors.sort();
    let ids: Vec<&str> = cursors.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["s1", "s2"]);
    for (id, upto) in &cursors {
        let session = world
            .sessions
            .get(adk_session::GetRequest {
                app_name: APP.to_string(),
                user_id: "alice".to_string(),
                session_id: id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        let last = session.events().all().iter().map(|event| event.timestamp).max().unwrap();
        assert_eq!(*upto, last, "{id}");
    }

    // A second run finds nothing new: the entry moved the cursor too.
    let again = third
        .distill_sessions(
            &alice(),
            &["s1".to_string(), "s2".to_string()],
            Utc::now() + chrono::Duration::hours(3),
            None,
        )
        .await
        .unwrap();
    assert!(again.sessions.is_empty(), "{again:?}");
    assert_eq!(distiller.requests().len(), 1);
}

/// I42 across a restart: on a served harness, the process that ran a turn takes the turn's
/// authentication with it, so a session a restart cut off is distilled only when its latest
/// durable run was authenticated. Without a run store a served harness distils neither, as
/// on `main`.
#[cfg(feature = "auth")]
#[tokio::test]
async fn a_served_harness_distils_a_cut_off_session_only_when_its_run_was_authenticated() {
    let world = World::new();
    let ok = || ScriptedResponse::text("ok");
    // Process 1 is cut off before idle: neither session reaches the distiller.
    let first = world.harness(&Arc::new(ScriptedLlm::new("m", vec![ok(), ok()])), &idle());
    send(&first, "signed", "Always run cargo fmt before you commit.").await;
    send(&first, "anonymous", "Always deploy on Fridays.").await;
    drop(first);
    let sessions = ["signed".to_string(), "anonymous".to_string()];
    let at = Utc::now() + chrono::Duration::hours(2);
    let served = |distiller: &Arc<ScriptedLlm>| {
        world.harness_with(&idle(), distiller, HarnessAgentBuilder::require_authenticated_identity)
    };

    let blind = Arc::new(ScriptedLlm::new("distiller", Vec::<ScriptedResponse>::new()));
    let report = served(&blind).distill_sessions(&alice(), &sessions, at, None).await.unwrap();
    assert_eq!((report.sessions, report.skipped), (vec![], sessions.to_vec()));
    assert!(blind.requests().is_empty());

    let runs = Arc::new(adk_core::InMemoryRunStore::new());
    for (session, authenticated) in [("signed", true), ("anonymous", false)] {
        let mut run = adk_core::NewRun::new("interactive", APP, "alice", APP, json!({}))
            .with_session_id(session);
        run.authenticated = authenticated;
        adk_core::DurableRunStore::enqueue(runs.as_ref(), run).await.unwrap();
    }
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![distilled(json!([{ "text": "Run cargo fmt before committing",
                                "kind": "project_fact", "source": "user_stated",
                                "confidence": 0.9 }]))],
    ));
    let report = served(&distiller)
        .with_run_store(runs)
        .distill_sessions(&alice(), &sessions, at, None)
        .await
        .unwrap();
    assert_eq!(
        (report.sessions, report.skipped),
        (vec!["signed".to_string()], vec!["anonymous".to_string()])
    );
    assert_eq!(distiller.requests().len(), 1);
    let stored = world.store.get(&report.stored[0]).await.unwrap().unwrap();
    assert_eq!(
        (stored.status, stored.provenance.session_id.as_str()),
        (FactStatus::Assumed, "signed")
    );
}

/// K-I22: an `Assumed` fact that names a `Known` one in `supersedes` is stored beside it,
/// listed for a person to confirm; the `Known` fact is untouched. On `main` the per-session
/// distiller refuses such a candidate.
#[tokio::test]
async fn an_assumed_fact_naming_a_known_one_is_stored_beside_it() {
    let world = World::new();
    let known =
        world.seed("Deploy from the release branch", FactStatus::Known, 3, None, vec![]).await;
    let cut_off = world
        .harness(&Arc::new(ScriptedLlm::new("m", vec![ScriptedResponse::text("ok")])), &idle());
    send(&cut_off, "s1", "We moved deploys to main last week.").await;
    drop(cut_off);
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![distilled(json!([{ "text": "Deploy from main", "kind": "decision",
            "source": "user_stated", "quote": "moved deploys to main", "confidence": 0.8,
            "supersedes": known.id }]))],
    ));
    let harness = world.harness(&idle(), &distiller);
    let record = harness
        .consolidate_memory(
            &alice(),
            ConsolidationRequest::new()
                .with_sessions(["s1"])
                .at(Utc::now() + chrono::Duration::hours(2)),
        )
        .await
        .unwrap();
    assert_eq!(world.get(&known).await, known, "the known fact is untouched");
    assert_eq!(record.distilled.len(), 1);
    let beside = world.store.get(&record.distilled[0]).await.unwrap().unwrap();
    assert_eq!(beside.status, FactStatus::Assumed);
    assert_eq!(beside.provenance.tick.as_deref(), Some(record.tick_id.as_str()));
    assert_eq!(
        record.beside_known,
        vec![BesideKnown { fact: beside.id.clone(), known: known.id.clone() }]
    );
    assert!(record.changes.is_empty(), "{:?}", record.changes);
    let audited = world.records("harness.memory.consolidated");
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0].ext["besideKnown"][0]["known"], json!(known.id));
}

/// Pass 1 (§7.5): the older of two `Assumed` near-duplicates is superseded by the newer and
/// an unused, low-confidence, stale `Assumed` fact is forgotten, history kept, without a
/// model call; a `Known` twin of the duplicates is untouched (K-I22). One
/// `harness.memory.consolidated` record carries ids and counts, never text.
#[tokio::test]
async fn pass_one_supersedes_and_expires_while_a_known_twin_stays() {
    let world = World::new();
    let older = world
        .seed(
            "the tests need the sqlite feature",
            FactStatus::Assumed,
            5,
            Some(0.7),
            at_cosine(1.0),
        )
        .await;
    let newer = world
        .seed("tests need --features sqlite", FactStatus::Assumed, 1, Some(0.7), at_cosine(0.95))
        .await;
    let twin = world
        .seed(
            "The suite runs only with the sqlite feature",
            FactStatus::Known,
            3,
            None,
            at_cosine(1.0),
        )
        .await;
    let stale = world
        .seed(
            "the cache lives in /tmp/build",
            FactStatus::Assumed,
            100,
            Some(0.3),
            vec![0.0, 0.0, 1.0],
        )
        .await;
    world.fillers(4).await;
    let distiller = idle();
    let harness = world.harness(&idle(), &distiller);

    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();

    assert!(distiller.requests().is_empty(), "pass 1 makes no model call");
    let superseded = world.get(&older).await;
    assert_eq!(superseded.status, FactStatus::Superseded { by: newer.id.clone() });
    assert!(superseded.valid_to.is_some(), "history kept");
    assert_eq!(world.get(&newer).await.status, FactStatus::Assumed);
    assert_eq!(world.get(&twin).await, twin, "the known twin is untouched");
    let expired = world.get(&stale).await;
    assert_eq!(expired.status, FactStatus::Forgotten);
    assert!(expired.valid_to.is_some());
    assert_eq!(
        record
            .changes
            .iter()
            .map(|change| (change.fact.clone(), change.reason))
            .collect::<Vec<_>>(),
        vec![
            (stale.id.clone(), ChangeReason::Expired),
            (older.id.clone(), ChangeReason::Duplicate)
        ]
    );
    assert_eq!(record.refused, None);
    assert_eq!(harness.consolidations(&alice()).await.unwrap(), vec![record.clone()]);

    let audited = world.records("harness.memory.consolidated");
    assert_eq!(audited.len(), 1, "one record per tick");
    let ext = &audited[0].ext;
    assert_eq!(ext["tickId"], json!(record.tick_id));
    assert_eq!(ext["superseded"], json!([older.id]));
    assert_eq!(ext["forgotten"], json!([stale.id]));
    assert_eq!(
        ext["counts"],
        json!({ "superseded": 1, "forgotten": 1, "merged": 0, "distilled": 0 })
    );
    let text = serde_json::to_string(ext).unwrap();
    assert!(!text.contains("sqlite") && !text.contains("/tmp/build"), "no fact text: {text}");
    let forgotten = world.records("harness.memory.fact.forgotten");
    assert_eq!(forgotten[0].ext["reason"], json!("expired"));
    assert_eq!(forgotten[0].ext["tickId"], json!(record.tick_id));
}

/// Pass 2 (§7.5): a cluster in the 0.80–0.92 band is merged on the distiller model at the
/// `distiller` effort; the merged fact is `Assumed`, source `AgentInferred`, its provenance
/// listing the sources, and the sources become `Superseded` by it. A `Known` fact the
/// answer names is not touched (K-I22).
#[tokio::test]
async fn pass_two_merges_a_cluster_with_provenance() {
    let world = World::new();
    let a = world
        .seed("build with make release", FactStatus::Assumed, 2, Some(0.6), at_cosine(1.0))
        .await;
    world.store.record_recall(std::slice::from_ref(&a.id), Utc::now()).await.unwrap();
    let b = world
        .seed("the release build runs make", FactStatus::Assumed, 1, Some(0.8), at_cosine(0.85))
        .await;
    let known =
        world.seed("ci builds on linux", FactStatus::Known, 1, None, vec![0.0, 0.0, 1.0]).await;
    world.fillers(6).await;
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![merge(
            "Release builds run `make release`",
            &[&a.id, &b.id, &known.id],
            &[&a.id, &b.id, &known.id],
        )],
    ));
    let harness = world.harness(&idle(), &distiller);

    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();

    assert_eq!(distiller.requests().len(), 1);
    let request = &distiller.requests()[0];
    assert_eq!(
        request.config.as_ref().and_then(|config| config.effort),
        Some(adk_core::Effort::Low)
    );
    assert_eq!(record.merged.len(), 1);
    let merged = world.store.get(&record.merged[0]).await.unwrap().unwrap();
    assert_eq!(merged.status, FactStatus::Assumed);
    assert_eq!(merged.provenance.source, FactSource::AgentInferred);
    assert_eq!(merged.provenance.source_ids, vec![a.id.clone(), b.id.clone()]);
    assert_eq!(merged.provenance.tick.as_deref(), Some(record.tick_id.as_str()));
    assert_eq!(
        (merged.kind, merged.confidence, merged.uses),
        (FactKind::ProjectFact, Some(0.8), 1)
    );
    for source in [&a, &b] {
        assert_eq!(
            world.get(source).await.status,
            FactStatus::Superseded { by: merged.id.clone() }
        );
    }
    assert_eq!(world.get(&known).await, known);
    assert_eq!(record.superseded().count(), 2);
    assert!(record.changes.iter().all(|change| change.reason == ChangeReason::Merged));
}

/// The loss cap (§7.5): passes that would change more than `max_loss_fraction` of a
/// scope's live facts apply nothing, and the record says why.
#[tokio::test]
async fn a_tick_over_the_loss_cap_applies_nothing() {
    let world = World::new();
    let mut seeded = Vec::new();
    for (text, vector) in [
        ("alpha one", at_cosine(1.0)),
        ("alpha two", at_cosine(0.99)),
        ("beta one", vec![0.0, 0.0, 1.0]),
        ("beta two", vec![0.0, 0.1, 0.995]),
    ] {
        seeded.push(world.seed(text, FactStatus::Assumed, 1, Some(0.9), vector).await);
    }
    let harness = world.harness(&idle(), &idle());
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert!(record.refused.as_deref().is_some_and(|why| why.contains("2 of the 4")), "{record:?}");
    assert!(record.changes.is_empty());
    for fact in &seeded {
        assert_eq!(world.get(fact).await, *fact);
    }
    assert_eq!(world.records("harness.memory.consolidated")[0].ext["refused"], json!(true));

    // The same tick under a looser cap applies both.
    let record = harness
        .consolidate_memory(
            &alice(),
            ConsolidationRequest::new()
                .with_config(ConsolidationConfig { max_loss_fraction: 0.5, ..Default::default() }),
        )
        .await
        .unwrap();
    assert_eq!((record.refused.as_deref(), record.superseded().count()), (None, 2));
}

/// The loss cap is `max(1, ⌊max_loss_fraction × live⌋)` lost facts per scope: a new user's
/// two-fact scope collapses its one duplicate pair, while a ten-fact scope, whose share is
/// two, still refuses three pass-1 supersessions whole.
#[tokio::test]
async fn the_loss_cap_lets_a_small_scope_collapse_one_duplicate() {
    // Pair `i` lies in dimensions 2i and 2i+1 at cosine 0.95, orthogonal to every other pair.
    let pair = |i: usize| {
        let mut older = vec![0.0; 6];
        let mut newer = vec![0.0; 6];
        older[2 * i] = 1.0;
        newer[2 * i] = 0.95;
        newer[2 * i + 1] = (1.0_f32 - 0.95 * 0.95).sqrt();
        (older, newer)
    };

    let small = World::new();
    let (older, newer) = pair(0);
    let first = small.seed("Use pnpm, not npm", FactStatus::Assumed, 2, Some(0.9), older).await;
    let second =
        small.seed("We use pnpm, never npm", FactStatus::Assumed, 1, Some(0.9), newer).await;
    let harness = small.harness(&idle(), &idle());
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert_eq!((record.refused.as_deref(), record.superseded().count()), (None, 1), "{record:?}");
    assert_eq!(small.get(&first).await.status, FactStatus::Superseded { by: second.id.clone() });

    let large = World::new();
    let mut seeded = Vec::new();
    for i in 0..3 {
        let (older, newer) = pair(i);
        seeded
            .push(large.seed(&format!("rule {i}"), FactStatus::Assumed, 2, Some(0.9), older).await);
        seeded.push(
            large.seed(&format!("rule {i} again"), FactStatus::Assumed, 1, Some(0.9), newer).await,
        );
    }
    large.fillers(4).await;
    let harness = large.harness(&idle(), &idle());
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert!(record.refused.as_deref().is_some_and(|why| why.contains("3 of the 10")), "{record:?}");
    assert!(record.changes.is_empty());
    for fact in &seeded {
        assert_eq!(large.get(fact).await, *fact);
    }
}

/// A merge loses the facts it supersedes less the one it writes: a two-fact scope merges
/// its one similar pair, while a ten-fact scope whose four-fact cluster would lose three,
/// more than its share of two, refuses the merge whole.
#[tokio::test]
async fn a_merge_counts_the_facts_it_loses_against_the_loss_cap() {
    let small = World::new();
    let a = small
        .seed("build with make release", FactStatus::Assumed, 2, Some(0.9), at_cosine(1.0))
        .await;
    let b = small
        .seed("the release build runs make", FactStatus::Assumed, 1, Some(0.9), at_cosine(0.85))
        .await;
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![merge("Release builds run `make release`", &[&a.id, &b.id], &[&a.id, &b.id])],
    ));
    let harness = small.harness(&idle(), &distiller);
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert_eq!((record.refused.as_deref(), record.merged.len()), (None, 1), "{record:?}");
    for source in [&a, &b] {
        assert_eq!(
            small.get(source).await.status,
            FactStatus::Superseded { by: record.merged[0].clone() }
        );
    }

    // A head and three older facts at cosine 0.85 to it and 0.72 to each other: one
    // cluster, no pass-1 duplicate.
    let large = World::new();
    let spoke = |axis: usize| {
        let mut vector = vec![0.85, 0.0, 0.0, 0.0];
        vector[axis] = (1.0_f32 - 0.85 * 0.85).sqrt();
        vector
    };
    let mut cluster = vec![
        large
            .seed(
                "deploys go out on fridays",
                FactStatus::Assumed,
                1,
                Some(0.9),
                vec![1.0, 0.0, 0.0, 0.0],
            )
            .await,
    ];
    for axis in 1..4 {
        cluster.push(
            large
                .seed(
                    &format!("deploy rule {axis}"),
                    FactStatus::Assumed,
                    2,
                    Some(0.9),
                    spoke(axis),
                )
                .await,
        );
    }
    large.fillers(6).await;
    let ids: Vec<&str> = cluster.iter().map(|fact| fact.id.as_str()).collect();
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![merge("Deploys go out on Fridays", &ids, &ids)],
    ));
    let harness = large.harness(&idle(), &distiller);
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert_eq!(distiller.requests().len(), 1, "the cluster went to the model");
    assert!(
        record.refused.as_deref().is_some_and(|why| why.contains("lose 3 of the 10")),
        "{record:?}"
    );
    assert!(record.changes.is_empty() && record.merged.is_empty());
    for fact in &cluster {
        assert_eq!(large.get(fact).await, *fact);
    }
}

/// `max_tokens_per_tick` (§7.5): once the tick's tokens are spent, pass 2 sends no more
/// clusters.
#[tokio::test]
async fn the_token_cap_stops_pass_two() {
    let world = World::new();
    let a = world.seed("alpha one", FactStatus::Assumed, 2, Some(0.9), at_cosine(1.0)).await;
    let b = world.seed("alpha two", FactStatus::Assumed, 1, Some(0.9), at_cosine(0.85)).await;
    // Older than the alpha pair, so the alpha cluster goes to the model first.
    world.seed("beta one", FactStatus::Assumed, 3, Some(0.9), vec![0.0, 0.0, 1.0]).await;
    world.seed("beta two", FactStatus::Assumed, 2, Some(0.9), vec![0.0, 0.53, 0.85]).await;
    world.fillers(8).await;
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![merge("alpha", &[&b.id, &a.id], &[&b.id, &a.id]), merge("beta", &[], &[])],
    ));
    let harness = world.harness(&idle(), &distiller);
    let config = ConsolidationConfig { max_tokens_per_tick: 50, ..Default::default() };
    let record = harness
        .consolidate_memory(&alice(), ConsolidationRequest::new().with_config(config))
        .await
        .unwrap();
    assert_eq!(distiller.requests().len(), 1, "the second cluster is never sent");
    assert_eq!((record.model_calls, record.total_tokens), (1, 100));
    assert_eq!(record.merged.len(), 1, "the answer that arrived is applied");
}

/// `undo` (§7.5): the statuses before the tick come back, the facts it wrote are forgotten
/// with their history kept, and a tick is undone once.
#[tokio::test]
async fn undo_restores_the_statuses_a_tick_changed() {
    let world = World::new();
    let a = world
        .seed("build with make release", FactStatus::Assumed, 2, Some(0.6), at_cosine(1.0))
        .await;
    let b = world
        .seed("the release build runs make", FactStatus::Assumed, 1, Some(0.8), at_cosine(0.85))
        .await;
    let stale = world
        .seed("old cache path", FactStatus::Assumed, 120, Some(0.2), vec![0.0, 0.0, 1.0])
        .await;
    world.fillers(9).await;
    let before = world.store.list(FactFilter::user(APP, "alice")).await.unwrap();
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![merge("Release builds run make release", &[&a.id, &b.id], &[&a.id, &b.id])],
    ));
    let harness = world.harness(&idle(), &distiller);
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert_eq!((record.superseded().count(), record.forgotten().count()), (2, 1));

    let report = harness.undo_consolidation(&alice(), &record.tick_id[..12]).await.unwrap();
    assert_eq!(report.restored.len(), 3);
    assert_eq!(report.forgotten, record.merged);
    for fact in &before {
        let now = world.get(fact).await;
        assert_eq!(
            (now.status, now.valid_to),
            (fact.status.clone(), fact.valid_to),
            "{}",
            fact.text
        );
    }
    let merged = world.store.get(&record.merged[0]).await.unwrap().unwrap();
    assert_eq!(merged.status, FactStatus::Forgotten, "history kept");
    assert_eq!(world.get(&stale).await.status, FactStatus::Assumed);
    let again = harness.undo_consolidation(&alice(), &record.tick_id).await.unwrap_err();
    assert_eq!(again.code, "harness.memory.tick_undone");
    let undone = harness.consolidations(&alice()).await.unwrap();
    assert!(undone[0].undone_at.is_some());
    let missing = harness.undo_consolidation(&alice(), "nope").await.unwrap_err();
    assert_eq!(missing.code, "harness.memory.tick_not_found");

    // A tick still running, or cut off before it finished, is not undone: what it changes
    // after the undo would escape it.
    let running =
        adk_harness::ConsolidationRecord::new("01JTRUNNING", APP, "alice", "default", Utc::now());
    world.store.put_consolidation(&running).await.unwrap();
    let unfinished = harness.undo_consolidation(&alice(), "01JTRUNNING").await.unwrap_err();
    assert_eq!(unfinished.code, "harness.memory.tick_unfinished");
    assert_eq!(world.store.consolidations(APP, "alice").await.unwrap()[0].undone_at, None);
}

/// A store that keeps no consolidation records refuses the tick before it changes anything.
#[tokio::test]
async fn a_store_without_records_refuses_before_any_change() {
    struct Plain(InMemoryFactStore);

    #[async_trait::async_trait]
    impl FactStore for Plain {
        async fn put(&self, fact: MemoryFact) -> adk_core::Result<MemoryFact> {
            self.0.put(fact).await
        }
        async fn get(&self, id: &str) -> adk_core::Result<Option<MemoryFact>> {
            self.0.get(id).await
        }
        async fn list(&self, filter: FactFilter) -> adk_core::Result<Vec<MemoryFact>> {
            self.0.list(filter).await
        }
        async fn set_status(
            &self,
            id: &str,
            status: FactStatus,
            by: &SubjectRef,
        ) -> adk_core::Result<()> {
            self.0.set_status(id, status, by).await
        }
        async fn put_embedding(
            &self,
            id: &str,
            embedding: adk_harness::FactEmbedding,
        ) -> adk_core::Result<()> {
            self.0.put_embedding(id, embedding).await
        }
        async fn embeddings(
            &self,
            scope: &FactScope,
        ) -> adk_core::Result<Vec<(String, adk_harness::FactEmbedding)>> {
            self.0.embeddings(scope).await
        }
    }

    let world = World::with_store(Arc::new(Plain(InMemoryFactStore::new())));
    let older = world.seed("alpha one", FactStatus::Assumed, 2, Some(0.9), at_cosine(1.0)).await;
    world.seed("alpha two", FactStatus::Assumed, 1, Some(0.9), at_cosine(0.99)).await;
    world.fillers(8).await;
    let harness = world.harness(&idle(), &idle());
    let error =
        harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap_err();
    assert_eq!(error.code, "harness.memory.consolidation_unsupported");
    assert_eq!(world.get(&older).await, older);
}

/// Spec 3.3 §12 phase 3, "Correct me, then check", on SQLite, where `adk-rust up` keeps facts
/// (`harness.db`): an overnight tidy-up merges two notes and Undo restores both; a confirmed
/// guess is known; a forgotten note is gone from the next turn while the others are still
/// recalled. The kernel's memory routes make these same calls (`adk-cli`'s
/// `HarnessMemoryBook`).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn correct_me_then_check_on_sqlite() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let store = adk_harness::SqliteFactStore::new(pool);
    store.migrate().await.unwrap();
    let world = World::with_store(Arc::new(store));
    let a = world
        .seed("build with make release", FactStatus::Assumed, 2, Some(0.6), at_cosine(1.0))
        .await;
    let b = world
        .seed("the release build runs make", FactStatus::Assumed, 1, Some(0.8), at_cosine(0.85))
        .await;
    let guess = world
        .seed(
            "tests run with cargo nextest",
            FactStatus::Assumed,
            1,
            Some(0.7),
            vec![0.0, 0.0, 1.0],
        )
        .await;
    world.fillers(9).await;
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![merge("Release builds run make release", &[&a.id, &b.id], &[&a.id, &b.id])],
    ));
    let model = Arc::new(ScriptedLlm::new("m", vec![ScriptedResponse::text("ok")]));
    let harness = world.harness(&model, &distiller);

    // The tidy-up merges the two notes into one.
    let record = harness.consolidate_memory(&alice(), ConsolidationRequest::new()).await.unwrap();
    assert_eq!(record.merged.len(), 1);
    for note in [&a, &b] {
        assert!(!world.get(note).await.status.is_live(), "{} was merged", note.text);
    }

    // Undo restores both, and forgets the merged note with its history kept.
    harness.undo_consolidation(&alice(), &record.tick_id).await.unwrap();
    assert_eq!(
        (world.get(&a).await.status, world.get(&b).await.status),
        (FactStatus::Assumed, FactStatus::Assumed)
    );
    let merged = world.store.get(&record.merged[0]).await.unwrap().unwrap();
    assert_eq!(merged.status, FactStatus::Forgotten);

    // That's right: the guess is known, as the person confirmed it.
    let confirmed = harness.accept_fact(&alice(), &guess.id).await.unwrap();
    assert_eq!(
        (confirmed.status, confirmed.provenance.source),
        (FactStatus::Known, FactSource::UserConfirmed)
    );

    // Forget it: gone from the next turn, while the note it was merged with, which the same
    // words recall, still is.
    harness.forget_fact(&alice(), &a.id).await.unwrap();
    send(&harness, "s1", "the release build").await;
    let request = model.requests().pop().expect("the next turn calls the model");
    let texts: Vec<&str> = request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(adk_core::Part::text)
        .collect();
    let block = texts
        .iter()
        .find(|text| text.starts_with("<memory-facts>"))
        .unwrap_or_else(|| panic!("the next turn recalls what is kept: {texts:?}"))
        .to_string();
    assert!(!block.contains(&a.id), "a forgotten note is not recalled: {block}");
    assert!(block.contains(&b.id), "{block}");
}
