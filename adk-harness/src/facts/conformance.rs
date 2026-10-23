//! Conformance cases for [`FactStore`](crate::FactStore) and [`FactIndex`](crate::FactIndex)
//! backends (spec 3.1 §11 "Long-term memory"; the distillation cursor, consolidation records
//! and `restore_assumed`, spec 3.2 §7.5, §10 HK2).

/// [`FactStore`](crate::FactStore) cases.
pub mod fact_store {
    use adk_core::principal::SubjectRef;

    use crate::{
        FactFilter, FactKind, FactProjects, FactScope, FactSource, FactStatus, FactStore,
        MemoryFact, Provenance,
    };

    fn fact(user: &str, project: Option<&str>, text: &str, status: FactStatus) -> MemoryFact {
        MemoryFact::new(
            FactScope::new("app", user, project.map(str::to_string)),
            FactKind::ProjectFact,
            text,
            status,
            Provenance::new(FactSource::AgentInferred, "s1", "inv-1"),
        )
    }

    fn ids(facts: &[MemoryFact]) -> Vec<&str> {
        let mut ids: Vec<&str> = facts.iter().map(|fact| fact.id.as_str()).collect();
        ids.sort_unstable();
        ids
    }

    /// A stored fact reads back unchanged.
    pub async fn put_then_get_round_trips(store: &dyn FactStore) {
        let mut stored =
            fact("alice", Some("p1"), "The tests need --features sqlite", FactStatus::Assumed)
                .with_confidence(0.7);
        stored.provenance.quote = Some("needs sqlite".into());
        stored.provenance.run_id = Some("run-1".into());
        assert_eq!(store.put(stored.clone()).await.unwrap(), stored);
        assert_eq!(store.get(&stored.id).await.unwrap(), Some(stored));
        assert_eq!(store.get("missing").await.unwrap(), None);
    }

