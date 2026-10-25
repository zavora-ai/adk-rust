//! Conformance cases for [`CharterStore`] backends (spec 3.5 §7.6, §7.11, §11, A-I14,
//! A-I23).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`empty_site`] | A site with no commit has no current version, no history and no version 1 |
//! | [`commit_reads_back`] | A commit is version 1, signed with its decision, and reads back whole |
//! | [`versions_increment`] | Each commit is the next version; history is oldest first |
//! | [`append_refuses_gaps_and_repeats`] | `append` takes only the version one past the latest |
//! | [`commit_refuses_an_invalid_draft`] | A target without a baseline (A-I14) or an agent holding a role is refused; nothing is stored |
//! | [`commit_refuses_an_unsatisfied_chain`] | A decision whose chain is not filled by distinct people holding the roles is refused (A-I23) |
//! | [`the_charter_in_force_governs_a_change`] | A draft cannot loosen the chain that approves it |
//! | [`concurrent_commits_take_one_version_each`] | Concurrent commits from two handles leave a gapless history, one version per success |
//! | [`versions_survive_a_restart`] | Versions read back through a new handle (durable backends only) |

use std::sync::Arc;

use async_trait::async_trait;
use awp_types::Role;
use chrono::Utc;
use uuid::Uuid;

use super::fixtures::{charter, decision, kpi, with_chain, with_role, with_target};
use crate::KernelError;
use crate::charter::{CHARTER_ACTION, CharterStore, CharterVersion};

/// Opens a [`CharterStore`] backend for the conformance suite (spec 3.5 §11).
#[async_trait]
pub trait CharterStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn CharterStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

async fn other(
    harness: &impl CharterStoreHarness,
    first: &Arc<dyn CharterStore>,
) -> Arc<dyn CharterStore> {
    if harness.is_durable() { harness.open().await } else { Arc::clone(first) }
}

/// A site with no commit has nothing.
pub async fn empty_site(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    assert!(store.current(site).await.expect("current").is_none());
    assert!(store.history(site).await.expect("history").is_empty());
    assert!(store.get(site, 1).await.expect("get").is_none());
}

/// A commit is version 1, signed with its decision, and reads back whole.
pub async fn commit_reads_back(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let now = Utc::now();
    let draft = charter(site, &["amina"]);
    let committed = store
        .commit(draft, decision("d-1", now, &[(Role::Owner, "amina")]).with_signed_note("note-1"))
        .await
        .expect("commit");
    assert_eq!(committed.version(), 1);
    let signed = committed.charter.signed.clone().expect("the commit signs the charter");
    assert_eq!((signed.decision.as_str(), signed.principal.as_str()), ("d-1", "amina"));
    assert_eq!(signed.note, "note-1");
    let current = store.current(site).await.expect("current").expect("a version");
    assert_eq!(current, committed);
    assert_eq!(store.get(site, 1).await.expect("get"), Some(committed.clone()));
    assert_eq!(store.history(site).await.expect("history"), vec![committed]);
}

/// Each commit is the next version, and history is oldest first.
pub async fn versions_increment(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let now = Utc::now();
    for (index, mission) in ["first", "second", "third"].into_iter().enumerate() {
        let mut draft = charter(site, &["amina"]);
        draft.mission = mission.to_string();
        let committed = store
            .commit(draft, decision(&format!("d-{index}"), now, &[(Role::Owner, "amina")]))
            .await
            .expect("commit");
        assert_eq!(committed.version() as usize, index + 1);
    }
    let history = store.history(site).await.expect("history");
    let missions: Vec<(u32, &str)> =
        history.iter().map(|v| (v.version(), v.charter.mission.as_str())).collect();
    assert_eq!(missions, [(1, "first"), (2, "second"), (3, "third")]);
    assert_eq!(store.current(site).await.expect("current").map(|v| v.version()), Some(3));
    assert_eq!(
        store.get(site, 2).await.expect("get").map(|v| v.charter.mission),
        Some("second".into())
    );
}

fn version(site: Uuid, number: u32) -> CharterVersion {
    let mut charter = charter(site, &["amina"]);
    charter.version = number;
    CharterVersion::new(charter, decision(&format!("d-{number}"), Utc::now(), &[]), Utc::now())
}

/// `append` takes only the version one past the latest.
pub async fn append_refuses_gaps_and_repeats(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    assert!(!store.append(version(site, 2)).await.expect("append"), "a gap is refused");
    assert!(store.append(version(site, 1)).await.expect("append"));
    assert!(!store.append(version(site, 1)).await.expect("append"), "a repeat is refused");
    assert!(!store.append(version(site, 3)).await.expect("append"), "a gap is refused");
    assert!(store.append(version(site, 2)).await.expect("append"));
    assert_eq!(store.history(site).await.expect("history").len(), 2);
}

