//! `DeviceSource` (spec 3.7 §7.13, Q9): the device listings of the three platforms, and
//! capture through a user-installed ffmpeg with `-f lavfi -i testsrc2` standing in for a
//! camera. The tests that run ffmpeg skip, saying so, when it is absent; nothing is
//! installed, and no camera is opened.
//!
//! `tests/fixtures/ffmpeg-devices/avfoundation-8.0.txt` is ffmpeg 8.0.1's listing on a Mac
//! with a USB camera. The others follow the formats of ffmpeg's source, since no Linux or
//! Windows machine recorded them: AVFoundation's `[uid:…]` suffix (`avfoundation.m` on
//! master), `-sources v4l2` with and without media types (`fftools/opt_common.c`), and
//! DirectShow's media-type form (`dshow.c`, ffmpeg 5 and later) and its headed form (4.4).

#![cfg(feature = "device")]

use std::time::Duration;

use adk_vision::stream::{
    CaptureDevice, DeviceApi, DeviceKind, DeviceSelector, DeviceSource, FfmpegBinary, FrameSource,
};
use adk_vision::{FrameSourceId, Sampling, VisionError};
use futures::StreamExt;

fn ffmpeg() -> Option<FfmpegBinary> {
    let binary = FfmpegBinary::from_env();
    match binary.check() {
        Ok(_) => Some(binary),
        Err(err) => {
            eprintln!("skipping: {err}");
            None
        }
    }
}

fn camera(index: u32, name: &str, selector: DeviceSelector) -> CaptureDevice {
    CaptureDevice { index, name: name.into(), kind: DeviceKind::Camera, selector }
}

#[test]
fn avfoundation_lists_video_devices_and_screens_without_audio() {
    let recorded = include_str!("fixtures/ffmpeg-devices/avfoundation-8.0.txt");
    assert_eq!(
        DeviceApi::AvFoundation.parse_device_list(recorded),
        vec![
            camera(0, "Huddly GO", DeviceSelector::Index(0)),
            camera(1, "OBS Virtual Camera", DeviceSelector::Index(1)),
            CaptureDevice {
                index: 2,
                name: "Capture screen 0".into(),
                kind: DeviceKind::Screen,
                selector: DeviceSelector::Index(2),
            },
        ]
    );
    let with_uids = include_str!("fixtures/ffmpeg-devices/avfoundation-uid.txt");
    let devices = DeviceApi::AvFoundation.parse_device_list(with_uids);
    assert_eq!(
        devices.iter().map(|d| (d.index, d.name.as_str(), d.kind)).collect::<Vec<_>>(),
        vec![
            (0, "FaceTime HD Camera", DeviceKind::Camera),
            (1, "Huddly GO", DeviceKind::Camera),
            (2, "Capture screen 0", DeviceKind::Screen),
            (3, "Capture screen 1", DeviceKind::Screen),
        ]
    );
}

#[test]
fn v4l2_lists_capture_nodes_by_number() {
    for (fixture, first) in [
        (
            include_str!("fixtures/ffmpeg-devices/v4l2-sources.txt"),
            "Integrated_Webcam_HD: Integrate",
        ),
        (
            include_str!("fixtures/ffmpeg-devices/v4l2-sources-4.4.txt"),
            "Integrated Camera: Integrated C",
        ),
    ] {
        assert_eq!(
            DeviceApi::V4l2.parse_device_list(fixture),
            vec![
                camera(0, first, DeviceSelector::Path("/dev/video0".into())),
                camera(2, "Huddly GO: Huddly GO", DeviceSelector::Path("/dev/video2".into())),
            ]
        );
    }
}

#[test]
fn directshow_lists_video_devices_in_both_formats() {
    let current = include_str!("fixtures/ffmpeg-devices/dshow.txt");
    assert_eq!(
        DeviceApi::DirectShow.parse_device_list(current),
        vec![
            camera(0, "Huddly GO", DeviceSelector::Name("Huddly GO".into())),
            camera(1, "OBS Virtual Camera", DeviceSelector::Name("OBS Virtual Camera".into())),
            camera(2, "Elgato Cam Link 4K", DeviceSelector::Name("Elgato Cam Link 4K".into())),
        ]
    );
    let legacy = include_str!("fixtures/ffmpeg-devices/dshow-4.4.txt");
    assert_eq!(
        DeviceApi::DirectShow.parse_device_list(legacy),
        vec![camera(0, "Integrated Camera", DeviceSelector::Name("Integrated Camera".into()))]
    );
}

