//! Conformance suites for harness store backends (spec 3.1 §11).
//!
//! Each function checks one behaviour every backend must have. The
//! [`mode_store_conformance_tests!`](crate::mode_store_conformance_tests) and
//! [`standing_rule_store_conformance_tests!`](crate::standing_rule_store_conformance_tests)
//! macros generate one `#[tokio::test]` per case for a store expression, evaluated afresh
//! in each test. [`shared_store_conformance_tests!`](crate::shared_store_conformance_tests)
//! runs the cases for two instances over one backend, as two replicas of a served harness
//! hold them (spec 3.1 §13 Q2).
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

#[cfg(feature = "memory")]
pub use crate::facts::conformance::{fact_index, fact_store};

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

/// Cases for two [`ModeStore`](crate::ModeStore) instances over one shared backend, as
/// two replicas of a served harness hold (spec 3.1 §13 Q2).
pub mod shared_mode_store {
    use crate::{HarnessMode, ModeRecord, ModeSource, ModeStore};

    /// A record written through one instance is read through the other, and a tightening
    /// through either applies at the other's next read (spec 3.1 §9.2 I14).
    pub async fn changes_reach_every_instance(first: &dyn ModeStore, second: &dyn ModeStore) {
        assert!(second.get("app", "alice", "s1").await.unwrap().is_none());
        let mut auto = ModeRecord::new(HarnessMode::Auto, ModeSource::User);
        auto.revision = 1;
        first.set("app", "alice", "s1", auto.clone()).await.unwrap();
        assert_eq!(second.get("app", "alice", "s1").await.unwrap(), Some(auto));
        let mut tightened = ModeRecord::new(HarnessMode::ReadOnly, ModeSource::SystemTighten);
        tightened.revision = 2;
        second.set("app", "alice", "s1", tightened.clone()).await.unwrap();
        assert_eq!(first.get("app", "alice", "s1").await.unwrap(), Some(tightened));
    }
}

/// [`ReadLedger`](adk_devtools::ReadLedger) cases for the harness's read-record stores
/// (spec 3.1 §7.26, §10 D7).
pub mod read_records {
    use adk_devtools::{ReadLedger, ReadScope};

    fn scope(user: &str, session: &str, agent: &str) -> ReadScope {
        ReadScope::new("app", user, session, agent)
    }

    /// A recorded digest reads back, and a later record of the path replaces it.
    pub async fn round_trip(store: &dyn ReadLedger) {
        let coder = scope("alice", "s1", "coder");
        assert_eq!(store.digest(&coder, "src/lib.rs").await.unwrap(), None);
        store.record(&coder, "src/lib.rs", "d1").await.unwrap();
        assert_eq!(store.digest(&coder, "src/lib.rs").await.unwrap().as_deref(), Some("d1"));
        store.record(&coder, "src/lib.rs", "d2").await.unwrap();
        assert_eq!(store.digest(&coder, "src/lib.rs").await.unwrap().as_deref(), Some("d2"));
    }

    /// Another user, session or agent never sees a scope's records.
    pub async fn scopes_are_isolated(store: &dyn ReadLedger) {
        store.record(&scope("alice", "s1", "coder"), "a.rs", "d1").await.unwrap();
        for other in [
            scope("bob", "s1", "coder"),
            scope("alice", "s2", "coder"),
            scope("alice", "s1", "reviewer"),
        ] {
            assert_eq!(store.digest(&other, "a.rs").await.unwrap(), None, "{other:?}");
        }
        let other_app = ReadScope::new("other", "alice", "s1", "coder");
        assert_eq!(store.digest(&other_app, "a.rs").await.unwrap(), None);
    }

    /// Forgetting a path removes it and keeps the scope's other paths.
    pub async fn forget_removes_one_path(store: &dyn ReadLedger) {
        let coder = scope("alice", "s1", "coder");
        store.record(&coder, "a.rs", "d1").await.unwrap();
        store.record(&coder, "b.rs", "d2").await.unwrap();
        store.forget(&coder, "a.rs").await.unwrap();
        store.forget(&coder, "never-recorded.rs").await.unwrap();
        assert_eq!(store.digest(&coder, "a.rs").await.unwrap(), None);
        assert_eq!(store.digest(&coder, "b.rs").await.unwrap().as_deref(), Some("d2"));
    }

    /// A record written through one instance is read through the other, as a held edit
    /// approved on another replica needs (spec 3.1 §7.33 fix 5).
    pub async fn records_reach_every_instance(first: &dyn ReadLedger, second: &dyn ReadLedger) {
        let coder = scope("alice", "s1", "coder");
        first.record(&coder, "a.rs", "d1").await.unwrap();
        assert_eq!(second.digest(&coder, "a.rs").await.unwrap().as_deref(), Some("d1"));
        second.forget(&coder, "a.rs").await.unwrap();
        assert_eq!(first.digest(&coder, "a.rs").await.unwrap(), None);
    }
}

