//! A camera on this machine on the live camera surface (spec 3.7 §7.13, §8.2): switching it off
//! stops capture at the source — the `DeviceSource`'s ffmpeg exits — and switching it on opens
//! the device again, with frames from the next one; Stop all closes the device the same way and
//! Resume all opens it again (V-I17). ffmpeg's `testsrc2` pattern stands in for a
//! camera; the test skips, saying so, without ffmpeg or `pgrep`. Frames are held in memory only:
//! the cameras keep no artifacts, so nothing is written.
//!
//! The run on a real camera is ignored by default; on macOS the terminal needs camera access:
//!
//! ```text
//! ADK_VISION_TEST_DEVICE=0 cargo nextest run -p adk-surface --features vision-camera \
//!     --test camera_device_tests --run-ignored only --no-capture
//! ```
//!
//! `ADK_VISION_TEST_DEVICE` is an index, a name or a `/dev/videoN` node, as
//! `adk-rust vision sources list` shows them (default `0`).

#![cfg(all(feature = "vision-camera", unix))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_surface::camera::{CameraFacts, LiveCamera, PipelineCameras};
use adk_vision::stream::{DeviceSelector, DeviceSource, FfmpegBinary};
use adk_vision::{FrameSourceId, Sampling};

const APP: &str = "desk";
const OWNER: &str = "ana";

/// How long the ffmpeg child may take to exit after the camera is switched off.
const EXIT_BOUND: Duration = Duration::from_secs(2);

/// The ffmpeg processes this test process started, or `None` without `pgrep`.
fn ffmpeg_children() -> Option<usize> {
    let out = std::process::Command::new("pgrep")
        .args(["-P", &std::process::id().to_string(), "ffmpeg"])
        .output()
        .ok()?;
    // `pgrep` exits 1 when nothing matches and 2 or more on an error.
    (out.status.code().unwrap_or(2) < 2)
        .then(|| String::from_utf8_lossy(&out.stdout).lines().count())
}

fn ready() -> bool {
    if let Err(error) = FfmpegBinary::from_env().check() {
        eprintln!("skipping: {error}");
        return false;
    }
    if ffmpeg_children().is_none() {
        eprintln!("skipping: `pgrep` is not available to count ffmpeg processes");
        return false;
    }
    true
}

/// Waits up to `within` for `done`; how long it took.
async fn until(what: &str, within: Duration, mut done: impl FnMut() -> bool) -> Duration {
    let started = Instant::now();
    while started.elapsed() < within {
        if done() {
            return started.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out after {within:?} waiting for {what}");
}

/// The `reason` of every `action` record in `audit`.
fn reasons(audit: &InMemoryAuditRecorder, action: &str) -> Vec<String> {
    audit
        .records()
        .into_iter()
        .filter(|record| record.action == action)
        .filter_map(|record| record.ext.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .collect()
}

/// Runs `source` as camera `desk`: frames, Off → no ffmpeg within [`EXIT_BOUND`] and no frame,
/// On → ffmpeg and frames again, Stop all → no ffmpeg and no frame, Resume all → ffmpeg and
/// frames again, Stop → no ffmpeg.
async fn off_stops_the_device(source: DeviceSource) {
    let camera = LiveCamera::stream(
        CameraFacts::new("desk", "Desk camera", APP, "s-desk"),
        Arc::new(source),
    )
    .sampling(Sampling { fps: 5.0, max_long_edge: Some(320) });
    let audit = InMemoryAuditRecorder::new();
    let live = PipelineCameras::builder(APP, OWNER)
        .audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default())
        .camera(camera)
        .build()
        .unwrap();
    let cameras = live.start();
    let observed = || live.counts("desk").unwrap().observed;

    let first = until("the first frames", Duration::from_secs(60), || observed() >= 3).await;
    assert_eq!(ffmpeg_children(), Some(1), "one ffmpeg captures");
    assert_eq!(live.is_capturing("desk"), Some(true));
    assert!(cameras.latest("desk").await.is_some(), "a frame is shown");

    let off = Instant::now();
    cameras.switch("desk", false, OWNER, chrono::Utc::now()).await.unwrap();
    let answered = off.elapsed();
    assert_eq!(live.is_capturing("desk"), Some(false), "closed before the switch answered");
    let exited = until("ffmpeg to exit", EXIT_BOUND, || ffmpeg_children() == Some(0)).await;
    let seen = observed();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(observed(), seen, "no frame while off");
    assert_eq!(ffmpeg_children(), Some(0), "nothing opens the device while off");
    assert!(cameras.latest("desk").await.is_none(), "off forgets the frames held");

    let on = Instant::now();
    cameras.switch("desk", true, OWNER, chrono::Utc::now()).await.unwrap();
    let again = until("frames again", Duration::from_secs(60), || observed() > seen).await;
    assert_eq!(ffmpeg_children(), Some(1), "the device is open again");
    let reopened = on.elapsed();

    // Stop all closes the device as off does; Resume all opens it again.
    let stop_all = Instant::now();
    cameras.freeze_all().await.unwrap();
    let stop_answered = stop_all.elapsed();
    assert_eq!(live.is_capturing("desk"), Some(false), "closed before Stop all answered");
    let stop_exited =
        until("ffmpeg to exit after Stop all", EXIT_BOUND, || ffmpeg_children() == Some(0)).await;
    let frozen = observed();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(observed(), frozen, "no frame while everything is stopped");
    assert_eq!(ffmpeg_children(), Some(0), "nothing opens the device while everything is stopped");
    cameras.thaw_all().await.unwrap();
    let resumed =
        until("frames after Resume all", Duration::from_secs(60), || observed() > frozen).await;
    assert_eq!(ffmpeg_children(), Some(1), "the device is open again");
    assert_eq!(
        reasons(&audit, adk_vision::stream::VISION_SOURCE_STOPPED),
        ["switchedOff", "stopAll"]
    );
    assert_eq!(
        reasons(&audit, adk_vision::stream::VISION_SOURCE_STARTED),
        ["switchedOn", "resumeAll"]
    );

    cameras.stop("desk", OWNER, chrono::Utc::now()).await.unwrap();
    until("ffmpeg to exit after the stop", EXIT_BOUND, || ffmpeg_children() == Some(0)).await;
    eprintln!(
        "first frames {first:?}; off answered in {answered:?}, ffmpeg gone {exited:?} after; \
         on: frames again {again:?} ({reopened:?} after the switch); Stop all answered in \
         {stop_answered:?}, ffmpeg gone {stop_exited:?} after; Resume all: frames again {resumed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_off_stops_a_device_cameras_ffmpeg() {
    if !ready() {
        return;
    }
    let pattern =
        DeviceSource::test_pattern(FrameSourceId("desk".into())).with_video_size(320, 240);
    off_stops_the_device(pattern).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "opens a real camera; see the module docs"]
async fn switching_off_stops_a_real_camera() {
    assert!(ready(), "the real camera run needs ffmpeg and pgrep");
    let device = std::env::var("ADK_VISION_TEST_DEVICE").unwrap_or_else(|_| "0".into());
    let camera = DeviceSource::new(FrameSourceId("desk".into()), DeviceSelector::parse(&device));
    off_stops_the_device(camera).await;
}
