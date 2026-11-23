//! Faces, not whole people (spec 3.7 Q3, S18): `FaceRedactor` covers each face and falls back
//! to the whole person when a person shows no face; the egress gate's caption says which;
//! the stream's face blur leaves the rest of the frame; and the on-device profile refuses
//! every release at the destination step (S19).
//!
//! The detectors are colour doubles: a red box is a face, a blue box a person. They report
//! only what the pixels they are handed still show.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::egress::EgressPolicy;
use adk_vision::stream::{FramePrivacy, RedactionMethod, RedactorPrivacy};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DataClass, DecodedImage, DetectQuery, Detector,
    EgressAction, EgressConfig, EgressGate, EgressRecord, FACE_MARGIN, FaceRedactor, Frame,
    FramePixels, FrameSourceId, InMemoryEgressRecorder, Locality, PersonRedactor, RawRegion,
    RedactionClass, ReleaseRequest, SentImage, SourceImage, VisionConfig, VisionCx, VisionError,
};

const RED: [u8; 3] = [230, 20, 20];
const BLUE: [u8; 3] = [20, 40, 220];
/// The darker stripe of a person's clothes, still "blue" to the double.
const BLUE_STRIPE: [u8; 3] = [20, 40, 190];

/// Finds the bounding box of each connected run of rows holding `colour`, labelled `label`.
struct Colour {
    descriptor: BackendDescriptor,
    colour: [u8; 3],
    label: &'static str,
    calls: AtomicUsize,
}

impl Colour {
    fn new(id: &str, colour: [u8; 3], label: &'static str) -> Self {
        Self {
            descriptor: BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels),
            colour,
            label,
            calls: AtomicUsize::new(0),
        }
    }

    fn faces() -> Self {
        Self::new("faces", RED, "face")
    }

    fn people() -> Self {
        Self::new("people", BLUE, "person")
    }
}

/// The boxes of `colour` in `pixels`, one per column band of matching pixels.
fn boxes_of(pixels: &DecodedImage, colour: [u8; 3]) -> Vec<BBox> {
    let (w, h) = pixels.size();
    let rgba = pixels.rgba();
    let close = |x: u32, y: u32| {
        let i = (y as usize * w as usize + x as usize) * 4;
        (0..3).all(|c| (i32::from(rgba[i + c]) - i32::from(colour[c])).abs() < 40)
    };
    // Column bands: runs of columns that hold the colour, each boxed over its rows.
    let mut found = Vec::new();
    let mut x = 0;
    while x < w {
        if !(0..h).any(|y| close(x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < w && (0..h).any(|y| close(x, y)) {
            x += 1;
        }
        let rows: Vec<u32> = (0..h).filter(|&y| (start..x).any(|cx| close(cx, y))).collect();
        found.push(BBox::new(
            start as f32,
            rows[0] as f32,
            x as f32,
            (rows[rows.len() - 1] + 1) as f32,
        ));
    }
    found
}

#[async_trait::async_trait]
impl Detector for Colour {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(boxes_of(image.pixels(), self.colour)
            .into_iter()
            .map(|b| RawRegion::boxed(b, self.label, 0.9))
            .collect())
    }
}

/// A grey scene with the given blue people and red faces drawn in order.
fn scene(width: u32, height: u32, people: &[BBox], faces: &[BBox]) -> DecodedImage {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            // A gentle gradient, so a blur changes what it touches.
            let v = (90 + (x * 7 + y * 3) % 60) as u8;
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let mut paint = |b: &BBox, stripes: [[u8; 3]; 2]| {
        for y in b.y0 as u32..b.y1 as u32 {
            for x in b.x0 as u32..b.x1 as u32 {
                let i = (y as usize * width as usize + x as usize) * 4;
                rgba[i..i + 3].copy_from_slice(&stripes[(y / 4 % 2) as usize]);
            }
        }
    };
    for p in people {
        paint(p, [BLUE, BLUE_STRIPE]);
    }
    for f in faces {
        paint(f, [RED, RED]);
    }
    DecodedImage::from_rgba(width, height, rgba).unwrap()
}

/// Pixels exactly `colour`.
fn count(pixels: &DecodedImage, colour: [u8; 3]) -> usize {
    pixels.rgba().chunks_exact(4).filter(|p| p[..3] == colour).count()
}

/// The box `FaceRedactor` covers for [`FACE`]: the detector's box with the redaction margin,
/// widened by `FACE_MARGIN` of its longer side.
fn covered_face() -> BBox {
    let found = FACE.expanded(adk_vision::REDACTION_MARGIN);
    found.expanded(FACE_MARGIN * found.width().max(found.height())).clamped(320, 240)
}

fn remote() -> BackendDescriptor {
    BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
        CoordFrame::SentPixels,
    )
}

fn allow_faces() -> EgressConfig {
    EgressConfig::default().with_default(EgressAction::Allow).redacting(RedactionClass::faces())
}