#[test]
fn a_listing_with_no_devices_is_empty() {
    for api in [DeviceApi::AvFoundation, DeviceApi::V4l2, DeviceApi::DirectShow] {
        assert_eq!(api.parse_device_list("dummy: Immediate exit requested\n"), vec![], "{api}");
    }
}

#[test]
fn a_missing_ffmpeg_says_how_to_install_it() {
    let dir = tempfile::tempdir().unwrap();
    let missing = FfmpegBinary::at(dir.path().join("no-ffmpeg-here"));
    let err = DeviceSource::list_with(&missing, DeviceApi::AvFoundation).unwrap_err();
    assert!(matches!(err, VisionError::Unavailable { .. }), "{err}");
    let text = err.to_string();
    assert!(text.contains("brew install ffmpeg") && text.contains("ADK_VISION_FFMPEG"), "{text}");
}

#[tokio::test]
async fn capture_without_ffmpeg_says_how_to_install_it() {
    let dir = tempfile::tempdir().unwrap();
    let source = DeviceSource::test_pattern(FrameSourceId("pattern".into()))
        .with_ffmpeg(FfmpegBinary::at(dir.path().join("no-ffmpeg-here")));
    let err =
        source.frames(Sampling { fps: 1.0, max_long_edge: None }).await.err().expect("refused");
    assert!(matches!(err, VisionError::Unavailable { .. }), "{err}");
    assert!(err.to_string().contains("winget install ffmpeg"), "{err}");
}

/// An ffmpeg stand-in that prints `fixture` the way ffmpeg prints a listing.
#[cfg(unix)]
fn fake_ffmpeg(dir: &std::path::Path, fixture: &str, stream: &str) -> FfmpegBinary {
    use std::os::unix::fs::PermissionsExt;
    let listing = dir.join("listing.txt");
    std::fs::write(&listing, fixture).unwrap();
    let script = dir.join("ffmpeg");
    let redirect = if stream == "stderr" { " >&2" } else { "" };
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat '{}'{redirect}\nexit 251\n", listing.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    FfmpegBinary::at(script)
}

#[cfg(unix)]
#[test]
fn listing_reads_either_stream_whatever_the_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fake_ffmpeg(
        dir.path(),
        include_str!("fixtures/ffmpeg-devices/avfoundation-8.0.txt"),
        "stderr",
    );
    let devices = DeviceSource::list_with(&binary, DeviceApi::AvFoundation).unwrap();
    assert_eq!(devices.len(), 3);
    let dir = tempfile::tempdir().unwrap();
    let binary =
        fake_ffmpeg(dir.path(), include_str!("fixtures/ffmpeg-devices/v4l2-sources.txt"), "stdout");
    let devices = DeviceSource::list_with(&binary, DeviceApi::V4l2).unwrap();
    assert_eq!(devices.iter().map(|d| d.index).collect::<Vec<_>>(), vec![0, 2]);
}

#[cfg(unix)]
#[test]
fn an_ffmpeg_without_the_capture_input_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fake_ffmpeg(dir.path(), "Unknown input format: 'avfoundation'\n", "stderr");
    let err = DeviceSource::list_with(&binary, DeviceApi::AvFoundation).unwrap_err();
    assert!(err.to_string().contains("built without the `avfoundation` input"), "{err}");
    let dir = tempfile::tempdir().unwrap();
    let binary = fake_ffmpeg(
        dir.path(),
        "Auto-detected sources for v4l2:\nCannot list sources: Function not implemented\n",
        "stdout",
    );
    let err = DeviceSource::list_with(&binary, DeviceApi::V4l2).unwrap_err();
    assert!(err.to_string().contains("Function not implemented"), "{err}");
}