/// [`StandingRuleStore`](crate::StandingRuleStore) cases.
pub mod standing_rule_store {
    use adk_core::principal::{SubjectKind, SubjectRef};
    use chrono::Utc;

    use crate::{
        OriginPin, RulePredicate, RuleScope, StandingDecision, StandingRule, StandingRuleStore,
    };

    /// The rule the cases grant: `bash` with a `cargo test` prefix, allowed, user-scoped
    /// unless `session` names a session.
    pub fn rule_for_tests(id: &str, app: &str, user: &str, session: Option<&str>) -> StandingRule {
        rule(id, app, user, session)
    }

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

    /// Replacing changes a stored rule in place, and never brings back one that was revoked
    /// (spec 3.3 §8.1 "Extend", spec 3.1 §9.2 I4).
    pub async fn replace_keeps_a_revoked_rule_revoked(store: &dyn StandingRuleStore) {
        let mut granted = rule("r1", "app", "alice", None);
        granted.expires_at = Some(Utc::now() + chrono::Duration::days(1));
        store.grant(granted.clone()).await.unwrap();
        store.grant(rule("r2", "app", "alice", None)).await.unwrap();
        let mut extended = granted;
        extended.expires_at = Some(Utc::now() + chrono::Duration::days(30));
        assert!(store.replace(extended.clone()).await.unwrap());
        let listed = store.list("app", "alice").await.unwrap();
        assert_eq!(listed.iter().find(|rule| rule.id == "r1"), Some(&extended));
        assert_eq!(ids(&listed), ["r1", "r2"]);

        assert!(store.revoke("app", "r1").await.unwrap());
        assert!(!store.replace(extended.clone()).await.unwrap());
        assert_eq!(ids(&store.list("app", "alice").await.unwrap()), ["r2"]);
        let mut elsewhere = extended;
        elsewhere.scope = rule("r1", "other", "alice", None).scope;
        assert!(!store.replace(elsewhere).await.unwrap(), "a rule of another app is not replaced");
        assert!(store.list("other", "alice").await.unwrap().is_empty());
    }

    /// Rules read back from the store keep their decision and expiry, so the matcher
    /// lets a Deny beat an Allow and refuses an expired rule (spec 3.1 §7.5).
    pub async fn deny_beats_allow_and_expiry_hold(store: &dyn StandingRuleStore) {
        use adk_core::ToolOrigin;
        use serde_json::json;

        use crate::approvals::matcher::{RuleCall, RuleOutcome, evaluate_standing_rules};
        use crate::{Isolation, ToolKind};

        let origin = ToolOrigin::new("static", &json!({ "name": "bash" }), None);
        let pinned = |id: &str, tokens: &[&str]| {
            let mut rule = rule(id, "app", "alice", None);
            rule.origin = OriginPin::exact(&origin);
            rule.predicates = vec![RulePredicate::ShellPrefix {
                pointer: "/command".into(),
                tokens: tokens.iter().map(|token| (*token).to_string()).collect(),
            }];
            rule
        };
        let mut deny = pinned("deny", &["cargo", "test"]);
        deny.decision = StandingDecision::Deny { reason: "not here".into() };
        let mut expired = pinned("expired", &["make"]);
        expired.expires_at = Some(Utc::now() - chrono::Duration::minutes(1));
        for granted in [pinned("allow", &["cargo", "test"]), deny, expired] {
            store.grant(granted).await.unwrap();
        }
        let acting = adk_core::ActingContext::derived(
            "app",
            "harness",
            "alice",
            adk_core::SubjectKind::Human,
        );
        let check = |rules: Vec<StandingRule>, command: &'static str| {
            let args = json!({ "command": command });
            let call = RuleCall {
                app_name: "app",
                user_id: "alice",
                session_id: "s1",
                acting: Some(&acting),
                tool_name: "bash",
                args: &args,
                origin: Some(&origin),
                kind: ToolKind::Shell(Isolation::Host),
                workspace_root: std::path::Path::new("/repo"),
            };
            evaluate_standing_rules(&rules, &call, Utc::now())
        };
        let rules = store.rules_for("app", "alice", "s1").await.unwrap();
        assert_eq!(
            check(rules.clone(), "cargo test --quiet").outcome,
            RuleOutcome::Deny { rule_id: "deny".into(), reason: "not here".into() }
        );
        let make = check(rules, "make all");
        assert_eq!((make.outcome, make.expired), (RuleOutcome::NoMatch, vec!["expired".into()]));
        assert!(store.revoke("app", "deny").await.unwrap());
        assert_eq!(
            check(store.rules_for("app", "alice", "s1").await.unwrap(), "cargo test").outcome,
            RuleOutcome::Allow { rule_id: "allow".into() }
        );
    }
}

