//! S9, the Inspector: the conveyor clip's 200 parts stream over RTSP from the in-process test
//! server through `RtspSource` and the stream pipeline — sampler, frame governor, privacy
//! hook, detector, tracker — into the watch stage, which keeps each event's frame and wakes.
//! SAM 2.1 Tiny refines the boxes of each woken frame to masks, the fiducial's mask
//! calibrates the plane, and each defect is measured.
//!
//! The spec's detector is a user's RF-DETR fine-tune in a manifest the user wrote. No
//! fine-tune is published, so the scenario writes that manifest itself, naming the built-in
//! RF-DETR Nano weights by revision and SHA-256, and loads it the way a user's would: the
//! manifest directory, the fetcher's verification, ONNX Runtime. Those COCO weights cannot see
//! a crack, so they run on each woken frame and find the clip's wall clock; the defects on the
//! stream come from `BeltDetector`, an on-device colour detector standing in for the
//! fine-tune's crack class.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest};
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_vision::onnx::{OnnxOptions, RfDetr, Sam2};
use adk_vision::stream::fixtures::ConveyorClip;
use adk_vision::stream::{NoRedaction, RtspSource, StreamPipeline, StreamWatch};
use adk_vision::{
    BBox, Calibration, CallOptions, DetectQuery, FiducialSpec, FrameSourceId, GreedyIouTracker,
    ManifestSet, MediaScope, MediaStore, ModelManifest, Sampling, SegmentPrompt, Shape, VisionCx,
    VisionRuntime, WatchCondition, WatchEvaluator,
};
use anyhow::{Context, Result};
use serde_json::Value;

use crate::Checks;
use crate::conveyor::{
    BeltDetector, StreamLog, WakeLog, clip_frame, defect_in, inspector_spec, seeded, serve,
    stream_speed,
};
use crate::fixtures::fetcher;

const APP: &str = "inspector";
const USER: &str = "line-1";
const SESSION: &str = "shift-1";
/// The user's manifest id for the model they registered.
const USER_MODEL: &str = "line-1-defects";
/// SAM 2.1 Tiny, 4-bit: 51 MB.
const SAM: &str = "sam2.1-hiera-tiny-q4";