fn face_redactor() -> FaceRedactor {
    FaceRedactor::new(Arc::new(Colour::faces())).with_person_fallback(Arc::new(Colour::people()))
}

/// The person with a face: the face covered with its margin, the rest of the person kept.
const PERSON: BBox = BBox { x0: 40.0, y0: 20.0, x1: 160.0, y1: 230.0 };
const FACE: BBox = BBox { x0: 80.0, y0: 40.0, x1: 120.0, y1: 90.0 };
/// A person seen from behind: no face.
const BACK: BBox = BBox { x0: 200.0, y0: 30.0, x1: 290.0, y1: 230.0 };

#[tokio::test]
async fn a_face_is_covered_and_the_rest_of_its_person_stays() {
    let source = SourceImage::from_pixels(scene(320, 240, &[PERSON], &[FACE])).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(EgressPolicy::public_only(), allow_faces(), recorder.clone())
        .with_redactor(Arc::new(face_redactor()));
    let backend = remote();
    let request = ReleaseRequest::new(&source, &backend, DataClass::new("frame"));

    let staged = gate.stage(&VisionCx::new(), &request).await.unwrap();
    let released = gate.release(&VisionCx::new(), request).await.unwrap();

    assert_eq!(staged.preview().filled(), &[(RedactionClass::faces(), covered_face())]);
    assert_eq!(staged.caption(), "to api.anthropic.com as frame · 1 face covered");
    assert_eq!(released.receipt().redactions.get(&RedactionClass::faces()), Some(&1));
    // No face is left, and most of the person is: a desk frame is not blanked.
    assert!(boxes_of(released.pixels(), RED).is_empty());
    let kept = count(released.pixels(), BLUE) + count(released.pixels(), BLUE_STRIPE);
    assert!(kept as f32 > 0.7 * PERSON.area(), "{kept} of {} person pixels kept", PERSON.area());
}

#[tokio::test]
async fn a_person_without_a_face_is_covered_whole() {
    let source = SourceImage::from_pixels(scene(320, 240, &[PERSON, BACK], &[FACE])).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(EgressPolicy::public_only(), allow_faces(), recorder.clone())
        .with_redactor(Arc::new(face_redactor()));
    let backend = remote();
    let request = ReleaseRequest::new(&source, &backend, DataClass::new("frame"));

    let staged = gate.stage(&VisionCx::new(), &request).await.unwrap();
    let released = gate.release(&VisionCx::new(), request).await.unwrap();

    let drawn: Vec<&RedactionClass> = staged.preview().filled().iter().map(|(c, _)| c).collect();
    assert_eq!(drawn, [&RedactionClass::faces(), &RedactionClass::people()]);
    let back = staged.preview().filled()[1].1;
    assert_eq!(back, BACK.expanded(adk_vision::REDACTION_MARGIN));
    assert_eq!(
        staged.caption(),
        "to api.anthropic.com as frame · 2 faces covered, 1 as the whole person"
    );
    assert_eq!(released.receipt().redactions.get(&RedactionClass::faces()), Some(&2));
    let overlay = staged.preview().overlay();
    let labels: Vec<Option<&str>> = overlay.regions.iter().map(|r| r.label.as_deref()).collect();
    assert_eq!(labels, [Some("faces"), Some("people")]);
}

#[tokio::test]
async fn with_no_face_detector_a_person_redactor_covers_people_whole() {
    let source = SourceImage::from_pixels(scene(320, 240, &[PERSON], &[FACE])).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(EgressPolicy::public_only(), allow_faces(), recorder.clone())
        .with_redactor(Arc::new(PersonRedactor::new(Arc::new(Colour::people()))));
    let backend = remote();
    let request = ReleaseRequest::new(&source, &backend, DataClass::new("frame"));

    let staged = gate.stage(&VisionCx::new(), &request).await.unwrap();

    assert_eq!(
        staged.caption(),
        "to api.anthropic.com as frame · 1 face · the whole person is covered"
    );
    assert_eq!(staged.preview().redactions().get(&RedactionClass::faces()), Some(&1));
    assert_eq!(staged.preview().filled()[0].0, RedactionClass::people());
}

#[tokio::test]
async fn faces_only_covers_faces_and_nothing_else() {
    let source = SourceImage::from_pixels(scene(320, 240, &[PERSON, BACK], &[FACE])).unwrap();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(EgressPolicy::public_only(), allow_faces(), recorder)
        .with_redactor(Arc::new(FaceRedactor::new(Arc::new(Colour::faces()))));
    let backend = remote();
    let request = ReleaseRequest::new(&source, &backend, DataClass::new("frame"));

    let staged = gate.stage(&VisionCx::new(), &request).await.unwrap();

    // The person seen from behind needs a person fallback to be covered.
    assert_eq!(staged.preview().filled(), &[(RedactionClass::faces(), covered_face())]);
}