/// Cases for two [`StandingRuleStore`](crate::StandingRuleStore) instances over one
/// shared backend, as two replicas of a served harness hold (spec 3.1 §13 Q2).
pub mod shared_standing_rule_store {
    use crate::{StandingDecision, StandingRuleStore};

    /// A rule granted through one instance is read through the other; a revocation or a
    /// replacement through either is seen at the other's next read, even after the other
    /// has read the rule before (spec 3.1 §9.2 I4).
    pub async fn changes_reach_every_instance(
        first: &dyn StandingRuleStore,
        second: &dyn StandingRuleStore,
    ) {
        let granted = super::standing_rule_store::rule_for_tests("r1", "app", "alice", None);
        assert!(second.rules_for("app", "alice", "s1").await.unwrap().is_empty());
        first.grant(granted.clone()).await.unwrap();
        assert_eq!(second.rules_for("app", "alice", "s1").await.unwrap(), vec![granted.clone()]);
        assert_eq!(second.list("app", "alice").await.unwrap(), vec![granted.clone()]);

        assert!(first.revoke("app", "r1").await.unwrap());
        assert!(second.rules_for("app", "alice", "s1").await.unwrap().is_empty());
        assert!(second.list("app", "alice").await.unwrap().is_empty());

        second.grant(granted.clone()).await.unwrap();
        assert_eq!(first.rules_for("app", "alice", "s1").await.unwrap(), vec![granted.clone()]);
        let mut denied = granted;
        denied.decision = StandingDecision::Deny { reason: "revoked by policy".into() };
        first.grant(denied.clone()).await.unwrap();
        assert_eq!(second.rules_for("app", "alice", "s1").await.unwrap(), vec![denied]);

        let bob = super::standing_rule_store::rule_for_tests("r2", "app", "bob", Some("s9"));
        second.grant(bob.clone()).await.unwrap();
        assert_eq!(first.rules_for("app", "bob", "s9").await.unwrap(), vec![bob]);
        assert!(first.rules_for("app", "bob", "s1").await.unwrap().is_empty());
        assert!(second.revoke("app", "r2").await.unwrap());
        assert!(first.rules_for("app", "bob", "s9").await.unwrap().is_empty());
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
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_replace_keeps_a_revoked_rule_revoked() {
            let store = $store;
            $crate::conformance::standing_rule_store::replace_keeps_a_revoked_rule_revoked(&store)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn rule_store_deny_beats_allow_and_expiry_hold() {
            let store = $store;
            $crate::conformance::standing_rule_store::deny_beats_allow_and_expiry_hold(&store)
                .await;
        }
    };
}

/// Generates the [`conformance::shared_mode_store`](crate::conformance::shared_mode_store)
/// and [`conformance::shared_standing_rule_store`](crate::conformance::shared_standing_rule_store)
/// cases for a backend two replicas share (spec 3.1 §13 Q2): `modes` and `rules` each
/// evaluate to a pair of stores over one backend, as two replicas hold them.
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::shared_store_conformance_tests!(
///     modes = (adk_harness::FileModeStore::new(&dir), adk_harness::FileModeStore::new(&dir)),
///     rules = (adk_harness::FileStandingRuleStore::new(&dir), adk_harness::FileStandingRuleStore::new(&dir)),
/// );
/// ```
#[macro_export]
macro_rules! shared_store_conformance_tests {
    ($(#[$attr:meta])* modes = $modes:expr, rules = $rules:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test]
        async fn shared_mode_store_changes_reach_every_instance() {
            let (first, second) = $modes;
            $crate::conformance::shared_mode_store::changes_reach_every_instance(&first, &second)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn shared_rule_store_changes_reach_every_instance() {
            let (first, second) = $rules;
            $crate::conformance::shared_standing_rule_store::changes_reach_every_instance(
                &first, &second,
            )
            .await;
        }
    };
}

/// Encrypted store cases (spec 3.1 §7.12, §9.2 I57, §11): every wrapped store of one
/// `harness.db` file behind one [`StoreKeyring`](crate::encrypted::StoreKeyring).
///
/// [`write_fixture`](encrypted_store::write_fixture) plants a row carrying one of
/// [`SENTINELS`](encrypted_store::SENTINELS) through each store — sessions, approvals,
/// modes, rules, the shell ledger, goals, facts — and the cases check that every store
/// reads its row back, that no sentinel reaches the file, that a rotation re-seals every
/// row so the retired key opens nothing, and that the database refuses to open without
/// the key.
#[cfg(all(feature = "encrypted", feature = "sqlite", feature = "memory", feature = "runner"))]
pub mod encrypted_store {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
    use adk_core::principal::{SubjectKind, SubjectRef};
    use adk_core::{ApprovalScope, ApprovalStore, Content, ToolApproval, ToolConfirmationRequest};
    use adk_session::{
        AppendEventRequest, CreateRequest, EncryptedApprovalStore, EncryptedSession, Event,
        GetRequest, SessionService, SqliteApprovalStore, SqliteSessionService,
    };
    use chrono::Utc;
    use serde_json::{Value, json};

    use adk_devtools::{ReadLedger, ReadScope};

    use crate::encrypted::{SqliteStoreMarker, StoreKeyring};
    use crate::{
        FactKind, FactScope, FactSource, FactStatus, FactStore, GoalRecord, GoalSpec, GoalStore,
        HarnessMode, MemoryFact, ModeRecord, ModeSource, ModeStore, OriginPin, Provenance,
        RulePredicate, RuleScope, ShellLedger, ShellLedgerEntry, SqliteFactStore, SqliteGoalStore,
        SqliteModeStore, SqliteReadRecords, SqliteShellLedger, SqliteStandingRuleStore,
        StandingDecision, StandingRule, StandingRuleStore,
    };

    /// The app of every fixture row.
    pub const APP: &str = "conformance-app";
    /// The user of every fixture row.
    pub const USER: &str = "alice";
    /// The session of every fixture row.
    pub const SESSION: &str = "s1";
    /// The texts the fixture plants, one per payload; none may reach the file.
    pub const SENTINELS: &[&str] = &[
        "sentinel-state-value",
        "sentinel-event-text",
        "sentinel-approval-argument",
        "sentinel-decision-message",
        "sentinel-rule-label",
        "sentinel-rule-token",
        "sentinel-shell-command",
        "sentinel-goal-text",
        "sentinel-goal-until",
        "sentinel-fact-text",
        "sentinel-read-path",
    ];

    /// Every store of a `harness.db` file behind one keyring.
    pub struct SealedDb {
        /// The database file.
        pub path: PathBuf,
        /// The pool every store shares.
        pub pool: sqlx::SqlitePool,
        /// The id of the keyring's current key.
        pub key_id: String,
        /// Sessions.
        pub sessions: EncryptedSession<SqliteSessionService>,
        /// Held calls and decisions.
        pub approvals: EncryptedApprovalStore<SqliteApprovalStore>,
        /// Modes.
        pub modes: SqliteModeStore,
        /// Standing rules.
        pub rules: SqliteStandingRuleStore,
        /// The shell ledger.
        pub ledger: SqliteShellLedger,
        /// Goals.
        pub goals: SqliteGoalStore,
        /// Long-term facts.
        pub facts: SqliteFactStore,
        /// The file tools' read records.
        pub reads: SqliteReadRecords,
        /// The key marker.
        pub marker: SqliteStoreMarker,
    }

    /// Opens (creating and migrating) every store of the file at `path` behind `keyring`.
    pub async fn open(path: &Path, keyring: &StoreKeyring) -> SealedDb {
        SealedDb::open_with_legacy(path, keyring, false).await
    }

    impl SealedDb {
        /// [`open`], with legacy migration on the session and approval stores when
        /// `legacy`: what a rotation over a database written before the key uses.
        pub async fn open_with_legacy(
            path: &Path,
            keyring: &StoreKeyring,
            legacy: bool,
        ) -> SealedDb {
            let url = format!("sqlite://{}?mode=rwc", path.display());
            let base = SqliteSessionService::new(&url).await.unwrap();
            base.migrate().await.unwrap();
            let pool = base.pool().clone();
            let sessions =
                EncryptedSession::new(base, keyring.current_key(), keyring.previous_keys())
                    .with_legacy_migration(legacy);
            let approvals = SqliteApprovalStore::from_pool(pool.clone());
            approvals.migrate().await.unwrap();
            let approvals = EncryptedApprovalStore::new(
                approvals,
                keyring.current_key(),
                keyring.previous_keys(),
            )
            .with_legacy_migration(legacy);
            let modes = SqliteModeStore::new(pool.clone()).with_keyring(keyring.clone());
            modes.migrate().await.unwrap();
            let rules = SqliteStandingRuleStore::new(pool.clone()).with_keyring(keyring.clone());
            rules.migrate().await.unwrap();
            let ledger = SqliteShellLedger::new(pool.clone()).with_keyring(keyring.clone());
            ledger.migrate().await.unwrap();
            let goals = SqliteGoalStore::new(pool.clone()).with_keyring(keyring.clone());
            goals.migrate().await.unwrap();
            let facts = SqliteFactStore::new(pool.clone()).with_keyring(keyring.clone());
            facts.migrate().await.unwrap();
            let reads = SqliteReadRecords::new(pool.clone()).with_keyring(keyring.clone());
            reads.migrate().await.unwrap();
            let marker = SqliteStoreMarker::new(pool.clone());
            marker.migrate().await.unwrap();
            SealedDb {
                path: path.to_path_buf(),
                pool,
                key_id: keyring.key_id(),
                sessions,
                approvals,
                modes,
                rules,
                ledger,
                goals,
                facts,
                reads,
                marker,
            }
        }
    }

    fn identity() -> AdkIdentity {
        AdkIdentity::new(
            AppName::try_from(APP).unwrap(),
            UserId::try_from(USER).unwrap(),
            SessionId::try_from(SESSION).unwrap(),
        )
    }

    fn scope() -> ApprovalScope {
        ApprovalScope::new(APP, USER, SESSION)
    }

    fn read_scope() -> ReadScope {
        ReadScope::new(APP, USER, SESSION, "harness")
    }

    /// The call the fixture holds for approval.
    pub fn held_call() -> ToolConfirmationRequest {
        ToolConfirmationRequest::new("bash", json!({ "command": "sentinel-approval-argument" }))
            .with_function_call_id("call-held")
    }

    /// The call the fixture decides.
    pub fn decided_call() -> ToolConfirmationRequest {
        ToolConfirmationRequest::new("write_file", json!({ "path": "a.txt", "content": "x" }))
            .with_function_call_id("call-decided")
    }

    fn rule() -> StandingRule {
        StandingRule {
            id: "rule-1".into(),
            scope: RuleScope::User {
                app_name: APP.into(),
                user_id: USER.into(),
                subject_kind: SubjectKind::Human,
            },
            tool: "bash".into(),
            origin: OriginPin::Exact {
                source: "static".into(),
                declaration_digest: "sha256:abc".into(),
            },
            agent: None,
            predicates: vec![RulePredicate::ShellPrefix {
                pointer: "/command".into(),
                tokens: vec!["sentinel-rule-token".into()],
            }],
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human(USER),
            created_at: Utc::now(),
            expires_at: None,
            label: "sentinel-rule-label".into(),
            source_fingerprint: None,
        }
    }

    fn goal() -> GoalRecord {
        GoalRecord::start(GoalSpec {
            goal: "sentinel-goal-text".into(),
            until: "sentinel-goal-until".into(),
            max_iterations: 3,
            check_timeout: Duration::from_secs(5),
        })
    }

    fn fact() -> MemoryFact {
        MemoryFact::new(
            FactScope::new(APP, USER, Some("project-1".into())),
            FactKind::Preference,
            "sentinel-fact-text",
            FactStatus::Known,
            Provenance::new(FactSource::UserStated, SESSION, "inv-1"),
        )
    }

    /// Writes one sentinel-carrying row through every store and records the key marker;
    /// returns the fact's id.
    pub async fn write_fixture(db: &SealedDb) -> String {
        db.marker.set_key_id(&db.key_id).await.unwrap();
        let mut state: HashMap<String, Value> = HashMap::new();
        state.insert("plan".into(), json!("sentinel-state-value"));
        db.sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: Some(SESSION.into()),
                state,
            })
            .await
            .unwrap();
        let mut event = Event::new("inv-1");
        event.author = "model".into();
        event.llm_response.content = Some(Content::new("model").with_text("sentinel-event-text"));
        db.sessions
            .append_event_for_identity(AppendEventRequest { identity: identity(), event })
            .await
            .unwrap();
        db.approvals.record_pending(&scope(), &held_call()).await.unwrap();
        db.approvals
            .decide(
                &scope(),
                &decided_call().fingerprint(),
                ToolApproval {
                    message: Some("sentinel-decision-message".into()),
                    ..ToolApproval::approve()
                },
            )
            .await
            .unwrap();
        db.modes
            .set(APP, USER, SESSION, ModeRecord::new(HarnessMode::Auto, ModeSource::User))
            .await
            .unwrap();
        db.rules.grant(rule()).await.unwrap();
        db.ledger
            .record(
                APP,
                USER,
                SESSION,
                ShellLedgerEntry::new("c1", "sentinel-shell-command", true, Some(0)),
            )
            .await
            .unwrap();
        db.goals.set(APP, USER, SESSION, goal()).await.unwrap();
        db.reads.record(&read_scope(), "sentinel-read-path", "d1").await.unwrap();
        db.facts.put(fact()).await.unwrap().id
    }

    /// Reads every fixture row back through its store and checks it is the fixture.
    ///
    /// # Errors
    ///
    /// Returns the first store's error: with the wrong keyring, `decrypt_failed`.
    pub async fn read_fixture(db: &SealedDb, fact_id: &str) -> adk_core::Result<()> {
        let session = db
            .sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        assert_eq!(session.state().get("plan"), Some(json!("sentinel-state-value")));
        let events = session.events().all();
        assert_eq!(events.len(), 1);
        let text = events[0].llm_response.content.as_ref().unwrap().parts[0].text().unwrap();
        assert_eq!(text, "sentinel-event-text");
        let pending = db.approvals.pending(&scope()).await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request, held_call());
        assert_eq!(pending[0].fingerprint, held_call().fingerprint());
        assert!(db.approvals.has_decision(&scope(), &decided_call().fingerprint()).await?);
        assert_eq!(
            db.modes.get(APP, USER, SESSION).await?.map(|r| r.mode),
            Some(HarnessMode::Auto)
        );
        let rules = db.rules.list(APP, USER).await?;
        assert_eq!(rules.len(), 1);
        assert_eq!(
            (rules[0].id.as_str(), rules[0].label.as_str()),
            ("rule-1", "sentinel-rule-label")
        );
        assert_eq!(rules[0].predicates, rule().predicates);
        let entry = db.ledger.get(APP, USER, SESSION, "c1").await?.expect("the ledger entry");
        assert_eq!(entry.command, "sentinel-shell-command");
        let goal = db.goals.get(APP, USER, SESSION).await?.expect("the goal");
        assert_eq!(goal.spec.goal, "sentinel-goal-text");
        let fact = db.facts.get(fact_id).await?.expect("the fact");
        assert_eq!(fact.text, "sentinel-fact-text");
        assert_eq!(
            db.reads.digest(&read_scope(), "sentinel-read-path").await?.as_deref(),
            Some("d1")
        );
        Ok(())
    }

    /// Re-seals every store under the keyring's current key and records it in the marker;
    /// returns the rows changed.
    ///
    /// # Errors
    ///
    /// Returns the first store's error.
    pub async fn rewrap_all(db: &SealedDb) -> adk_core::Result<u64> {
        let mut rows = db.sessions.rewrap_sqlite().await?;
        rows += db.approvals.rewrap_sqlite().await?;
        rows += db.modes.rewrap().await?;
        rows += db.rules.rewrap().await?;
        rows += db.ledger.rewrap().await?;
        rows += db.goals.rewrap().await?;
        rows += db.facts.rewrap().await?;
        rows += db.reads.rewrap().await?;
        db.marker.set_key_id(&db.key_id).await?;
        crate::encrypted::compact(&db.pool).await?;
        Ok(rows)
    }

    /// The database file and its write-ahead log.
    pub fn file_bytes(path: &Path) -> Vec<u8> {
        let mut bytes = std::fs::read(path).unwrap();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        if let Ok(more) = std::fs::read(&wal) {
            bytes.extend(more);
        }
        bytes
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
    }

    /// Every store reads back what it sealed, and the payload columns hold envelopes.
    pub async fn every_store_reads_back_what_it_sealed(dir: &Path, keyring: StoreKeyring) {
        let db = open(&dir.join("harness.db"), &keyring).await;
        let fact_id = write_fixture(&db).await;
        read_fixture(&db, &fact_id).await.unwrap();
        for (table, statement) in [
            ("adk_harness_modes", "SELECT record FROM adk_harness_modes"),
            ("adk_harness_rules", "SELECT rule FROM adk_harness_rules"),
            ("adk_harness_shell_ledger", "SELECT entry FROM adk_harness_shell_ledger"),
            ("adk_harness_goals", "SELECT record FROM adk_harness_goals"),
            ("adk_harness_facts", "SELECT record FROM adk_harness_facts"),
            ("adk_harness_read_records", "SELECT records FROM adk_harness_read_records"),
            ("adk_approval_pending", "SELECT request FROM adk_approval_pending"),
            ("adk_approval_decisions", "SELECT approval FROM adk_approval_decisions"),
            ("events", "SELECT llm_response FROM events"),
            ("sessions", "SELECT state FROM sessions"),
        ] {
            let rows: Vec<(String,)> = sqlx::query_as(statement).fetch_all(&db.pool).await.unwrap();
            assert!(!rows.is_empty(), "{table} has a row");
            for (text,) in rows {
                assert!(text.contains("adk-enc:v1:"), "{table} holds an envelope: {text}");
                for sentinel in SENTINELS {
                    assert!(!text.contains(sentinel), "{table} holds {sentinel}");
                }
            }
        }
        db.pool.close().await;
    }

    /// No sentinel reaches the database file or its write-ahead log.
    pub async fn no_sentinel_reaches_the_file(dir: &Path, keyring: StoreKeyring) {
        let path = dir.join("harness.db");
        let db = open(&path, &keyring).await;
        write_fixture(&db).await;
        db.pool.close().await;
        let bytes = file_bytes(&path);
        for sentinel in SENTINELS {
            assert!(!contains(&bytes, sentinel), "{sentinel} reached the file in plaintext");
        }
        assert!(contains(&bytes, "adk-enc:v1:"), "the file holds envelopes");
    }

    /// A rotation re-seals every row under the new key; afterwards the new key alone
    /// opens everything and the retired key alone opens nothing.
    pub async fn rotation_reseals_every_row_and_the_old_key_opens_nothing(
        dir: &Path,
        keyring: StoreKeyring,
    ) {
        let path = dir.join("harness.db");
        let db = open(&path, &keyring).await;
        let fact_id = write_fixture(&db).await;
        db.pool.close().await;

        let rotated = keyring.rotated(adk_session::EncryptionKey::generate());
        let db = open(&path, &rotated).await;
        assert_eq!(db.marker.key_id().await.unwrap(), Some(keyring.key_id()));
        db.marker.check(Some(&rotated)).await.unwrap();
        let rows = rewrap_all(&db).await.unwrap();
        assert_eq!(
            rows, 10,
            "the session, its event, a held call, a decision, a mode, a rule, a ledger entry, a \
             goal, a fact and a read record"
        );
        assert_eq!(rewrap_all(&db).await.unwrap(), 0, "nothing is left under the old key");
        assert_eq!(db.marker.key_id().await.unwrap(), Some(rotated.key_id()));
        read_fixture(&db, &fact_id).await.unwrap();
        db.pool.close().await;

        let only_new = StoreKeyring::new(rotated.current_key(), Vec::new());
        let db = open(&path, &only_new).await;
        db.marker.check(Some(&only_new)).await.unwrap();
        read_fixture(&db, &fact_id).await.unwrap();
        let approval =
            db.approvals.take_approval(&scope(), &decided_call().fingerprint()).await.unwrap();
        assert_eq!(approval.and_then(|a| a.message).as_deref(), Some("sentinel-decision-message"));
        db.pool.close().await;

        let db = open(&path, &keyring).await;
        assert_eq!(
            db.marker.check(Some(&keyring)).await.unwrap_err().code,
            "harness.store.wrong_key"
        );
        let error = read_fixture(&db, &fact_id).await.unwrap_err();
        assert!(error.code.ends_with("decrypt_failed"), "{error}");
        for code in [
            db.modes.get(APP, USER, SESSION).await.unwrap_err().code,
            db.rules.list(APP, USER).await.unwrap_err().code,
            db.ledger.get(APP, USER, SESSION, "c1").await.unwrap_err().code,
            db.goals.get(APP, USER, SESSION).await.unwrap_err().code,
            db.facts.get(&fact_id).await.unwrap_err().code,
            db.reads.digest(&read_scope(), "sentinel-read-path").await.unwrap_err().code,
            db.approvals.pending(&scope()).await.unwrap_err().code,
        ] {
            assert!(code.ends_with("decrypt_failed"), "{code}");
        }
        db.pool.close().await;
    }

    /// A rotation over a database written before any key seals every plaintext row, so the
    /// store is encrypted in place and reads back through the keyring.
    pub async fn rotation_seals_rows_written_before_the_key(dir: &Path, keyring: StoreKeyring) {
        let path = dir.join("harness.db");
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let base = SqliteSessionService::new(&url).await.unwrap();
        base.migrate().await.unwrap();
        let pool = base.pool().clone();
        let mut state: HashMap<String, Value> = HashMap::new();
        state.insert("plan".into(), json!("sentinel-state-value"));
        base.create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state,
        })
        .await
        .unwrap();
        let mut event = Event::new("inv-1");
        event.author = "model".into();
        event.llm_response.content = Some(Content::new("model").with_text("sentinel-event-text"));
        base.append_event_for_identity(AppendEventRequest { identity: identity(), event })
            .await
            .unwrap();
        let approvals = SqliteApprovalStore::from_pool(pool.clone());
        approvals.migrate().await.unwrap();
        approvals.record_pending(&scope(), &held_call()).await.unwrap();
        approvals
            .decide(
                &scope(),
                &decided_call().fingerprint(),
                ToolApproval {
                    message: Some("sentinel-decision-message".into()),
                    ..ToolApproval::approve()
                },
            )
            .await
            .unwrap();
        let modes = SqliteModeStore::new(pool.clone());
        modes.migrate().await.unwrap();
        modes
            .set(APP, USER, SESSION, ModeRecord::new(HarnessMode::Auto, ModeSource::User))
            .await
            .unwrap();
        let rules = SqliteStandingRuleStore::new(pool.clone());
        rules.migrate().await.unwrap();
        rules.grant(rule()).await.unwrap();
        let ledger = SqliteShellLedger::new(pool.clone());
        ledger.migrate().await.unwrap();
        ledger
            .record(
                APP,
                USER,
                SESSION,
                ShellLedgerEntry::new("c1", "sentinel-shell-command", true, Some(0)),
            )
            .await
            .unwrap();
        let goals = SqliteGoalStore::new(pool.clone());
        goals.migrate().await.unwrap();
        goals.set(APP, USER, SESSION, goal()).await.unwrap();
        let facts = SqliteFactStore::new(pool.clone());
        facts.migrate().await.unwrap();
        let fact_id = facts.put(fact()).await.unwrap().id;
        let reads = SqliteReadRecords::new(pool.clone());
        reads.migrate().await.unwrap();
        reads.record(&read_scope(), "sentinel-read-path", "d1").await.unwrap();
        pool.close().await;
        assert!(contains(&file_bytes(&path), "sentinel-fact-text"), "written in plaintext");

