//! [`StreamWatch`]: the watch stage of a stream — conditions, the kept frame, the wake.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;

use super::pipeline::{Observation, ObservationSink, SourceEvent};
use super::retention::{FrameKeeper, KeepReason};
use crate::cx::VisionCx;
use crate::error::Result;
use crate::watch::{WatchCondition, WatchEvaluator, WatchSink};

/// The watch stage of a stream (spec 3.7 §7.10): an [`ObservationSink`] that holds watch
/// conditions against every observation and, for each event that passes,
///
/// 1. keeps the frame through its [`FrameKeeper`] as [`KeepReason::WakePayload`] — the frame
///    is stored redacted, once however many events it carries;
/// 2. names the kept artifact in the event ([`WatchEvent::with_frame_artifact`]);
/// 3. writes the `vision.watch.fired` audit record, by digest only (V-I15);
/// 4. delivers the event to its [`WatchSink`], such as the kernel's `VisionWakeSource`.
///
/// Conditions can be added and removed while the stream runs; the others keep their
/// streaks. A second sink, such as the live camera surface, can see every observation and
/// source event after the watch ([`with_observer`](Self::with_observer)).
///
/// # Example
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use adk_artifact::InMemoryArtifactService;
/// use adk_vision::stream::{FrameKeeper, StreamWatch};
/// use adk_vision::{FrameSourceId, MediaScope, WatchCondition, WatchEvaluator, WatchEvent, WatchSink};
///
/// #[derive(Default)]
/// struct Wakes(Mutex<Vec<String>>);
///
/// #[async_trait::async_trait]
/// impl WatchSink for Wakes {
///     async fn deliver(&self, event: &WatchEvent) -> adk_vision::Result<()> {
///         self.0.lock().unwrap().push(event.key());
///         Ok(())
///     }
/// }
///
/// let source = FrameSourceId("conveyor-1".into());
/// let keeper = FrameKeeper::new(
///     Arc::new(InMemoryArtifactService::new()),
///     MediaScope::new("inspector", "line-1", "shift-1"),
///     source,
/// );
/// let watch = StreamWatch::new(WatchEvaluator::new(Vec::new()).unwrap(), Arc::new(keeper), Arc::new(Wakes::default()));
/// watch.add_condition(WatchCondition::new("torn-seal", ["torn seal"])).unwrap();
/// assert_eq!(watch.conditions().len(), 1);
/// ```
///
/// [`WatchEvent::with_frame_artifact`]: crate::WatchEvent::with_frame_artifact
pub struct StreamWatch {
    // Locked only between awaits: `observe` takes the events out before it keeps or wakes.
    evaluator: Mutex<WatchEvaluator>,
    keeper: Arc<FrameKeeper>,
    wakes: Arc<dyn WatchSink>,
    observer: Option<Arc<dyn ObservationSink>>,
    fired: AtomicU64,
}

impl std::fmt::Debug for StreamWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamWatch")
            .field("conditions", &self.conditions().len())
            .field("keeper", &self.keeper)
            .field("fired", &self.fired())
            .finish_non_exhaustive()
    }
}

impl StreamWatch {
    /// A watch holding `evaluator`'s conditions, keeping each event's frame with `keeper`
    /// and delivering the event to `wakes`.
    pub fn new(
        evaluator: WatchEvaluator,
        keeper: Arc<FrameKeeper>,
        wakes: Arc<dyn WatchSink>,
    ) -> Self {
        Self {
            evaluator: Mutex::new(evaluator),
            keeper,
            wakes,
            observer: None,
            fired: AtomicU64::new(0),
        }
    }

    /// Hands every observation and source event on to `observer` after the watch has seen it.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ObservationSink>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn evaluator(&self) -> MutexGuard<'_, WatchEvaluator> {
        self.evaluator.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The conditions now held.
    pub fn conditions(&self) -> Vec<WatchCondition> {
        self.evaluator().conditions().to_vec()
    }

    /// Adds a condition while the stream runs.
    ///
    /// # Errors
    ///
    /// As [`WatchEvaluator::add`].
    pub fn add_condition(&self, condition: WatchCondition) -> Result<()> {
        self.evaluator().add(condition)
    }

    /// Removes the condition named `name`.
    pub fn remove_condition(&self, name: &str) -> Option<WatchCondition> {
        self.evaluator().remove(name)
    }

    /// How many events the watch has delivered.
    pub fn fired(&self) -> u64 {
        self.fired.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ObservationSink for StreamWatch {
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> Result<()> {
        let fired: Vec<_> = {
            let mut evaluator = self.evaluator();
            let events = evaluator.observe(&observation.tracked());
            events
                .into_iter()
                .map(|event| {
                    let digest = evaluator.condition(&event.condition).map(WatchCondition::digest);
                    (event, digest)
                })
                .collect()
        };
        if !fired.is_empty() {
            let kept = self.keeper.keep(cx, &observation.image, KeepReason::WakePayload).await?;
            for (event, condition) in fired {
                let event = event.with_frame_artifact(kept.image.artifact.clone());
                if let Some(condition) = condition {
                    cx.audit(event.audit_draft(condition)).await?;
                }
                tracing::info!(
                    vision.source = %event.source.0,
                    vision.condition = %event.condition,
                    vision.track = event.track.0,
                    vision.frame = event.frame.index,
                    "watch condition fired"
                );
                self.wakes.deliver(&event).await?;
                self.fired.fetch_add(1, Ordering::Relaxed);
            }
        }
        match &self.observer {
            Some(observer) => observer.observe(cx, observation).await,
            None => Ok(()),
        }
    }

    async fn source_event(&self, cx: &VisionCx, event: &SourceEvent) -> Result<()> {
        match &self.observer {
            Some(observer) => observer.source_event(cx, event).await,
            None => Ok(()),
        }
    }
}
