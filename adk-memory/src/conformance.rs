//! The [`MemoryService`] conformance suite for memory ids and expiry.
//!
//! Every backend in this crate runs it, and a third-party backend proves the same contract
//! by implementing [`MemoryServiceHarness`] and invoking [`memory_conformance_tests!`]
//! (the `conformance` feature).
//!
//! | Check | Behaviour |
//! |-------|-----------|
//! | [`ids_are_assigned_and_kept`] | A stored entry keeps the id it was given, gets one when it had none, and returns it from search and export |
//! | [`expired_entries_are_not_recalled`] | Search never returns an entry whose expiry has passed; a future expiry round-trips to the millisecond |
//! | [`the_default_ttl_applies`] | An entry written without an expiry takes the service's default TTL from the write; one with its own keeps it |
//! | [`project_entries_expire_too`] | Project-scoped entries follow the same rule (backends with project scoping) |
//! | [`delete_memory_removes_one_entry`] | `delete_memory` removes the entry with that id, in any project, and nothing of another user |
//! | [`purge_removes_only_expired_entries`] | After `purge_expired`, the user's export holds the live entries and none of the expired ones |
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::conformance::MemoryServiceHarness;
//! use adk_memory::{InMemoryMemoryService, MemoryService};
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! struct InMemory;
//!
//! #[async_trait::async_trait]
//! impl MemoryServiceHarness for InMemory {
//!     async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
//!         let service = InMemoryMemoryService::new();
//!         Arc::new(match default_ttl {
//!             Some(ttl) => service.with_default_ttl(ttl),
//!             None => service,
//!         })
//!     }
//! }
//!
//! adk_memory::memory_conformance_tests!(harness = InMemory);
//! ```

use crate::service::{MemoryEntry, MemoryService, SearchRequest};
use adk_core::Content;
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;

/// Opens the backend under test for one check of the suite.
#[async_trait]
pub trait MemoryServiceHarness: Send + Sync {
    /// Opens a service whose entries written without an expiry live `default_ttl` from the
    /// write, or never expire when it is `None`.
    ///
    /// Each check writes under an app and users of its own, so a store shared between
    /// checks is fine.
    async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService>;
}

/// The default TTL [`the_default_ttl_applies`] runs with.
const SHORT_TTL: Duration = Duration::from_secs(2);

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry::new(Content::new("user").with_text(text), "user")
}

/// The ids of the entries a search of `query` returns, sorted.
async fn recalled(
    service: &dyn MemoryService,
    app: &str,
    user: &str,
    query: &str,
    project: Option<&str>,
) -> Vec<MemoryEntry> {
    let mut memories = service
        .search(SearchRequest {
            query: query.to_string(),
            user_id: user.to_string(),
            app_name: app.to_string(),
            limit: Some(20),
            min_score: None,
            project_id: project.map(str::to_string),
        })
        .await
        .expect("search")
        .memories;
    memories.sort_by(|a, b| a.id.cmp(&b.id));
    memories
}

fn ids(memories: &[MemoryEntry]) -> Vec<String> {
    memories
        .iter()
        .map(|memory| memory.id.clone().expect("a returned entry carries an id"))
        .collect()
}

/// A stored entry keeps the id it was given, gets one when it had none, and both searches
/// and exports return it.
pub async fn ids_are_assigned_and_kept<H: MemoryServiceHarness + ?Sized>(harness: &H) {
    let service = harness.open(None).await;
    let (app, user) = (unique("app"), unique("user"));
    service
        .add_session(
            &app,
            &user,
            "s1",
            vec![entry("kestrel alpha").with_id("kept-1"), entry("kestrel beta")],
        )
        .await
        .expect("add_session");

    let found = ids(&recalled(service.as_ref(), &app, &user, "kestrel", None).await);
    assert_eq!(found.len(), 2, "both entries are recalled: {found:?}");
    assert!(found.contains(&"kept-1".to_string()), "a given id is kept: {found:?}");
    let assigned: Vec<&String> = found.iter().filter(|id| *id != "kept-1").collect();
    assert!(!assigned[0].is_empty(), "an entry without an id is given one");

    let mut exported: Vec<String> = service
        .export_user(&app, &user)
        .await
        .expect("export_user")
        .into_iter()
        .filter_map(|memory| memory.entry.id)
        .collect();
    exported.sort();
    assert_eq!(exported, found, "export returns the ids search does");
}

