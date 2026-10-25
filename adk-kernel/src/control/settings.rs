//! Quiet time, the daily spending limit and the mode while the kernel runs (spec 3.3 §10
//! K3, §8.1), per area: a partition, or `default` on a kernel without partitions.
//!
//! | Route (under `/api`) | Caller | Does |
//! |---|---|---|
//! | `GET /kernel/settings` | operator, or a device with `read` | Every area's settings in force ([`SettingsResponse`]) |
//! | `GET /kernel/settings/{partition}` | operator, or a device with `read` | One area's ([`AreaResponse`]) |
//! | `POST /kernel/settings/{partition}/quiet-time` | the person at the kernel or the home | Sets or turns off quiet time ([`QuietTimeRequest`]) |
//! | `POST /kernel/settings/{partition}/spending-limit` | the person at the kernel or the home | Sets or removes the daily spending limit ([`SpendingLimitRequest`]) |
//! | `POST /kernel/settings/{partition}/mode` | the person at the kernel or the home | Sets how much the area's agents act alone ([`PartitionModeRequest`](super::api::PartitionModeRequest)) |
//!
//! A change is recorded as `kernel.control` (`settings.quiet_time`, `settings.spending_limit`,
//! `settings.mode`, with `from` and `to`) before it is stored in the
//! [`ControlStore`](super::ControlStore), and it replaces, for the area, the value the kernel
//! and the area's partition record started with. Admission reads it at every claim, so it
//! takes effect from the agents' next step: no proactive process of the area starts inside
//! the new quiet time, and no background process starts past the new limit. A quiet window
//! a workspace sets (`[kernel] quiet_hours` of its `.adk/harness.toml`) still holds beside
//! the person's and is listed as `alsoQuiet`.
//!
//! The changing routes need the `operator` scope and refuse a paired device's token: they
//! are for the person at the kernel's own terminal or socket, or signed in to the home, never
//! a phone's token alone or a chat channel, which reaches no kernel route.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::api::PartitionModeRequest;
use super::paths;
use super::routes::{Caller, ControlBackend, record};
use super::store::{AreaSettings, Change, ControlStore};
use crate::error::{KernelError, NOT_FOUND_CODE, Result};
use crate::kernel::Kernel;
use crate::partition::{Partition, PartitionId, PartitionMode};
use crate::sched::QuietHours;

type Backend = State<Arc<dyn ControlBackend>>;

/// One area's settings in force (spec 3.3 §10 K3), as the home's "What your agents can do"
/// shows them.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::AreaView;
///
/// let wire = serde_json::json!({
///     "partition": "default",
///     "quietTime": [{ "start": "22:00", "end": "07:00" }],
///     "dailySpendingLimit": 5_000_000,
///     "spentToday": 1_840_000,
///     "mode": "ask"
/// });
/// let area: AreaView = serde_json::from_value(wire).unwrap();
/// assert_eq!(area.daily_spending_limit, Some(5_000_000));
/// assert!(area.changed.is_none() && area.also_quiet.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AreaView {
    /// The area.
    pub partition: PartitionId,
    /// The person's quiet time: the window changed while the kernel runs, else those it
    /// started with. Empty is no quiet time.
    #[serde(default)]
    pub quiet_time: Vec<QuietHours>,
    /// Windows a workspace sets, which hold beside the person's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_quiet: Vec<QuietHours>,
    /// The daily spending limit in micro-USD; `None` is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_spending_limit: Option<u64>,
    /// Spent in the area today (UTC day), in micro-USD.
    #[serde(default)]
    pub spent_today: u64,
    /// How much the area's agents act alone; `None` when the kernel was not told the mode
    /// its agents started in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PartitionMode>,
    /// What the person changed while the kernel ran, with when and by whom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<AreaSettings>,
}

/// `GET /kernel/settings` answer (spec 3.3 §10 K3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResponse {
    /// Every area, ordered by partition id.
    pub areas: Vec<AreaView>,
}

