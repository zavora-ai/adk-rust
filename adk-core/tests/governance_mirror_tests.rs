//! The replicated kill switch: a `GovernanceControl` mirroring a shared `GovernanceStateStore`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::governance::{GovernanceMirrorConfig, GovernanceWatch};
use adk_core::governance_state::{
    GovernanceScope, GovernanceState, GovernanceStateStore, InMemoryGovernanceStateStore,
};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, GOVERNANCE_STALE_CODE, GovernanceControl, Result,
};
use async_trait::async_trait;

/// A store that can be made unreachable.
#[derive(Default)]
struct FlakyStore {
    inner: InMemoryGovernanceStateStore,
    down: AtomicBool,
}

impl FlakyStore {
    fn reachable(&self) -> Result<()> {
        if self.down.load(Ordering::SeqCst) {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Unavailable,
                "test.store_down",
                "governance store unreachable",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl GovernanceStateStore for FlakyStore {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        self.reachable()?;
        self.inner.get(scope).await
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        self.reachable()?;
        self.inner.set(scope, expected_epoch, frozen, reason, issued_by).await
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        self.reachable()?;
        self.inner.list().await
    }
}

const POLL: Duration = Duration::from_millis(50);

fn config() -> GovernanceMirrorConfig {
    GovernanceMirrorConfig::default()
        .with_poll_interval(POLL)
        .with_max_staleness(Duration::from_millis(300))
}

async fn wait_until(bound: Duration, condition: impl Fn() -> bool) -> Option<Duration> {
    let started = tokio::time::Instant::now();
    while started.elapsed() <= bound {
        if condition() {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    None
}

#[tokio::test]
async fn a_freeze_reaches_every_replica_within_the_poll_bound() {
    let store: Arc<dyn GovernanceStateStore> = Arc::new(InMemoryGovernanceStateStore::new());
    let replicas: Vec<GovernanceControl> =
        (0..3).map(|_| GovernanceControl::mirrored(Arc::clone(&store), config())).collect();
    let mirrors: Vec<_> = replicas.iter().filter_map(GovernanceControl::spawn_mirror).collect();
    for replica in &replicas {
        wait_until(POLL * 4, || replica.check().is_ok()).await.expect("first read lands");
    }

    replicas[0].freeze_replicated("incident 12", Some(SubjectRef::human("oncall"))).await.unwrap();
    // The writer applies its own write at once; the others within one poll plus scheduling slack.
    assert!(replicas[0].is_frozen());
    for replica in &replicas[1..] {
        let seen = wait_until(POLL * 3, || replica.is_frozen()).await;
        assert!(seen.is_some(), "a replica missed the freeze for three poll intervals");
        assert_eq!(replica.reason().as_deref(), Some("incident 12"));
        assert_eq!(replica.check().unwrap_err().code, "governance.frozen");
    }

    replicas[2].unfreeze_replicated(None).await.unwrap();
    for replica in &replicas {
        assert!(
            wait_until(POLL * 3, || !replica.is_frozen()).await.is_some(),
            "a replica missed the unfreeze"
        );
    }
    mirrors.iter().for_each(tokio::task::JoinHandle::abort);
}

#[tokio::test]
async fn an_unreachable_store_fails_closed_past_the_staleness_limit() {
    let store = Arc::new(FlakyStore::default());
    let control = GovernanceControl::mirrored(store.clone(), config());
    assert_eq!(
        control.check().unwrap_err().code,
        GOVERNANCE_STALE_CODE,
        "a control that never read its store refuses work"
    );
    let mirror = control.spawn_mirror().unwrap();
    wait_until(POLL * 4, || control.check().is_ok()).await.expect("first read lands");

    store.down.store(true, Ordering::SeqCst);
    // Still inside the staleness lease: the last read is trusted.
    assert!(control.check().is_ok());
    let refused = wait_until(Duration::from_secs(2), || control.check().is_err()).await;
    assert!(refused.is_some(), "the control kept running on a stale view");
    let error = control.check().unwrap_err();
    assert_eq!(error.code, GOVERNANCE_STALE_CODE);
    assert!(error.is_retryable(), "staleness clears once the store is back");
    assert!(!control.is_frozen(), "stale is not frozen");

    store.down.store(false, Ordering::SeqCst);
    assert!(
        wait_until(POLL * 4, || control.check().is_ok()).await.is_some(),
        "the control recovers once the store is reachable"
    );
    mirror.abort();
}

#[tokio::test]
async fn a_fail_open_mirror_keeps_running_on_a_stale_view() {
    let store = Arc::new(FlakyStore::default());
    store.down.store(true, Ordering::SeqCst);
    let control = GovernanceControl::mirrored(store, config().with_fail_open(true));
    assert!(control.is_stale());
    assert!(control.check().is_ok());
}

#[tokio::test]
async fn a_failed_replicated_freeze_still_freezes_this_replica() {
    let store = Arc::new(FlakyStore::default());
    let control = GovernanceControl::mirrored(store.clone(), config());
    control.refresh().await.unwrap();
    store.down.store(true, Ordering::SeqCst);

    assert!(control.freeze_replicated("store down, stop anyway", None).await.is_err());
    assert!(control.is_frozen());

    store.down.store(false, Ordering::SeqCst);
    assert!(
        control.unfreeze_replicated(None).await.unwrap().is_none(),
        "nothing to lift in the store"
    );
    assert!(!control.is_frozen(), "unfreeze clears the local freeze too");
}

#[tokio::test]
async fn concurrent_replicated_freezes_retry_past_the_conflict() {
    let store: Arc<dyn GovernanceStateStore> = Arc::new(InMemoryGovernanceStateStore::new());
    let controls: Vec<GovernanceControl> =
        (0..4).map(|_| GovernanceControl::mirrored(Arc::clone(&store), config())).collect();
    let writes = futures::future::join_all(
        controls.iter().enumerate().map(|(i, c)| c.freeze_replicated(format!("op {i}"), None)),
    )
    .await;
    assert!(writes.iter().all(Result::is_ok), "{writes:?}");
    let state = store.get(&GovernanceScope::Org).await.unwrap().unwrap();
    assert_eq!(state.epoch, 4, "each freeze landed once, in some order");
    assert!(state.frozen);
}

#[tokio::test]
async fn a_watch_notification_is_applied_before_the_next_poll() {
    #[derive(Default)]
    struct Watch(tokio::sync::Notify);

    #[async_trait]
    impl GovernanceWatch for Watch {
        async fn changed(&self) -> Result<()> {
            self.0.notified().await;
            Ok(())
        }
    }

    let store: Arc<dyn GovernanceStateStore> = Arc::new(InMemoryGovernanceStateStore::new());
    let watch = Arc::new(Watch::default());
    let slow = config()
        .with_poll_interval(Duration::from_secs(60))
        .with_max_staleness(Duration::from_secs(120))
        .with_watch(watch.clone());
    let control = GovernanceControl::mirrored(Arc::clone(&store), slow);
    let mirror = control.spawn_mirror().unwrap();
    wait_until(Duration::from_secs(1), || control.check().is_ok()).await.unwrap();

    store.set(GovernanceScope::Org, 0, true, Some("paged".into()), None).await.unwrap();
    watch.0.notify_waiters();
    assert!(
        wait_until(Duration::from_secs(1), || control.is_frozen()).await.is_some(),
        "the watch did not trigger a read"
    );
    mirror.abort();
}

#[tokio::test]
async fn any_mirrored_scope_freezes_and_a_local_freeze_is_not_lifted_by_the_store() {
    let store: Arc<dyn GovernanceStateStore> = Arc::new(InMemoryGovernanceStateStore::new());
    let tenant = GovernanceScope::Tenant("acme".into());
    let control = GovernanceControl::mirrored(
        Arc::clone(&store),
        config().with_scopes([GovernanceScope::Org, tenant.clone()]),
    );
    store.set(tenant, 0, true, Some("tenant hold".into()), None).await.unwrap();
    control.refresh().await.unwrap();
    assert_eq!(control.reason().as_deref(), Some("tenant hold"));

    let other = GovernanceControl::mirrored(Arc::clone(&store), config());
    other.freeze("local only");
    other.refresh().await.unwrap();
    assert!(other.is_frozen(), "the store does not lift a local freeze");
}
