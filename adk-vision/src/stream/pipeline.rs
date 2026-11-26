//! [`StreamPipeline`]: source, sampler, governor, privacy, detector, tracker and sink.

use std::sync::{Arc, Mutex};

use adk_core::ErrorCategory;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::FrameSource;
use super::governor::{
    EscalationDecision, EscalationVerdict, FrameBudget, FrameGovernor, FrameVerdict, GovernorState,
    GovernorStep,
};
use super::privacy::{FramePrivacy, PrivacyHook, RedactedFrame};
use super::records::{
    VISION_SOURCE_PAUSED, VISION_SOURCE_RESUMED, VISION_SOURCE_STARTED, VISION_SOURCE_STOPPED,
    governor_drafts, source_draft,
};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::{Region, TrackId};
use crate::media::{FrameMeta, FrameSourceId, SourceImage};
use crate::runtime::{CallOptions, CallRecord, VisionRuntime};
use crate::traits::{DetectQuery, Sampling, Tracker};
use crate::watch::{FrameRef, TrackedObservation};

/// One processed frame, as an [`ObservationSink`] receives it.
///
/// [`tracked`](Self::tracked) turns it into the [`TrackedObservation`] a
/// [`WatchEvaluator`](crate::WatchEvaluator) takes.
#[derive(Clone, Debug)]
pub struct Observation {
    /// The frame source.
    pub source: FrameSourceId,
    /// The source session: one per [`StreamPipeline::run`], the scope the track identifiers
    /// belong to and reset with (V-I8).
    pub session: String,
    /// The frame's identity and timing.
    pub frame: FrameMeta,
    /// The redacted frame, still ephemeral: keep it through a
    /// [`FrameKeeper`](super::FrameKeeper) to make it evidence.
    pub image: Arc<RedactedFrame>,
    /// The digest of the redacted frame's PNG encoding: the regions' image, and the digest
    /// the frame is stored under if it is kept.
    pub digest: Digest,
    /// What the detector found, in source pixels, with the tracker's anonymous track
    /// identifiers.
    pub regions: Vec<Region>,
    /// The tracks the tracker holds after this frame, seen in it or kept through a brief
    /// miss ([`Tracker::live_tracks`]).
    pub tracks: Vec<TrackId>,
    /// The detector's metered calls.
    pub calls: Vec<CallRecord>,
}

impl Observation {
    /// The frame as a watch condition names it: index, capture time, digest and size, with no
    /// artifact while the frame is ephemeral.
    pub fn frame_ref(&self) -> FrameRef {
        let (width, height) = self.image.pixels().size();
        FrameRef::new(self.frame.index, self.frame.captured_at, self.digest, width, height)
    }

    /// The observation as a [`WatchEvaluator`](crate::WatchEvaluator) takes it: source,
    /// session, frame, tracked regions and the tracks the tracker still holds.
    pub fn tracked(&self) -> TrackedObservation {
        TrackedObservation::new(
            self.source.clone(),
            self.session.clone(),
            self.frame_ref(),
            self.regions.clone(),
        )
        .with_tracks(self.tracks.iter().copied())
    }
}

/// Why a stream stopped.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum StopCause {
    /// The source ended: a file's last frame, a camera that hung up.
    Ended,
    /// The run was cancelled.
    Cancelled,
    /// A stage failed.
    Failed {
        /// The error's code.
        code: String,
        /// The error's message.
        message: String,
    },
}

/// A change in a stream's life, as an [`ObservationSink`] receives it. Each is also a
/// `vision.source.*` or `vision.governor.step` audit record.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum SourceEvent {
    /// Frames started.
    Started {
        /// The sampling asked of the source.
        sampling: Sampling,
    },
    /// The frame governor changed the stream.
    Governor {
        /// The change.
        step: GovernorStep,
    },
    /// The kill switch froze the run; frames are dropped until it thaws.
    Frozen,
    /// The kill switch thawed; frames are processed again.
    Thawed,
    /// Frames stopped.
    Stopped {
        /// Why.
        cause: StopCause,
    },
}

