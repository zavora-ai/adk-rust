//! [`FrameSources`]: the named frame sources an agent can watch (spec 3.7 §7.8, §7.10).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use chrono::{DateTime, Utc};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::FrameSource;
use super::governor::FrameBudget;
use super::pipeline::{ObservationSink, StopCause, StreamPipeline, StreamSummary};
use super::privacy::PrivacyHook;
use super::retention::FrameKeeper;
use super::watching::StreamWatch;
use crate::audit::VISION_AUDIT_KIND;
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::media::FrameSourceId;
use crate::runtime::VisionRuntime;
use crate::traits::{DetectQuery, Sampling, Tracker};
use crate::watch::{
    VISION_WATCH_STOPPED, WatchCondition, WatchEvaluator, WatchSink, watch_registered_draft,
};

/// Makes a fresh tracker for each run of a source, so track identifiers reset with every
/// source session (V-I8).
pub type TrackerFactory = Arc<dyn Fn() -> Box<dyn Tracker> + Send + Sync>;

/// One source an operator registers for watching: the frame source and every stage its
/// stream runs, from the privacy hook to the wake sink.
///
/// An agent names the source; it never supplies one. The camera URL and its credentials stay
/// with the operator, so the source's [`FrameSourceId`] is all a tool call, a record or a
/// wake carries.
///
/// | Part | Required | What it is |
/// |------|----------|------------|
/// | Source | yes | The [`FrameSource`]: a camera, a file, a screen, a browser, the web home's camera |
/// | Privacy | yes | The [`PrivacyHook`] every frame passes first |
/// | Keeper | yes | The [`FrameKeeper`] that stores the frame of each event, in the scope the woken agent loads it from |
/// | Wakes | yes | The [`WatchSink`] events go to, such as the kernel's `VisionWakeSource` |
/// | Detector | for a watch | The runtime and query each frame is detected with |
/// | Tracker | no | A [`TrackerFactory`]; without one, regions have no tracks and no condition fires |
/// | Sampling, budget | no | As [`StreamPipelineBuilder`](super::StreamPipelineBuilder) takes them |
/// | Observer | no | A sink that sees every observation after the watch, such as the live camera surface |
///
/// # Example
///
/// ```ignore
/// let spec = SourceSpec::new(camera, Arc::new(NoRedaction), keeper, wake_source)
///     .describe("Line 1 conveyor, top camera")
///     .detector(runtime, DetectQuery::Classes(vec!["clock".into()]))
///     .tracker(Arc::new(|| Box::new(GreedyIouTracker::new(0.3, 4)) as Box<dyn Tracker>))
///     .sampling(Sampling::at(2.0));
/// sources.register(spec)?;
/// ```
#[must_use]
pub struct SourceSpec {
    source: Arc<dyn FrameSource>,
    privacy: Arc<dyn PrivacyHook>,
    keeper: Arc<FrameKeeper>,
    wakes: Arc<dyn WatchSink>,
    description: Option<String>,
    detection: Option<(Arc<VisionRuntime>, DetectQuery)>,
    tracker: Option<TrackerFactory>,
    sampling: Option<Sampling>,
    budget: Option<FrameBudget>,
    observer: Option<Arc<dyn ObservationSink>>,
}