/// The answer of a settings read or change: the area after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AreaResponse {
    /// The area.
    pub area: AreaView,
}

/// `POST /kernel/settings/{partition}/quiet-time` body (spec 3.3 §10 K3): the new window,
/// or `null` for no quiet time.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::QuietTimeRequest;
///
/// let body: QuietTimeRequest = serde_json::from_str(r#"{"window":{"start":"23:00","end":"07:00"}}"#).unwrap();
/// assert!(body.window.is_some());
/// let off: QuietTimeRequest = serde_json::from_str(r#"{"window":null}"#).unwrap();
/// assert!(off.window.is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietTimeRequest {
    /// The window; `None` turns quiet time off.
    pub window: Option<QuietHours>,
}

/// `POST /kernel/settings/{partition}/spending-limit` body (spec 3.3 §10 K3): the new limit
/// in micro-USD, or `null` for no limit.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::SpendingLimitRequest;
///
/// let body = SpendingLimitRequest { micro_usd: Some(5_000_000) };
/// assert_eq!(serde_json::to_value(&body).unwrap()["microUsd"], 5_000_000);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendingLimitRequest {
    /// The limit per UTC day; `None` removes it.
    pub micro_usd: Option<u64>,
}

/// What an area started with, before any change: the values a change replaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Start {
    /// The kernel's quiet hours and the partition record's own.
    pub(crate) quiet: Vec<QuietHours>,
    /// The workspace's window, which a change never replaces.
    pub(crate) beside: Vec<QuietHours>,
    /// The lower of the kernel's daily spend cap and the partition record's.
    pub(crate) cap: Option<u64>,
    /// The partition record's mode, else the one the area's agents started in.
    pub(crate) mode: Option<PartitionMode>,
}

/// The quiet windows in force in an area: the changed window in place of `kernel` and the
/// partition record's, then the workspace's `beside` (spec 3.3 §10 K3).
pub(crate) fn quiet_windows(
    kernel: Option<QuietHours>,
    beside: Option<QuietHours>,
    record: Option<&Partition>,
    changed: Option<&AreaSettings>,
) -> Vec<QuietHours> {
    let own: Vec<QuietHours> = match changed.and_then(|settings| settings.quiet_time.as_ref()) {
        Some(change) => change.value.into_iter().collect(),
        None => kernel.into_iter().chain(record.and_then(|record| record.quiet_hours)).collect(),
    };
    own.into_iter().chain(beside).collect()
}

/// The daily spend cap in force in an area: the changed limit in place of `start`, the
/// lower of the kernel's and the partition record's (spec 3.3 §10 K3).
pub(crate) fn spend_cap(start: Option<u64>, changed: Option<&AreaSettings>) -> Option<u64> {
    match changed.and_then(|settings| settings.daily_spending_limit.as_ref()) {
        Some(change) => change.value,
        None => start,
    }
}

/// The routes, relative to `/api`, over the backend in their state.
pub(super) fn routes() -> Router<Arc<dyn ControlBackend>> {
    Router::new()
        .route(paths::SETTINGS, get(list))
        .route(paths::AREA_SETTINGS, get(one))
        .route(paths::QUIET_TIME, post(set_quiet_time))
        .route(paths::SPENDING_LIMIT, post(set_spending_limit))
        .route(paths::AREA_MODE, post(set_mode))
}

/// The kernel behind `backend`, or a `Config` error naming why there is none.
fn kernel_of(backend: &dyn ControlBackend) -> Result<&Kernel> {
    backend.kernel().ok_or_else(|| KernelError::Config {
        reason: "the settings routes read a kernel's admission settings; serve them from a \
                 Kernel"
            .into(),
    })
}

