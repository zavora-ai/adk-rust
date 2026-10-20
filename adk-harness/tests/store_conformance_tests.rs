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
