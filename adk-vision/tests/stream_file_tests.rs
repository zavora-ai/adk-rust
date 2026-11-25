//! `FileSource` through a user-installed ffmpeg (spec 3.7 §7.13, Q9). The MP4 is encoded
//! from the generated conveyor clip at test time; the tests that need ffmpeg skip, saying
//! so, when it is absent. Nothing is installed.

#![cfg(all(feature = "ffmpeg", feature = "stream-fixtures"))]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec, write_mp4};
use adk_vision::stream::{FfmpegBinary, FileSource, FrameSource, StreamPipeline};
use adk_vision::{
    DecodedImage, Frame, FramePixels, FrameSourceId, Sampling, VisionCx, VisionError,
};
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

fn clip() -> Arc<ConveyorClip> {
    Arc::new(ConveyorClip::new(ConveyorSpec::default()))
}

/// Writes `frames` frames of the clip as an H.264 MP4.
fn mp4(
    binary: &FfmpegBinary,
    dir: &Path,
    clip: &Arc<ConveyorClip>,
    frames: u64,
) -> std::path::PathBuf {
    let path = dir.join("conveyor.mp4");
    let fps = clip.truth().spec.fps;
    write_mp4(binary.path(), &path, &|i| clip.render(i), frames, fps).unwrap();
    path
}

fn pixels(frame: &Frame) -> &DecodedImage {
    match &frame.pixels {
        FramePixels::Ephemeral(pixels) => pixels,
        FramePixels::Stored(_) => panic!("stream frames are ephemeral"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ffmpeg_samples_and_scales_the_file() {
    let Some(binary) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = clip();
    let path = mp4(&binary, dir.path(), &clip, 50);
    let source = FileSource::new(FrameSourceId("clip".into()), &path).with_ffmpeg(binary);
    let frames: Vec<Frame> = source
        .frames(Sampling { fps: 2.0, max_long_edge: Some(320) })
        .await
        .unwrap()
        .map(|f| f.unwrap())
        .collect()
        .await;
    // Five seconds at 2 fps.
    assert_eq!(frames.len(), 10, "{:?}", frames.iter().map(|f| f.pts).collect::<Vec<_>>());
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.index, i as u64);
        assert_eq!(frame.pts, Duration::from_millis(i as u64 * 500));
        assert_eq!(pixels(frame).size(), (320, 180));
        // The frame is the clip at its presentation time, scaled down.
        let source_index = i as u64 * 5;
        let expected =
            adk_vision::stream::Sampler::new(Sampling { fps: 1.0, max_long_edge: Some(320) })
                .unwrap()
                .fit(clip.render(source_index));
        let error = mean_error(pixels(frame), &expected);
        assert!(error < 10.0, "frame {i}: mean error {error}");
    }
    // Media time drives `captured_at`, so the governor meters the file as it was recorded.
    let span = frames.last().unwrap().captured_at - frames[0].captured_at;
    assert_eq!(span, chrono::TimeDelta::milliseconds(4_500));
}

fn mean_error(a: &DecodedImage, b: &DecodedImage) -> f64 {
    let (sum, n) =
        a.rgba().chunks(4).zip(b.rgba().chunks(4)).fold((0u64, 0u64), |(s, n), (p, q)| {
            (
                s + (0..3)
                    .map(|c| u64::from((i16::from(p[c]) - i16::from(q[c])).unsigned_abs()))
                    .sum::<u64>(),
                n + 3,
            )
        });
    sum as f64 / n as f64
}

#[cfg(feature = "conformance")]
#[tokio::test(flavor = "multi_thread")]
async fn file_source_conformance() {
    use adk_vision::stream::check_frame_source;

    let Some(binary) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = mp4(&binary, dir.path(), &clip(), 40);
    let source = FileSource::new(FrameSourceId("clip".into()), &path).with_ffmpeg(binary);
    let report =
        check_frame_source(&source, Sampling { fps: 4.0, max_long_edge: Some(320) }, 12).await;
    assert_eq!(report.sizes.into_iter().collect::<Vec<_>>(), vec![(320, 180)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pipeline_reads_a_file_to_its_end() {
    let Some(binary) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = mp4(&binary, dir.path(), &clip(), 30);
    let source = Arc::new(FileSource::new(FrameSourceId("clip".into()), &path).with_ffmpeg(binary));
    let summary = StreamPipeline::builder(source)
        .sampling(Sampling { fps: 5.0, max_long_edge: None })
        .build()
        .unwrap()
        .run(&VisionCx::new())
        .await
        .unwrap();
    assert_eq!(summary.observations, 15);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_stream_ends_the_ffmpeg_process() {
    let Some(binary) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = mp4(&binary, dir.path(), &clip(), 100);
    let source =
        FileSource::new(FrameSourceId("clip".into()), &path).with_ffmpeg(binary).paced(true);
    let mut stream = source.frames(Sampling { fps: 10.0, max_long_edge: None }).await.unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    let running = || {
        std::process::Command::new("pgrep")
            .args(["-f", &path.display().to_string()])
            .output()
            .map(|out| !out.stdout.is_empty())
            .unwrap_or(false)
    };
    assert!(running(), "ffmpeg plays the file in real time");
    drop(stream);
    for _ in 0..100 {
        if !running() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("ffmpeg still runs after its stream was dropped");
}

#[tokio::test]
async fn a_missing_ffmpeg_says_how_to_install_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, b"not decoded").unwrap();
    let source = FileSource::new(FrameSourceId("clip".into()), &path)
        .with_ffmpeg(FfmpegBinary::at(dir.path().join("no-ffmpeg-here")));
    let err =
        source.frames(Sampling { fps: 1.0, max_long_edge: None }).await.err().expect("refused");
    assert!(matches!(err, VisionError::Unavailable { .. }), "{err}");
    let text = err.to_string();
    assert!(text.contains("brew install ffmpeg") && text.contains("apt install ffmpeg"), "{text}");
    assert!(text.contains("ADK_VISION_FFMPEG"), "{text}");
}

#[tokio::test]
async fn only_mp4_and_mov_files_are_read() {
    let source = FileSource::new(FrameSourceId("notes".into()), "notes.txt");
    let err =
        source.frames(Sampling { fps: 1.0, max_long_edge: None }).await.err().expect("refused");
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
    let missing = FileSource::new(FrameSourceId("gone".into()), "/nonexistent/clip.mov");
    let err =
        missing.frames(Sampling { fps: 1.0, max_long_edge: None }).await.err().expect("refused");
    assert!(matches!(err, VisionError::Io { .. }), "{err}");
}
