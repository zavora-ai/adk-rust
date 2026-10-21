//! No host file tool runs while a sandboxed command runs unapproved (spec 3.1 §9.2 I26).
//!
//! With `auto_allow_sandboxed` on, a sandboxed or container command can rearrange the
//! workspace — swap a file for a symlink, a directory for another — while a host file tool
//! resolves a path in it. The host file tools of the harness hold the gate shared while they
//! run, and a sandboxed `bash` holds it exclusive for the whole command, so the two never
//! overlap, in any session or team member of the harness.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Result, RunConfig, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::tools::GOVERNANCE_POLL_INTERVAL;

/// The harness-wide lock between host file tools and unapproved sandboxed commands.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileToolGate {
    lock: Arc<RwLock<()>>,
}

impl FileToolGate {
    /// `tool`, holding the gate shared while it executes.
    pub(crate) fn wrap(&self, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        Arc::new(GatedFileTool { inner: tool, gate: self.clone() })
    }

    /// Waits until no host file tool runs and holds them off; refuses once the run's kill
    /// switch is set, so a frozen run never waits on a long file operation (I20).
    pub(crate) async fn exclusive(&self, config: &RunConfig) -> Result<OwnedRwLockWriteGuard<()>> {
        let acquire = self.lock.clone().write_owned();
        tokio::pin!(acquire);
        let mut poll = tokio::time::interval(GOVERNANCE_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                guard = &mut acquire => return Ok(guard),
                _ = poll.tick() => config.check_governance()?,
            }
        }
    }

    async fn shared(&self) -> OwnedRwLockReadGuard<()> {
        self.lock.clone().read_owned().await
    }
}

/// A host file tool that holds the [`FileToolGate`] shared while it runs; everything else
/// is the inner tool's, so its declaration digest, origin and class are unchanged.
struct GatedFileTool {
    inner: Arc<dyn Tool>,
    gate: FileToolGate,
}

#[async_trait]
impl Tool for GatedFileTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn timeout_override(&self) -> Option<Option<Duration>> {
        self.inner.timeout_override()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let _shared = self.gate.shared().await;
        self.inner.execute(ctx, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gated_tool_keeps_its_declaration() {
        let ws = adk_devtools::Workspace::new(".");
        let read: Arc<dyn Tool> = Arc::new(adk_devtools::ReadFileTool::new(ws));
        let gated = FileToolGate::default().wrap(read.clone());
        assert_eq!(gated.declaration(), read.declaration());
        assert_eq!(
            adk_core::ToolOrigin::of(gated.as_ref()),
            adk_core::ToolOrigin::of(read.as_ref())
        );
        assert_eq!(
            (gated.is_read_only(), gated.is_concurrency_safe(), gated.effect()),
            (read.is_read_only(), read.is_concurrency_safe(), read.effect())
        );
    }

    #[tokio::test]
    async fn file_tools_share_the_gate_and_a_command_excludes_them() {
        let gate = FileToolGate::default();
        let first = gate.shared().await;
        let second = gate.shared().await;
        let config = RunConfig::default();
        let waiting = tokio::time::timeout(Duration::from_millis(50), gate.exclusive(&config));
        assert!(waiting.await.is_err(), "a command waits while file tools run");
        drop((first, second));
        let held = gate.exclusive(&config).await.unwrap();
        let reading = tokio::time::timeout(Duration::from_millis(50), gate.shared());
        assert!(reading.await.is_err(), "a file tool waits while a command runs");
        drop(held);
        let _ = gate.shared().await;
    }
}