impl std::fmt::Debug for SourceSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSpec")
            .field("source", self.source.id())
            .field("description", &self.description)
            .field("sampling", &self.sampling)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl SourceSpec {
    /// A source whose frames pass `privacy` first, whose events' frames `keeper` stores and
    /// whose events go to `wakes`.
    pub fn new(
        source: Arc<dyn FrameSource>,
        privacy: Arc<dyn PrivacyHook>,
        keeper: Arc<FrameKeeper>,
        wakes: Arc<dyn WatchSink>,
    ) -> Self {
        Self {
            source,
            privacy,
            keeper,
            wakes,
            description: None,
            detection: None,
            tracker: None,
            sampling: None,
            budget: None,
            observer: None,
        }
    }

    /// Says what the source shows, for the agent choosing one.
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Detects `query` on every frame through `runtime`, metered in frames. A watch
    /// condition may name only the labels `query` lists.
    pub fn detector(mut self, runtime: Arc<VisionRuntime>, query: DetectQuery) -> Self {
        self.detection = Some((runtime, query));
        self
    }

    /// Tracks detections with a fresh tracker from `factory` for each run.
    pub fn tracker(mut self, factory: TrackerFactory) -> Self {
        self.tracker = Some(factory);
        self
    }

    /// Samples the source at `sampling`.
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// Holds the stream to `budget`.
    pub fn budget(mut self, budget: FrameBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Hands every observation and source event to `observer` after the watch.
    pub fn observer(mut self, observer: Arc<dyn ObservationSink>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn labels(&self) -> Vec<String> {
        match &self.detection {
            Some((_, DetectQuery::Classes(labels) | DetectQuery::Concepts(labels))) => {
                labels.clone()
            }
            None => Vec::new(),
        }
    }

    /// Refuses a condition this source can never fire.
    fn check(&self, condition: &WatchCondition) -> Result<()> {
        condition.validate()?;
        let id = &self.source.id().0;
        if self.detection.is_none() {
            return Err(VisionError::InvalidInput(format!(
                "source `{id}` has no detector, so no watch condition can fire on it"
            )));
        }
        let labels = self.labels();
        let unknown: Vec<&String> = condition
            .labels
            .iter()
            .filter(|wanted| !labels.iter().any(|label| label.eq_ignore_ascii_case(wanted)))
            .collect();
        if !unknown.is_empty() {
            return Err(VisionError::InvalidInput(format!(
                "source `{id}` detects {labels:?}; the watch condition `{}` names {unknown:?}, \
                 which it never reports",
                condition.name
            )));
        }
        Ok(())
    }

    fn pipeline(&self, watch: Arc<StreamWatch>) -> Result<StreamPipeline> {
        let mut builder = StreamPipeline::builder(self.source.clone(), self.privacy.clone());
        if let Some(sampling) = self.sampling {
            builder = builder.sampling(sampling);
        }
        if let Some((runtime, query)) = &self.detection {
            builder = builder.detector(runtime.clone(), query.clone());
        }
        if let Some(factory) = &self.tracker {
            builder = builder.tracker(factory());
        }
        if let Some(budget) = &self.budget {
            builder = builder.budget(budget.clone());
        }
        builder.sink(watch).build()
    }
}

/// How a watch request changed a source.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WatchChange {
    /// The condition was registered.
    Registered,
    /// The same condition was already registered: nothing changed.
    Unchanged,
    /// The condition was removed.
    Removed,
    /// No condition of that name was registered: nothing changed.
    NotWatched,
}

/// How a source's last stream ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEnd {
    /// Why it stopped.
    pub cause: StopCause,
    /// What it did; empty when it failed before it summarised.
    pub summary: StreamSummary,
    /// When the registry saw it end.
    pub ended_at: DateTime<Utc>,
}

/// A registered source as an agent sees it: no URL, no credential.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    /// The source's name.
    pub source: FrameSourceId,
    /// What it shows, as the operator described it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The labels its detector reports, which a condition may name.
    pub labels: Vec<String>,
    /// Whether its stream is running.
    pub streaming: bool,
    /// The registered watch conditions.
    pub conditions: Vec<WatchCondition>,
    /// Events delivered by the running stream.
    pub fired: u64,
    /// How the last stream ended, when one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_end: Option<StreamEnd>,
}

/// The answer to a watch request: what changed, and the source after it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchOutcome {
    /// What changed.
    pub change: WatchChange,
    /// The source now.
    pub status: SourceStatus,
}

struct Running {
    watch: Arc<StreamWatch>,
    cancel: CancellationToken,
    task: JoinHandle<Result<StreamSummary>>,
}

struct Entry {
    spec: Arc<SourceSpec>,
    conditions: Vec<WatchCondition>,
    running: Option<Running>,
    last_end: Option<StreamEnd>,
}

impl Entry {
    /// Collects a stream that ended on its own.
    fn reap(&mut self) {
        let finished = self.running.as_ref().is_some_and(|running| running.task.is_finished());
        if !finished {
            return;
        }
        if let Some(running) = self.running.take() {
            let outcome = running.task.now_or_never();
            self.last_end = Some(end_of(outcome, false));
        }
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            source: self.spec.source.id().clone(),
            description: self.spec.description.clone(),
            labels: self.spec.labels(),
            streaming: self.running.is_some(),
            conditions: self.conditions.clone(),
            fired: self.running.as_ref().map_or(0, |running| running.watch.fired()),
            last_end: self.last_end.clone(),
        }
    }
}

type Joined = std::result::Result<Result<StreamSummary>, tokio::task::JoinError>;

fn end_of(outcome: Option<Joined>, cancelled: bool) -> StreamEnd {
    let (cause, summary) = match outcome {
        Some(Ok(Ok(summary))) if cancelled => (StopCause::Cancelled, summary),
        Some(Ok(Ok(summary))) => (StopCause::Ended, summary),
        Some(Ok(Err(err))) => (
            StopCause::Failed { code: err.code().to_string(), message: err.to_string() },
            StreamSummary::default(),
        ),
        Some(Err(err)) => (
            StopCause::Failed { code: "vision.stream.panicked".into(), message: err.to_string() },
            StreamSummary::default(),
        ),
        None => (StopCause::Cancelled, StreamSummary::default()),
    };
    StreamEnd { cause, summary, ended_at: Utc::now() }
}