    /// The same live (scope, digest) is one fact: `uses` is bumped and the first returned.
    pub async fn put_deduplicates_by_scope_and_digest(store: &dyn FactStore) {
        let first = store
            .put(fact("alice", Some("p1"), "Use pnpm, not npm", FactStatus::Assumed))
            .await
            .unwrap();
        let again = store
            .put(fact("alice", Some("p1"), "use  PNPM, not npm.", FactStatus::Assumed))
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.uses, first.uses + 1);
        let elsewhere = store
            .put(fact("alice", Some("p2"), "Use pnpm, not npm", FactStatus::Assumed))
            .await
            .unwrap();
        assert_ne!(elsewhere.id, first.id, "another project is another scope");
        store
            .set_status(&first.id, FactStatus::Forgotten, &SubjectRef::human("alice"))
            .await
            .unwrap();
        let after = store
            .put(fact("alice", Some("p1"), "Use pnpm, not npm", FactStatus::Assumed))
            .await
            .unwrap();
        assert_ne!(after.id, first.id, "a forgotten fact is history, not a duplicate");
    }

    /// `list` filters by project, kind, status and time, newest first, and never crosses
    /// users or apps.
    pub async fn list_filters_and_isolates(store: &dyn FactStore) {
        let p1 = store.put(fact("alice", Some("p1"), "one", FactStatus::Known)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        let wide = store.put(fact("alice", None, "two", FactStatus::Assumed)).await.unwrap();
        let p2 = store.put(fact("alice", Some("p2"), "three", FactStatus::Assumed)).await.unwrap();
        let bob = store.put(fact("bob", Some("p1"), "four", FactStatus::Known)).await.unwrap();
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        let recall = store.list(FactFilter::recall(&scope)).await.unwrap();
        assert_eq!(ids(&recall), ids(&[p1.clone(), wide.clone()]));
        assert_eq!(recall[0].id, wide.id, "newest first");
        let all = store.list(FactFilter::user("app", "alice")).await.unwrap();
        assert_eq!(ids(&all), ids(&[p1.clone(), wide.clone(), p2.clone()]));
        assert!(!all.iter().any(|f| f.id == bob.id));
        assert!(store.list(FactFilter::user("other", "alice")).await.unwrap().is_empty());
        let only = store
            .list(FactFilter {
                projects: FactProjects::Only(Some("p2".into())),
                ..FactFilter::user("app", "alice")
            })
            .await
            .unwrap();
        assert_eq!(ids(&only), ids(std::slice::from_ref(&p2)));
        let kinds = store
            .list(FactFilter {
                kinds: vec![FactKind::Decision],
                ..FactFilter::user("app", "alice")
            })
            .await
            .unwrap();
        assert!(kinds.is_empty());
        store.set_status(&p2.id, FactStatus::Forgotten, &SubjectRef::human("alice")).await.unwrap();
        let live = store
            .list(FactFilter { live_only: true, ..FactFilter::user("app", "alice") })
            .await
            .unwrap();
        assert_eq!(ids(&live), ids(&[p1, wide.clone()]));
        let since = store
            .list(FactFilter { since: Some(wide.valid_from), ..FactFilter::user("app", "alice") })
            .await
            .unwrap();
        assert!(since.iter().all(|f| f.valid_from >= wide.valid_from));
    }

    /// Status changes: promotion records `UserConfirmed`; superseding and forgetting set
    /// `valid_to` and are final.
    pub async fn status_changes_follow_the_contract(store: &dyn FactStore) {
        let by = SubjectRef::human("alice");
        let assumed = store.put(fact("alice", Some("p1"), "a", FactStatus::Assumed)).await.unwrap();
        store.set_status(&assumed.id, FactStatus::Known, &by).await.unwrap();
        let promoted = store.get(&assumed.id).await.unwrap().unwrap();
        assert_eq!(promoted.status, FactStatus::Known);
        assert_eq!(promoted.provenance.source, FactSource::UserConfirmed);
        assert_eq!(promoted.valid_to, None);
        let next = store.put(fact("alice", Some("p1"), "b", FactStatus::Known)).await.unwrap();
        store
            .set_status(&assumed.id, FactStatus::Superseded { by: next.id.clone() }, &by)
            .await
            .unwrap();
        let superseded = store.get(&assumed.id).await.unwrap().unwrap();
        assert_eq!(superseded.status, FactStatus::Superseded { by: next.id.clone() });
        assert!(superseded.valid_to.is_some());
        assert!(
            store.set_status(&assumed.id, FactStatus::Known, &by).await.is_err(),
            "history is final"
        );
        assert!(store.set_status("missing", FactStatus::Forgotten, &by).await.is_err());
        store.set_status(&next.id, FactStatus::Forgotten, &by).await.unwrap();
        assert!(store.get(&next.id).await.unwrap().unwrap().valid_to.is_some());
    }

    /// Embeddings are returned for the live facts of exactly the scope.
    pub async fn embeddings_follow_scope_and_status(store: &dyn FactStore) {
        let p1 = store.put(fact("alice", Some("p1"), "a", FactStatus::Assumed)).await.unwrap();
        let wide = store.put(fact("alice", None, "b", FactStatus::Assumed)).await.unwrap();
        store.put_embedding(&p1.id, vec![1.0, 0.0]).await.unwrap();
        store.put_embedding(&wide.id, vec![0.0, 1.0]).await.unwrap();
        assert!(store.put_embedding("missing", vec![1.0]).await.is_err());
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        assert_eq!(store.embeddings(&scope).await.unwrap(), vec![(p1.id.clone(), vec![1.0, 0.0])]);
        assert_eq!(
            store.embeddings(&scope.user_wide()).await.unwrap(),
            vec![(wide.id.clone(), vec![0.0, 1.0])]
        );
        store.set_status(&p1.id, FactStatus::Forgotten, &SubjectRef::human("alice")).await.unwrap();
        assert!(store.embeddings(&scope).await.unwrap().is_empty());
    }

    /// `record_recall` sets `last_recalled` and bumps `uses`.
    pub async fn recall_is_recorded(store: &dyn FactStore) {
        let stored = store.put(fact("alice", Some("p1"), "a", FactStatus::Known)).await.unwrap();
        let at = chrono::Utc::now();
        store.record_recall(std::slice::from_ref(&stored.id), at).await.unwrap();
        let read = store.get(&stored.id).await.unwrap().unwrap();
        assert_eq!(read.uses, stored.uses + 1);
        assert_eq!(read.last_recalled.map(|t| t.timestamp_millis()), Some(at.timestamp_millis()));
    }

    /// The distillation cursor is per (app, user, session) and only moves forward
    /// (spec 3.2 §10 HK2).
    pub async fn distillation_cursor_moves_forward(store: &dyn FactStore) {
        let early = chrono::DateTime::parse_from_rfc3339("2026-10-20T08:00:00.5Z").unwrap();
        let early = early.with_timezone(&chrono::Utc);
        let later = early + chrono::Duration::milliseconds(1_250);
        store.mark_distilled("app", "alice", "s1", early).await.unwrap();
        store.mark_distilled("app", "alice", "s1", later).await.unwrap();
        store.mark_distilled("app", "alice", "s1", early).await.unwrap();
        store.mark_distilled("app", "alice", "s2", early).await.unwrap();
        store.mark_distilled("app", "bob", "s1", early).await.unwrap();
        let mut cursors = store.distilled_sessions("app", "alice").await.unwrap();
        cursors.sort();
        assert_eq!(cursors, vec![("s1".to_string(), later), ("s2".to_string(), early)]);
        assert_eq!(store.distilled_sessions("app", "bob").await.unwrap().len(), 1);
        assert!(store.distilled_sessions("other", "alice").await.unwrap().is_empty());
    }

    /// Consolidation records round-trip per user, newest first, and a second put replaces
    /// the first (spec 3.2 §7.5).
    pub async fn consolidation_records_round_trip(store: &dyn FactStore) {
        let at = chrono::Utc::now();
        let mut first = crate::ConsolidationRecord::new("01JTICKA", "app", "alice", "default", at);
        first.changes.push(crate::FactChange {
            fact: "01JFACT".into(),
            scope: FactScope::new("app", "alice", Some("p1".into())),
            to: FactStatus::Superseded { by: "01JNEW".into() },
            reason: crate::ChangeReason::Duplicate,
        });
        first.merged.push("01JMERGED".into());
        store.put_consolidation(&first).await.unwrap();
        let second = crate::ConsolidationRecord::new(
            "01JTICKB",
            "app",
            "alice",
            "default",
            at + chrono::Duration::seconds(1),
        );
        store.put_consolidation(&second).await.unwrap();
        store
            .put_consolidation(&crate::ConsolidationRecord::new("01JTICKC", "app", "bob", "x", at))
            .await
            .unwrap();
        assert_eq!(
            store.consolidations("app", "alice").await.unwrap(),
            vec![second.clone(), first.clone()]
        );
        first.undone_at = Some(at);
        store.put_consolidation(&first).await.unwrap();
        assert_eq!(store.consolidations("app", "alice").await.unwrap(), vec![second, first]);
        assert!(store.consolidations("other", "alice").await.unwrap().is_empty());
    }

    /// `restore_assumed` sets a superseded or forgotten fact back to `Assumed` with
    /// `valid_to` cleared, and refuses a live fact (spec 3.2 §7.5).
    pub async fn restore_assumed_reverts_a_status(store: &dyn FactStore) {
        let by = SubjectRef::service("adk-harness");
        let stored = store.put(fact("alice", Some("p1"), "a", FactStatus::Assumed)).await.unwrap();
        store
            .set_status(&stored.id, FactStatus::Superseded { by: "01JNEW".into() }, &by)
            .await
            .unwrap();
        store.restore_assumed(&stored.id, &by).await.unwrap();
        let read = store.get(&stored.id).await.unwrap().unwrap();
        assert_eq!((read.status, read.valid_to), (FactStatus::Assumed, None));
        assert!(store.restore_assumed(&stored.id, &by).await.is_err(), "a live fact");
        assert!(store.restore_assumed("missing", &by).await.is_err());
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        let live = store
            .list(FactFilter { live_only: true, ..FactFilter::exactly(&scope) })
            .await
            .unwrap();
        assert_eq!(ids(&live), vec![stored.id.as_str()], "listed as live again");
    }
}

