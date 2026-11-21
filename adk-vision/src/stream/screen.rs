//! [`ScreenSource`]: screenshots at an interval.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{ActuationOutput, ComputerAction, ComputerActuator};
use async_trait::async_trait;
use serde_json::Value;

use super::capture::{Capture, interval_frames, observation_image};
use super::{FrameSource, FrameStream};
use crate::error::{Result, VisionError};
use crate::media::FrameSourceId;
use crate::traits::Sampling;

/// A source of computer-use observations: the `observe_visual` half of
/// `adk_computer_use::ComputerUseRuntime`, which a computer-use-mcp runtime answers with a
/// `snapshot`.
///
/// `adk-vision` does not depend on `adk-computer-use`; wrap its runtime in a newtype:
///
/// ```ignore
/// struct Observations(Arc<dyn adk_computer_use::ComputerUseRuntime>);
///
/// #[async_trait::async_trait]
/// impl adk_vision::stream::VisualObserver for Observations {
///     async fn observe_visual(&self) -> adk_core::Result<serde_json::Value> {
///         self.0.observe_visual().await.map_err(|e| adk_core::AdkError::tool(e.to_string()))
///     }
/// }
/// ```
#[async_trait]
pub trait VisualObserver: Send + Sync {
    /// One fresh visual observation, as JSON holding a screenshot.
    ///
    /// # Errors
    ///
    /// The runtime's error.
    async fn observe_visual(&self) -> adk_core::Result<Value>;
}

struct FromActuator(Arc<dyn ComputerActuator>);

#[async_trait]
impl Capture for FromActuator {
    async fn capture(&self) -> Result<Vec<u8>> {
        match self.0.perform(ComputerAction::Screenshot).await? {
            ActuationOutput::Image(shot) => Ok(shot.data),
            other => Err(VisionError::Backend {
                backend: "screen".into(),
                message: format!("the actuator answered a screenshot with {other:?}"),
            }),
        }
    }
}

struct FromObserver(Arc<dyn VisualObserver>);

#[async_trait]
impl Capture for FromObserver {
    async fn capture(&self) -> Result<Vec<u8>> {
        let observation = self.0.observe_visual().await?;
        Ok(observation_image(&observation)?.0)
    }
}

/// Screenshots of a display at an interval: from a [`ComputerActuator`] (a virtual desktop,
/// a remote session, the WebDriver viewport `adk-browser` drives) or from computer-use-mcp
/// observations through a [`VisualObserver`].
///
/// Taking a screenshot changes nothing, so the source calls the actuator or the runtime
/// directly; the stream pipeline around it is what the kill switch and the budget govern.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_core::{ActuationOutput, ComputerAction, ComputerActuator, Screenshot};
/// use adk_vision::stream::{FrameSource, ScreenSource};
/// use adk_vision::{DecodedImage, FrameSourceId, Sampling};
/// use futures::StreamExt;
///
/// struct Desktop(Vec<u8>);
///
/// #[async_trait::async_trait]
/// impl ComputerActuator for Desktop {
///     fn display_size(&self) -> Option<(u32, u32)> {
///         Some((32, 16))
///     }
///     async fn perform(&self, _action: ComputerAction) -> adk_core::Result<ActuationOutput> {
///         Ok(ActuationOutput::Image(Screenshot::png(self.0.clone()).unwrap()))
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let png = DecodedImage::filled(32, 16, [20, 40, 60, 255]).encode_png().unwrap();
/// let screen = ScreenSource::from_actuator(FrameSourceId("desk".into()), Arc::new(Desktop(png)))
///     .with_interval(Duration::from_millis(10));
/// let mut frames = screen.frames(Sampling { fps: 50.0, max_long_edge: None }).await.unwrap();
/// let first = frames.next().await.unwrap().unwrap();
/// assert_eq!(first.index, 0);
/// # });
/// ```
pub struct ScreenSource {
    id: FrameSourceId,
    capture: Arc<dyn Capture>,
    interval: Duration,
}

impl std::fmt::Debug for ScreenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenSource")
            .field("id", &self.id)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl ScreenSource {
    /// Screenshots from `actuator`, one a second by default.
    pub fn from_actuator(id: FrameSourceId, actuator: Arc<dyn ComputerActuator>) -> Self {
        Self { id, capture: Arc::new(FromActuator(actuator)), interval: Duration::from_secs(1) }
    }

    /// Screenshots from computer-use observations, one a second by default.
    pub fn from_observer(id: FrameSourceId, observer: Arc<dyn VisualObserver>) -> Self {
        Self { id, capture: Arc::new(FromObserver(observer)), interval: Duration::from_secs(1) }
    }

    /// Takes a screenshot every `interval`, or every sampling interval when that is longer.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

#[async_trait]
impl FrameSource for ScreenSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> Result<FrameStream> {
        interval_frames(self.id.clone(), Arc::clone(&self.capture), self.interval, sampling)
    }
}