/// Receives a stream's observations: watch conditions and wakes, the live camera surface, a
/// recorder.
///
/// The pipeline awaits each call before it reads the next frame, so a slow sink slows the
/// stream and the source drops the frames it cannot hold; a sink that fans out to slower
/// consumers should queue on its own. An error stops the stream.
///
/// # Example
///
/// ```
/// use std::sync::Mutex;
/// use adk_vision::VisionCx;
/// use adk_vision::stream::{Observation, ObservationSink};
///
/// /// Counts the regions of each label it sees.
/// #[derive(Default)]
/// struct Counter(Mutex<std::collections::BTreeMap<String, usize>>);
///
/// #[async_trait::async_trait]
/// impl ObservationSink for Counter {
///     async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
///         let mut counts = self.0.lock().unwrap();
///         for region in &observation.regions {
///             *counts.entry(region.label.clone().unwrap_or_default()).or_default() += 1;
///         }
///         Ok(())
///     }
/// }
/// ```
///
/// # Watch conditions and wakes
///
/// An observation converts to the [`TrackedObservation`] a
/// [`WatchEvaluator`](crate::WatchEvaluator) takes. A sink that holds watch conditions keeps
/// the frame of each event as its wake payload, so the frame is stored redacted, and hands the
/// event with the frame's artifact to a [`WatchSink`](crate::WatchSink) such as the kernel's
/// `VisionWakeSource`. [`StreamWatch`](super::StreamWatch) is that sink, with the
/// `vision.watch.fired` record; in outline:
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use adk_vision::stream::{FrameKeeper, KeepReason, Observation, ObservationSink};
/// use adk_vision::{VisionCx, WatchEvaluator, WatchSink};
///
/// struct Watch {
///     evaluator: Mutex<WatchEvaluator>,
///     keeper: FrameKeeper,
///     wakes: Arc<dyn WatchSink>,
/// }
///
/// #[async_trait::async_trait]
/// impl ObservationSink for Watch {
///     async fn observe(&self, cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
///         let events = self.evaluator.lock().unwrap().observe(&observation.tracked());
///         for event in events {
///             let kept = self.keeper.keep(cx, &observation.image, KeepReason::WakePayload).await?;
///             self.wakes.deliver(&event.with_frame_artifact(kept.image.artifact)).await?;
///         }
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait ObservationSink: Send + Sync {
    /// One processed frame.
    ///
    /// # Errors
    ///
    /// Any failure; the stream stops with it.
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> Result<()>;

    /// A change in the stream's life. Does nothing by default.
    ///
    /// # Errors
    ///
    /// Any failure; the stream stops with it, except after it has stopped.
    async fn source_event(&self, _cx: &VisionCx, _event: &SourceEvent) -> Result<()> {
        Ok(())
    }
}

/// What a finished stream did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSummary {
    /// Frames the source delivered.
    pub received: u64,
    /// Frames the governor thinned.
    pub thinned: u64,
    /// Frames dropped while the governor paused the source.
    pub paused: u64,
    /// Frames dropped while the kill switch was frozen.
    pub frozen: u64,
    /// Frames dropped because the privacy hook failed.
    pub privacy_failures: u64,
    /// Frames skipped because the detector was briefly unavailable.
    pub detection_errors: u64,
    /// Observations delivered to the sink.
    pub observations: u64,
}

/// The stream's frame governor, shared with whatever escalates its tracks.
///
/// Escalation requests and frames go through the same governor, so its quotas cover both;
/// each change it makes writes its `vision.governor.step` record.
#[derive(Clone)]
pub struct GovernorHandle {
    source: FrameSourceId,
    governor: Arc<Mutex<FrameGovernor>>,
}

impl std::fmt::Debug for GovernorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernorHandle").field("source", &self.source).finish_non_exhaustive()
    }
}

impl GovernorHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, FrameGovernor> {
        self.governor.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The governor's state now.
    pub fn state(&self) -> GovernorState {
        self.lock().state()
    }

