//! Conformance suites over every mode and standing-rule store backend (spec 3.1 §11).

mod in_memory {
    adk_harness::mode_store_conformance_tests!(store = adk_harness::InMemoryModeStore::new());
    adk_harness::standing_rule_store_conformance_tests!(
        store = adk_harness::InMemoryStandingRuleStore::new()
    );
}

mod file {
    adk_harness::mode_store_conformance_tests!(
        store = adk_harness::FileModeStore::new(tempfile::tempdir().unwrap().keep())
    );
    adk_harness::standing_rule_store_conformance_tests!(
        store = adk_harness::FileStandingRuleStore::new(tempfile::tempdir().unwrap().keep())
    );
    adk_harness::shared_store_conformance_tests!(
        modes = {
            let dir = tempfile::tempdir().unwrap().keep();
            (adk_harness::FileModeStore::new(&dir), adk_harness::FileModeStore::new(&dir))
        },
        rules = {
            let dir = tempfile::tempdir().unwrap().keep();
            (
                adk_harness::FileStandingRuleStore::new(&dir),
                adk_harness::FileStandingRuleStore::new(&dir),
            )
        },
    );

    #[tokio::test]
    async fn files_are_private_and_survive_a_reopen() {
        use adk_harness::{FileModeStore, HarnessMode, ModeRecord, ModeSource, ModeStore};
        let dir = tempfile::tempdir().unwrap();
        FileModeStore::new(dir.path())
            .set("app", "alice", "s1", ModeRecord::new(HarnessMode::Auto, ModeSource::User))
            .await
            .unwrap();
        let reopened = FileModeStore::new(dir.path());
        assert_eq!(
            reopened.get("app", "alice", "s1").await.unwrap().unwrap().mode,
            HarnessMode::Auto
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = std::fs::read_dir(dir.path()).unwrap().next().unwrap().unwrap();
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(
            std::fs::read_dir(dir.path()).unwrap().next().unwrap().unwrap().path(),
            "{not json",
        )
        .unwrap();
        assert_eq!(
            reopened.get("app", "alice", "s1").await.unwrap().unwrap().mode,
            HarnessMode::ReadOnly
        );
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    async fn pool() -> sqlx::SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    adk_harness::mode_store_conformance_tests!(
        store = {
            let store = adk_harness::SqliteModeStore::new(pool().await);
            store.migrate().await.unwrap();
            store
        }
    );
    adk_harness::standing_rule_store_conformance_tests!(
        store = {
            let store = adk_harness::SqliteStandingRuleStore::new(pool().await);
            store.migrate().await.unwrap();
            store
        }
    );
    adk_harness::shared_store_conformance_tests!(
        modes = {
            let pool = pool().await;
            let store = adk_harness::SqliteModeStore::new(pool.clone());
            store.migrate().await.unwrap();
            (store, adk_harness::SqliteModeStore::new(pool))
        },
        rules = {
            let pool = pool().await;
            let store = adk_harness::SqliteStandingRuleStore::new(pool.clone());
            store.migrate().await.unwrap();
            (store, adk_harness::SqliteStandingRuleStore::new(pool))
        },
    );

    #[tokio::test]
    async fn the_shell_ledger_consumes_a_retry_once() {
        use adk_harness::{ShellLedger, ShellLedgerEntry};
        let ledger = adk_harness::SqliteShellLedger::new(pool().await);
        ledger.migrate().await.unwrap();
        ledger
            .record("app", "u", "s", ShellLedgerEntry::new("c1", "make", true, Some(2)))
            .await
            .unwrap();
        assert!(ledger.get("app", "u", "s", "c1").await.unwrap().unwrap().failed());
        assert!(ledger.mark_retried("app", "u", "s", "c1", "c2").await.unwrap());
        assert!(!ledger.mark_retried("app", "u", "s", "c1", "c3").await.unwrap());
        assert_eq!(
            ledger.get("app", "u", "s", "c1").await.unwrap().unwrap().retried_by.as_deref(),
            Some("c2")
        );
        assert!(!ledger.mark_retried("app", "u", "other", "c1", "c4").await.unwrap());
    }
}

/// A store that implements only grant, revoke and list: the trait's default `replace` changes
/// nothing, so an extension can never write back a rule revoked while it ran (spec 3.3 §8.1,
/// spec 3.1 §9.2 I4). Fails on `origin/main`, whose default reads the rules and then grants.
mod without_replace {
    use std::sync::Arc;

    use adk_core::principal::{SubjectKind, SubjectRef};
    use adk_core::{ErrorCategory, Result};
    use adk_harness::{
        InMemoryStandingRuleStore, OriginPin, RuleScope, StandingDecision, StandingRule,
        StandingRuleStore,
    };
    use async_trait::async_trait;

    /// Revokes every rule it lists, as a person's "Stop allowing" in another tab would between
    /// an extension's read and its write.
    #[derive(Debug)]
    struct RevokedWhileRead {
        inner: Arc<InMemoryStandingRuleStore>,
    }

    #[async_trait]
    impl StandingRuleStore for RevokedWhileRead {
        async fn rules_for(
            &self,
            app: &str,
            user: &str,
            session: &str,
        ) -> Result<Vec<StandingRule>> {
            self.inner.rules_for(app, user, session).await
        }
        async fn grant(&self, rule: StandingRule) -> Result<StandingRule> {
            self.inner.grant(rule).await
        }
        async fn revoke(&self, app: &str, id: &str) -> Result<bool> {
            self.inner.revoke(app, id).await
        }
        async fn list(&self, app: &str, user: &str) -> Result<Vec<StandingRule>> {
            let listed = self.inner.list(app, user).await?;
            for rule in &listed {
                self.inner.revoke(app, &rule.id).await?;
            }
            Ok(listed)
        }
    }

    #[tokio::test]
    async fn the_default_replace_never_brings_a_revoked_rule_back() {
        let inner = Arc::new(InMemoryStandingRuleStore::new());
        let rule = StandingRule {
            id: "r1".into(),
            scope: RuleScope::User {
                app_name: "app".into(),
                user_id: "alice".into(),
                subject_kind: SubjectKind::Human,
            },
            tool: "send_email".into(),
            origin: OriginPin::AnyDeclaration { source: "static".into() },
            agent: None,
            predicates: Vec::new(),
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human("alice"),
            created_at: chrono::Utc::now(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::days(1)),
            label: "send_email".into(),
            source_fingerprint: None,
        };
        inner.grant(rule.clone()).await.unwrap();
        let store = RevokedWhileRead { inner: inner.clone() };
        let error = store.replace(rule).await.unwrap_err();
        assert_eq!(
            (error.code, error.category),
            ("harness.rule.replace_unsupported", ErrorCategory::Unsupported)
        );
        // A read revokes it here; the replacement wrote nothing after it.
        let _ = store.list("app", "alice").await.unwrap();
        assert!(
            inner.list("app", "alice").await.unwrap().is_empty(),
            "the revoked rule stays revoked"
        );
    }
}
