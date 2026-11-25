//! `RtspSource` against the in-process RTSP server serving the generated conveyor clip
//! (spec 3.7 §7.13, §11): MJPEG decoded in process, sampling before decode, credentials,
//! drop-oldest under a slow consumer, the whole pipeline with a detector and tracker, and
//! H.264 through a user-installed ffmpeg (skipped when it is absent).

#![cfg(all(feature = "rtsp", feature = "stream-fixtures"))]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_vision::stream::fixtures::{
    ConveyorClip, ConveyorSpec, RtspTestOptions, RtspTestServer, TestVideo,
};
use adk_vision::stream::{FrameSource, Observation, ObservationSink, RtspSource, StreamPipeline};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, Frame, FramePixels,
    FrameSourceId, GreedyIouTracker, Locality, RawRegion, Sampling, SentImage, VisionCx,
    VisionError, VisionRuntime,
};
use futures::StreamExt;

fn clip() -> Arc<ConveyorClip> {
    Arc::new(ConveyorClip::new(ConveyorSpec::default()))
}

fn mjpeg(clip: &Arc<ConveyorClip>, frames: u64) -> TestVideo {
    let render = Arc::clone(clip);
    TestVideo::Mjpeg {
        render: Arc::new(move |i| render.render(i)),
        frames,
        fps: clip.truth().spec.fps,
        quality: 85,
    }
}

fn fast() -> RtspTestOptions {
    RtspTestOptions { speed: 8.0, ..RtspTestOptions::default() }
}

fn pixels(frame: &Frame) -> &DecodedImage {
    match &frame.pixels {
        FramePixels::Ephemeral(pixels) => pixels,
        FramePixels::Stored(_) => panic!("stream frames are ephemeral"),
    }
}

/// Mean absolute difference per channel, alpha excluded.
fn difference(a: &DecodedImage, b: &DecodedImage) -> f64 {
    assert_eq!(a.size(), b.size());
    let (sum, n) =
        a.rgba().chunks(4).zip(b.rgba().chunks(4)).fold((0u64, 0u64), |(sum, n), (p, q)| {
            let d: u64 =
                (0..3).map(|c| u64::from((i16::from(p[c]) - i16::from(q[c])).unsigned_abs())).sum();
            (sum + d, n + 3)
        });
    sum as f64 / n as f64
}

async fn collect(source: &RtspSource, sampling: Sampling, limit: usize) -> Vec<Frame> {
    let stream = source.frames(sampling).await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), stream.take(limit).map(|f| f.unwrap()).collect())
        .await
        .expect("frames within a minute")
}

#[tokio::test(flavor = "multi_thread")]
async fn mjpeg_frames_decode_to_the_rendered_clip_in_order() {
    let clip = clip();
    let server = RtspTestServer::start_with(mjpeg(&clip, 30), fast()).await.unwrap();
    let source = RtspSource::new(FrameSourceId("conveyor".into()), &server.url()).unwrap();
    let frames = collect(&source, Sampling { fps: 10.0, max_long_edge: None }, 30).await;
    assert_eq!(frames.len(), 30);
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.index, i as u64);
        assert_eq!(frame.pts, Duration::from_millis(i as u64 * 100), "RTP timestamps at 90 kHz");
        let error = difference(pixels(frame), &clip.render(frame.index));
        assert!(error < 4.0, "frame {i}: mean error {error}");
    }
    server.shutdown().await;
}

