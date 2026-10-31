//! Conformance cases for [`SubjectRegistry`] backends.
//!
//! | Case | Contract | Backend primitive |
//! |------|----------|-------------------|
//! | [`ensure_registers_once`] | `ensure` returns one record per subject, stamped with the store's clock, across handles and a restart; kinds are separate subjects | Create-if-absent keyed by kind and id |
//! | [`concurrent_ensure_has_one_pseudonym`] | 32 concurrent `ensure` calls across two handles return one pseudonym | Atomic create-if-absent |
//! | [`pseudonyms_resolve`] | Pseudonyms are unique and `by_pseudonym` finds their subject | Pseudonym index |
//! | [`links_once_per_visitor`] | A visitor id links to one identified subject once; a second link, to anyone, fails with `subject.link.consumed`; a non-visitor or a visitor target fails with `subject.link.invalid` | Compare-and-set on the link |
//! | [`concurrent_links_have_one_winner`] | 16 concurrent links of one visitor id to different subjects succeed exactly once | Atomic compare-and-set |
//! | [`erasing_blocks_links`] | A subject being erased can neither link nor be linked to | State check inside the link write |
//! | [`remove_forgets_the_subject`] | `remove` drops the record, the pseudonym, and the link; a later `ensure` issues a new pseudonym | Delete by key and index |

use std::sync::Arc;

use async_trait::async_trait;

use super::{base_time, unique_unicode};
use crate::principal::SubjectRef;
use crate::subject::{
    PSEUDONYM_PREFIX, SUBJECT_ERASING_CODE, SUBJECT_LINK_CONSUMED_CODE, SUBJECT_LINK_INVALID_CODE,
    SubjectRecord, SubjectRegistry, SubjectState,
};
use crate::{Clock, ManualClock};

/// Opens a [`SubjectRegistry`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::SubjectRegistryHarness;
/// use adk_core::subject::{InMemorySubjectRegistry, SubjectRegistry};
/// use adk_core::{Clock, async_trait};
///
/// #[derive(Default)]
/// struct InMemory(InMemorySubjectRegistry);
///
/// #[async_trait]
/// impl SubjectRegistryHarness for InMemory {
///     async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn SubjectRegistry> {
///         Arc::new(self.0.clone().with_clock(clock))
///     }
/// }
/// ```
#[async_trait]
pub trait SubjectRegistryHarness: Send + Sync {
    /// Opens a registry that stamps times from `clock`.
    ///
    /// Every call returns a new handle over the same stored data, as a restarted process
    /// or a second process would see it. Panics when the backend cannot be opened.
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn SubjectRegistry>;
}

fn visitor() -> SubjectRef {
    SubjectRef::visitor(unique_unicode("visitor"))
}

fn person() -> SubjectRef {
    SubjectRef::human(unique_unicode("person"))
}

#[track_caller]
fn assert_code(result: crate::Result<SubjectRecord>, code: &str, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.code, code, "{context}: {error}");
}

/// `ensure` registers a subject once, stamped with the store's clock, and the record reads
/// back unchanged through a second handle. A human and a service with one id are separate
/// subjects.
pub async fn ensure_registers_once<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(base_time()));
    let registry = harness.open(Arc::clone(&clock)).await;
    let alice = person();
    assert_eq!(registry.get(&alice).await.expect("get"), None);

    let record = registry.ensure(&alice).await.expect("ensure");
    assert!(record.pseudonym.starts_with(PSEUDONYM_PREFIX), "{}", record.pseudonym);
    let expected = SubjectRecord {
        subject: alice.clone(),
        pseudonym: record.pseudonym.clone(),
        state: SubjectState::Active,
        linked_to: None,
        linked_at: None,
        created_at: base_time(),
    };
    assert_eq!(record, expected);
    assert_eq!(registry.ensure(&alice).await.expect("ensure again"), expected);

    let reopened = harness.open(clock).await;
    assert_eq!(reopened.get(&alice).await.expect("get"), Some(expected.clone()));

    let service = SubjectRef::service(alice.id.clone());
    let other = reopened.ensure(&service).await.expect("ensure service");
    assert_ne!(other.pseudonym, expected.pseudonym, "kinds are separate subjects");
    assert_eq!(reopened.get(&alice).await.expect("get"), Some(expected));
}

/// Concurrent `ensure` calls for one subject across two handles return one pseudonym.
pub async fn concurrent_ensure_has_one_pseudonym<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(base_time()));
    let first = harness.open(Arc::clone(&clock)).await;
    let second = harness.open(clock).await;
    let subject = visitor();
    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let registry = if i % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
            let subject = subject.clone();
            tokio::spawn(async move { registry.ensure(&subject).await })
        })
        .collect();
    let mut pseudonyms = std::collections::BTreeSet::new();
    for task in tasks {
        pseudonyms.insert(task.await.expect("task").expect("ensure").pseudonym);
    }
    assert_eq!(pseudonyms.len(), 1, "one pseudonym per subject: {pseudonyms:?}");
}