        let db = SealedDb::open_with_legacy(&path, &keyring, true).await;
        assert!(
            read_fixture(&db, &fact_id).await.is_err(),
            "plaintext rows do not read through a key"
        );
        let rows = rewrap_all(&db).await.unwrap();
        assert_eq!(rows, 10, "every plaintext row is sealed");
        assert_eq!(rewrap_all(&db).await.unwrap(), 0);
        read_fixture(&db, &fact_id).await.unwrap();
        assert_eq!(db.marker.key_id().await.unwrap(), Some(keyring.key_id()));
        db.pool.close().await;
        let bytes = file_bytes(&path);
        for sentinel in SENTINELS {
            assert!(!contains(&bytes, sentinel), "{sentinel} is still in the file");
        }
        let db = open(&path, &keyring).await;
        read_fixture(&db, &fact_id).await.unwrap();
        db.pool.close().await;
    }

    /// A database written under a key refuses to open without one: the marker says so,
    /// and every store refuses an envelope it has no key for.
    pub async fn a_database_written_under_a_key_refuses_to_open_without_it(
        dir: &Path,
        keyring: StoreKeyring,
    ) {
        let path = dir.join("harness.db");
        let db = open(&path, &keyring).await;
        let fact_id = write_fixture(&db).await;
        db.pool.close().await;

        let url = format!("sqlite://{}?mode=rwc", path.display());
        let base = SqliteSessionService::new(&url).await.unwrap();
        let pool = base.pool().clone();
        let marker = SqliteStoreMarker::new(pool.clone());
        assert_eq!(marker.check(None).await.unwrap_err().code, "harness.store.key_required");
        for code in [
            SqliteModeStore::new(pool.clone()).get(APP, USER, SESSION).await.unwrap_err().code,
            SqliteStandingRuleStore::new(pool.clone()).list(APP, USER).await.unwrap_err().code,
            SqliteShellLedger::new(pool.clone())
                .get(APP, USER, SESSION, "c1")
                .await
                .unwrap_err()
                .code,
            SqliteGoalStore::new(pool.clone()).get(APP, USER, SESSION).await.unwrap_err().code,
            SqliteFactStore::new(pool.clone()).get(&fact_id).await.unwrap_err().code,
            SqliteReadRecords::new(pool.clone())
                .digest(&ReadScope::new(APP, USER, SESSION, "harness"), "sentinel-read-path")
                .await
                .unwrap_err()
                .code,
        ] {
            assert_eq!(code, "harness.store.key_required");
        }
        pool.close().await;
    }
}

