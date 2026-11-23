//! S18 (blur by default on streams): two drawn passers-by — generated faces, not anyone's
//! likeness — walk across the Inspector's conveyor camera, one close and one far. Every frame
//! passes the face blur (YuNet, RF-DETR Nano for a person with no face found) before any
//! other stage; a keeper stores every frame. The store, the order of the writes and YuNet run
//! again on what was stored decide the checks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_artifact::{
    ArtifactService, DeleteRequest, InMemoryArtifactService, ListRequest, ListResponse,
    LoadRequest, LoadResponse, SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
use adk_core::Part;
use adk_core::egress::EgressPolicy;
use adk_vision::onnx::{OnnxOptions, YuNet};
use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec, draw_portrait};
use adk_vision::stream::{
    FrameKeeper, FrameSource, FrameStream, KeepReason, Observation, ObservationSink, PrivacyHook,
    RedactionMethod, RedactorPrivacy, StreamPipeline,
};
use adk_vision::{
    BBox, DEFAULT_FACE_DETECTOR, DataClass, DecodeLimits, DecodedImage, DetectQuery, Digest,
    EgressAction, EgressConfig, EgressGate, FaceRedactor, Frame, FrameMeta, FramePixels,
    FrameSourceId, InMemoryEgressRecorder, Locality, ManifestSet, MediaScope, RedactionClass,
    Redactor, ReleaseRequest, Sampling, SourceImage, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use futures::StreamExt;

use crate::Checks;
use crate::fixtures::{APP, SESSION, USER, covered, fetcher, models};

const FRAMES: u64 = 70;
/// Faces the check finds from: under the redactor's 0.5, so a face the blur only weakened
/// still counts as found.
const CHECK_SCORE: f32 = 0.3;

/// One drawn passer-by in a frame: the figure's box and its face's.
#[derive(Clone, Copy, Debug)]
struct Passer {
    figure: BBox,
    face: BBox,
}

/// The conveyor with two people walking across it, one at a time so neither hides the
/// other's face: one close (300 px tall) left to right over frames 0–29, one far (110 px)
/// right to left over frames 30–59, nobody after.
struct PassersBy {
    id: FrameSourceId,
    clip: ConveyorClip,
}

impl PassersBy {
    fn new() -> Self {
        Self {
            id: FrameSourceId("line-1".into()),
            clip: ConveyorClip::new(ConveyorSpec::default()),
        }
    }

    /// Frame `index` as the camera captures it, and who is in it.
    fn render(&self, index: u64) -> (DecodedImage, Vec<Passer>) {
        let mut frame = self.clip.render(100 + index);
        let (w, _) = frame.size();
        let mut passers = Vec::new();
        let walkers: [(u64, u64, f32, f32, bool, u32); 2] =
            [(0, 30, 300.0, 30.0, true, 0), (30, 60, 110.0, 120.0, false, 2)];
        for (from, to, height, top, rightwards, palette) in walkers {
            if !(from..to).contains(&index) {
                continue;
            }
            let width = height * 180.0 / 228.0;
            let t = (index - from) as f32 / (to - from - 1) as f32;
            let span = w as f32 - width - 20.0;
            let x0 = 10.0 + if rightwards { t } else { 1.0 - t } * span;
            let figure = BBox::new(x0, top, x0 + width, top + height);
            let face = draw_portrait(&mut frame, figure, palette);
            passers.push(Passer { figure, face });
        }
        (frame, passers)
    }
}

#[async_trait::async_trait]
impl FrameSource for PassersBy {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, _sampling: Sampling) -> adk_vision::Result<FrameStream> {
        let frames: Vec<adk_vision::Result<Frame>> = (0..FRAMES)
            .map(|index| {
                Ok(Frame {
                    source: self.id.clone(),
                    index,
                    pts: Duration::from_millis(index * 100),
                    captured_at: chrono::Utc::now(),
                    pixels: FramePixels::Ephemeral(Arc::new(self.render(index).0)),
                })
            })
            .collect();
        Ok(futures::stream::iter(frames).boxed())
    }
}

/// What happened, in order: a frame redacted, or an image written to the artifact store.
#[derive(Clone, Debug)]
enum Entry {
    Redacted { index: u64, raw: Digest, redacted: Digest, boxes: Vec<BBox> },
    Saved { file: String, pixels: Digest },
}

type Log = Arc<Mutex<Vec<Entry>>>;

/// The face blur, logging each frame's digest before and after.
struct Watched {
    inner: RedactorPrivacy,
    log: Log,
}

