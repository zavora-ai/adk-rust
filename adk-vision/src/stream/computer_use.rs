//! Computer-use observations as a frame source (spec 3.7 §7.13, features `computer-use` and
//! `screen`).
//!
//! [`ScreenSource`] reads screenshots through the [`VisualObserver`] trait;
//! [`RuntimeObserver`] is that trait over any `adk_computer_use::ComputerUseRuntime`: each
//! frame is one `observe_visual` call, whose snapshot carries the screenshot. Observing
//! changes nothing on the desktop, so the source calls the runtime directly; the stream
//! pipeline around it is what the kill switch and the frame budget govern.
//!
//! | Piece | What it does |
//! |-------|--------------|
//! | [`RuntimeObserver`] | `VisualObserver` over any `ComputerUseRuntime` |
//! | [`screen_source`] | A `ScreenSource` over a runtime, one frame a second until `with_interval` says otherwise |
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_vision::FrameSourceId;
//! use adk_vision::stream::screen_source;
//!
//! let runtime: Arc<dyn adk_computer_use::ComputerUseRuntime> = Arc::new(mcp_runtime);
//! let desk = screen_source(FrameSourceId("desk".into()), runtime)
//!     .with_interval(std::time::Duration::from_secs(2));
//! // Register `desk` with `FrameSources` to watch it.
//! ```

use std::sync::Arc;

use adk_computer_use::ComputerUseRuntime;
use adk_core::AdkError;
use async_trait::async_trait;
use serde_json::Value;

use super::screen::{ScreenSource, VisualObserver};
use crate::media::FrameSourceId;

/// The [`VisualObserver`] over a [`ComputerUseRuntime`]: each observation is one
/// `observe_visual` snapshot.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_vision::FrameSourceId;
/// use adk_vision::stream::{RuntimeObserver, ScreenSource};
///
/// let observer = Arc::new(RuntimeObserver::new(runtime));
/// let desk = ScreenSource::from_observer(FrameSourceId("desk".into()), observer);
/// ```
#[derive(Clone)]
pub struct RuntimeObserver {
    runtime: Arc<dyn ComputerUseRuntime>,
}

impl std::fmt::Debug for RuntimeObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeObserver").finish_non_exhaustive()
    }
}

impl RuntimeObserver {
    /// Observes through `runtime`.
    pub fn new(runtime: Arc<dyn ComputerUseRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl VisualObserver for RuntimeObserver {
    async fn observe_visual(&self) -> adk_core::Result<Value> {
        self.runtime.observe_visual().await.map_err(AdkError::from)
    }
}

/// A `ScreenSource` named `id` over `runtime`'s visual observations, one a second by
/// default.
pub fn screen_source(id: FrameSourceId, runtime: Arc<dyn ComputerUseRuntime>) -> ScreenSource {
    ScreenSource::from_observer(id, Arc::new(RuntimeObserver::new(runtime)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use adk_computer_use::{
        ActionEnvelope, ActionPostcondition, ActionPreview, ComputerUseError, ControlLease,
        ExecutionReceipt, VerificationOutcome,
    };
    use futures::StreamExt;
    use serde_json::json;

    use super::*;
    use crate::media::{DecodedImage, FramePixels};
    use crate::stream::FrameSource;
    use crate::traits::Sampling;

    /// A desktop whose `snapshot` carries a PNG through adk-tool's MCP envelope, as
    /// computer-use-mcp answers it.
    struct Desktop {
        png: Vec<u8>,
        observed: AtomicUsize,
    }

    fn unsupported<T>(operation: &'static str) -> Result<T, ComputerUseError> {
        Err(ComputerUseError::Unsupported { operation })
    }

    #[async_trait]
    impl ComputerUseRuntime for Desktop {
        async fn discover_capabilities(&self) -> Result<Value, ComputerUseError> {
            Ok(json!({}))
        }
        async fn observe_visual(&self) -> Result<Value, ComputerUseError> {
            self.observed.fetch_add(1, Ordering::SeqCst);
            Ok(json!({
                "response": { "output": "1 window" },
                "inline_data": [{ "mime_type": "image/png", "data": self.png }],
            }))
        }
        async fn observe_semantic(&self) -> Result<Value, ComputerUseError> {
            Ok(json!({}))
        }
        async fn preview_action(&self, _: Value) -> Result<ActionPreview, ComputerUseError> {
            unsupported("preview_action")
        }
        async fn acquire_lease(
            &self,
            _: &ActionEnvelope,
        ) -> Result<ControlLease, ComputerUseError> {
            unsupported("acquire_lease")
        }
        async fn execute_action(
            &self,
            _: &ActionEnvelope,
            _: &ControlLease,
            _: Option<&str>,
        ) -> Result<ExecutionReceipt, ComputerUseError> {
            unsupported("execute_action")
        }
        async fn verify(
            &self,
            _: &ExecutionReceipt,
            _: Option<&ActionPostcondition>,
        ) -> Result<VerificationOutcome, ComputerUseError> {
            unsupported("verify")
        }
    }

    #[tokio::test]
    async fn each_frame_is_one_visual_observation_of_the_runtime() {
        let png = DecodedImage::filled(48, 24, [10, 120, 200, 255]).encode_png().unwrap();
        let desktop = Arc::new(Desktop { png, observed: AtomicUsize::new(0) });
        let source = screen_source(FrameSourceId("desk".into()), desktop.clone())
            .with_interval(Duration::from_millis(5));
        assert_eq!(source.id().0, "desk");
        let frames: Vec<_> = source
            .frames(Sampling { fps: 100.0, max_long_edge: None })
            .await
            .unwrap()
            .take(3)
            .collect()
            .await;
        assert_eq!(frames.len(), 3);
        for (index, frame) in frames.into_iter().enumerate() {
            let frame = frame.unwrap();
            assert_eq!(frame.index, index as u64);
            let FramePixels::Ephemeral(pixels) = &frame.pixels else {
                panic!("a screen frame is ephemeral until kept");
            };
            assert_eq!(pixels.size(), (48, 24));
            assert_eq!(pixels.pixel(0, 0), [10, 120, 200, 255]);
        }
        assert!(desktop.observed.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn a_runtime_error_reaches_the_stream_as_its_error() {
        struct Down;
        #[async_trait]
        impl VisualObserver for Down {
            async fn observe_visual(&self) -> adk_core::Result<Value> {
                Err(ComputerUseError::Mcp("server gone".into()).into())
            }
        }
        let error = Down.observe_visual().await.unwrap_err();
        assert_eq!(error.code, "tool.computer_use.mcp");
    }
}