/// [`FactIndex`](crate::FactIndex) cases. Each takes the index and the store recall reads
/// facts from; for an index that keeps its own entries, the store only holds the facts.
pub mod fact_index {
    use adk_core::principal::SubjectRef;

    use crate::{
        FactIndex, FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact, Provenance,
    };

    async fn add(
        index: &dyn FactIndex,
        store: &dyn FactStore,
        user: &str,
        project: Option<&str>,
        text: &str,
    ) -> MemoryFact {
        let fact = MemoryFact::new(
            FactScope::new("app", user, project.map(str::to_string)),
            FactKind::ProjectFact,
            text,
            FactStatus::Assumed,
            Provenance::new(FactSource::AgentInferred, "s1", "inv-1"),
        );
        let stored = store.put(fact).await.unwrap();
        index.upsert(&stored).await.unwrap();
        stored
    }

    fn found(hits: &[(String, f32)], fact: &MemoryFact) -> bool {
        hits.iter().any(|(id, _)| *id == fact.id)
    }

    /// A query finds the facts that hold its words, with scores in `0.0..=1.0`.
    pub async fn search_finds_matching_facts(index: &dyn FactIndex, store: &dyn FactStore) {
        let sqlite =
            add(index, store, "alice", Some("p1"), "the integration tests need sqlite enabled")
                .await;
        let pnpm =
            add(index, store, "alice", Some("p1"), "dependencies are installed with pnpm").await;
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        let hits = index.search(&scope, "sqlite", 5).await.unwrap();
        assert!(found(&hits, &sqlite), "{hits:?}");
        assert!(!found(&hits, &pnpm), "{hits:?}");
        assert!(hits.iter().all(|(_, score)| (0.0..=1.0).contains(score)), "{hits:?}");
        assert!(index.search(&scope, "", 5).await.unwrap().is_empty());
    }