/// Search never returns an entry whose expiry has passed, and a future expiry comes back
/// as it was written, to the millisecond.
pub async fn expired_entries_are_not_recalled<H: MemoryServiceHarness + ?Sized>(harness: &H) {
    let service = harness.open(None).await;
    let (app, user) = (unique("app"), unique("user"));
    let in_an_hour = Utc::now() + chrono::Duration::hours(1);
    let fresh = entry("heron fresh").with_id("fresh").with_expires_at(in_an_hour);
    service
        .add_session(
            &app,
            &user,
            "s1",
            vec![
                entry("heron stale")
                    .with_id("stale")
                    .with_expires_at(Utc::now() - chrono::Duration::minutes(1)),
                fresh.clone(),
                entry("heron forever").with_id("forever"),
            ],
        )
        .await
        .expect("add_session");

    let found = recalled(service.as_ref(), &app, &user, "heron", None).await;
    assert_eq!(ids(&found), ["forever", "fresh"], "only unexpired entries are recalled");
    assert_eq!(found[0].expires_at, None, "an entry without an expiry has none");
    assert_eq!(found[1].expires_at, fresh.expires_at, "the expiry round-trips");
}

/// An entry written without an expiry takes the default TTL from the write; one with its
/// own expiry keeps it, and outlives the default.
pub async fn the_default_ttl_applies<H: MemoryServiceHarness + ?Sized>(harness: &H) {
    let service = harness.open(Some(SHORT_TTL)).await;
    let (app, user) = (unique("app"), unique("user"));
    let own = Utc::now() + chrono::Duration::hours(1);
    let before = crate::expiry::to_millis(Utc::now());
    service
        .add_session(
            &app,
            &user,
            "s1",
            vec![
                entry("osprey default").with_id("default"),
                entry("osprey own").with_id("own").with_expires_at(own),
            ],
        )
        .await
        .expect("add_session");
    let after = Utc::now();

    let found = recalled(service.as_ref(), &app, &user, "osprey", None).await;
    assert_eq!(ids(&found), ["default", "own"], "both are live right after the write");
    let defaulted = found[0].expires_at.expect("the default TTL sets an expiry");
    assert!(
        defaulted >= crate::expiry::expiry_after(before, SHORT_TTL)
            && defaulted <= crate::expiry::expiry_after(after, SHORT_TTL),
        "the default TTL is measured from the write, got {defaulted}"
    );
    assert_eq!(found[1].expires_at, Some(crate::expiry::to_millis(own)));

    let wait = (defaulted - Utc::now()).to_std().unwrap_or_default() + Duration::from_millis(500);
    tokio::time::sleep(wait).await;
    let found = recalled(service.as_ref(), &app, &user, "osprey", None).await;
    assert_eq!(ids(&found), ["own"], "the defaulted entry expires; the one with its own lives");
}

/// Project-scoped entries expire by the same rule. A backend without project scoping passes
/// trivially.
pub async fn project_entries_expire_too<H: MemoryServiceHarness + ?Sized>(harness: &H) {
    let service = harness.open(None).await;
    if !service.supports_project_scoping() {
        return;
    }
    let (app, user) = (unique("app"), unique("user"));
    service
        .add_session_to_project(
            &app,
            &user,
            "s1",
            "p1",
            vec![
                entry("falcon stale")
                    .with_id("stale")
                    .with_expires_at(Utc::now() - chrono::Duration::minutes(1)),
                entry("falcon fresh").with_id("fresh"),
            ],
        )
        .await
        .expect("add_session_to_project");
    let found = recalled(service.as_ref(), &app, &user, "falcon", Some("p1")).await;
    assert_eq!(ids(&found), ["fresh"]);
}

