//! Conformance suites for harness store backends (spec 3.1 §11).
//!
//! Each function checks one behaviour every backend must have. The
//! [`mode_store_conformance_tests!`](crate::mode_store_conformance_tests) and
//! [`standing_rule_store_conformance_tests!`](crate::standing_rule_store_conformance_tests)
//! macros generate one `#[tokio::test]` per case for a store expression, evaluated afresh
//! in each test.
//!
//! # Example
//!
//! ```rust,ignore
//! mod in_memory {
//!     adk_harness::mode_store_conformance_tests!(store = adk_harness::InMemoryModeStore::new());
//!     adk_harness::standing_rule_store_conformance_tests!(
//!         store = adk_harness::InMemoryStandingRuleStore::new()
//!     );
//! }
//! ```

/// [`ModeStore`](crate::ModeStore) cases.
pub mod mode_store {
    use crate::{HarnessMode, ModeRecord, ModeSource, ModeStore};

    /// A session with no record reads as `None`.
    pub async fn missing_is_none(store: &dyn ModeStore) {
        assert!(store.get("app", "alice", "s1").await.unwrap().is_none());
    }

    /// A stored record reads back unchanged.
    pub async fn set_then_get_round_trips(store: &dyn ModeStore) {
        let mut record = ModeRecord::new(HarnessMode::Auto, ModeSource::PlanApproval);
        record.revision = 7;
        record.effective_from = Some("inv-1".into());
        store.set("app", "alice", "s1", record.clone()).await.unwrap();
        assert_eq!(store.get("app", "alice", "s1").await.unwrap(), Some(record));
    }

    /// A later record replaces an earlier one.
    pub async fn set_replaces(store: &dyn ModeStore) {
        store
            .set("app", "alice", "s1", ModeRecord::new(HarnessMode::Auto, ModeSource::User))
            .await
            .unwrap();
        store
            .set(
                "app",
                "alice",
                "s1",
                ModeRecord::new(HarnessMode::ReadOnly, ModeSource::SystemTighten),
            )
            .await
            .unwrap();
        assert_eq!(
            store.get("app", "alice", "s1").await.unwrap().unwrap().mode,
            HarnessMode::ReadOnly
        );
    }

    /// Records never cross apps, users or sessions.
    pub async fn scopes_are_isolated(store: &dyn ModeStore) {
        store
            .set("app", "alice", "s1", ModeRecord::new(HarnessMode::Auto, ModeSource::User))
            .await
            .unwrap();
        for (app, user, session) in
            [("other", "alice", "s1"), ("app", "bob", "s1"), ("app", "alice", "s2")]
        {
            assert!(
                store.get(app, user, session).await.unwrap().is_none(),
                "{app}/{user}/{session}"
            );
        }
    }
}

/// [`StandingRuleStore`](crate::StandingRuleStore) cases.
pub mod standing_rule_store {
    use adk_core::principal::{SubjectKind, SubjectRef};
    use chrono::Utc;

    use crate::{
        OriginPin, RulePredicate, RuleScope, StandingDecision, StandingRule, StandingRuleStore,
    };

    fn rule(id: &str, app: &str, user: &str, session: Option<&str>) -> StandingRule {
        let scope = match session {
            None => RuleScope::User {
                app_name: app.into(),
                user_id: user.into(),
                subject_kind: SubjectKind::Human,
            },
            Some(session) => RuleScope::Session {
                app_name: app.into(),
                user_id: user.into(),
                subject_kind: SubjectKind::Human,
                session_id: session.into(),
            },
        };
        StandingRule {
            id: id.into(),
            scope,
            tool: "bash".into(),
            origin: OriginPin::Exact {
                source: "static".into(),
                declaration_digest: "sha256:abc".into(),
            },
            agent: None,
            predicates: vec![RulePredicate::ShellPrefix {
                pointer: "/command".into(),
                tokens: vec!["cargo".into(), "test".into()],
            }],
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human(user),
            created_at: Utc::now(),
            expires_at: None,
            label: id.into(),
            source_fingerprint: None,
        }
    }

    fn ids(rules: &[StandingRule]) -> Vec<&str> {
        let mut ids: Vec<&str> = rules.iter().map(|rule| rule.id.as_str()).collect();
        ids.sort_unstable();
        ids
    }

    /// A granted rule is returned unchanged by `rules_for` and `list`.
    pub async fn grant_then_read(store: &dyn StandingRuleStore) {
        let granted = store.grant(rule("r1", "app", "alice", None)).await.unwrap();
        assert_eq!(store.rules_for("app", "alice", "s1").await.unwrap(), vec![granted.clone()]);
        assert_eq!(store.list("app", "alice").await.unwrap(), vec![granted]);
    }