    /// Asks to escalate `frames` frames now: granted with the frames it is billed, or
    /// refused with the reason. Changes are recorded on `cx`'s audit trail.
    ///
    /// # Errors
    ///
    /// The audit trail's error when it fails closed.
    pub async fn request_escalation(
        &self,
        cx: &VisionCx,
        frames: u64,
    ) -> Result<EscalationVerdict> {
        self.request_escalation_at(cx, Utc::now(), frames).await
    }

    /// As [`request_escalation`](Self::request_escalation), at `at`.
    ///
    /// # Errors
    ///
    /// The audit trail's error when it fails closed.
    pub async fn request_escalation_at(
        &self,
        cx: &VisionCx,
        at: DateTime<Utc>,
        frames: u64,
    ) -> Result<EscalationVerdict> {
        let EscalationDecision { verdict, steps } = self.lock().request_escalation(at, frames);
        for step in &steps {
            for draft in governor_drafts(&self.source, step) {
                cx.audit(draft).await?;
            }
        }
        Ok(verdict)
    }

    fn offer(&self, at: DateTime<Utc>) -> (FrameVerdict, Vec<GovernorStep>) {
        let admission = self.lock().offer(at);
        (admission.verdict, admission.steps)
    }
}

struct Detection {
    runtime: Arc<VisionRuntime>,
    query: DetectQuery,
    options: CallOptions,
}

/// Runs one [`FrameSource`] through the stages of spec 3.7 §7.13 and hands each processed
/// frame to an [`ObservationSink`].
///
/// | Stage | What it does |
/// |-------|--------------|
/// | Source and sampler | Frames at the sampling rate and size; the source drops what the pipeline cannot keep up with |
/// | Frame governor | Thins, or pauses, frames to the [`FrameBudget`]; records each change |
/// | Privacy | The [`PrivacyHook`] of [`with_privacy`](StreamPipelineBuilder::with_privacy), when one is given, redacts every frame first; a failing hook drops the frame |
/// | Detector | [`VisionRuntime::detect_with`], metered in frames, regions in source pixels |
/// | Tracker | Anonymous track identifiers, reset when the stream starts and stops (V-I8) |
/// | Sink | [`ObservationSink::observe`] |
///
/// The run writes `vision.source.started` and `vision.source.stopped`, the governor's
/// steps, and `vision.source.paused` and `.resumed` around a kill-switch freeze, to the
/// audit trail of the [`VisionCx`] it runs under.
///
/// # Example
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use adk_vision::stream::{FrameSource, FrameStream, Observation, ObservationSink, StreamPipeline};
/// use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, Sampling, VisionCx};
/// use futures::StreamExt;
///
/// struct Three(FrameSourceId);
///
/// #[async_trait::async_trait]
/// impl FrameSource for Three {
///     fn id(&self) -> &FrameSourceId {
///         &self.0
///     }
///     async fn frames(&self, _sampling: Sampling) -> adk_vision::Result<FrameStream> {
///         let id = self.0.clone();
///         Ok(futures::stream::iter((0..3u64).map(move |index| {
///             Ok(Frame {
///                 source: id.clone(),
///                 index,
///                 pts: std::time::Duration::from_secs(index),
///                 captured_at: chrono::Utc::now(),
///                 pixels: FramePixels::Ephemeral(Arc::new(DecodedImage::filled(4, 4, [9; 4]))),
///             })
///         }))
///         .boxed())
///     }
/// }
///
/// #[derive(Default)]
/// struct Seen(Mutex<Vec<u64>>);
///
/// #[async_trait::async_trait]
/// impl ObservationSink for Seen {
///     async fn observe(&self, _cx: &VisionCx, o: Observation) -> adk_vision::Result<()> {
///         self.0.lock().unwrap().push(o.frame.index);
///         Ok(())
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let seen = Arc::new(Seen::default());
/// let pipeline = StreamPipeline::builder(Arc::new(Three(FrameSourceId("cam".into()))))
///     .sampling(Sampling { fps: 1.0, max_long_edge: None })
///     .sink(seen.clone())
///     .build()
///     .unwrap();
/// let summary = pipeline.run(&VisionCx::new()).await.unwrap();
/// assert_eq!(summary.observations, 3);
/// assert_eq!(*seen.0.lock().unwrap(), vec![0, 1, 2]);
/// # });
/// ```
pub struct StreamPipeline {
    source: Arc<dyn FrameSource>,
    sampling: Sampling,
    privacy: FramePrivacy,
    detection: Option<Detection>,
    // A `Mutex` makes the pipeline `Sync`, so its run is a `Send` future, though a tracker
    // is only `Send`; it is locked only between awaits.
    tracker: Option<Mutex<Box<dyn Tracker>>>,
    governor: Option<GovernorHandle>,
    sink: Arc<dyn ObservationSink>,
}

