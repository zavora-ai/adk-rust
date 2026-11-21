//! Calibration and measurement on a generated calibration image (spec 3.7 §7.13, S9): a 20 mm
//! fiducial turned 12° in the plane calibrates the scale, and six seeded defects of known
//! length and a blob of known area measure within 5%, directly and through `vision_measure`
//! on the governed path, with the measurement in the answer record the annotated viewer reads.

mod governed;

use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest, LoadRequest};
use adk_core::Part;
use adk_core::audit_log::InMemoryAuditRecorder;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::{
    BBox, BackendDescriptor, Calibration, CalibrationSource, CoordFrame, DecodedImage,
    FiducialSpec, Locality, MEASUREMENT_VALIDITY, Mask, MeasuredGeometry, MediaStore, RawRegion,
    Result, SegmentPrompt, Segmenter, SentImage, Shape, SourceImage, VisionCx, VisionRuntime,
    VisionToolset,
};
use governed::{APP, PlannerModel, SESSION, USER, run_turn, save_artifact, tool_responses};
use serde_json::{Value, json};

/// Pixels per millimetre the image is drawn at.
const SCALE: f64 = 7.5;
const WIDTH: u32 = 1600;
const HEIGHT: u32 = 1000;
const FIDUCIAL_MM: f64 = 20.0;
const FIDUCIAL_COLOUR: [u8; 4] = [0, 0, 0, 255];
const DEFECT_COLOUR: [u8; 4] = [150, 20, 20, 255];
const BLOB_COLOUR: [u8; 4] = [20, 40, 160, 255];
const IMAGE: &str = "calibration.png";

/// One drawn object: a rotated rectangle or an ellipse, in millimetres on the plane.
#[derive(Clone, Copy, Debug)]
struct Drawn {
    centre_px: [f64; 2],
    /// Length and width, or the two diameters of an ellipse, in millimetres.
    size_mm: [f64; 2],
    angle_deg: f64,
    ellipse: bool,
    colour: [u8; 4],
}

impl Drawn {
    /// Whether the pixel whose centre is `(x, y)` is inside.
    fn covers(&self, x: u32, y: u32) -> bool {
        let (dx, dy) =
            (f64::from(x) + 0.5 - self.centre_px[0], f64::from(y) + 0.5 - self.centre_px[1]);
        let (sin, cos) = self.angle_deg.to_radians().sin_cos();
        let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        let (a, b) = (self.size_mm[0] * SCALE / 2.0, self.size_mm[1] * SCALE / 2.0);
        if self.ellipse {
            (u / a).powi(2) + (v / b).powi(2) <= 1.0
        } else {
            u.abs() <= a && v.abs() <= b
        }
    }

    /// A box around it with a margin, in pixels.
    fn bbox(&self) -> BBox {
        let reach = self.size_mm[0].max(self.size_mm[1]) * SCALE / 2.0 + 6.0;
        let [cx, cy] = self.centre_px;
        BBox::new(
            (cx - reach) as f32,
            (cy - reach) as f32,
            (cx + reach) as f32,
            (cy + reach) as f32,
        )
    }

    /// The true maximum length of a rectangle, its diagonal; of an ellipse, its long diameter.
    fn longest_mm(&self) -> f64 {
        if self.ellipse {
            self.size_mm[0].max(self.size_mm[1])
        } else {
            self.size_mm[0].hypot(self.size_mm[1])
        }
    }

    fn area_mm2(&self) -> f64 {
        let [a, b] = self.size_mm;
        if self.ellipse { std::f64::consts::PI * a * b / 4.0 } else { a * b }
    }
}

fn fiducial() -> Drawn {
    Drawn {
        centre_px: [220.0, 220.0],
        size_mm: [FIDUCIAL_MM, FIDUCIAL_MM],
        angle_deg: 12.0,
        ellipse: false,
        colour: FIDUCIAL_COLOUR,
    }
}