/// `delete_memory` removes the entry with that id — global or in a project — reports how
/// many it removed, and leaves another user's entry with the same id alone.
pub async fn delete_memory_removes_one_entry<H: MemoryServiceHarness + ?Sized>(harness: &H) {
    let service = harness.open(None).await;
    let (app, user, other) = (unique("app"), unique("user"), unique("user"));
    service
        .add_session(
            &app,
            &user,
            "s1",
            vec![entry("plover one").with_id("del-a"), entry("plover two").with_id("del-b")],
        )
        .await
        .expect("add_session");
    service
        .add_session(&app, &other, "s1", vec![entry("plover other").with_id("del-a")])
        .await
        .expect("add_session");

    assert_eq!(service.delete_memory(&app, &user, "del-a").await.expect("delete_memory"), 1);
    assert_eq!(ids(&recalled(service.as_ref(), &app, &user, "plover", None).await), ["del-b"]);
    assert_eq!(
        ids(&recalled(service.as_ref(), &app, &other, "plover", None).await),
        ["del-a"],
        "another user's entry with the same id is kept"
    );
    assert_eq!(service.delete_memory(&app, &user, "missing").await.expect("delete_memory"), 0);

    if service.supports_project_scoping() {
        service
            .add_session_to_project(
                &app,
                &user,
                "s2",
                "p1",
                vec![entry("plover project").with_id("del-p")],
            )
            .await
            .expect("add_session_to_project");
        assert_eq!(service.delete_memory(&app, &user, "del-p").await.expect("delete_memory"), 1);
        assert_eq!(
            ids(&recalled(service.as_ref(), &app, &user, "plover", Some("p1")).await),
            ["del-b"]
        );
    }
}

/// After `purge_expired`, the user's export holds the live entries and none of the expired
/// ones.
pub async fn purge_removes_only_expired_entries<H: MemoryServiceHarness + ?Sized>(harness: &H) {
    let service = harness.open(None).await;
    let (app, user) = (unique("app"), unique("user"));
    service
        .add_session(
            &app,
            &user,
            "s1",
            vec![
                entry("tern stale")
                    .with_id("stale")
                    .with_expires_at(Utc::now() - chrono::Duration::minutes(1)),
                entry("tern fresh").with_id("fresh"),
            ],
        )
        .await
        .expect("add_session");

    service.purge_expired().await.expect("purge_expired");
    let mut held: Vec<String> = service
        .export_user(&app, &user)
        .await
        .expect("export_user")
        .into_iter()
        .filter_map(|memory| memory.entry.id)
        .collect();
    held.sort();
    assert_eq!(held, ["fresh"], "the purge removed the expired entry and kept the live one");
    assert_eq!(ids(&recalled(service.as_ref(), &app, &user, "tern", None).await), ["fresh"]);
}

/// Expands to one `#[tokio::test]` per check of the suite, each opening the backend with
/// `harness`.
///
/// Attributes before `harness`, such as `#[ignore = "needs a server"]`, apply to every
/// generated test.
#[macro_export]
macro_rules! memory_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[tokio::test]
        async fn memory_ids_are_assigned_and_kept() {
            $crate::conformance::ids_are_assigned_and_kept(&$harness).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn memory_expired_entries_are_not_recalled() {
            $crate::conformance::expired_entries_are_not_recalled(&$harness).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn memory_the_default_ttl_applies() {
            $crate::conformance::the_default_ttl_applies(&$harness).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn memory_project_entries_expire_too() {
            $crate::conformance::project_entries_expire_too(&$harness).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn memory_delete_memory_removes_one_entry() {
            $crate::conformance::delete_memory_removes_one_entry(&$harness).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn memory_purge_removes_only_expired_entries() {
            $crate::conformance::purge_removes_only_expired_entries(&$harness).await;
        }
    };
}