#[async_trait::async_trait]
impl PrivacyHook for Watched {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn redact(
        &self,
        cx: &VisionCx,
        frame: &FrameMeta,
        pixels: &mut DecodedImage,
    ) -> adk_vision::Result<Vec<BBox>> {
        let raw = Digest::of(pixels.rgba());
        let boxes = self.inner.redact(cx, frame, pixels).await?;
        let redacted = Digest::of(pixels.rgba());
        self.log.lock().unwrap().push(Entry::Redacted {
            index: frame.index,
            raw,
            redacted,
            boxes: boxes.clone(),
        });
        Ok(boxes)
    }
}

/// The artifact store, logging the pixels of every image written to it.
struct Recording {
    inner: InMemoryArtifactService,
    log: Log,
}

#[async_trait::async_trait]
impl ArtifactService for Recording {
    async fn save(&self, req: SaveRequest) -> adk_core::Result<SaveResponse> {
        if let Part::InlineData { mime_type, data, .. } = &req.part
            && mime_type.starts_with("image/")
        {
            let (pixels, _) = DecodedImage::decode(data, &DecodeLimits::default())
                .map_err(|e| adk_core::AdkError::artifact(e.to_string()))?;
            self.log.lock().unwrap().push(Entry::Saved {
                file: req.file_name.clone(),
                pixels: Digest::of(pixels.rgba()),
            });
        }
        self.inner.save(req).await
    }

    async fn load(&self, req: LoadRequest) -> adk_core::Result<LoadResponse> {
        self.inner.load(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        self.inner.delete(req).await
    }

    async fn list(&self, req: ListRequest) -> adk_core::Result<ListResponse> {
        self.inner.list(req).await
    }

    async fn versions(&self, req: VersionsRequest) -> adk_core::Result<VersionsResponse> {
        self.inner.versions(req).await
    }
}

/// Keeps every frame, as if a person asked for each.
struct KeepAll(FrameKeeper);

#[async_trait::async_trait]
impl ObservationSink for KeepAll {
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        self.0.keep(cx, &observation.image, KeepReason::PersonKept).await?;
        Ok(())
    }
}

/// The faces YuNet finds in `image` from [`CHECK_SCORE`].
async fn faces(yunet: &Arc<YuNet>, image: &DecodedImage) -> Result<Vec<(BBox, f32)>> {
    let runtime = VisionRuntime::builder().detector(yunet.clone()).build()?;
    let source = SourceImage::from_pixels(image.clone())?;
    let query = DetectQuery::Classes(vec!["face".into()]);
    let found = runtime.detect(&VisionCx::new(), &source, &query).await?.value;
    Ok(found
        .iter()
        .filter_map(|r| Some((r.shape.bbox()?, r.score.unwrap_or(1.0))))
        .filter(|(_, score)| *score >= CHECK_SCORE)
        .collect())
}

/// Stages a release of `image` to Claude under `redactor` and returns the share of the image
/// covered, the card's caption and the egress pixels.
async fn stage(
    image: &SourceImage,
    redactor: Arc<dyn Redactor>,
) -> Result<(f32, String, DecodedImage)> {
    let backend = adk_vision::BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
        adk_vision::CoordFrame::SentPixels,
    );
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default()
            .with_default(EgressAction::Allow)
            .redacting(RedactionClass::faces()),
        Arc::new(InMemoryEgressRecorder::new()),
    )
    .with_redactor(redactor);
    let request = ReleaseRequest::new(image, &backend, DataClass::new("desk_frame"));
    let staged = gate.stage(&VisionCx::new(), &request).await?;
    let (w, h) = image.size();
    let area = staged
        .preview()
        .filled()
        .iter()
        .map(|(_, b)| b.clamped(w, h).area())
        .fold(0.0, |a, b| a + b);
    Ok((area / (w * h) as f32, staged.caption(), staged.preview().pixels().clone()))
}