#[cfg(feature = "conformance")]
#[tokio::test(flavor = "multi_thread")]
async fn rtsp_source_conformance() {
    use adk_vision::stream::check_frame_source;

    let clip = clip();
    let options = RtspTestOptions { looping: true, ..fast() };
    let server = RtspTestServer::start_with(mjpeg(&clip, 20), options).await.unwrap();
    let source = RtspSource::new(FrameSourceId("conveyor".into()), &server.url()).unwrap();
    let report =
        check_frame_source(&source, Sampling { fps: 5.0, max_long_edge: Some(320) }, 12).await;
    assert_eq!(report.sizes.into_iter().collect::<Vec<_>>(), vec![(320, 180)]);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sampling_keeps_the_rate_and_scales_frames_down() {
    let clip = clip();
    let server = RtspTestServer::start_with(mjpeg(&clip, 40), fast()).await.unwrap();
    let source = RtspSource::new(FrameSourceId("conveyor".into()), &server.url()).unwrap();
    let frames = collect(&source, Sampling { fps: 2.5, max_long_edge: Some(320) }, 10).await;
    let indices: Vec<u64> = frames.iter().map(|f| f.index).collect();
    assert_eq!(indices, vec![0, 4, 8, 12, 16, 20, 24, 28, 32, 36]);
    assert!(frames.iter().all(|f| pixels(f).size() == (320, 180)));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn credentials_in_the_url_authenticate_and_never_appear_in_errors() {
    let clip = clip();
    let options =
        RtspTestOptions { credentials: Some(("operator".into(), "s3cret-pw".into())), ..fast() };
    let server = RtspTestServer::start_with(mjpeg(&clip, 5), options).await.unwrap();
    let with_password = server.url().replacen("rtsp://", "rtsp://operator:s3cret-pw@", 1);
    let source = RtspSource::new(FrameSourceId("conveyor".into()), &with_password).unwrap();
    assert!(!format!("{source:?}").contains("s3cret-pw"));
    assert_eq!(collect(&source, Sampling { fps: 10.0, max_long_edge: None }, 5).await.len(), 5);

    let wrong = server.url().replacen("rtsp://", "rtsp://operator:not-the-pw@", 1);
    let source = RtspSource::new(FrameSourceId("conveyor".into()), &wrong).unwrap();
    let err = match source.frames(Sampling { fps: 10.0, max_long_edge: None }).await {
        Ok(_) => panic!("a wrong password must not play"),
        Err(err) => err,
    };
    assert!(matches!(err, VisionError::Unavailable { .. }), "{err}");
    let text = err.to_string();
    assert!(!text.contains("not-the-pw") && !text.contains("s3cret-pw"), "{text}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_camera_is_unavailable() {
    let source = RtspSource::new(FrameSourceId("gone".into()), "rtsp://127.0.0.1:9/none").unwrap();
    let err =
        source.frames(Sampling { fps: 1.0, max_long_edge: None }).await.err().expect("refused");
    assert_eq!(err.category(), adk_core::ErrorCategory::Unavailable, "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_consumer_gets_the_newest_frames_and_the_rest_are_dropped() {
    // Small frames, so the server encodes far faster than the consumer reads.
    let clip = Arc::new(ConveyorClip::new(ConveyorSpec {
        width: 160,
        height: 96,
        ..ConveyorSpec::default()
    }));
    let options = RtspTestOptions { speed: 0.0, ..RtspTestOptions::default() };
    let server = RtspTestServer::start_with(mjpeg(&clip, 80), options).await.unwrap();
    let source = RtspSource::new(FrameSourceId("conveyor".into()), &server.url()).unwrap();
    let mut stream = source.frames(Sampling { fps: 1_000.0, max_long_edge: None }).await.unwrap();
    let mut seen = Vec::new();
    // The server hangs up after the last frame, which ends the stream.
    while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_secs(20), stream.next()).await {
        seen.push(frame.unwrap().index);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert!(seen.len() < 80, "every frame arrived: {}", seen.len());
    assert_eq!(seen.last(), Some(&79), "the newest frame is never the one dropped");
    assert!(seen.windows(2).all(|w| w[0] < w[1]));
    server.shutdown().await;
}

/// Finds the orange printed parts: columns of part-coloured pixels grouped into boxes.
struct PartDetector(BackendDescriptor);

#[async_trait::async_trait]
impl Detector for PartDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let pixels = image.pixels();
        let (w, h) = pixels.size();
        let orange = |x: u32, y: u32| {
            let [r, g, b, _] = pixels.pixel(x, y);
            r > 170 && g > 70 && g < 170 && b < 100
        };
        let columns: Vec<Option<(u32, u32)>> = (0..w)
            .map(|x| {
                let ys: Vec<u32> = (0..h).filter(|y| orange(x, *y)).collect();
                (ys.len() >= 4).then(|| (ys[0], *ys.last().unwrap()))
            })
            .collect();
        let mut regions = Vec::new();
        let mut x = 0;
        while x < w as usize {
            if columns[x].is_none() {
                x += 1;
                continue;
            }
            let start = x;
            let (mut y0, mut y1) = (u32::MAX, 0);
            while x < w as usize
                && let Some((a, b)) = columns[x]
            {
                y0 = y0.min(a);
                y1 = y1.max(b);
                x += 1;
            }
            if x - start >= 6 {
                let bbox = BBox::new(start as f32, y0 as f32, x as f32, (y1 + 1) as f32);
                regions.push(RawRegion::boxed(bbox, "part", 0.95));
            }
        }
        Ok(regions)
    }
}

#[derive(Default)]
struct Seen(Mutex<Vec<Observation>>);

#[async_trait::async_trait]
impl ObservationSink for Seen {
    async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        self.0.lock().unwrap().push(observation);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pipeline_tracks_each_part_over_rtsp() {
    // Twelve seconds of belt at 5 fps: parts 0 to 8 cross the frame.
    let clip = clip();
    let server = RtspTestServer::start_with(mjpeg(&clip, 120), fast()).await.unwrap();
    let source =
        Arc::new(RtspSource::new(FrameSourceId("conveyor".into()), &server.url()).unwrap());
    let detector = Arc::new(PartDetector(BackendDescriptor::new(
        "parts",
        Locality::InProcess,
        CoordFrame::SentPixels,
    )));
    let runtime = Arc::new(VisionRuntime::builder().detector(detector).build().unwrap());
    let seen = Arc::new(Seen::default());
    let summary = StreamPipeline::builder(source)
        .sampling(Sampling { fps: 5.0, max_long_edge: None })
        .detector(runtime, DetectQuery::Classes(vec!["part".into()]))
        .tracker(Box::new(GreedyIouTracker::new(0.2, 3)))
        .sink(seen.clone())
        .build()
        .unwrap()
        .run(&VisionCx::new())
        .await
        .unwrap();
    assert_eq!(summary.observations, 60);
    let observations = seen.0.lock().unwrap().clone();
    let mut tracks_per_part: std::collections::BTreeMap<usize, BTreeSet<u64>> = Default::default();
    for observation in observations.iter() {
        let truth = clip.visible(observation.frame.index);
        // Parts more than 10 px into the frame are found where the clip drew them.
        for part in truth.iter().filter(|p| p.bbox[2] - p.bbox[0] > 10.0) {
            let found = observation
                .regions
                .iter()
                .find(|r| {
                    r.shape.bbox().unwrap().iou(&BBox::new(
                        part.bbox[0],
                        part.bbox[1],
                        part.bbox[2],
                        part.bbox[3],
                    )) > 0.6
                })
                .unwrap_or_else(|| {
                    panic!("frame {}: part {} not found", observation.frame.index, part.index)
                });
            tracks_per_part.entry(part.index).or_default().insert(found.track.unwrap().0);
        }
    }
    assert!(tracks_per_part.len() >= 7, "{tracks_per_part:?}");
    for (part, tracks) in &tracks_per_part {
        assert_eq!(tracks.len(), 1, "part {part} split across tracks {tracks:?}");
    }
    let distinct: BTreeSet<u64> = tracks_per_part.values().flatten().copied().collect();
    assert_eq!(distinct.len(), tracks_per_part.len(), "no two parts share a track");
    server.shutdown().await;
}

#[cfg(feature = "ffmpeg")]
#[tokio::test(flavor = "multi_thread")]
async fn h264_cameras_decode_through_a_user_installed_ffmpeg() {
    use adk_vision::stream::FfmpegBinary;
    use adk_vision::stream::fixtures::H264Stream;

    let ffmpeg = FfmpegBinary::from_env();
    if let Err(err) = ffmpeg.check() {
        eprintln!("skipping the H.264 RTSP test: {err}");
        return;
    }
    let clip = clip();
    let render = Arc::clone(&clip);
    let stream = tokio::task::spawn_blocking({
        let path = ffmpeg.path().to_path_buf();
        move || H264Stream::encode(&path, &|i| render.render(i), 30, 10)
    })
    .await
    .unwrap();
    let stream = match stream {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("skipping the H.264 RTSP test: this ffmpeg cannot encode H.264 ({err})");
            return;
        }
    };
    let server =
        RtspTestServer::start_with(TestVideo::H264 { stream: Arc::new(stream), fps: 10 }, fast())
            .await
            .unwrap();
    let source = RtspSource::new(FrameSourceId("conveyor-h264".into()), &server.url())
        .unwrap()
        .with_ffmpeg(ffmpeg);
    let frames = collect(&source, Sampling { fps: 5.0, max_long_edge: None }, 15).await;
    assert_eq!(
        frames.iter().map(|f| f.index).collect::<Vec<_>>(),
        (0..30).step_by(2).collect::<Vec<u64>>()
    );
    for frame in &frames {
        let error = difference(pixels(frame), &clip.render(frame.index));
        assert!(error < 8.0, "frame {}: mean error {error}", frame.index);
    }
    drop(frames);
    server.shutdown().await;
}