/// Refuses a change from anyone but the person at the kernel or signed in to the home: the
/// `operator` scope, and no paired device's token (spec 3.3 §10 K3, S-I7).
pub(super) fn require_owner(caller: &Caller, action: &str) -> Result<()> {
    caller.require_operator(action)?;
    if caller.context.metadata.contains_key(crate::pairing::DEVICE_ID_METADATA) {
        return Err(crate::pairing::forbidden(format!(
            "a paired device may not {action}: change it in the home or on the kernel's own \
             terminal"
        )));
    }
    Ok(())
}

/// The areas of `kernel`: `default` when the partition table is empty or names it, every
/// partition in the table, and every area with a change.
async fn areas(kernel: &Kernel, store: &dyn ControlStore) -> Result<Vec<PartitionId>> {
    let partitions = kernel.partitions().list().await?;
    let mut ids: BTreeSet<PartitionId> = partitions.into_iter().map(|p| p.id).collect();
    if ids.is_empty() {
        ids.insert(PartitionId::default());
    }
    ids.extend(store.list_area_settings().await?.into_iter().map(|s| s.partition));
    Ok(ids.into_iter().collect())
}

/// `id` when it names an area of `kernel`, else a `404` naming the areas there are.
async fn known_area(kernel: &Kernel, store: &dyn ControlStore, id: String) -> Result<PartitionId> {
    let id = PartitionId::new(id);
    let known = areas(kernel, store).await?;
    if known.contains(&id) || id.as_str() == PartitionId::DEFAULT {
        return Ok(id);
    }
    let names = known.iter().map(PartitionId::as_str).collect::<Vec<_>>().join(", ");
    Err(KernelError::Core(adk_core::AdkError::new(
        adk_core::ErrorComponent::Server,
        adk_core::ErrorCategory::NotFound,
        NOT_FOUND_CODE,
        format!("no area `{id}` on this kernel; its areas are {names}"),
    )))
}

/// What `partition` started with on `kernel`.
pub(crate) async fn start_of(kernel: &Kernel, partition: &PartitionId) -> Result<Start> {
    let config = kernel.config();
    let record = kernel.partitions().get(partition).await?;
    let quiet = config
        .quiet_hours
        .into_iter()
        .chain(record.as_ref().and_then(|record| record.quiet_hours))
        .collect();
    let cap = match &record {
        Some(record) => config.quotas.stricter(&record.quotas).daily_spend_cap,
        None => config.quotas.daily_spend_cap,
    };
    let mode = record
        .as_ref()
        .map(|record| record.default_mode)
        .or_else(|| config.control.start_modes.get(partition).copied());
    Ok(Start { quiet, beside: config.proactivity.quiet_hours.into_iter().collect(), cap, mode })
}

/// Wakes `partition`'s processes admission put to sleep for `reason` (`quiet_hours` or
/// `budget`), so admission decides on them again under a changed setting: one still held
/// sleeps again, until the new end of quiet time or a budget with room.
async fn readmit(kernel: &Kernel, partition: &PartitionId, reason: &str) {
    let sup = kernel.supervision();
    let filter = crate::stores::ProcessFilter::default().partition(partition.clone());
    let metas = match sup.processes.list(&filter).await {
        Ok(metas) => metas,
        Err(error) => {
            tracing::warn!(partition = %partition, error = %error, "could not list the processes a changed setting releases");
            return;
        }
    };
    let mut woken = 0;
    for mut meta in metas {
        if meta.labels.get(crate::supervise::ADMISSION_LABEL).map(String::as_str) != Some(reason)
            || meta.suspended_by.is_some()
        {
            continue;
        }
        let Ok(Some(run)) = sup.runs.get(&meta.pid).await else { continue };
        if run.state != adk_core::RunState::Waiting {
            continue;
        }
        meta.wake = None;
        meta.labels.remove(crate::supervise::ADMISSION_LABEL);
        sup.put_meta(meta).await;
        match sup.runs.wake(&run.run_id).await {
            Ok(()) => woken += 1,
            Err(error) => {
                tracing::warn!(process.pid = %run.run_id, error = %error, "could not wake a process a changed setting releases")
            }
        }
    }
    if woken > 0 {
        sup.nudge();
    }
}