/// The manifest a user writes for their detector: here the built-in RF-DETR Nano's files,
/// revision and SHA-256 under the user's own id, where a fine-tune's export and labels go.
fn user_manifest() -> Result<String> {
    let builtin = ManifestSet::builtin()?;
    let nano = builtin.get("rf-detr-n").context("RF-DETR Nano is built in")?;
    let mut value = serde_json::to_value(nano)?;
    value["id"] = Value::String(USER_MODEL.into());
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Grows `bbox` by `margin` and to at least `min` on each side, inside `size`.
fn around(bbox: &BBox, margin: f32, min: f32, (w, h): (u32, u32)) -> BBox {
    let (cx, cy) = ((bbox.x0 + bbox.x1) / 2.0, (bbox.y0 + bbox.y1) / 2.0);
    let half_w = (bbox.width() / 2.0 + margin).max(min / 2.0);
    let half_h = (bbox.height() / 2.0 + margin).max(min / 2.0);
    BBox::new(
        (cx - half_w).max(0.0),
        (cy - half_h).max(0.0),
        (cx + half_w).min(w as f32),
        (cy + half_h).min(h as f32),
    )
}

/// The best mask SAM 2.1 returns for a box prompt, with a positive point at its centre, on a
/// crop around it.
async fn refine(
    sam: &VisionRuntime,
    cx: &VisionCx,
    image: &adk_vision::SourceImage,
    bbox: &BBox,
    crop: BBox,
) -> Result<Shape> {
    let centre = [(bbox.x0 + bbox.x1) / 2.0, (bbox.y0 + bbox.y1) / 2.0];
    let out = sam
        .segment_with(
            cx,
            image,
            &[
                SegmentPrompt::Box { bbox: *bbox },
                SegmentPrompt::Point { at: centre, positive: true },
            ],
            &CallOptions::new().with_region(crop),
        )
        .await?;
    out.value
        .into_iter()
        .max_by(|a, b| a.score.unwrap_or(0.0).total_cmp(&b.score.unwrap_or(0.0)))
        .map(|region| region.shape)
        .context("SAM 2.1 returned no mask")
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let speed = stream_speed();
    checks.note(format!(
        "200 parts at 1280×720 over RTSP ({speed}× real time) at 5 frames a second; defects by \
         BeltDetector, the stand-in for a fine-tune; RF-DETR Nano from a user manifest the \
         scenario wrote; SAM 2.1 Tiny q4 on the device"
    ));

    // The user's manifest, read from the user's manifest directory and verified on fetch.
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join(format!("{USER_MODEL}.json")), user_manifest()?)?;
    let users = ManifestSet::new().with_dir(dir.path())?;
    let manifest: ModelManifest = users.get(USER_MODEL).cloned().context("the user's model")?;
    let fetcher = fetcher();
    let verified = fetcher.fetch(&manifest).await.context("fetching the user's model")?;
    let rf_detr = Arc::new(RfDetr::new(&verified, &OnnxOptions::default())?.with_threshold(0.3));
    let sam_manifest = ManifestSet::builtin()?.get(SAM).cloned().context("SAM 2.1 is built in")?;
    let sam = Arc::new(Sam2::new(
        &fetcher.fetch(&sam_manifest).await.context("fetching SAM 2.1 Tiny")?,
        &OnnxOptions::default(),
    )?);
    checks.check(
        "the user's manifest loads from their directory, verified file by file on fetch",
        verified.manifest().id == USER_MODEL && verified.manifest().files.len() == 1,
    );

    // The stream: every other frame of the 10 fps clip, five a second.
    let clip = Arc::new(ConveyorClip::new(inspector_spec()));
    let frames = clip.frame_count() / 2;
    let server = serve(&clip, 0, 2, frames, 5, speed).await?;
    let id = FrameSourceId("line-1".into());
    let source = Arc::new(RtspSource::new(id.clone(), &server.url())?);
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let audit = InMemoryAuditRecorder::new();
    let cx =
        VisionCx::new().with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());
    let keeper = Arc::new(adk_vision::stream::FrameKeeper::new(
        artifacts.clone(),
        MediaScope::new(APP, USER, SESSION),
        id.clone(),
    ));
    let wakes = Arc::new(WakeLog::default());
    let log = Arc::new(StreamLog::default());
    let watch = StreamWatch::new(
        WatchEvaluator::new(vec![
            WatchCondition::new("crack", ["crack"]).with_min_score(0.6).with_min_frames(3),
        ])?,
        keeper.clone(),
        wakes.clone(),
    )
    .with_observer(log.clone());
    let detector =
        Arc::new(VisionRuntime::builder().detector(Arc::new(BeltDetector::new())).build()?);
    let started = std::time::Instant::now();
    let summary = StreamPipeline::builder(source, Arc::new(NoRedaction))
        .sampling(Sampling::at(5.0))
        .detector(detector, DetectQuery::Classes(vec!["part".into(), "crack".into()]))
        // Cracks on different parts never overlap, so a low IoU floor is safe, and it keeps a
        // track through a frame the source dropped on a loaded machine (a 58 px jump).
        .tracker(Box::new(GreedyIouTracker::new(0.1, 3)))
        .sink(Arc::new(watch))
        .build()?
        .run(&cx)
        .await?;
    server.shutdown().await;
    checks.note(format!(
        "{} frames received, {} observed in {:.0?}",
        summary.received,
        summary.observations,
        started.elapsed()
    ));

    // Each crack sighting, by track, against the defect it lies on.
    let defects = seeded(&clip);
    let mut by_track: BTreeMap<u64, BTreeSet<Option<usize>>> = BTreeMap::new();
    for seen in log.cracks() {
        let index = clip_frame(&clip, 0, seen.pts_ms);
        by_track
            .entry(seen.track.unwrap_or(u64::MAX))
            .or_default()
            .insert(defect_in(&clip, index, &seen.bbox));
    }
    let mut tracks_of: BTreeMap<usize, BTreeSet<u64>> = BTreeMap::new();
    let mut strays = 0;
    for (track, parts) in &by_track {
        match parts.iter().collect::<Vec<_>>().as_slice() {
            [Some(part)] => {
                tracks_of.entry(*part).or_default().insert(*track);
            }
            _ => strays += 1,
        }
    }
    checks.check(
        &format!(
            "each of the {} seeded defects is one track ({} crack tracks, {strays} on no defect)",
            defects.len(),
            by_track.len()
        ),
        defects.len() == 12
            && strays == 0
            && defects.iter().all(|(part, _)| tracks_of.get(part).is_some_and(|t| t.len() == 1)),
    );

    let events = wakes.events();
    let woke: Vec<Option<usize>> = events
        .iter()
        .map(|event| {
            by_track.get(&event.track.0).and_then(|parts| match parts.iter().next() {
                Some(Some(part)) if parts.len() == 1 => Some(*part),
                _ => None,
            })
        })
        .collect();
    let distinct: BTreeSet<usize> = woke.iter().flatten().copied().collect();
    checks.check(
        &format!("each defect wakes once: {} wakes for {} defects", events.len(), distinct.len()),
        events.len() == defects.len() && distinct.len() == defects.len(),
    );
    checks.check(
        "no wake on a defect-free part: every woken track lies on a seeded defect",
        woke.iter().all(Option::is_some),
    );

    // Only woken frames were stored.
    let kept = keeper.kept().await?;
    let stored = artifacts
        .list(ListRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
        })
        .await?
        .file_names;
    let images: Vec<&String> = stored.iter().filter(|name| name.ends_with(".png")).collect();
    let woken_digests: BTreeSet<String> =
        events.iter().map(|e| e.frame.digest.to_string()).collect();
    let kept_digests: BTreeSet<String> = kept.iter().map(|e| e.image.digest.to_string()).collect();
    checks.check(
        &format!(
            "only the woken frames are stored: {} images of {} frames observed, each a wake's frame",
            images.len(),
            log.observed()
        ),
        images.len() == woken_digests.len()
            && kept_digests == woken_digests
            && kept.iter().all(|e| e.reasons == [adk_vision::stream::KeepReason::WakePayload])
            && audit.records().iter().filter(|r| r.action == "vision.evidence.kept").count()
                == woken_digests.len(),
    );

    // Each woken frame: the user's model on the device, then SAM 2.1, calibration, length.
    let store = MediaStore::new(artifacts.clone());
    let users_model = VisionRuntime::builder().detector(rf_detr).build()?;
    let segmenter = VisionRuntime::builder().segmenter(sam).build()?;
    let [kx0, ky0, kx1, ky1] = clip.truth().clock;
    let clock = BBox::new(kx0, ky0, kx1, ky1);
    let lengths: BTreeMap<usize, (f32, f32)> = clip
        .truth()
        .parts
        .iter()
        .filter_map(|p| p.defect.as_ref().map(|d| (p.index, (d.length_mm, d.angle_deg))))
        .collect();
    let (mut found_clock, mut scales, mut errors) = (0, Vec::new(), Vec::new());
    for (event, part) in events.iter().zip(&woke) {
        let entry = kept
            .iter()
            .find(|e| e.image.digest == event.frame.digest)
            .context("the woken frame is kept")?;
        let image = store.load(&entry.image).await?;
        let size = image.size();
        let seen =
            users_model.detect(&cx, &image, &DetectQuery::Classes(vec!["clock".into()])).await?;
        if seen.value.iter().any(|r| r.shape.bbox().is_some_and(|b| b.iou(&clock) >= 0.5)) {
            found_clock += 1;
        }
        let fiducial = BeltDetector::fiducial(image.pixels()).context("the fiducial is in view")?;
        let mask =
            refine(&segmenter, &cx, &image, &fiducial, around(&fiducial, 40.0, 0.0, size)).await?;
        let calibration = Calibration::from_fiducial(&mask, FiducialSpec::square(20.0), size)?
            .with_image(image.digest());
        scales.push(calibration.px_per_mm);
        let crack = event
            .regions
            .iter()
            .filter(|r| r.track == Some(event.track) && r.label.as_deref() == Some("crack"))
            .find_map(|r| r.shape.bbox())
            .context("the event names its crack")?;
        let mask = refine(
            &segmenter,
            &cx,
            &image,
            &around(&crack, 1.0, 0.0, size),
            around(&crack, 24.0, 160.0, size),
        )
        .await?;
        let measured = calibration.measure(&mask, size)?;
        if let Some((want, angle)) = part.and_then(|p| lengths.get(&p)) {
            let error = (measured.longest_mm as f32 - want) / want;
            checks.note(format!(
                "part {}: {:.2} mm measured ± {:.2}, {want:.2} mm seeded at {angle:.0}° ({:+.1}%)",
                part.unwrap_or_default(),
                measured.longest_mm,
                measured.longest_uncertainty_mm,
                error * 100.0
            ));
            errors.push(error);
        }
    }
    checks.check(
        &format!(
            "the user's model, loaded from their manifest, finds the wall clock on {found_clock} of {} woken frames",
            events.len()
        ),
        !events.is_empty() && found_clock == events.len(),
    );
    let scale = scales.iter().sum::<f64>() / scales.len().max(1) as f64;
    checks.check(
        &format!("the fiducial's SAM 2.1 mask calibrates {scale:.3} px/mm against 6 px/mm drawn"),
        !scales.is_empty() && scales.iter().all(|k| (k / 6.0 - 1.0).abs() <= 0.02),
    );
    let worst = errors.iter().map(|e| e.abs()).fold(0.0, f32::max);
    checks.check(
        &format!(
            "every defect measures within 5% of its seeded length (worst {:.1}%)",
            worst * 100.0
        ),
        errors.len() == defects.len() && worst <= 0.05,
    );
    Ok(checks)
}