/// Six seeded defects, 0.5 mm wide, 6 to 45 mm long, at six angles; and one blob.
fn defects() -> Vec<Drawn> {
    let crack = |cx: f64, cy: f64, length: f64, angle: f64| Drawn {
        centre_px: [cx, cy],
        size_mm: [length, 0.5],
        angle_deg: angle,
        ellipse: false,
        colour: DEFECT_COLOUR,
    };
    vec![
        crack(560.0, 160.0, 6.0, 0.0),
        crack(820.0, 200.0, 9.5, 17.0),
        crack(1150.0, 220.0, 14.0, 33.0),
        crack(500.0, 600.0, 22.0, 58.0),
        crack(900.0, 640.0, 31.0, 90.0),
        crack(1320.0, 620.0, 45.0, 121.0),
        Drawn {
            centre_px: [250.0, 760.0],
            size_mm: [16.0, 10.0],
            angle_deg: 25.0,
            ellipse: true,
            colour: BLOB_COLOUR,
        },
    ]
}

fn draw() -> DecodedImage {
    let objects: Vec<Drawn> = std::iter::once(fiducial()).chain(defects()).collect();
    let mut rgba = vec![255u8; (WIDTH * HEIGHT * 4) as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if let Some(object) = objects.iter().find(|o| o.covers(x, y)) {
                let at = ((y * WIDTH + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&object.colour);
            }
        }
    }
    DecodedImage::from_rgba(WIDTH, HEIGHT, rgba).unwrap()
}

/// The mask of the pixels of `colour` inside `within`, as a segmenter refining a box answers.
fn mask_of(pixels: &DecodedImage, colour: [u8; 4], within: &BBox) -> Mask {
    let (w, h) = pixels.size();
    let mut bits = vec![false; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let centre = [x as f32 + 0.5, y as f32 + 0.5];
            bits[(y * w + x) as usize] = within.contains(centre) && pixels.pixel(x, y) == colour;
        }
    }
    Mask::encode(w, h, &bits)
}

/// A local segmenter that refines each box prompt to the mask of the non-white colour most
/// common in it, as SAM 2.1 refines a detector's boxes.
struct ColourSegmenter(BackendDescriptor);