/// The settings in force in `partition` on `kernel`.
pub(crate) async fn view(
    kernel: &Kernel,
    store: &dyn ControlStore,
    partition: &PartitionId,
) -> Result<AreaView> {
    let start = start_of(kernel, partition).await?;
    let changed = store.area_settings(partition).await?;
    let quiet_time = match changed.as_ref().and_then(|settings| settings.quiet_time.as_ref()) {
        Some(change) => change.value.into_iter().collect(),
        None => start.quiet.clone(),
    };
    let daily_spending_limit = spend_cap(start.cap, changed.as_ref());
    let mode = changed.as_ref().and_then(|settings| settings.mode.as_ref()).map(|c| c.value);
    let sup = kernel.supervision();
    let today = sup.clock.now().date_naive();
    let scope = crate::stores::SpendScope::Partition(partition.clone());
    Ok(AreaView {
        partition: partition.clone(),
        quiet_time,
        also_quiet: start.beside,
        daily_spending_limit,
        spent_today: sup.processes.spend(&scope, today, today).await?,
        mode: mode.or(start.mode),
        changed,
    })
}

async fn list(State(backend): Backend, caller: Caller) -> Result<Json<SettingsResponse>> {
    caller.require_operator("read the settings")?;
    let kernel = kernel_of(backend.as_ref())?;
    let store = kernel.config().control.control_store.clone();
    let mut views = Vec::new();
    for area in areas(kernel, store.as_ref()).await? {
        views.push(view(kernel, store.as_ref(), &area).await?);
    }
    Ok(Json(SettingsResponse { areas: views }))
}

async fn one(
    State(backend): Backend,
    caller: Caller,
    UrlPath(partition): UrlPath<String>,
) -> Result<Json<AreaResponse>> {
    caller.require_operator("read the settings")?;
    let kernel = kernel_of(backend.as_ref())?;
    let store = kernel.config().control.control_store.clone();
    let partition = known_area(kernel, store.as_ref(), partition).await?;
    Ok(Json(AreaResponse { area: view(kernel, store.as_ref(), &partition).await? }))
}

/// What a change of one setting does to an area's record: given the record, the area
/// before, who changes it and when, it sets the change and answers what the setting was and
/// what it becomes, for the record of the change.
type Apply = Box<
    dyn FnOnce(
            &mut AreaSettings,
            &AreaView,
            String,
            chrono::DateTime<chrono::Utc>,
        ) -> (serde_json::Value, serde_json::Value)
        + Send,
>;

/// Records `action` with the setting's value before and after, stores the change `apply`
/// makes to the area's record, wakes the processes admission held for `readmit` so they
/// meet the change at their next step, and answers with the area after it.
async fn change(
    backend: &dyn ControlBackend,
    caller: &Caller,
    partition: String,
    action: &str,
    readmit_held: Option<&str>,
    apply: Apply,
) -> Result<Json<AreaResponse>> {
    require_owner(
        caller,
        &format!("change {}", action.trim_start_matches("settings.").replace('_', " ")),
    )?;
    let kernel = kernel_of(backend)?;
    let store = kernel.config().control.control_store.clone();
    let partition = known_area(kernel, store.as_ref(), partition).await?;
    let before = view(kernel, store.as_ref(), &partition).await?;
    let mut settings = store
        .area_settings(&partition)
        .await?
        .unwrap_or_else(|| AreaSettings::new(partition.clone()));
    let (from, to) =
        apply(&mut settings, &before, caller.context.user_id.clone(), kernel.config().clock.now());
    record(backend, caller, action, Some(partition.as_str()), &[("from", from), ("to", to)])
        .await?;
    store.put_area_settings(settings).await?;
    if let Some(reason) = readmit_held {
        readmit(kernel, &partition, reason).await;
    }
    Ok(Json(AreaResponse { area: view(kernel, store.as_ref(), &partition).await? }))
}