/// The frame sources an operator registered, by name, and the watches running on them
/// (spec 3.7 §7.8, §7.10): the registry behind the `vision_watch` tool.
///
/// A source streams only while a watch condition is registered on it. The first condition
/// starts its [`StreamPipeline`] with a [`StreamWatch`] as the sink; later ones join the
/// running watch; removing the last one stops the stream. Each stream runs under the
/// registry's own [`VisionCx`] — its audit trail, metering scope and kill switch — with a
/// cancellation token of its own, so it outlives the tool call that started it and a freeze
/// pauses it at its next frame (V-I17). A stream that ends on its own, such as a file's last
/// frame, keeps its conditions registered; the next registration starts it again.
///
/// | Record | Written to | When |
/// |--------|------------|------|
/// | `vision.watch.registered` | the caller's trail, else the registry's | a condition is registered |
/// | `vision.watch.stopped` | the caller's trail, else the registry's | a condition is removed |
/// | `vision.source.started`, `.stopped`, `.paused`, `.resumed`, `vision.governor.step` | the registry's trail | the stream's life |
/// | `vision.evidence.kept`, `vision.watch.fired` | the registry's trail | an event's frame is kept and delivered |
///
/// # Example
///
/// ```
/// use adk_vision::VisionCx;
/// use adk_vision::stream::FrameSources;
///
/// let sources = FrameSources::new(VisionCx::new());
/// assert!(sources.statuses().is_empty());
/// ```
#[derive(Clone)]
pub struct FrameSources {
    inner: Arc<Inner>,
}

struct Inner {
    cx: VisionCx,
    entries: Mutex<BTreeMap<FrameSourceId, Entry>>,
}

impl std::fmt::Debug for FrameSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameSources").field("sources", &self.names()).finish_non_exhaustive()
    }
}

impl FrameSources {
    /// A registry whose streams run under `cx`: its audit trail, metering scope and kill
    /// switch. Cancelling `cx` stops every stream.
    pub fn new(cx: VisionCx) -> Self {
        Self { inner: Arc::new(Inner { cx, entries: Mutex::new(BTreeMap::new()) }) }
    }

    fn entries(&self) -> MutexGuard<'_, BTreeMap<FrameSourceId, Entry>> {
        self.inner.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a source under its [`FrameSource::id`].
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when a source of that name is already registered.
    pub fn register(&self, spec: SourceSpec) -> Result<()> {
        let id = spec.source.id().clone();
        let mut entries = self.entries();
        if entries.contains_key(&id) {
            return Err(VisionError::InvalidInput(format!(
                "a frame source named `{}` is already registered",
                id.0
            )));
        }
        entries.insert(
            id,
            Entry { spec: Arc::new(spec), conditions: Vec::new(), running: None, last_end: None },
        );
        Ok(())
    }

    /// The registered sources' names.
    pub fn names(&self) -> Vec<FrameSourceId> {
        self.entries().keys().cloned().collect()
    }

    /// Every source's status.
    pub fn statuses(&self) -> Vec<SourceStatus> {
        self.entries()
            .values_mut()
            .map(|entry| {
                entry.reap();
                entry.status()
            })
            .collect()
    }

    /// One source's status.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a name no source is registered under.
    pub fn status(&self, source: &FrameSourceId) -> Result<SourceStatus> {
        let mut entries = self.entries();
        let entry = entries.get_mut(source).ok_or_else(|| unknown(source))?;
        entry.reap();
        Ok(entry.status())
    }

    async fn audit(&self, cx: &VisionCx, draft: AuditRecordDraft) -> Result<()> {
        if cx.audit_trail().is_some() {
            cx.audit(draft).await
        } else {
            self.inner.cx.audit(draft).await
        }
    }

