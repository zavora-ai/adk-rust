//! A camera's frames to a realtime model (spec 3.7 §7.13 "Realtime"; feature `voice`).
//!
//! [`feed_realtime`] subscribes to one camera of the hub and sends the frames that pass its gate
//! to a realtime session under the voice screen's [`FramePolicy`]: one a second for Gemini Live,
//! changed frames at most every five seconds for the OpenAI Realtime API. A frame the hub drops —
//! the camera off or stopped — never reaches it, and switching off closes the bridge's own gate
//! too, so the first frame after switching on again is new. [`with_camera`] turns on Gemini
//! Live's context window compression, without which a session with video ends after about two
//! minutes.

use std::sync::Arc;

use adk_realtime::{RealtimeConfig, RealtimeRunner, RealtimeSession};
use async_trait::async_trait;
use base64::Engine as _;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::hub::{CameraUpdate, Cameras};
use crate::voice::{FramePolicy, VisualInputs, VisualSource};

/// Where a camera's frames go: a realtime session, a runner, or a test's recorder.
#[async_trait]
pub trait VideoSink: Send + Sync {
    /// Sends one base64-encoded frame of `mime_type`.
    ///
    /// # Errors
    ///
    /// The provider's error; the bridge logs it and goes on.
    async fn send_video_frame(
        &self,
        mime_type: &str,
        data_base64: &str,
    ) -> adk_realtime::Result<()>;
}

#[async_trait]
impl VideoSink for Arc<dyn RealtimeSession> {
    async fn send_video_frame(
        &self,
        mime_type: &str,
        data_base64: &str,
    ) -> adk_realtime::Result<()> {
        RealtimeSession::send_video_frame(self.as_ref(), mime_type, data_base64).await
    }
}

#[async_trait]
impl VideoSink for RealtimeRunner {
    async fn send_video_frame(
        &self,
        mime_type: &str,
        data_base64: &str,
    ) -> adk_realtime::Result<()> {
        RealtimeRunner::send_video_frame(self, mime_type, data_base64).await
    }
}

/// `config` for a session that watches a camera: Gemini Live's context window compression on
/// (spec 3.7 §7.13), so the session outlives its first two minutes of video.
///
/// # Example
///
/// ```rust
/// use adk_realtime::RealtimeConfig;
///
/// let config = adk_surface::camera::bridge::with_camera(RealtimeConfig::default());
/// assert_eq!(config.extra.unwrap()["context_window_compression"], true);
/// ```
pub fn with_camera(config: RealtimeConfig) -> RealtimeConfig {
    config.with_context_window_compression(None)
}

/// Sends `camera`'s frames from `cameras` to `sink` under `policy` until the task is aborted or
/// the hub goes away. Only frames that passed the hub's gate arrive here; the bridge's own gate
/// follows the camera's switch, so nothing is sent while the camera is off.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::camera::{Cameras, bridge};
/// use adk_surface::voice::FramePolicy;
///
/// # fn example(cameras: Cameras, session: Arc<dyn adk_realtime::RealtimeSession>) {
/// let task = bridge::feed_realtime(cameras, "line-2", session, FramePolicy::GEMINI);
/// task.abort();
/// # }
/// ```
pub fn feed_realtime<S>(
    cameras: Cameras,
    camera: impl Into<String>,
    sink: S,
    policy: FramePolicy,
) -> JoinHandle<()>
where
    S: VideoSink + 'static,
{
    let camera = camera.into();
    let mut updates = cameras.subscribe();
    tokio::spawn(async move {
        let mut gate = VisualInputs::new(policy);
        gate.switch(VisualSource::Camera, cameras.is_on(&camera).await);
        loop {
            let update = match updates.recv().await {
                Ok(update) => update,
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(camera.id = %camera, frames.skipped = skipped, "the realtime bridge fell behind");
                    continue;
                }
                Err(RecvError::Closed) => return,
            };
            if update.camera() != camera {
                continue;
            }
            match update {
                CameraUpdate::Changed { .. } => {
                    let on = cameras.is_on(&camera).await;
                    if on != gate.is_on(VisualSource::Camera) {
                        gate.switch(VisualSource::Camera, on);
                    }
                }
                CameraUpdate::Frame { frame, .. } => {
                    if !gate.offer(VisualSource::Camera, &frame.frame.bytes, Instant::now()) {
                        continue;
                    }
                    let data = base64::engine::general_purpose::STANDARD.encode(&frame.frame.bytes);
                    if let Err(error) = sink.send_video_frame(&frame.frame.mime_type, &data).await {
                        tracing::debug!(error = %error, camera.id = %camera, "a camera frame could not be sent");
                    }
                }
            }
        }
    })
}