/// A target without a baseline, or an agent holding a role, is refused; nothing is stored.
pub async fn commit_refuses_an_invalid_draft(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let now = Utc::now();
    let mut draft = charter(site, &["amina"]);
    let mut leads = kpi("leads", "leads.qualified", "commerce");
    leads.target = with_target(leads.clone(), 10.0, 20.0, now).target;
    draft.kpis.push(leads);
    let error = store
        .commit(draft, decision("d-1", now, &[(Role::Owner, "amina")]))
        .await
        .expect_err("a target without a baseline (A-I14)");
    assert!(matches!(error, KernelError::Config { .. }), "{error}");
    let draft = with_role(charter(site, &["amina"]), Role::Editor, "urn:adk:agent:site/growth");
    let error = store
        .commit(draft, decision("d-2", now, &[(Role::Owner, "amina")]))
        .await
        .expect_err("an agent holding a role");
    assert!(matches!(error, KernelError::Config { .. }), "{error}");
    assert!(store.current(site).await.expect("current").is_none(), "nothing was stored");
}

/// A decision whose chain distinct people holding the roles did not fill is refused.
pub async fn commit_refuses_an_unsatisfied_chain(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let now = Utc::now();
    let two_owners = || {
        with_chain(charter(site, &["amina", "baraka"]), CHARTER_ACTION, &[Role::Owner, Role::Owner])
    };
    let refused = [
        decision("one", now, &[(Role::Owner, "amina")]),
        decision("same-twice", now, &[(Role::Owner, "amina"), (Role::Owner, "amina")]),
        decision("not-an-owner", now, &[(Role::Owner, "amina"), (Role::Owner, "zawadi")]),
        decision(
            "an-agent",
            now,
            &[(Role::Owner, "amina"), (Role::Owner, "urn:adk:agent:site/steward")],
        ),
        decision("nobody", now, &[]),
    ];
    for refused in refused {
        let id = refused.decision_id.clone();
        let error = store.commit(two_owners(), refused).await.expect_err(&id);
        assert!(matches!(error, KernelError::Unauthorized { .. }), "{id}: {error}");
    }
    assert!(store.current(site).await.expect("current").is_none(), "nothing was stored");
    let committed = store
        .commit(
            two_owners(),
            decision("two", now, &[(Role::Owner, "baraka"), (Role::Owner, "amina")]),
        )
        .await
        .expect("two different owners");
    assert_eq!(committed.version(), 1);
}

/// A draft cannot loosen the chain that approves it: the charter in force governs.
pub async fn the_charter_in_force_governs_a_change(harness: &impl CharterStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let now = Utc::now();
    let strict = with_chain(
        charter(site, &["amina", "baraka"]),
        CHARTER_ACTION,
        &[Role::Owner, Role::Owner],
    );
    store
        .commit(strict, decision("v1", now, &[(Role::Owner, "amina"), (Role::Owner, "baraka")]))
        .await
        .expect("version 1");
    let loose = with_chain(charter(site, &["amina", "baraka"]), CHARTER_ACTION, &[Role::Owner]);
    let error = store
        .commit(loose.clone(), decision("v2-alone", now, &[(Role::Owner, "amina")]))
        .await
        .expect_err("version 1's chain needs two owners");
    assert!(matches!(error, KernelError::Unauthorized { .. }), "{error}");
    store
        .commit(loose, decision("v2", now, &[(Role::Owner, "amina"), (Role::Owner, "baraka")]))
        .await
        .expect("both owners loosen it");
    store
        .commit(charter(site, &["amina", "baraka"]), decision("v3", now, &[(Role::Owner, "amina")]))
        .await
        .expect("version 2's chain needs one owner");
}

/// Concurrent commits from two handles leave a gapless history, one version per success.
pub async fn concurrent_commits_take_one_version_each(harness: &impl CharterStoreHarness) {
    let first = harness.open().await;
    let second = other(harness, &first).await;
    let site = Uuid::new_v4();
    let mut tasks = Vec::new();
    for index in 0..12 {
        let store = if index % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        tasks.push(tokio::spawn(async move {
            let mut draft = charter(site, &["amina"]);
            draft.mission = format!("mission {index}");
            store
                .commit(
                    draft,
                    decision(&format!("d-{index}"), Utc::now(), &[(Role::Owner, "amina")]),
                )
                .await
        }));
    }
    let mut committed = 0;
    for task in tasks {
        match task.await.expect("task") {
            Ok(_) => committed += 1,
            Err(KernelError::Core(error)) => {
                assert_eq!(error.category, adk_core::ErrorCategory::Conflict, "{error}");
            }
            Err(error) => panic!("unexpected error {error}"),
        }
    }
    assert!(committed >= 1);
    let numbers: Vec<u32> =
        first.history(site).await.expect("history").iter().map(CharterVersion::version).collect();
    assert_eq!(numbers, (1..=committed).collect::<Vec<_>>());
}

/// Versions read back through a new handle.
pub async fn versions_survive_a_restart(harness: &impl CharterStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let site = Uuid::new_v4();
    let committed = harness
        .open()
        .await
        .commit(charter(site, &["amina"]), decision("d-1", Utc::now(), &[(Role::Owner, "amina")]))
        .await
        .expect("commit");
    assert_eq!(harness.open().await.current(site).await.expect("current"), Some(committed));
}