    /// Search covers the project and the user-wide facts, never another project, user or app.
    pub async fn search_respects_scope(index: &dyn FactIndex, store: &dyn FactStore) {
        let here = add(index, store, "alice", Some("p1"), "deploys go through staging first").await;
        let wide = add(index, store, "alice", None, "deploys need a changelog entry").await;
        let there = add(index, store, "alice", Some("p2"), "deploys use blue green").await;
        let bob = add(index, store, "bob", Some("p1"), "deploys are frozen on fridays").await;
        let hits = index
            .search(&FactScope::new("app", "alice", Some("p1".into())), "deploys", 10)
            .await
            .unwrap();
        assert!(found(&hits, &here) && found(&hits, &wide), "{hits:?}");
        assert!(!found(&hits, &there) && !found(&hits, &bob), "{hits:?}");
        let other_app = index
            .search(&FactScope::new("other", "alice", Some("p1".into())), "deploys", 10)
            .await
            .unwrap();
        assert!(other_app.is_empty(), "{other_app:?}");
    }

    /// `limit` caps the hits.
    pub async fn search_honours_the_limit(index: &dyn FactIndex, store: &dyn FactStore) {
        for i in 0..5 {
            add(
                index,
                store,
                "alice",
                Some("p1"),
                &format!("benchmark run {i} used the release profile"),
            )
            .await;
        }
        let hits = index
            .search(&FactScope::new("app", "alice", Some("p1".into())), "benchmark", 3)
            .await
            .unwrap();
        assert_eq!(hits.len(), 3, "{hits:?}");
    }

    /// A removed fact is not found again once the store no longer holds it live.
    pub async fn removed_facts_are_not_found(index: &dyn FactIndex, store: &dyn FactStore) {
        let gone =
            add(index, store, "alice", Some("p1"), "the linter config lives in clippy.toml").await;
        let kept =
            add(index, store, "alice", Some("p1"), "the linter runs in the pre-commit hook").await;
        store
            .set_status(&gone.id, FactStatus::Forgotten, &SubjectRef::human("alice"))
            .await
            .unwrap();
        index.remove(&gone).await.unwrap();
        let hits = index
            .search(&FactScope::new("app", "alice", Some("p1".into())), "linter", 5)
            .await
            .unwrap();
        assert!(!found(&hits, &gone), "{hits:?}");
        assert!(found(&hits, &kept), "{hits:?}");
    }
}

/// Generates one `#[tokio::test]` per [`fact_store`] case for the store `store` evaluates
/// to (spec 3.1 §11).
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::fact_store_conformance_tests!(store = adk_harness::InMemoryFactStore::new());
/// ```
#[macro_export]
macro_rules! fact_store_conformance_tests {
    ($(#[$attr:meta])* store = $store:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_put_then_get_round_trips() {
            let store = $store;
            $crate::conformance::fact_store::put_then_get_round_trips(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_put_deduplicates_by_scope_and_digest() {
            let store = $store;
            $crate::conformance::fact_store::put_deduplicates_by_scope_and_digest(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_list_filters_and_isolates() {
            let store = $store;
            $crate::conformance::fact_store::list_filters_and_isolates(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_status_changes_follow_the_contract() {
            let store = $store;
            $crate::conformance::fact_store::status_changes_follow_the_contract(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_embeddings_follow_scope_and_status() {
            let store = $store;
            $crate::conformance::fact_store::embeddings_follow_scope_and_status(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_recall_is_recorded() {
            let store = $store;
            $crate::conformance::fact_store::recall_is_recorded(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_distillation_cursor_moves_forward() {
            let store = $store;
            $crate::conformance::fact_store::distillation_cursor_moves_forward(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_consolidation_records_round_trip() {
            let store = $store;
            $crate::conformance::fact_store::consolidation_records_round_trip(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_store_restore_assumed_reverts_a_status() {
            let store = $store;
            $crate::conformance::fact_store::restore_assumed_reverts_a_status(&store).await;
        }
    };
}

/// Generates one `#[tokio::test]` per [`fact_index`] case. `setup` evaluates to an
/// `(index, store)` pair of `Arc`s.
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::fact_index_conformance_tests!(setup = {
///     let store = std::sync::Arc::new(adk_harness::InMemoryFactStore::new());
///     (std::sync::Arc::new(adk_harness::LocalFactIndex::new(store.clone())), store)
/// });
/// ```
#[macro_export]
macro_rules! fact_index_conformance_tests {
    ($(#[$attr:meta])* setup = $setup:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_index_search_finds_matching_facts() {
            let (index, store) = $setup;
            $crate::conformance::fact_index::search_finds_matching_facts(&*index, &*store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_index_search_respects_scope() {
            let (index, store) = $setup;
            $crate::conformance::fact_index::search_respects_scope(&*index, &*store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_index_search_honours_the_limit() {
            let (index, store) = $setup;
            $crate::conformance::fact_index::search_honours_the_limit(&*index, &*store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn fact_index_removed_facts_are_not_found() {
            let (index, store) = $setup;
            $crate::conformance::fact_index::removed_facts_are_not_found(&*index, &*store).await;
        }
    };
}