impl std::fmt::Debug for StreamPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamPipeline")
            .field("source", self.source.id())
            .field("sampling", &self.sampling)
            .field("privacy", &self.privacy)
            .field("governor", &self.governor)
            .finish_non_exhaustive()
    }
}

/// Builds a [`StreamPipeline`]. Frames are not redacted unless a hook is given with
/// [`with_privacy`](Self::with_privacy).
pub struct StreamPipelineBuilder {
    source: Arc<dyn FrameSource>,
    privacy: Arc<dyn PrivacyHook>,
    sampling: Option<Sampling>,
    detection: Option<Detection>,
    tracker: Option<Box<dyn Tracker>>,
    budget: Option<FrameBudget>,
    sink: Option<Arc<dyn ObservationSink>>,
}

/// A sink that drops every observation, for a pipeline built without one.
struct Discard;

#[async_trait]
impl ObservationSink for Discard {
    async fn observe(&self, _cx: &VisionCx, _observation: Observation) -> Result<()> {
        Ok(())
    }
}

impl StreamPipelineBuilder {
    /// Passes every frame through `hook` before any other stage sees it, such as
    /// [`RedactorPrivacy::blur_faces`](super::RedactorPrivacy::blur_faces); a frame the hook
    /// fails on is dropped.
    pub fn with_privacy(mut self, hook: Arc<dyn PrivacyHook>) -> Self {
        self.privacy = hook;
        self
    }

    /// Samples the source at `sampling`. Defaults to the budget's local rate when a
    /// [`budget`](Self::budget) is set, and to one frame a second otherwise.
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// Detects `query` on every processed frame through `runtime`'s detection route. Each
    /// call is metered in [`UsageUnit::Frames`](adk_core::UsageUnit::Frames), so a stream
    /// counts against frame caps and quotas, not image ones (spec 3.7 X3).
    pub fn detector(mut self, runtime: Arc<VisionRuntime>, query: DetectQuery) -> Self {
        self.detection =
            Some(Detection { runtime, query, options: CallOptions::default().as_frames() });
        self
    }

    /// Sets the detection call's options: a route, a region of interest. The calls stay
    /// metered as frames.
    pub fn detect_options(mut self, options: CallOptions) -> Self {
        if let Some(detection) = &mut self.detection {
            detection.options = options.as_frames();
        }
        self
    }

    /// Tracks detections across frames.
    pub fn tracker(mut self, tracker: Box<dyn Tracker>) -> Self {
        self.tracker = Some(tracker);
        self
    }

