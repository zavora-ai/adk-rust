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
//! A change holds while the area still starts with the value it replaced
//! ([`AreaStart`](super::AreaStart)): a different start value — a later `adk-rust up --mode`,
//! `adk-rust partitions set-mode` followed by a restart, or quiet hours and a cap set at
//! start — is newer and takes over, and the change is dropped at the area's next change.
//!
//! | Mode change | When it applies |
//! |---|---|
//! | Stricter than the area's mode | At every agent's next step; it also holds the overnight tidy-up and the kernel's questions back |
//! | Looser | Refused while an agent of the area is working, so no turn changes its rules midway (spec 3.1 I14); a session whose mode was set on its own, or tightened by the harness, keeps its mode |
//! | To a mode the kernel refuses | Refused, saying why: `adk-rust up` refuses Go ahead beyond loopback without `--sandbox` |
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
use super::store::{AreaSettings, AreaStart, Change, ControlStore};
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
    /// The mode the area's agents started in, else the partition record's.
    pub(crate) mode: Option<PartitionMode>,
}

impl Start {
    /// The start values a change records it replaced.
    pub(crate) fn area(&self) -> AreaStart {
        AreaStart::new(self.quiet.clone(), self.cap, self.mode)
    }
}

/// How strict `mode` is: 0 for Go ahead, up to 3 for Just look.
fn strictness(mode: PartitionMode) -> u8 {
    match mode {
        PartitionMode::Auto => 0,
        PartitionMode::Ask => 1,
        PartitionMode::Plan => 2,
        PartitionMode::ReadOnly => 3,
    }
}

/// The stricter of `a` and `b`.
pub(crate) fn stricter(a: PartitionMode, b: PartitionMode) -> PartitionMode {
    if strictness(b) > strictness(a) { b } else { a }
}

/// The quiet windows in force in an area: the changed window in place of `kernel` and the
/// partition record's, while the area still starts with those, then the workspace's
/// `beside` (spec 3.3 §10 K3).
pub(crate) fn quiet_windows(
    kernel: Option<QuietHours>,
    beside: Option<QuietHours>,
    record: Option<&Partition>,
    changed: Option<&AreaSettings>,
) -> Vec<QuietHours> {
    let start: Vec<QuietHours> =
        kernel.into_iter().chain(record.and_then(|record| record.quiet_hours)).collect();
    let own = match changed.and_then(|settings| settings.quiet_time_over(&start)) {
        Some(change) => change.value.into_iter().collect(),
        None => start,
    };
    own.into_iter().chain(beside).collect()
}

/// The quiet windows that were in force in an area at `at`, as its changes record them
/// (spec 3.3 §5.1): the person's quiet time set at or before `at`, else the start windows,
/// then the workspace's.
#[cfg(feature = "proc")]
pub(crate) fn quiet_windows_at(
    start: &Start,
    changed: Option<&AreaSettings>,
    at: chrono::DateTime<chrono::Utc>,
) -> Vec<QuietHours> {
    let current = changed.and_then(|settings| settings.quiet_time.as_ref());
    let stale = changed.is_some_and(|settings| {
        settings.quiet_time.is_some() && settings.quiet_time_over(&start.quiet).is_none()
    });
    let own = match changed.and_then(|settings| settings.quiet_time_at(at)) {
        // A change the area's start has replaced since held until a restart the record does
        // not date: the start windows judge what came after it.
        Some(change) if stale && Some(change) == current => start.quiet.clone(),
        Some(change) => change.value.into_iter().collect(),
        None => start.quiet.clone(),
    };
    own.into_iter().chain(start.beside.iter().copied()).collect()
}

/// The daily spend cap in force in an area: the changed limit in place of `start`, the
/// lower of the kernel's and the partition record's, while the area still starts with it
/// (spec 3.3 §10 K3).
pub(crate) fn spend_cap(start: Option<u64>, changed: Option<&AreaSettings>) -> Option<u64> {
    match changed.and_then(|settings| settings.daily_spending_limit_over(start)) {
        Some(change) => change.value,
        None => start,
    }
}

/// The mode the person changed for an area, while its agents still start in the mode it
/// replaced and the kernel does not refuse it (spec 3.3 §10 K3).
pub(crate) fn changed_mode(
    start: Option<PartitionMode>,
    changed: Option<&AreaSettings>,
    control: &super::ControlPlaneConfig,
) -> Option<PartitionMode> {
    changed
        .and_then(|settings| settings.mode_over(start))
        .map(|change| change.value)
        .filter(|mode| control.refused_mode(*mode).is_none())
}

