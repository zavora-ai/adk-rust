//! `ScreenSource`, `BrowserSource` and `SurfaceCameraSource` (spec 3.7 §7.13) over
//! scripted actuators, observers, tools and a pushed camera feed.

#![cfg(all(feature = "screen", feature = "browser", feature = "surface-camera"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    ActuationOutput, CallbackContext, ComputerAction, ComputerActuator, Content, EventActions,
    ReadonlyContext, Screenshot, Tool, ToolContext,
};
use adk_vision::stream::{
    BrowserSource, FrameSource, ScreenSource, SurfaceCameraSource, VisualObserver,
};
use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, Sampling, VisionError};
use base64::Engine;
use futures::StreamExt;
use serde_json::{Value, json};

fn png(w: u32, h: u32, shade: u8) -> Vec<u8> {
    DecodedImage::filled(w, h, [shade, shade, 255 - shade, 255]).encode_png().unwrap()
}

fn size(frame: &Frame) -> (u32, u32) {
    match &frame.pixels {
        FramePixels::Ephemeral(pixels) => pixels.size(),
        FramePixels::Stored(image) => (image.width, image.height),
    }
}

struct Desktop {
    shots: AtomicUsize,
}

#[async_trait::async_trait]
impl ComputerActuator for Desktop {
    fn display_size(&self) -> Option<(u32, u32)> {
        Some((3024, 1964))
    }
    async fn perform(&self, action: ComputerAction) -> adk_core::Result<ActuationOutput> {
        assert_eq!(action, ComputerAction::Screenshot, "a screen source only ever looks");
        let n = self.shots.fetch_add(1, Ordering::SeqCst);
        Ok(ActuationOutput::Image(Screenshot::png(png(302, 196, n as u8)).unwrap()))
    }
}

#[tokio::test]
async fn a_screen_is_captured_at_its_interval_and_scaled_down() {
    let desktop = Arc::new(Desktop { shots: AtomicUsize::new(0) });
    let screen = ScreenSource::from_actuator(FrameSourceId("display-1".into()), desktop.clone())
        .with_interval(Duration::from_millis(40));
    let frames: Vec<Frame> = screen
        .frames(Sampling { fps: 100.0, max_long_edge: Some(151) })
        .await
        .unwrap()
        .take(4)
        .map(|f| f.unwrap())
        .collect()
        .await;
    assert_eq!(frames.iter().map(|f| f.index).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    assert!(frames.iter().all(|f| size(f) == (151, 98)));
    for pair in frames.windows(2) {
        assert!(
            pair[1].pts - pair[0].pts >= Duration::from_millis(30),
            "{:?}",
            pair[1].pts - pair[0].pts
        );
    }
    // The sampling interval is longer than the capture interval here, so it wins.
    let slow = ScreenSource::from_actuator(FrameSourceId("display-1".into()), desktop)
        .with_interval(Duration::from_millis(1));
    let frames: Vec<Frame> = slow
        .frames(Sampling { fps: 20.0, max_long_edge: None })
        .await
        .unwrap()
        .take(3)
        .map(|f| f.unwrap())
        .collect()
        .await;
    assert!(frames[2].pts - frames[0].pts >= Duration::from_millis(90));
}

#[cfg(feature = "conformance")]
#[tokio::test]
async fn screen_and_browser_source_conformance() {
    use adk_vision::stream::check_frame_source;

    let desktop = Arc::new(Desktop { shots: AtomicUsize::new(0) });
    let screen = ScreenSource::from_actuator(FrameSourceId("display-1".into()), desktop)
        .with_interval(Duration::from_millis(5));
    check_frame_source(&screen, Sampling { fps: 40.0, max_long_edge: Some(100) }, 4).await;
    let ctx: Arc<dyn ToolContext> = Arc::new(Ctx::default());
    let browser =
        BrowserSource::new(FrameSourceId("crm".into()), Arc::new(ScreenshotTool::default()), ctx)
            .with_interval(Duration::from_millis(5));
    check_frame_source(&browser, Sampling { fps: 40.0, max_long_edge: None }, 4).await;
}

/// A computer-use-mcp runtime's `snapshot`, through adk-tool's MCP envelope.
struct Observations;

#[async_trait::async_trait]
impl VisualObserver for Observations {
    async fn observe_visual(&self) -> adk_core::Result<Value> {
        Ok(json!({
            "response": { "output": { "windows": 2 } },
            "inline_data": [{ "mime_type": "image/png", "data": png(64, 40, 9) }],
            "file_data": []
        }))
    }
}

#[tokio::test]
async fn computer_use_observations_become_frames() {
    let screen = ScreenSource::from_observer(FrameSourceId("cu".into()), Arc::new(Observations))
        .with_interval(Duration::from_millis(5));
    let mut frames = screen.frames(Sampling { fps: 50.0, max_long_edge: None }).await.unwrap();
    assert_eq!(size(&frames.next().await.unwrap().unwrap()), (64, 40));
}

struct Broken;

#[async_trait::async_trait]
impl VisualObserver for Broken {
    async fn observe_visual(&self) -> adk_core::Result<Value> {
        Ok(json!({ "output": "the window closed" }))
    }
}

#[tokio::test]
async fn an_observation_without_an_image_ends_the_stream_with_the_reason() {
    let screen = ScreenSource::from_observer(FrameSourceId("cu".into()), Arc::new(Broken));
    let mut frames = screen.frames(Sampling { fps: 50.0, max_long_edge: None }).await.unwrap();
    let err = frames.next().await.unwrap().unwrap_err();
    assert!(matches!(err, VisionError::Parse { .. }), "{err}");
    assert!(frames.next().await.is_none());
}

/// `browser_screenshot`, scripted.
#[derive(Default)]
struct ScreenshotTool {
    args: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl Tool for ScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }
    fn description(&self) -> &str {
        "Take a screenshot of the current page or a specific element."
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.args.lock().unwrap().push(args);
        let image = base64::engine::general_purpose::STANDARD.encode(png(80, 48, 30));
        Ok(
            json!({ "success": true, "base64_image": image, "saved_to_artifacts": false, "artifact_name": null }),
        )
    }
}