    /// Registers `condition` on `source`, starting its stream if none runs. Registering the
    /// same condition again changes nothing; a different condition under a registered name
    /// is refused.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an unknown source, an invalid condition, one naming
    /// labels the source's detector never reports, or a name already registered with other
    /// settings; the stream's build error; the audit trail's error when it fails closed.
    pub async fn start_watch(
        &self,
        cx: &VisionCx,
        source: &FrameSourceId,
        condition: WatchCondition,
    ) -> Result<WatchOutcome> {
        cx.check()?;
        let change = {
            let mut entries = self.entries();
            let entry = entries.get_mut(source).ok_or_else(|| unknown(source))?;
            entry.reap();
            entry.spec.check(&condition)?;
            let change = match entry.conditions.iter().find(|c| c.name == condition.name) {
                Some(existing) if *existing == condition => WatchChange::Unchanged,
                Some(_) => {
                    return Err(VisionError::InvalidInput(format!(
                        "a watch named `{}` is already registered on `{}` with other settings; \
                         stop it first",
                        condition.name, source.0
                    )));
                }
                None => {
                    entry.conditions.push(condition.clone());
                    WatchChange::Registered
                }
            };
            match &entry.running {
                Some(running) => {
                    if change == WatchChange::Registered {
                        running.watch.add_condition(condition.clone())?;
                    }
                }
                None => {
                    if let Err(err) = self.spawn(entry) {
                        if change == WatchChange::Registered {
                            entry.conditions.retain(|c| c.name != condition.name);
                        }
                        return Err(err);
                    }
                }
            }
            change
        };
        if change == WatchChange::Registered {
            self.audit(cx, watch_registered_draft(source, &condition)).await?;
            tracing::info!(vision.source = %source.0, vision.condition = %condition.name, "watch registered");
        }
        Ok(WatchOutcome { change, status: self.status(source)? })
    }

    /// Starts the entry's stream with its conditions. Called with the registry locked.
    fn spawn(&self, entry: &mut Entry) -> Result<()> {
        let evaluator = WatchEvaluator::new(entry.conditions.clone())?;
        let mut watch =
            StreamWatch::new(evaluator, entry.spec.keeper.clone(), entry.spec.wakes.clone());
        if let Some(observer) = &entry.spec.observer {
            watch = watch.with_observer(observer.clone());
        }
        let watch = Arc::new(watch);
        let pipeline = entry.spec.pipeline(watch.clone())?;
        let cancel = self.inner.cx.cancellation().child_token();
        let cx = self.inner.cx.clone().with_cancellation(cancel.clone());
        let task = tokio::spawn(async move { pipeline.run(&cx).await });
        entry.running = Some(Running { watch, cancel, task });
        Ok(())
    }

    /// Removes the condition named `name` from `source`; removing the last one stops the
    /// stream and waits for it to finish.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an unknown source; the audit trail's error when it
    /// fails closed.
    pub async fn stop_watch(
        &self,
        cx: &VisionCx,
        source: &FrameSourceId,
        name: &str,
    ) -> Result<WatchOutcome> {
        let (removed, stopping) = {
            let mut entries = self.entries();
            let entry = entries.get_mut(source).ok_or_else(|| unknown(source))?;
            entry.reap();
            let removed = entry
                .conditions
                .iter()
                .position(|c| c.name == name)
                .map(|index| entry.conditions.remove(index));
            if removed.is_some()
                && let Some(running) = &entry.running
            {
                running.watch.remove_condition(name);
            }
            let stopping = match entry.conditions.is_empty() {
                true => entry.running.take(),
                false => None,
            };
            (removed, stopping)
        };
        if let Some(running) = stopping {
            running.cancel.cancel();
            let outcome = running.task.await;
            let end = end_of(Some(outcome), true);
            if let Some(entry) = self.entries().get_mut(source) {
                entry.last_end = Some(end);
            }
        }
        let change = match &removed {
            Some(condition) => {
                self.audit(cx, watch_stopped_draft(source, condition)).await?;
                tracing::info!(vision.source = %source.0, vision.condition = %name, "watch stopped");
                WatchChange::Removed
            }
            None => WatchChange::NotWatched,
        };
        Ok(WatchOutcome { change, status: self.status(source)? })
    }

    /// Stops every stream and waits for each to finish; the conditions stay registered.
    pub async fn shutdown(&self) {
        let running: Vec<(FrameSourceId, Running)> = self
            .entries()
            .iter_mut()
            .filter_map(|(id, entry)| entry.running.take().map(|running| (id.clone(), running)))
            .collect();
        for (id, running) in running {
            running.cancel.cancel();
            let end = end_of(Some(running.task.await), true);
            if let Some(entry) = self.entries().get_mut(&id) {
                entry.last_end = Some(end);
            }
        }
    }
}

fn unknown(source: &FrameSourceId) -> VisionError {
    VisionError::InvalidInput(format!(
        "no frame source is registered as `{}`; list the sources with the `list` action",
        source.0
    ))
}

/// The `vision.watch.stopped` audit record of `condition` removed from `source`.
pub(crate) fn watch_stopped_draft(
    source: &FrameSourceId,
    condition: &WatchCondition,
) -> AuditRecordDraft {
    AuditRecordDraft::new(
        AuditRecordKind::Other(VISION_AUDIT_KIND.to_string()),
        VISION_WATCH_STOPPED,
    )
    .outcome(AuditRecordOutcome::Succeeded)
    .payload_ref(condition.digest().to_string())
    .ext("source", json!(source))
    .ext("condition", json!(condition.name))
    .ext("conditionDigest", json!(condition.digest()))
}
