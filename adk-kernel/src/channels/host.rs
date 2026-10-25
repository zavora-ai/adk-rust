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
use crate::sched::QuietHours;

/// The end of the quiet time of `windows` that holds `at`, followed through windows that
/// touch, or `None` when none holds it.
fn quiet_end(windows: &[QuietHours], at: DateTime<Utc>) -> Option<DateTime<Utc>> {
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

/// Stops the turns of the chat account `account` on `channel` that have not finished, those
/// queued behind its thread's session included: a removed account's messages reach no
/// agent (spec 3.3 §10 K1, S-I6). Returns how many it stopped.
pub(crate) async fn stop_account_turns(kernel: &Kernel, channel: &str, account: &str) -> usize {
    let open =
        [adk_core::RunState::Queued, adk_core::RunState::Running, adk_core::RunState::Waiting];
    let filter =
        adk_core::RunFilter::default().app_name(kernel.config().app_name.clone()).states(open);
    let runs = match kernel.processes().run_store().list(filter).await {
        Ok(runs) => runs,
        Err(error) => {
            tracing::warn!(channel.name = channel, error = %error, "could not list a removed chat account's turns");
            return 0;
        }
    };
    let mut stopped = 0;
    for run in runs {
        let Some(origin) = super::TurnOrigin::of(&run) else { continue };
        if origin.channel != channel || origin.account != account {
            continue;
        }
        match kernel.processes().kill(&run.run_id, "the chat account was removed").await {
            Ok(()) => stopped += 1,
            Err(error) => {
                tracing::warn!(process.pid = %run.run_id, error = %error, "could not stop a removed chat account's turn");
            }
        }
    }
    stopped
}

#[async_trait]
impl ChannelHost for Kernel {
    fn app_name(&self) -> &str {
        &self.config().app_name
    }

    fn now(&self) -> DateTime<Utc> {
        self.config().clock.now()
    }

    /// The end of the kernel's quiet hours and its proactivity policy's, whichever hold
    /// `at`, followed through windows that touch: the windows the kernel started with.
    fn quiet_until(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let config = self.config();
        let windows: Vec<QuietHours> = config
            .quiet_hours
            .iter()
            .chain(config.proactivity.quiet_hours.iter())
            .copied()
            .collect();
        quiet_end(&windows, at)
    }

    /// The end of the quiet time in force in the decision's area that holds `at`: the
    /// person's window changed while the kernel runs in place of the start ones, beside the
    /// workspace's (spec 3.3 §10 K3). The area is the partition of the process that holds the
    /// call, else `default`.
    async fn quiet_ends(
        &self,
        request: &ApprovalRequest,
        at: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let sup = self.supervision();
        let area = match &request.run_id {
            Some(pid) => sup.processes.get(pid).await.ok().flatten().map(|meta| meta.partition),
            None => None,
        };
        let windows = sup.quiet_windows(&area.unwrap_or_default()).await;
        quiet_end(&windows, at)
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

    /// Whether a mounted driver declares the called tool of a class nothing may allow
    /// without a person (spec 3.2 K-I9).
    fn rates_high(&self, request: &ApprovalRequest) -> bool {
        crate::control::rated_high(Some(self), request)
    }

    fn audit_trail(&self) -> Option<AuditTrail> {
        Kernel::audit_trail(self).cloned()
    }

    async fn turn(&self, pid: &str) -> Result<Option<RunRecord>> {
        Ok(self.processes().run_store().get(pid).await?)
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