#[async_trait::async_trait]
impl Segmenter for ColourSegmenter {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    async fn segment(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>> {
        let pixels = image.pixels();
        let mut regions = Vec::new();
        for prompt in prompts {
            let SegmentPrompt::Box { bbox } = prompt else { continue };
            let (label, colour) =
                [("fiducial", FIDUCIAL_COLOUR), ("defect", DEFECT_COLOUR), ("blob", BLOB_COLOUR)]
                    .into_iter()
                    .max_by_key(|(_, colour)| mask_of(pixels, *colour, bbox).area())
                    .unwrap();
            let mut raw = RawRegion::shape(Shape::Mask(mask_of(pixels, colour, bbox)));
            raw.label = Some(label.into());
            raw.score = Some(0.95);
            regions.push(raw);
        }
        Ok(regions)
    }
}

#[test]
fn s9_seeded_lengths_and_area_measure_within_five_percent() {
    let pixels = draw();
    let fiducial = fiducial();
    let mask = mask_of(&pixels, FIDUCIAL_COLOUR, &fiducial.bbox());
    let calibration = Calibration::from_fiducial(
        &Shape::Mask(mask),
        FiducialSpec::square(FIDUCIAL_MM),
        (WIDTH, HEIGHT),
    )
    .unwrap();
    let scale_error = (calibration.px_per_mm - SCALE).abs() / SCALE;
    assert!(scale_error < 0.01, "scale {} px/mm against {SCALE}", calibration.px_per_mm);
    assert!(
        (calibration.px_per_mm - SCALE).abs() <= 3.0 * calibration.uncertainty_px_per_mm,
        "the scale's uncertainty {} covers its error",
        calibration.uncertainty_px_per_mm
    );
    let CalibrationSource::Fiducial { measured_px, .. } = calibration.source else {
        unreachable!()
    };
    assert!((measured_px[0] - 150.0).abs() < 2.0, "{measured_px:?}");

    let mut worst: f64 = 0.0;
    for object in defects() {
        let mask = mask_of(&pixels, object.colour, &object.bbox());
        let measured = calibration.measure(&Shape::Mask(mask), (WIDTH, HEIGHT)).unwrap();
        assert_eq!(measured.geometry, MeasuredGeometry::Mask);
        let error = (measured.longest_mm - object.longest_mm()).abs() / object.longest_mm();
        assert!(
            error < 0.05,
            "{object:?}: {} mm against {} mm",
            measured.longest_mm,
            object.longest_mm()
        );
        assert!(
            (measured.longest_mm - object.longest_mm()).abs()
                <= 3.0 * measured.longest_uncertainty_mm,
            "{object:?}: the uncertainty {} mm covers the error",
            measured.longest_uncertainty_mm
        );
        worst = worst.max(error);
        if object.ellipse {
            let area_error = (measured.area_mm2 - object.area_mm2()).abs() / object.area_mm2();
            assert!(
                area_error < 0.05,
                "blob area {} mm² against {}",
                measured.area_mm2,
                object.area_mm2()
            );
            assert!(
                (measured.area_mm2 - object.area_mm2()).abs()
                    <= 3.0 * measured.area_uncertainty_mm2,
                "the area's uncertainty {} covers its error",
                measured.area_uncertainty_mm2
            );
        }
    }
    assert!(worst < 0.05, "worst length error {worst}");
}

#[test]
fn a_scale_holds_only_for_its_image_size_and_a_point_has_no_extent() {
    let calibration = Calibration::from_scale(7.5, 0.05, (WIDTH, HEIGHT)).unwrap();
    let scratch = Shape::Box(BBox::new(10.0, 10.0, 85.0, 13.0));
    let error = calibration.measure(&scratch, (800, 500)).unwrap_err();
    assert!(error.to_string().contains("1600x1000"), "{error}");
    let point = Shape::Point(adk_vision::Point2 { x: 4.0, y: 4.0 });
    assert!(calibration.measure(&point, (WIDTH, HEIGHT)).is_err());
    let measured = calibration.measure(&scratch, (WIDTH, HEIGHT)).unwrap();
    assert_eq!(measured.geometry, MeasuredGeometry::Box);
    assert!((measured.longest_mm - 75.06f64 / 7.5).abs() < 0.01);
    assert!(Calibration::from_scale(0.0, 0.0, (WIDTH, HEIGHT)).is_err());
}

#[test]
fn a_region_that_is_not_the_fiducial_is_refused() {
    // A 20 x 5 mm strip offered as the 20 mm square.
    let strip = Shape::Mask(Mask::from_box(WIDTH, HEIGHT, &BBox::new(100.0, 100.0, 250.0, 137.5)));
    let error = Calibration::from_fiducial(&strip, FiducialSpec::square(20.0), (WIDTH, HEIGHT))
        .unwrap_err();
    assert!(error.to_string().contains("not the fiducial"), "{error}");
    let tiny = Shape::Mask(Mask::from_box(WIDTH, HEIGHT, &BBox::new(100.0, 100.0, 106.0, 106.0)));
    assert!(Calibration::from_fiducial(&tiny, FiducialSpec::square(1.0), (WIDTH, HEIGHT)).is_err());
}

struct Bench {
    artifacts: Arc<InMemoryArtifactService>,
    toolset: VisionToolset,
    source: SourceImage,
}

async fn bench() -> Bench {
    let pixels = draw();
    let png = pixels.encode_png().unwrap();
    let source = SourceImage::decode(&png, &adk_vision::DecodeLimits::default()).unwrap();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    save_artifact(&artifacts, IMAGE, &png).await;
    let segmenter = ColourSegmenter(BackendDescriptor::new(
        "colour",
        Locality::InProcess,
        CoordFrame::SentPixels,
    ));
    let runtime = VisionRuntime::builder()
        .segmenter(Arc::new(segmenter))
        .media_store(MediaStore::new(artifacts.clone()))
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    Bench { artifacts, toolset, source }
}

async fn measure_through_the_tool(bench: &Bench, args: Value) -> (Value, InMemoryAuditRecorder) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let model = Arc::new(PlannerModel::calling(vec![("vision_measure", args)]));
    let agent = adk_agent::LlmAgentBuilder::new("inspector")
        .model(model)
        .toolset(Arc::new(bench.toolset.clone()))
        .build()
        .unwrap();
    let audit = InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .artifact_service(bench.artifacts.clone())
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let events = run_turn(&runner, "Measure the defects").await;
    let responses = tool_responses(&events, "vision_measure");
    assert_eq!(responses.len(), 1, "{events:#?}");
    (responses[0].clone(), audit)
}

fn bbox_json(bbox: BBox) -> Value {
    json!([bbox.x0, bbox.y0, bbox.x1, bbox.y1])
}