async fn set_quiet_time(
    State(backend): Backend,
    caller: Caller,
    UrlPath(partition): UrlPath<String>,
    Json(request): Json<QuietTimeRequest>,
) -> Result<Json<AreaResponse>> {
    if let Some(window) = request.window
        && window.start == window.end
    {
        return Err(KernelError::Config {
            reason: "quiet time needs a start and an end that differ; send `null` to turn it off"
                .into(),
        });
    }
    let apply: Apply = Box::new(move |settings, before, by, at| {
        settings.quiet_time = Some(Change::new(request.window, by, at));
        (json!(before.quiet_time), json!(request.window))
    });
    change(backend.as_ref(), &caller, partition, "settings.quiet_time", Some("quiet_hours"), apply)
        .await
}

async fn set_spending_limit(
    State(backend): Backend,
    caller: Caller,
    UrlPath(partition): UrlPath<String>,
    Json(request): Json<SpendingLimitRequest>,
) -> Result<Json<AreaResponse>> {
    let apply: Apply = Box::new(move |settings, before, by, at| {
        settings.daily_spending_limit = Some(Change::new(request.micro_usd, by, at));
        (json!(before.daily_spending_limit), json!(request.micro_usd))
    });
    change(backend.as_ref(), &caller, partition, "settings.spending_limit", Some("budget"), apply)
        .await
}

async fn set_mode(
    State(backend): Backend,
    caller: Caller,
    UrlPath(partition): UrlPath<String>,
    Json(request): Json<PartitionModeRequest>,
) -> Result<Json<AreaResponse>> {
    let apply: Apply = Box::new(move |settings, before, by, at| {
        settings.mode = Some(Change::new(request.mode, by, at));
        (json!(before.mode), json!(request.mode))
    });
    change(backend.as_ref(), &caller, partition, "settings.mode", None, apply).await
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn window(text: &str) -> QuietHours {
        text.parse().unwrap()
    }

    #[test]
    fn a_changed_window_replaces_the_start_windows_and_keeps_the_workspace_one() {
        let kernel = Some(window("22:00-07:00"));
        let beside = Some(window("12:00-13:00"));
        let record = Partition::new("work", "work").with_quiet_hours(window("19:00-08:00"));
        assert_eq!(
            quiet_windows(kernel, beside, Some(&record), None),
            [window("22:00-07:00"), window("19:00-08:00"), window("12:00-13:00")]
        );
        let changed = AreaSettings::new("work").with_quiet_time(Change::new(
            Some(window("23:00-06:00")),
            "ana",
            Utc::now(),
        ));
        assert_eq!(
            quiet_windows(kernel, beside, Some(&record), Some(&changed)),
            [window("23:00-06:00"), window("12:00-13:00")]
        );
        let off = AreaSettings::new("work").with_quiet_time(Change::new(None, "ana", Utc::now()));
        assert_eq!(
            quiet_windows(kernel, beside, Some(&record), Some(&off)),
            [window("12:00-13:00")]
        );
    }

    #[test]
    fn a_changed_limit_replaces_the_start_cap() {
        assert_eq!(spend_cap(Some(5), None), Some(5));
        let raised = AreaSettings::new("default").with_daily_spending_limit(Change::new(
            Some(9),
            "ana",
            Utc::now(),
        ));
        assert_eq!(spend_cap(Some(5), Some(&raised)), Some(9));
        let removed = AreaSettings::new("default").with_daily_spending_limit(Change::new(
            None,
            "ana",
            Utc::now(),
        ));
        assert_eq!(spend_cap(Some(5), Some(&removed)), None);
        let mode_only = AreaSettings::new("default").with_mode(Change::new(
            PartitionMode::Auto,
            "ana",
            Utc::now(),
        ));
        assert_eq!(spend_cap(Some(5), Some(&mode_only)), Some(5), "another change keeps the cap");
    }
}