#[tokio::test]
async fn a_browser_element_is_captured_through_browser_screenshot() {
    let tool = Arc::new(ScreenshotTool::default());
    let ctx: Arc<dyn ToolContext> = Arc::new(Ctx::default());
    let browser = BrowserSource::new(FrameSourceId("crm".into()), tool.clone(), ctx)
        .with_selector("#queue")
        .with_interval(Duration::from_millis(5));
    let frames: Vec<Frame> = browser
        .frames(Sampling { fps: 100.0, max_long_edge: None })
        .await
        .unwrap()
        .take(2)
        .map(|f| f.unwrap())
        .collect()
        .await;
    assert!(frames.iter().all(|f| size(f) == (80, 48)));
    let args = tool.args.lock().unwrap();
    assert!(
        args.iter().all(|a| *a == json!({ "selector": "#queue" })),
        "never saves to artifacts: {args:?}"
    );
}

#[tokio::test]
async fn the_web_home_camera_drops_frames_while_off_and_on_switching_off() {
    let (camera, feed) = SurfaceCameraSource::new(FrameSourceId("web-home".into()));
    let now = chrono::Utc::now();
    let at = |ms: i64| now + chrono::TimeDelta::milliseconds(ms);
    // No stream reads it yet: dropped, even when on.
    feed.switch(true);
    assert!(!feed.push_frame(png(16, 12, 1), at(0)));
    let mut frames = camera.frames(Sampling { fps: 5.0, max_long_edge: None }).await.unwrap();
    feed.switch(false);
    assert!(!feed.push_frame(png(16, 12, 2), at(10)), "off: dropped on the server");
    feed.switch(true);
    // 20 fps pushed, 5 fps kept: frames 0, 4 and 8 by capture time.
    for i in 0..9 {
        assert!(feed.push_frame(png(16, 12, 10 + i as u8), at(1_000 + i * 50)));
        if i % 4 == 0 {
            let frame = frames.next().await.unwrap().unwrap();
            assert_eq!(frame.index, i as u64);
            assert_eq!(frame.pts, Duration::from_millis(i as u64 * 50));
        }
    }
    // Queued frames go when the camera is switched off.
    feed.push_frame(png(16, 12, 99), at(2_000));
    feed.switch(false);
    feed.switch(true);
    feed.push_frame(png(16, 12, 100), at(2_400));
    let next = frames.next().await.unwrap().unwrap();
    assert_eq!(
        next.captured_at,
        at(2_400),
        "the frame queued before the switch was dropped with it"
    );

    // A second reader replaces the first, whose stream ends.
    let _second = camera.frames(Sampling { fps: 5.0, max_long_edge: None }).await.unwrap();
    assert!(frames.next().await.is_none());
}

#[tokio::test]
async fn a_corrupt_camera_frame_is_skipped_not_fatal() {
    let (camera, feed) = SurfaceCameraSource::new(FrameSourceId("web-home".into()));
    let mut frames = camera.frames(Sampling { fps: 100.0, max_long_edge: None }).await.unwrap();
    feed.switch(true);
    let now = chrono::Utc::now();
    assert!(feed.push_frame(b"\xFF\xD8 not really a jpeg".to_vec(), now));
    assert!(feed.push_frame(png(8, 8, 5), now + chrono::TimeDelta::seconds(1)));
    let frame = frames.next().await.unwrap().unwrap();
    assert_eq!(frame.index, 1);
}

/// The context a watcher process calls the tool under.
struct Ctx {
    content: Content,
    actions: Mutex<EventActions>,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            content: Content { role: "user".into(), parts: Vec::new() },
            actions: Mutex::new(EventActions::default()),
        }
    }
}

#[async_trait::async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        "watch"
    }
    fn agent_name(&self) -> &str {
        "watcher"
    }
    fn user_id(&self) -> &str {
        "u1"
    }
    fn app_name(&self) -> &str {
        "crm"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait::async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait::async_trait]
impl ToolContext for Ctx {
    fn function_call_id(&self) -> &str {
        "watch-1"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}