#[cfg(feature = "conformance")]
#[tokio::test(flavor = "multi_thread")]
async fn the_test_pattern_keeps_the_frame_source_contract() {
    use adk_vision::stream::check_frame_source;

    let Some(binary) = ffmpeg() else { return };
    let source = DeviceSource::test_pattern(FrameSourceId("pattern".into()))
        .with_ffmpeg(binary)
        .with_video_size(640, 360);
    let report =
        check_frame_source(&source, Sampling { fps: 5.0, max_long_edge: Some(320) }, 8).await;
    assert_eq!(report.sizes.into_iter().collect::<Vec<_>>(), vec![(320, 180)]);
    // ffmpeg samples a 30 fps capture down to 5.
    assert_eq!(report.min_gap, Some(Duration::from_millis(200)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capture_is_paced_like_a_camera() {
    let Some(binary) = ffmpeg() else { return };
    let source = DeviceSource::test_pattern(FrameSourceId("pattern".into()))
        .with_ffmpeg(binary)
        .with_video_size(160, 120);
    let started = std::time::Instant::now();
    let frames: Vec<_> = source
        .frames(Sampling { fps: 10.0, max_long_edge: None })
        .await
        .unwrap()
        .take(10)
        .collect()
        .await;
    assert_eq!(frames.len(), 10);
    // Ten frames at 10 a second take most of a second of real time, unlike a file.
    assert!(started.elapsed() >= Duration::from_millis(800), "{:?}", started.elapsed());
    let last = frames.last().unwrap().as_ref().unwrap();
    assert_eq!(last.pts, Duration::from_millis(900));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_that_falls_behind_sees_the_newest_frames() {
    let Some(binary) = ffmpeg() else { return };
    let source = DeviceSource::test_pattern(FrameSourceId("pattern".into()))
        .with_ffmpeg(binary)
        .with_video_size(160, 120);
    let mut stream = source.frames(Sampling { fps: 10.0, max_long_edge: None }).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    // Two seconds unread: about twenty frames, of which the source holds the newest four.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let next = stream.next().await.unwrap().unwrap();
    assert!(next.index >= first.index + 10, "{} then {}", first.index, next.index);
    assert!(next.pts >= first.pts + Duration::from_secs(1), "{:?} then {:?}", first.pts, next.pts);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_stream_kills_ffmpeg() {
    let Some(binary) = ffmpeg() else { return };
    // A size no other test uses, to find this ffmpeg among the processes.
    let source = DeviceSource::test_pattern(FrameSourceId("pattern".into()))
        .with_ffmpeg(binary)
        .with_video_size(162, 122);
    let mut stream = source.frames(Sampling { fps: 10.0, max_long_edge: None }).await.unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    let running = || {
        std::process::Command::new("pgrep")
            .args(["-f", "testsrc2=size=162x122"])
            .output()
            .map(|out| !out.stdout.is_empty())
            .unwrap_or(false)
    };
    assert!(running(), "ffmpeg captures while the stream is read");
    drop(stream);
    for _ in 0..100 {
        if !running() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("ffmpeg still runs after its stream was dropped");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_is_not_there_is_refused_with_what_to_try() {
    let Some(binary) = ffmpeg() else { return };
    if DeviceApi::native().is_none() {
        return;
    }
    let source = DeviceSource::new(FrameSourceId("nowhere".into()), DeviceSelector::Index(97))
        .with_ffmpeg(binary);
    let opened = source.frames(Sampling { fps: 1.0, max_long_edge: None }).await;
    let err = match opened {
        // AVFoundation and v4l2 refuse when ffmpeg opens the device; DirectShow when the
        // index is resolved against the listing.
        Ok(mut stream) => tokio::time::timeout(Duration::from_secs(20), stream.next())
            .await
            .expect("ffmpeg gives up on a missing device")
            .expect("an error before the stream ends")
            .expect_err("no frame from a missing device"),
        Err(err) => err,
    };
    assert!(matches!(err, VisionError::Unavailable { .. } | VisionError::InvalidInput(_)), "{err}");
    assert!(err.to_string().contains("sources list"), "{err}");
}