/// Pseudonyms are unique, and `by_pseudonym` finds the subject of each.
pub async fn pseudonyms_resolve<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let registry = harness.open(Arc::new(ManualClock::new(base_time()))).await;
    let (a, b) = (person(), visitor());
    let a_record = registry.ensure(&a).await.expect("ensure a");
    let b_record = registry.ensure(&b).await.expect("ensure b");
    assert_ne!(a_record.pseudonym, b_record.pseudonym);
    assert_eq!(registry.by_pseudonym(&a_record.pseudonym).await.expect("resolve"), Some(a));
    assert_eq!(registry.by_pseudonym(&b_record.pseudonym).await.expect("resolve"), Some(b));
    assert_eq!(registry.by_pseudonym("psn:unknown").await.expect("resolve"), None);
}

/// A visitor id links to one identified subject, once, stamped with the store's clock.
/// Links must join a visitor to a subject that is not a visitor.
pub async fn links_once_per_visitor<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let registry = harness.open(Arc::new(clock.clone())).await;
    let (v1, v2, alice, bob) = (visitor(), visitor(), person(), person());

    clock.advance(std::time::Duration::from_secs(30));
    let record = registry.link(&v1, &alice).await.expect("link");
    assert_eq!(record.subject, v1);
    assert_eq!(record.linked_to, Some(alice.clone()));
    assert_eq!(record.linked_at, Some(clock.now()));
    assert!(
        registry.get(&alice).await.expect("get").is_some(),
        "the identified subject is registered"
    );

    assert_code(registry.link(&v1, &bob).await, SUBJECT_LINK_CONSUMED_CODE, "relink to another");
    assert_code(registry.link(&v1, &alice).await, SUBJECT_LINK_CONSUMED_CODE, "relink to the same");
    assert_code(registry.link(&alice, &bob).await, SUBJECT_LINK_INVALID_CODE, "human as visitor");
    assert_code(registry.link(&v2, &v1).await, SUBJECT_LINK_INVALID_CODE, "visitor as target");
    assert_eq!(registry.get(&v2).await.expect("get"), None, "a refused link registers nothing");

    registry.link(&v2, &alice).await.expect("second visitor");
    let mut linked = registry.linked(&alice).await.expect("linked");
    linked.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expected = vec![v1, v2];
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(linked, expected);
    assert_eq!(registry.linked(&bob).await.expect("linked"), Vec::<SubjectRef>::new());
}

/// Concurrent links of one visitor id to different subjects, across two handles, succeed
/// exactly once.
pub async fn concurrent_links_have_one_winner<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(base_time()));
    let first = harness.open(Arc::clone(&clock)).await;
    let second = harness.open(clock).await;
    let shared = visitor();
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let registry = if i % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
            let (shared, target) = (shared.clone(), person());
            tokio::spawn(async move { registry.link(&shared, &target).await })
        })
        .collect();
    let mut winners = 0;
    for task in tasks {
        match task.await.expect("task") {
            Ok(_) => winners += 1,
            Err(error) => assert_eq!(error.code, SUBJECT_LINK_CONSUMED_CODE, "{error}"),
        }
    }
    assert_eq!(winners, 1, "a visitor id links once");
}

/// A subject being erased can neither link nor be linked to, and its state reads back
/// through a second handle.
pub async fn erasing_blocks_links<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(base_time()));
    let registry = harness.open(Arc::clone(&clock)).await;
    let (v1, v2, alice) = (visitor(), visitor(), person());

    let erasing = registry.set_state(&alice, SubjectState::Erasing).await.expect("set state");
    assert_eq!(erasing.state, SubjectState::Erasing);
    let reopened = harness.open(clock).await;
    assert_eq!(
        reopened.get(&alice).await.expect("get").map(|r| r.state),
        Some(SubjectState::Erasing)
    );
    assert_code(reopened.link(&v1, &alice).await, SUBJECT_ERASING_CODE, "erasing target");

    reopened.set_state(&v2, SubjectState::Erasing).await.expect("set state");
    reopened.set_state(&alice, SubjectState::Active).await.expect("set state");
    assert_code(reopened.link(&v2, &alice).await, SUBJECT_ERASING_CODE, "erasing visitor");
    reopened.link(&v1, &alice).await.expect("active subjects link");
}

/// `remove` forgets the subject: its record, its pseudonym, and its link. A later `ensure`
/// issues a new pseudonym, and a repeat removes nothing.
pub async fn remove_forgets_the_subject<H: SubjectRegistryHarness + ?Sized>(harness: &H) {
    let registry = harness.open(Arc::new(ManualClock::new(base_time()))).await;
    let (v1, alice) = (visitor(), person());
    let linked = registry.link(&v1, &alice).await.expect("link");

    assert!(registry.remove(&v1).await.expect("remove"));
    assert_eq!(registry.get(&v1).await.expect("get"), None);
    assert_eq!(registry.by_pseudonym(&linked.pseudonym).await.expect("resolve"), None);
    assert_eq!(registry.linked(&alice).await.expect("linked"), Vec::<SubjectRef>::new());
    assert!(!registry.remove(&v1).await.expect("second remove"));
    assert!(registry.get(&alice).await.expect("get").is_some(), "others stay registered");

    let again = registry.ensure(&v1).await.expect("ensure");
    assert_ne!(again.pseudonym, linked.pseudonym);
    assert_eq!(again.linked_to, None);
}
