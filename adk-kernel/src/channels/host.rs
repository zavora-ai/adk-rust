//! The [`Kernel`] as a [`ChannelHost`] (spec 3.3 §10 K1).

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{ApprovalRequest, ApprovalService, NewRun, RunRecord};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{BoxStream, StreamExt};

use super::ChannelHost;
use crate::bus::KernelEvent;
use crate::error::Result;
use crate::kernel::Kernel;
use crate::process::{ProcessKind, ProcessSpec};

#[async_trait]
impl ChannelHost for Kernel {
    fn app_name(&self) -> &str {
        &self.config().app_name
    }

    fn now(&self) -> DateTime<Utc> {
        self.config().clock.now()
    }

    /// The end of the kernel's quiet hours and its proactivity policy's, whichever hold
    /// `at`, followed through windows that touch.
    fn quiet_until(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let config = self.config();
        let windows: Vec<_> =
            config.quiet_hours.iter().chain(config.proactivity.quiet_hours.iter()).collect();
        let mut until = at;
        // Two windows release into each other at most once each.
        for _ in 0..=windows.len() {
            match windows
                .iter()
                .filter(|quiet| quiet.contains(until))
                .map(|quiet| quiet.release_at(until))
                .max()
            {
                Some(next) if next > until => until = next,
                _ => break,
            }
        }
        (until > at).then_some(until)
    }

    fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
        self.approval_service().cloned()
    }

    /// Whether a mounted driver declares the called tool `financial` (spec 3.2 K-I9); a
    /// name two drivers share counts when either declares it so.
    fn moves_money(&self, request: &ApprovalRequest) -> bool {
        #[cfg(feature = "drivers")]
        if let Some(drivers) = Kernel::drivers(self) {
            return drivers.mounted().iter().filter_map(|id| drivers.record(id)).any(|record| {
                record.manifest.class_of(&request.tool_name)
                    == crate::drivers::EffectClass::Financial
            });
        }
        let _ = request;
        false
    }

    fn audit_trail(&self) -> Option<AuditTrail> {
        Kernel::audit_trail(self).cloned()
    }

    async fn start_turn(&self, run: NewRun) -> Result<String> {
        let process =
            self.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
        Ok(process.meta.pid)
    }

    /// The bus's finished processes whose runs a channel started, read from the run store.
    async fn finished_turns(&self) -> Result<BoxStream<'static, RunRecord>> {
        let runs = self.processes().run_store().clone();
        let events = self.events().into_stream();
        Ok(events
            .filter_map(move |record| {
                let runs = runs.clone();
                async move {
                    let KernelEvent::Process { pid, to, .. } = record.event else { return None };
                    if !to.is_terminal() {
                        return None;
                    }
                    let run = runs.get(&pid).await.ok().flatten()?;
                    super::TurnOrigin::of(&run).map(|_| run)
                }
            })
            .boxed())
    }

    async fn stop_all(&self, reason: &str, by: AuditActor) -> Result<()> {
        Kernel::freeze(self, reason, by).await
    }
}