/// The changes of `settings` the area still starts with the values of, as the person sees
/// them: `None` when none is left.
fn in_force(settings: AreaSettings, start: &AreaStart) -> Option<AreaSettings> {
    let mut shown = AreaSettings::new(settings.partition.clone());
    shown.quiet_time = settings.quiet_time_over(&start.quiet_time).cloned();
    shown.daily_spending_limit =
        settings.daily_spending_limit_over(start.daily_spending_limit).cloned();
    shown.mode = settings.mode_over(start.mode).cloned();
    let any =
        shown.quiet_time.is_some() || shown.daily_spending_limit.is_some() || shown.mode.is_some();
    any.then_some(shown)
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
    // The mode the area's harness started in governs it until the next start, whatever the
    // partition record says now.
    let mode = config
        .control
        .start_modes
        .get(partition)
        .copied()
        .or_else(|| record.as_ref().map(|record| record.default_mode));
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
        let held = meta.clone();
        meta.wake = None;
        meta.labels.remove(crate::supervise::ADMISSION_LABEL);
        sup.put_meta(meta).await;
        match sup.runs.wake(&run.run_id).await {
            Ok(()) => woken += 1,
            Err(error) => {
                tracing::warn!(process.pid = %run.run_id, error = %error, "could not wake a process a changed setting releases");
                // Its wake set and label stay, so its wake time or the next change wakes it.
                sup.put_meta(held).await;
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
    let quiet_time =
        match changed.as_ref().and_then(|settings| settings.quiet_time_over(&start.quiet)) {
            Some(change) => change.value.into_iter().collect(),
            None => start.quiet.clone(),
        };
    let daily_spending_limit = spend_cap(start.cap, changed.as_ref());
    let mode = changed_mode(start.mode, changed.as_ref(), &kernel.config().control);
    let changed = changed.and_then(|settings| in_force(settings, &start.area()));
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

/// What a change of one setting does to an area's record: it sets the change, made by
/// `by` at `at`.
type Set = Box<dyn Fn(&mut AreaSettings, &str, chrono::DateTime<chrono::Utc>) + Send + Sync>;

/// A change of one setting.
struct Setting {
    /// The record's action, such as `settings.quiet_time`.
    action: &'static str,
    /// The admission label of the processes it releases, which meet it at their next step.
    readmit: Option<&'static str>,
    /// The setting's value in the area before.
    from: fn(&AreaView) -> serde_json::Value,
    /// Its value after.
    to: serde_json::Value,
    /// The mode it sets, which [`check_mode`] admits first.
    mode: Option<PartitionMode>,
    /// What it does to the area's record.
    set: Set,
}

/// Whether an agent of `partition` is working now: a process of the area, not one of the
/// kernel's own, is running.
async fn working(kernel: &Kernel, partition: &PartitionId) -> Result<bool> {
    let sup = kernel.supervision();
    let filter = adk_core::RunFilter::default()
        .app_name(kernel.config().app_name.clone())
        .states([adk_core::RunState::Running]);
    let running = sup.runs.list(filter).await?;
    for run in running.iter().filter(|run| !run.target.starts_with("kernel.")) {
        let area = sup.processes.get(&run.run_id).await?.map(|meta| meta.partition);
        if area.unwrap_or_default() == *partition {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuses a mode the kernel refuses, and a looser mode than `before`, the area's, while an
/// agent of the area is working: a loosening applies from an agent's next turn, never midway
/// through one (spec 3.1 I14, spec 3.3 §10 K3).
async fn check_mode(
    kernel: &Kernel,
    partition: &PartitionId,
    before: Option<PartitionMode>,
    mode: PartitionMode,
) -> Result<()> {
    if let Some(reason) = kernel.config().control.refused_mode(mode) {
        return Err(KernelError::Config { reason: reason.to_string() });
    }
    let loosens = match before {
        Some(before) => strictness(mode) < strictness(before),
        None => mode != PartitionMode::ReadOnly,
    };
    if loosens && working(kernel, partition).await? {
        return Err(KernelError::Core(adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::Conflict,
            crate::error::INVALID_STATE_CODE,
            format!(
                "an agent in area `{partition}` is working: let its agents do more once it \
                 finishes, so no turn changes its rules midway"
            ),
        )));
    }
    Ok(())
}

/// Records the change with the setting's value before and after, stores it in the area's
/// record, after dropping the changes the area's start values have replaced, wakes the
/// processes admission held for it so they meet it at their next step, and answers with the
/// area after it.
async fn change(
    backend: &dyn ControlBackend,
    caller: &Caller,
    partition: String,
    setting: Setting,
) -> Result<Json<AreaResponse>> {
    require_owner(
        caller,
        &format!("change {}", setting.action.trim_start_matches("settings.").replace('_', " ")),
    )?;
    let kernel = kernel_of(backend)?;
    let store = kernel.config().control.control_store.clone();
    let partition = known_area(kernel, store.as_ref(), partition).await?;
    let before = view(kernel, store.as_ref(), &partition).await?;
    if let Some(mode) = setting.mode {
        check_mode(kernel, &partition, before.mode, mode).await?;
    }
    let extra = [("from", (setting.from)(&before)), ("to", setting.to.clone())];
    record(backend, caller, setting.action, Some(partition.as_str()), &extra).await?;
    let start = start_of(kernel, &partition).await?.area();
    let (by, at) = (caller.context.user_id.as_str(), kernel.config().clock.now());
    let set = &setting.set;
    store
        .update_area_settings(&partition, &|settings: &mut AreaSettings| {
            settings.settle(&start);
            set(settings, by, at);
        })
        .await?;
    if let Some(reason) = setting.readmit {
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
    let window = request.window;
    if let Some(window) = window
        && window.start == window.end
    {
        return Err(KernelError::Config {
            reason: "quiet time needs a start and an end that differ; send `null` to turn it off"
                .into(),
        });
    }
    let setting = Setting {
        action: "settings.quiet_time",
        readmit: Some("quiet_hours"),
        from: |before| json!(before.quiet_time),
        to: json!(window),
        mode: None,
        set: Box::new(move |settings, by, at| {
            settings.change_quiet_time(Change::new(window, by, at));
        }),
    };
    change(backend.as_ref(), &caller, partition, setting).await
}

async fn set_spending_limit(
    State(backend): Backend,
    caller: Caller,
    UrlPath(partition): UrlPath<String>,
    Json(request): Json<SpendingLimitRequest>,
) -> Result<Json<AreaResponse>> {
    let limit = request.micro_usd;
    let setting = Setting {
        action: "settings.spending_limit",
        readmit: Some("budget"),
        from: |before| json!(before.daily_spending_limit),
        to: json!(limit),
        mode: None,
        set: Box::new(move |settings, by, at| {
            settings.daily_spending_limit = Some(Change::new(limit, by, at));
        }),
    };
    change(backend.as_ref(), &caller, partition, setting).await
}

async fn set_mode(
    State(backend): Backend,
    caller: Caller,
    UrlPath(partition): UrlPath<String>,
    Json(request): Json<PartitionModeRequest>,
) -> Result<Json<AreaResponse>> {
    let mode = request.mode;
    let setting = Setting {
        action: "settings.mode",
        readmit: None,
        from: |before| json!(before.mode),
        to: json!(mode),
        mode: Some(mode),
        set: Box::new(move |settings, by, at| settings.mode = Some(Change::new(mode, by, at))),
    };
    change(backend.as_ref(), &caller, partition, setting).await
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

    /// A window kept in a named time zone keeps its local times when the clocks change,
    /// where a fixed offset moves it by an hour (spec 3.3 §10 K3).
    #[test]
    fn a_window_in_a_time_zone_follows_daylight_saving_time() {
        use chrono::TimeZone;

        let body: QuietTimeRequest = serde_json::from_value(json!({
            "window": { "start": "22:00", "end": "07:00", "timeZone": "Europe/London" }
        }))
        .unwrap();
        let zoned = body.window.unwrap();
        assert_eq!(serde_json::to_value(zoned).unwrap()["timeZone"], "Europe/London");
        // Saved in summer from a browser at UTC+1.
        let fixed = window("22:00-07:00").with_utc_offset_minutes(60);
        let summer = Utc.with_ymd_and_hms(2026, 7, 1, 21, 30, 0).unwrap();
        let winter = Utc.with_ymd_and_hms(2026, 12, 1, 21, 30, 0).unwrap();
        assert!(zoned.contains(summer) && fixed.contains(summer), "22:30 in London");
        assert!(!zoned.contains(winter), "21:30 in London: not quiet yet");
        assert!(fixed.contains(winter), "a fixed offset has moved the window by an hour");
        assert_eq!(
            zoned.release_at(Utc.with_ymd_and_hms(2026, 12, 2, 6, 0, 0).unwrap()),
            Utc.with_ymd_and_hms(2026, 12, 2, 7, 0, 0).unwrap()
        );
        let error = serde_json::from_value::<QuietTimeRequest>(json!({
            "window": { "start": "22:00", "end": "07:00", "timeZone": "Mars/Olympus" }
        }))
        .unwrap_err();
        assert!(error.to_string().contains("IANA"), "{error}");
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
