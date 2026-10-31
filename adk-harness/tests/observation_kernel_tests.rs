//! A `/proc` observation answered through the harness (spec 3.2 §7.10, spec 3.1 §7.15): the
//! harness is its partition's audience, so an observation is a question for its user. In an
//! Auto partition the question stays open and no run starts; a person's confirmation is
//! remembered as one `Known` fact whose source is the person, and a rejection remembers
//! nothing.

#![cfg(all(feature = "kernel", feature = "memory"))]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, InMemoryRunStore, RunFilter, SubjectRef};
use adk_harness::{
    FactKind, FactSource, FactStatus, HarnessMode, InMemoryFactStore, LongTermMemoryConfig,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    AgendaItem, AgendaOrigin, AgendaState, KernelConfig, KernelError, PartitionId, PartitionMode,
    ProposalOutcome, Wake, WakeSet,
};
use adk_server::background::DurableWorkerConfig;
use common::*;

const QUESTION: &str = "Your routine has changed over the last two weeks: the day's last \
                        activity has been later than usual. Would you like to review what changed?";

const STATEMENT: &str =
    "Since 2026-10-13, compared with the 28 days before: the day's last activity has been later.";

fn observation(id: &str) -> AgendaItem {
    AgendaItem::new(
        id,
        SubjectRef::human("alice"),
        QUESTION,
        STATEMENT,
        AgendaOrigin::Observation { id: id.into() },
        chrono::Utc::now(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_confirmed_observation_is_one_known_fact_and_a_rejected_one_is_none() {
    let store = Arc::new(InMemoryFactStore::new());
    let Fixture { dir: _dir, harness, user, .. } = fixture(Vec::new(), |builder| {
        builder
            .mode(HarnessMode::Auto)
            .long_term_memory(LongTermMemoryConfig::default())
            .fact_store(store.clone())
    })
    .await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let harness = harness.with_run_store(Arc::clone(&runs));
    let audience = harness.observation_audience(&user).unwrap();
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(DurableWorkerConfig::default().with_worker_id("k1"))
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .observation_audience(PartitionId::default(), Arc::new(audience))
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let agenda = kernel.agenda();

    // Auto: the question waits for a person, open, and no run starts.
    for id in ["obs-20261022-aaaaaaaa", "obs-20261029-aaaaaaaa"] {
        let outcome = agenda.propose(observation(id), PartitionMode::Auto).await.unwrap();
        assert!(
            matches!(
                outcome,
                ProposalOutcome::Held { .. } | ProposalOutcome::AwaitingApproval { .. }
            ),
            "{outcome:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        agenda.get("obs-20261022-aaaaaaaa").await.unwrap().unwrap().state,
        AgendaState::Proposed
    );
    assert!(runs.list(RunFilter::default()).await.unwrap().is_empty(), "no run starts");
    let error = agenda
        .schedule(
            "obs-20261022-aaaaaaaa",
            WakeSet::any_of([Wake::Manual]),
            &SubjectRef::human("alice"),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::AgendaState { .. }), "{error}");
    assert!(harness.facts(&user).await.unwrap().is_empty());

    // Confirmed by alice: one Known fact, hers, and the item closed.
    let confirmed =
        agenda.approve("obs-20261022-aaaaaaaa", &SubjectRef::human("alice")).await.unwrap();
    assert_eq!(confirmed.state, AgendaState::Done);
    let facts = harness.facts(&user).await.unwrap();
    assert_eq!(facts.len(), 1);
    let fact = &facts[0];
    assert_eq!(
        (fact.text.as_str(), &fact.kind, &fact.status, &fact.provenance.source),
        (STATEMENT, &FactKind::Preference, &FactStatus::Known, &FactSource::UserConfirmed)
    );
    assert_eq!(fact.scope.project, None, "a routine is user-wide");
    let answer = confirmed.answer.unwrap();
    assert!(answer.confirmed);
    assert_eq!((answer.by, answer.fact), (SubjectRef::human("alice"), Some(fact.id.clone())));

    // Rejected: closed, and nothing more is remembered.
    let rejected = agenda
        .reject("obs-20261029-aaaaaaaa", &SubjectRef::human("alice"), Some("a one-off"))
        .await
        .unwrap();
    assert_eq!(rejected.state, AgendaState::Rejected);
    assert!(!rejected.answer.unwrap().confirmed);
    assert_eq!(harness.facts(&user).await.unwrap().len(), 1, "a rejection writes no fact");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(runs.list(RunFilter::default()).await.unwrap().is_empty(), "no run ever started");
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn confirming_what_is_already_assumed_promotes_it_instead_of_storing_it_twice() {
    use adk_harness::{FactScope, FactStore, MemoryFact, Provenance};

    let store = Arc::new(InMemoryFactStore::new());
    let Fixture { dir: _dir, harness, user, .. } = fixture(Vec::new(), |builder| {
        builder.long_term_memory(LongTermMemoryConfig::default()).fact_store(store.clone())
    })
    .await;
    let assumed = store
        .put(MemoryFact::new(
            FactScope::new(APP, "alice", None),
            FactKind::Preference,
            STATEMENT,
            FactStatus::Assumed,
            Provenance::new(FactSource::AgentInferred, "s0", "inv-0"),
        ))
        .await
        .unwrap();
    let audience = harness.observation_audience(&user).unwrap();
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .observation_audience(PartitionId::default(), Arc::new(audience))
        .build()
        .unwrap();
    kernel.agenda().propose(observation("obs-1"), PartitionMode::Ask).await.unwrap();
    let confirmed = kernel.agenda().approve("obs-1", &SubjectRef::human("alice")).await.unwrap();
    assert_eq!(confirmed.answer.unwrap().fact, Some(assumed.id.clone()));
    let facts = harness.facts(&user).await.unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!((facts[0].id.as_str(), &facts[0].status), (assumed.id.as_str(), &FactStatus::Known));
}