#[tokio::test]
async fn the_stream_face_blur_changes_the_face_and_keeps_the_rest_of_the_frame() {
    let raw = scene(320, 240, &[PERSON, BACK], &[FACE]);
    let hook = RedactorPrivacy::new(Arc::new(face_redactor()), RedactionMethod::FACE_BLUR);
    let frame = Frame {
        source: FrameSourceId("desk".into()),
        index: 0,
        pts: std::time::Duration::ZERO,
        captured_at: chrono::Utc::now(),
        pixels: FramePixels::Ephemeral(Arc::new(raw.clone())),
    };

    let redacted = FramePrivacy::new(Arc::new(hook)).apply(&VisionCx::new(), frame).await.unwrap();

    assert_eq!(redacted.hook(), "redactor:faces");
    let face = covered_face().expanded(adk_vision::REDACTION_MARGIN);
    let back = BACK.expanded(adk_vision::REDACTION_MARGIN).expanded(adk_vision::REDACTION_MARGIN);
    assert_eq!(redacted.redacted(), &[face, back]);
    // The face's centre is no longer its colour, and pixels outside both boxes are untouched.
    let (w, _) = raw.size();
    let at = |image: &DecodedImage, x: u32, y: u32| {
        let i = (y as usize * w as usize + x as usize) * 4;
        [image.rgba()[i], image.rgba()[i + 1], image.rgba()[i + 2]]
    };
    assert!(boxes_of(redacted.pixels(), RED).is_empty(), "the face is blurred past recognition");
    assert_eq!(at(redacted.pixels(), 60, 200), at(&raw, 60, 200), "the person's body stays");
    assert_eq!(at(redacted.pixels(), 10, 10), at(&raw, 10, 10), "the background stays");
    assert_ne!(at(redacted.pixels(), 240, 120), at(&raw, 240, 120), "the back is blurred");
}

#[tokio::test]
async fn the_on_device_profile_refuses_at_the_destination_before_any_redactor_runs() {
    let config = VisionConfig::from_toml_str(
        "[egress]\non_device = true\ndefault = \"allow\"\nredact = [\"faces\"]\n\n\
         [[egress.rule]]\nhost = \"api.anthropic.com\"\naction = \"allow\"\n",
    )
    .unwrap();
    assert!(config.egress.on_device);
    let faces = Arc::new(Colour::faces());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(EgressPolicy::public_only(), config.egress, recorder.clone())
        .with_redactor(Arc::new(FaceRedactor::new(faces.clone())));
    let source = SourceImage::from_pixels(scene(320, 240, &[PERSON], &[FACE])).unwrap();
    let backend = remote();

    let err = gate
        .release(&VisionCx::new(), ReleaseRequest::new(&source, &backend, DataClass::new("frame")))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, VisionError::EgressRefused { host, .. } if host == "api.anthropic.com"),
        "{err}"
    );
    assert_eq!(faces.calls.load(Ordering::SeqCst), 0, "nothing was minimised or redacted");
    let records = recorder.records();
    assert_eq!(records.len(), 1);
    let EgressRecord::Refused(refusal) = &records[0] else { panic!("{records:?}") };
    assert_eq!(refusal.step, "destination");
    assert!(refusal.reason.contains("on-device profile"), "{}", refusal.reason);
    assert!(refusal.egress.is_none(), "no egress copy was made");
    assert!(recorder.released().is_empty());
}

#[tokio::test]
async fn a_held_face_is_blurred_again_in_the_frames_that_miss_it() {
    let hook = RedactorPrivacy::new(
        Arc::new(FaceRedactor::new(Arc::new(Colour::faces()))),
        RedactionMethod::FACE_BLUR,
    )
    .with_hold(2);
    let privacy = FramePrivacy::new(Arc::new(hook));
    let frame = |source: &str, index: u64, faces: &[BBox]| Frame {
        source: FrameSourceId(source.into()),
        index,
        pts: std::time::Duration::from_millis(index * 100),
        captured_at: chrono::Utc::now(),
        pixels: FramePixels::Ephemeral(Arc::new(scene(320, 240, &[], faces))),
    };
    let cx = VisionCx::new();
    let mut seen = Vec::new();
    for (source, index, faces) in [
        ("desk", 0, vec![FACE]),
        ("desk", 1, vec![]),
        ("desk", 2, vec![]),
        ("desk", 3, vec![]),
        ("door", 1, vec![]),
    ] {
        let redacted = privacy.apply(&cx, frame(source, index, &faces)).await.unwrap();
        seen.push(redacted.redacted().to_vec());
    }

    let found = seen[0][0];
    assert_eq!(seen[0].len(), 1);
    // Held for two frames, growing a quarter of its longer side a frame; then dropped.
    for (held, age) in [(&seen[1], 1.0), (&seen[2], 2.0)] {
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].contains(FACE.center()));
        assert!(held[0].width() > found.width() + age * 0.4 * found.width(), "{held:?}");
    }
    assert!(seen[3].is_empty(), "held past its two frames: {:?}", seen[3]);
    assert!(seen[4].is_empty(), "another source holds nothing of this one's");
}