/// Generates one `#[tokio::test]` per
/// [`conformance::encrypted_store`](crate::conformance::encrypted_store) case over a fresh
/// temporary `harness.db` and the keyring `keyring` evaluates to (spec 3.1 §11). The
/// invoking crate needs `tempfile` as a dev-dependency.
///
/// # Example
///
/// ```rust,ignore
/// adk_harness::encrypted_store_conformance_tests!(
///     keyring = adk_harness::encrypted::StoreKeyring::generate()
/// );
/// ```
#[cfg(all(feature = "encrypted", feature = "sqlite", feature = "memory", feature = "runner"))]
#[macro_export]
macro_rules! encrypted_store_conformance_tests {
    ($(#[$attr:meta])* keyring = $keyring:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test]
        async fn encrypted_store_every_store_reads_back_what_it_sealed() {
            let dir = ::tempfile::tempdir().unwrap();
            $crate::conformance::encrypted_store::every_store_reads_back_what_it_sealed(dir.path(), $keyring).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn encrypted_store_no_sentinel_reaches_the_file() {
            let dir = ::tempfile::tempdir().unwrap();
            $crate::conformance::encrypted_store::no_sentinel_reaches_the_file(dir.path(), $keyring).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn encrypted_store_rotation_reseals_every_row_and_the_old_key_opens_nothing() {
            let dir = ::tempfile::tempdir().unwrap();
            $crate::conformance::encrypted_store::rotation_reseals_every_row_and_the_old_key_opens_nothing(dir.path(), $keyring).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn encrypted_store_rotation_seals_rows_written_before_the_key() {
            let dir = ::tempfile::tempdir().unwrap();
            $crate::conformance::encrypted_store::rotation_seals_rows_written_before_the_key(dir.path(), $keyring).await;
        }
        $(#[$attr])*
        #[::tokio::test]
        async fn encrypted_store_a_database_written_under_a_key_refuses_to_open_without_it() {
            let dir = ::tempfile::tempdir().unwrap();
            $crate::conformance::encrypted_store::a_database_written_under_a_key_refuses_to_open_without_it(dir.path(), $keyring).await;
        }
    };
}
