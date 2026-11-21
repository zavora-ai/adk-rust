//! `Harness::close` reaches every toolset of a harness with members (spec 3.1 §7.9, §7.10):
//! the compiled team root forwards `close` to its coordinator, so the runner's close releases
//! the coordinator's toolsets, extra toolsets included. Last, it seals the audit log.

#![cfg(all(feature = "runner", feature = "team"))]

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::audit_log::{AuditRecordDraft, AuditRecorder};
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use adk_harness::MemberSpec;
use async_trait::async_trait;
use common::fixture;

/// A toolset that counts how often it is closed.
struct CountingToolset {
    closed: Arc<AtomicUsize>,
}

#[async_trait]
impl Toolset for CountingToolset {
    fn name(&self) -> &str {
        "counting"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(Vec::new())
    }

    async fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn closing_a_harness_with_members_closes_the_coordinator_toolsets_once() {
    let closed = Arc::new(AtomicUsize::new(0));
    let toolset = Arc::new(CountingToolset { closed: closed.clone() });
    let f = fixture(Vec::new(), |b| b.member(MemberSpec::explore()).toolset(toolset)).await;
    assert!(f.harness.agent().info().members.contains(&"explore".to_string()));

    f.harness.close().await;

    assert_eq!(closed.load(Ordering::SeqCst), 1, "the extra toolset closes once");
}

/// A recorder that keeps nothing and counts how often it is sealed.
#[derive(Default)]
struct SealCounter(AtomicUsize);

#[async_trait]
impl AuditRecorder for SealCounter {
    fn digest(&self, _value: &serde_json::Value) -> String {
        String::new()
    }

    async fn record(&self, _draft: AuditRecordDraft) -> Result<()> {
        Ok(())
    }

    async fn seal(&self) -> Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn closing_a_harness_seals_its_audit_log_once() {
    let seals = Arc::new(SealCounter::default());
    let f = fixture(Vec::new(), |b| b.audit_log(seals.clone())).await;
    assert_eq!(seals.0.load(Ordering::SeqCst), 0);

    f.harness.close().await;

    assert_eq!(seals.0.load(Ordering::SeqCst), 1);
}