    /// Holds the stream to `budget` through a [`FrameGovernor`].
    pub fn budget(mut self, budget: FrameBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Hands observations and source events to `sink`.
    pub fn sink(mut self, sink: Arc<dyn ObservationSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Builds the pipeline.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an invalid budget or sampling rate.
    pub fn build(self) -> Result<StreamPipeline> {
        let governor = match self.budget {
            Some(budget) => Some(GovernorHandle {
                source: self.source.id().clone(),
                governor: Arc::new(Mutex::new(FrameGovernor::new(budget)?)),
            }),
            None => None,
        };
        let sampling = self.sampling.unwrap_or_else(|| Sampling {
            fps: governor.as_ref().map_or(1.0, |g| g.lock().budget().local_fps),
            max_long_edge: None,
        });
        super::Sampler::new(sampling)?;
        Ok(StreamPipeline {
            source: self.source,
            sampling,
            privacy: FramePrivacy::new(self.privacy),
            detection: self.detection,
            tracker: self.tracker.map(Mutex::new),
            governor,
            sink: self.sink.unwrap_or_else(|| Arc::new(Discard)),
        })
    }
}

impl StreamPipeline {
    /// A builder over `source`, its frames unredacted until
    /// [`with_privacy`](StreamPipelineBuilder::with_privacy) gives a hook.
    pub fn builder(source: Arc<dyn FrameSource>) -> StreamPipelineBuilder {
        StreamPipelineBuilder {
            source,
            privacy: Arc::new(super::NoRedaction),
            sampling: None,
            detection: None,
            tracker: None,
            budget: None,
            sink: None,
        }
    }

    /// The source's identifier.
    pub fn source_id(&self) -> &FrameSourceId {
        self.source.id()
    }

    /// The frame governor, when the pipeline has a budget: escalations go through it.
    pub fn governor(&self) -> Option<GovernorHandle> {
        self.governor.clone()
    }

    /// Forgets every track: identifiers restart with each source session (V-I8).
    fn reset_tracker(&self) {
        if let Some(tracker) = &self.tracker {
            tracker.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reset();
        }
    }

    async fn event(&self, cx: &VisionCx, event: SourceEvent) -> Result<()> {
        self.sink.source_event(cx, &event).await
    }

    async fn governor_steps(&self, cx: &VisionCx, steps: Vec<GovernorStep>) -> Result<()> {
        let id = self.source.id();
        for step in steps {
            for draft in governor_drafts(id, &step) {
                cx.audit(draft).await?;
            }
            self.event(cx, SourceEvent::Governor { step }).await?;
        }
        Ok(())
    }

    /// Runs the stream until the source ends, `cx` is cancelled, or a stage fails.
    ///
    /// # Errors
    ///
    /// The source's error opening or reading frames, a detector error other than a brief
    /// unavailability (a budget refusal stops the stream), the sink's error, or the audit
    /// trail's when it fails closed. A frozen kill switch at the start refuses the run.
    pub async fn run(self, cx: &VisionCx) -> Result<StreamSummary> {
        cx.check()?;
        let id = self.source.id().clone();
        let mut frames = self.source.frames(self.sampling).await?;
        let budget = self.governor.as_ref().map(|g| g.lock().budget().clone());
        let session = uuid::Uuid::new_v4().to_string();
        cx.audit(source_draft(
            VISION_SOURCE_STARTED,
            &id,
            json!({
                "session": session,
                "sampling": self.sampling,
                "budget": budget,
                "privacy": self.privacy.hook_name(),
            }),
        ))
        .await?;
        tracing::info!(stream.source = %id.0, stream.fps = self.sampling.fps, "stream started");
        self.reset_tracker();
        let mut summary = StreamSummary::default();
        let outcome = match self.event(cx, SourceEvent::Started { sampling: self.sampling }).await {
            Ok(()) => self.process(cx, &session, &mut frames, &mut summary).await,
            Err(err) => Err(err),
        };
        drop(frames);
        self.reset_tracker();
        let cause = match &outcome {
            Ok(cause) => cause.clone(),
            Err(err) => {
                StopCause::Failed { code: err.code().to_string(), message: err.to_string() }
            }
        };
        tracing::info!(
            stream.source = %id.0,
            stream.received = summary.received,
            stream.observations = summary.observations,
            "stream stopped"
        );
        let recorded = cx
            .audit(source_draft(
                VISION_SOURCE_STOPPED,
                &id,
                json!({ "session": session, "cause": cause, "summary": summary }),
            ))
            .await;
        // The stream is over: a sink error here has nothing left to stop.
        if let Err(err) = self.event(cx, SourceEvent::Stopped { cause }).await {
            tracing::warn!(stream.source = %id.0, error = %err, "sink refused the stop event");
        }
        outcome?;
        recorded?;
        Ok(summary)
    }

    async fn process(
        &self,
        cx: &VisionCx,
        session: &str,
        frames: &mut super::FrameStream,
        summary: &mut StreamSummary,
    ) -> Result<StopCause> {
        let id = self.source.id().clone();
        let mut frozen = false;
        loop {
            let next = tokio::select! {
                biased;
                () = cx.cancellation().cancelled() => return Ok(StopCause::Cancelled),
                next = frames.next() => next,
            };
            let Some(frame) = next else { return Ok(StopCause::Ended) };
            let frame = frame?;
            summary.received += 1;

            // The kill switch pauses the stream at its next frame (V-I17).
            match cx.check() {
                Err(VisionError::Cancelled) => return Ok(StopCause::Cancelled),
                Err(err) => {
                    summary.frozen += 1;
                    if !frozen {
                        frozen = true;
                        tracing::warn!(stream.source = %id.0, error = %err, "stream paused by the kill switch");
                        cx.audit(source_draft(
                            VISION_SOURCE_PAUSED,
                            &id,
                            json!({ "reason": "frozen", "code": err.code() }),
                        ))
                        .await?;
                        self.event(cx, SourceEvent::Frozen).await?;
                    }
                    continue;
                }
                Ok(()) if frozen => {
                    frozen = false;
                    cx.audit(source_draft(
                        VISION_SOURCE_RESUMED,
                        &id,
                        json!({ "reason": "thawed" }),
                    ))
                    .await?;
                    self.event(cx, SourceEvent::Thawed).await?;
                }
                Ok(()) => {}
            }

            if let Some(governor) = &self.governor {
                let (verdict, steps) = governor.offer(frame.captured_at);
                self.governor_steps(cx, steps).await?;
                match verdict {
                    FrameVerdict::Process => {}
                    FrameVerdict::Thinned => {
                        summary.thinned += 1;
                        continue;
                    }
                    FrameVerdict::Paused => {
                        summary.paused += 1;
                        continue;
                    }
                }
            }

            let meta = frame.meta();
            let redacted = match self.privacy.apply(cx, frame).await {
                Ok(redacted) => Arc::new(redacted),
                Err(VisionError::Cancelled) => return Ok(StopCause::Cancelled),
                Err(err) => {
                    // Fail closed: a frame the hook could not redact goes nowhere.
                    summary.privacy_failures += 1;
                    tracing::warn!(stream.source = %id.0, frame.index = meta.index, error = %err, "frame dropped: privacy hook failed");
                    continue;
                }
            };
            let pixels = redacted.shared_pixels();
            let digest = Digest::of(&pixels.encode_png()?);
            let (regions, calls) = match &self.detection {
                None => (Vec::new(), Vec::new()),
                Some(detection) => {
                    let image = SourceImage::from_parts(digest, "image/png".into(), pixels);
                    match detection
                        .runtime
                        .detect_with(cx, &image, &detection.query, &detection.options)
                        .await
                    {
                        Ok(found) => (found.value, found.calls),
                        Err(VisionError::Cancelled) => return Ok(StopCause::Cancelled),
                        Err(err)
                            if matches!(
                                err.category(),
                                ErrorCategory::RateLimited
                                    | ErrorCategory::Timeout
                                    | ErrorCategory::Unavailable
                            ) =>
                        {
                            summary.detection_errors += 1;
                            tracing::warn!(stream.source = %id.0, frame.index = meta.index, error = %err, "frame skipped: detector unavailable");
                            continue;
                        }
                        Err(err) => return Err(err),
                    }
                }
            };
            let (regions, tracks) = match &self.tracker {
                Some(tracker) => {
                    let mut tracker =
                        tracker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let regions = tracker.update(&meta, regions);
                    (regions, tracker.live_tracks())
                }
                None => (regions, Vec::new()),
            };
            self.sink
                .observe(
                    cx,
                    Observation {
                        source: id.clone(),
                        session: session.to_string(),
                        frame: meta,
                        image: redacted,
                        digest,
                        regions,
                        tracks,
                        calls,
                    },
                )
                .await?;
            summary.observations += 1;
        }
    }
}