    /// A session-scoped rule applies only to its session; a user-scoped one to every
    /// session.
    pub async fn session_rules_stay_in_their_session(store: &dyn StandingRuleStore) {
        store.grant(rule("user", "app", "alice", None)).await.unwrap();
        store.grant(rule("s1", "app", "alice", Some("s1"))).await.unwrap();
        assert_eq!(ids(&store.rules_for("app", "alice", "s1").await.unwrap()), ["s1", "user"]);
        assert_eq!(ids(&store.rules_for("app", "alice", "s2").await.unwrap()), ["user"]);
        assert_eq!(ids(&store.list("app", "alice").await.unwrap()), ["s1", "user"]);
    }

    /// Rules never cross users or apps.
    pub async fn subjects_and_apps_are_isolated(store: &dyn StandingRuleStore) {
        store.grant(rule("r1", "app", "alice", None)).await.unwrap();
        assert!(store.rules_for("app", "bob", "s1").await.unwrap().is_empty());
        assert!(store.rules_for("other", "alice", "s1").await.unwrap().is_empty());
        assert!(store.list("app", "bob").await.unwrap().is_empty());
    }

    /// Granting a rule with an existing id replaces it.
    pub async fn grant_replaces_by_id(store: &dyn StandingRuleStore) {
        store.grant(rule("r1", "app", "alice", None)).await.unwrap();
        let mut changed = rule("r1", "app", "alice", None);
        changed.decision = StandingDecision::Deny { reason: "no".into() };
        store.grant(changed.clone()).await.unwrap();
        assert_eq!(store.list("app", "alice").await.unwrap(), vec![changed]);
    }

    /// Revoking removes exactly the named rule of the app, once.
    pub async fn revoke_removes_one_rule(store: &dyn StandingRuleStore) {
        store.grant(rule("r1", "app", "alice", None)).await.unwrap();
        store.grant(rule("r2", "app", "alice", None)).await.unwrap();
        assert!(!store.revoke("other", "r1").await.unwrap());
        assert!(store.revoke("app", "r1").await.unwrap());
        assert!(!store.revoke("app", "r1").await.unwrap());
        assert_eq!(ids(&store.list("app", "alice").await.unwrap()), ["r2"]);
    }
}

/// Generates one `#[tokio::test]` per [`conformance::mode_store`](crate::conformance::mode_store)
/// case for the store `store` evaluates to (spec 3.1 §11).
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::mode_store_conformance_tests!(store = adk_harness::InMemoryModeStore::new());
/// ```
#[macro_export]
macro_rules! mode_store_conformance_tests {
    ($(#[$attr:meta])* store = $store:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test]
        async fn mode_store_missing_is_none() {
            let store = $store;
            $crate::conformance::mode_store::missing_is_none(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn mode_store_set_then_get_round_trips() {
            let store = $store;
            $crate::conformance::mode_store::set_then_get_round_trips(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn mode_store_set_replaces() {
            let store = $store;
            $crate::conformance::mode_store::set_replaces(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn mode_store_scopes_are_isolated() {
            let store = $store;
            $crate::conformance::mode_store::scopes_are_isolated(&store).await;
        }
    };
}

/// Generates one `#[tokio::test]` per
/// [`conformance::standing_rule_store`](crate::conformance::standing_rule_store) case for
/// the store `store` evaluates to (spec 3.1 §11).
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::standing_rule_store_conformance_tests!(
///     store = adk_harness::InMemoryStandingRuleStore::new()
/// );
/// ```
#[macro_export]
macro_rules! standing_rule_store_conformance_tests {
    ($(#[$attr:meta])* store = $store:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_grant_then_read() {
            let store = $store;
            $crate::conformance::standing_rule_store::grant_then_read(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_session_rules_stay_in_their_session() {
            let store = $store;
            $crate::conformance::standing_rule_store::session_rules_stay_in_their_session(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_subjects_and_apps_are_isolated() {
            let store = $store;
            $crate::conformance::standing_rule_store::subjects_and_apps_are_isolated(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_grant_replaces_by_id() {
            let store = $store;
            $crate::conformance::standing_rule_store::grant_replaces_by_id(&store).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_revoke_removes_one_rule() {
            let store = $store;
            $crate::conformance::standing_rule_store::revoke_removes_one_rule(&store).await;
        }
    };
}