#[tokio::test]
async fn s9_vision_measure_calibrates_from_the_fiducial_and_measures_each_defect() {
    let bench = bench().await;
    assert!(bench.toolset.is_tool("vision_measure"), "a runtime with a segmenter measures");
    let objects = defects();
    let args = json!({
        "image": IMAGE,
        "boxes": objects.iter().map(|o| bbox_json(o.bbox())).collect::<Vec<_>>(),
        "fiducial": { "box": bbox_json(fiducial().bbox()), "widthMm": FIDUCIAL_MM },
    });
    let (answer, audit) = measure_through_the_tool(&bench, args).await;

    assert_eq!(answer["image"], json!(bench.source.digest()));
    assert_eq!(answer["validity"], MEASUREMENT_VALIDITY);
    let scale = answer["calibration"]["pxPerMm"].as_f64().unwrap();
    assert!((scale - SCALE).abs() / SCALE < 0.01, "{}", answer["calibration"]);
    assert_eq!(answer["calibration"]["from"], "the fiducial");
    assert_eq!(answer["calibration"]["region"], objects.len());
    let measurements = answer["measurements"].as_array().unwrap();
    assert_eq!(measurements.len(), objects.len());
    for (object, measured) in objects.iter().zip(measurements) {
        let longest = measured["longestMm"].as_f64().unwrap();
        let error = (longest - object.longest_mm()).abs() / object.longest_mm();
        assert!(error < 0.05, "{object:?}: {measured}");
        assert_eq!(measured["geometry"], "mask");
        assert!(measured["longestUncertaintyMm"].as_f64().unwrap() > 0.0);
        assert!(measured["line"].as_array().is_some_and(|line| line.len() == 2));
    }
    // One segmentation for the fiducial, one for the defects, each with its record.
    let calls: Vec<_> = audit.records().into_iter().filter(|r| r.action == "vision.call").collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|r| r.ext["tool"] == "vision_measure"));

    // The answer record carries each measurement with its calibration, as the viewer reads it.
    let names = bench
        .artifacts
        .list(ListRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
        })
        .await
        .unwrap()
        .file_names;
    let name = names.iter().find(|n| n.starts_with(adk_vision::ANSWER_ARTIFACT_PREFIX)).unwrap();
    let part = bench
        .artifacts
        .load(LoadRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            file_name: name.clone(),
            version: None,
        })
        .await
        .unwrap()
        .part;
    let Part::InlineData { data, .. } = part else { panic!("the record is JSON") };
    let record: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(record["tool"], "vision_measure");
    let regions = record["regionSet"]["regions"].as_array().unwrap();
    assert_eq!(regions.len(), objects.len() + 1, "the defects, then the fiducial");
    assert_eq!(regions[objects.len()]["label"], "fiducial");
    let first = &record["measurements"][0];
    assert_eq!(first["tool"], "vision_measure");
    assert_eq!(first["region"], 0);
    assert_eq!(first["calibration"]["referenceMm"], FIDUCIAL_MM);
    assert_eq!(first["calibration"]["region"], objects.len());
    assert!(first["calibration"]["plane"].is_string());
    assert_eq!(first["longestMm"], measurements[0]["longestMm"]);
}

#[tokio::test]
async fn vision_measure_reuses_a_scale_and_refuses_without_one() {
    let bench = bench().await;
    let crack = defects()[3];
    let args = json!({
        "image": IMAGE,
        "boxes": [bbox_json(crack.bbox())],
        "calibration": { "pxPerMm": SCALE, "uncertaintyPxPerMm": 0.02 },
    });
    let (answer, _) = measure_through_the_tool(&bench, args).await;
    assert_eq!(answer["calibration"]["from"], "a given scale");
    assert!(answer["calibration"].get("region").is_none());
    let longest = answer["measurements"][0]["longestMm"].as_f64().unwrap();
    assert!((longest - crack.longest_mm()).abs() / crack.longest_mm() < 0.05, "{answer}");

    let (refused, _) = measure_through_the_tool(
        &bench,
        json!({ "image": IMAGE, "boxes": [bbox_json(crack.bbox())] }),
    )
    .await;
    assert!(refused.to_string().contains("name a `fiducial`"), "{refused}");
}