/// The mean absolute difference of two images inside `region`.
fn difference(a: &DecodedImage, b: &DecodedImage, region: &BBox) -> f32 {
    let (w, h) = a.size();
    let (x0, y0, bw, bh) = region.pixel_bounds(w, h);
    let mut total = 0u64;
    for y in y0..y0 + bh {
        for x in x0..x0 + bw {
            let i = ((y * w + x) * 4) as usize;
            for c in 0..3 {
                total += u64::from(a.rgba()[i + c].abs_diff(b.rgba()[i + c]));
            }
        }
    }
    total as f32 / (3 * bw * bh).max(1) as f32
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let manifest = ManifestSet::builtin()?
        .get(DEFAULT_FACE_DETECTOR)
        .cloned()
        .context("YuNet is in the built-in manifest")?;
    let verified = fetcher().fetch(&manifest).await.context("fetching YuNet")?;
    let yunet =
        Arc::new(YuNet::new(&verified, &OnnxOptions::default())?.with_threshold(CHECK_SCORE));
    let people = models().await?.detector.clone();
    checks.note(
        "YuNet 2023mar (MIT) and RF-DETR Nano on the device, fetched by revision and SHA-256; \
         drawn passers-by, not real people",
    );

    let log: Log = Arc::default();
    let artifacts = Arc::new(Recording { inner: InMemoryArtifactService::new(), log: log.clone() });
    let source = Arc::new(PassersBy::new());
    let keeper = FrameKeeper::new(
        artifacts.clone(),
        MediaScope::new(APP, USER, SESSION),
        source.id().clone(),
    );
    let redactor = FaceRedactor::new(yunet.clone()).with_person_fallback(people.clone());
    let hook = Watched {
        // As `RedactorPrivacy::blur_faces` builds it, over the scenario's own detectors.
        inner: RedactorPrivacy::new(Arc::new(redactor), RedactionMethod::FACE_BLUR).with_hold(2),
        log: log.clone(),
    };
    let summary = StreamPipeline::builder(source.clone(), Arc::new(hook))
        .sink(Arc::new(KeepAll(keeper)))
        .build()?
        .run(&VisionCx::new())
        .await?;
    checks.check(
        &format!("all {FRAMES} frames passed the face blur before any other stage, none dropped"),
        summary.received == FRAMES
            && summary.observations == FRAMES
            && summary.privacy_failures == 0,
    );

    let log = log.lock().unwrap().clone();
    let redactions: BTreeMap<u64, (usize, Digest, Digest, Vec<BBox>)> = log
        .iter()
        .enumerate()
        .filter_map(|(at, e)| match e {
            Entry::Redacted { index, raw, redacted, boxes } => {
                Some((*index, (at, *raw, *redacted, boxes.clone())))
            }
            Entry::Saved { .. } => None,
        })
        .collect();
    let saved: Vec<(usize, String, Digest)> = log
        .iter()
        .enumerate()
        .filter_map(|(at, e)| match e {
            Entry::Saved { file, pixels } => Some((at, file.clone(), *pixels)),
            Entry::Redacted { .. } => None,
        })
        .collect();

    // Every drawn face was found and covered by a blurred box.
    let mut faces_drawn = 0;
    let mut faces_covered = 0;
    let mut missed = Vec::new();
    let mut face_share = Vec::new();
    for index in 0..FRAMES {
        let (_, passers) = source.render(index);
        let boxes = redactions.get(&index).map(|r| r.3.clone()).unwrap_or_default();
        for passer in &passers {
            faces_drawn += 1;
            let hit = boxes.iter().find(|b| b.contains(passer.face.center()));
            if hit.is_some_and(|b| covered(&passer.face, b) >= 0.6) {
                faces_covered += 1;
            } else {
                let (raw, _) = source.render(index);
                let seen: Vec<f32> = faces(&yunet, &raw)
                    .await?
                    .into_iter()
                    .filter(|(b, _)| passer.face.contains(b.center()))
                    .map(|(_, score)| score)
                    .collect();
                missed.push(format!(
                    "frame {index}, {:.0} px figure, YuNet scores {seen:?}",
                    passer.figure.height()
                ));
            }
            if passer.figure.height() > 200.0
                && let Some(b) = hit
            {
                face_share.push(b.area() / passer.figure.area());
            }
        }
    }
    if !missed.is_empty() {
        checks.note(format!("faces not blurred: {}", missed.join("; ")));
    }
    checks.check(
        &format!("every drawn face was found and blurred ({faces_covered} of {faces_drawn})"),
        faces_drawn > 0 && faces_covered == faces_drawn,
    );

    // YuNet again, on exactly what was stored.
    let by_pixels: BTreeMap<Digest, u64> =
        redactions.iter().map(|(index, r)| (r.2, *index)).collect();
    let mut stored_faces = 0;
    let mut checked = BTreeSet::new();
    for (_, file, pixels) in &saved {
        let Some(index) = by_pixels.get(pixels) else { continue };
        let loaded = artifacts
            .load(LoadRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                file_name: file.clone(),
                version: None,
            })
            .await?;
        let Part::InlineData { data, .. } = loaded.part else { continue };
        let (image, _) = DecodedImage::decode(&data, &DecodeLimits::default())?;
        let (_, passers) = source.render(*index);
        let found = faces(&yunet, &image).await?;
        let left: Vec<&(BBox, f32)> = found
            .iter()
            .filter(|(f, _)| {
                passers.iter().any(|p| p.face.contains(f.center()) || covered(f, &p.face) > 0.3)
            })
            .collect();
        if !left.is_empty() {
            checks.note(format!("frame {index} still shows a face at {left:?}"));
        }
        stored_faces += left.len();
        checked.insert(*index);
    }
    checks.check(
        &format!(
            "no stored frame contains a detectable face: YuNet from {CHECK_SCORE} finds {stored_faces} in the {} frames stored",
            checked.len()
        ),
        stored_faces == 0 && checked.len() as u64 == FRAMES,
    );

    // The order of the writes, and what was written.
    let before = saved.iter().all(|(at, _, pixels)| {
        by_pixels.get(pixels).and_then(|index| redactions.get(index)).is_some_and(|r| r.0 < *at)
    });
    checks.check(
        &format!(
            "blurring happens before the artifact write: each of the {} images written is a frame's redacted pixels, written after its blur",
            saved.len()
        ),
        !saved.is_empty() && before,
    );
    let raw_with_faces: BTreeSet<Digest> =
        redactions.values().filter(|r| r.1 != r.2).map(|r| r.1).collect();
    let persisted_raw = saved.iter().filter(|(_, _, p)| raw_with_faces.contains(p)).count();
    checks.check(
        &format!(
            "the unblurred frame is never persisted: none of the {} frames that showed a face reached the store as captured",
            raw_with_faces.len()
        ),
        !raw_with_faces.is_empty() && persisted_raw == 0,
    );

    // A face, not its whole person; frames with nobody are kept as captured.
    let mut body_kept = true;
    for index in 0..FRAMES {
        let (raw, passers) = source.render(index);
        let Some((_, _, digest, _)) = redactions.get(&index) else { continue };
        let Some((_, file, _)) = saved.iter().find(|(_, _, p)| p == digest) else { continue };
        let loaded = artifacts
            .load(LoadRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                file_name: file.clone(),
                version: None,
            })
            .await?;
        let Part::InlineData { data, .. } = loaded.part else { continue };
        let (stored, _) = DecodedImage::decode(&data, &DecodeLimits::default())?;
        for passer in passers.iter().filter(|p| p.figure.height() > 200.0) {
            let f = passer.figure;
            let shirt = BBox::new(
                f.x0 + 0.2 * f.width(),
                f.y1 - 0.18 * f.height(),
                f.x1 - 0.2 * f.width(),
                f.y1,
            );
            body_kept &= difference(&raw, &stored, &shirt) < 1.0;
        }
    }
    let largest = face_share.iter().copied().fold(0.0f32, f32::max);
    checks.check(
        &format!(
            "a face is blurred, not its whole person: the close passer-by's blur covers at most {:.0}% of the figure and the shirt is stored as captured",
            largest * 100.0
        ),
        !face_share.is_empty() && largest < 0.5 && body_kept,
    );
    // The last passer-by's face is held for two frames after it leaves.
    let empty_untouched = (62..FRAMES).all(|i| redactions.get(&i).is_some_and(|r| r.1 == r.2));
    checks.check(
        "frames with nobody in them, once the last face's two-frame hold ends, are stored as captured",
        empty_untouched,
    );

    // The image privacy guardrail on a close desk frame: the face, not the frame.
    let figure = BBox::new(186.0, 10.0, 454.0, 350.0);
    let (desk, face) = {
        let mut frame = DecodedImage::filled(640, 360, [126, 128, 132, 255]);
        let face = draw_portrait(&mut frame, figure, 1);
        (SourceImage::from_pixels(frame)?, face)
    };
    let face_first: Arc<dyn Redactor> =
        Arc::new(FaceRedactor::new(yunet.clone()).with_person_fallback(people.clone()));
    let (faces_share, faces_caption, egress) = stage(&desk, face_first).await?;
    let left =
        faces(&yunet, &egress).await?.iter().filter(|(f, _)| face.contains(f.center())).count();
    checks.note(format!(
        "close desk frame: the face cover is {:.0}% of the frame ({faces_caption}); the figure a whole-person box covers is {:.0}%",
        faces_share * 100.0,
        figure.area() / (640.0 * 360.0) * 100.0,
    ));
    checks.check(
        "the image privacy guardrail covers the face of a close desk frame, not the frame",
        faces_caption.ends_with("1 face covered") && faces_share < 0.2 && left == 0,
    );
    Ok(checks)
}
