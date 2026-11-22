//! The Screen copilot's grounding on the S5 screenshot: a 3024×1964 capture of a
//! 1512×982-point display at device pixel ratio 2 with twelve labelled targets (spec 3.7
//! §7.14, S5, S14, V-I14, Q8).
//!
//! Candidates come from `browser_elements` (CSS pixels × DPR), local OCR word boxes and
//! Claude's computer use toolset coordinates on the screenshot fitted to its limits; every
//! chosen mark maps through `ScreenSpace` to a click point inside its target in logical
//! points. A seeded mis-grounding — two identical "Save" buttons — is caught by checking
//! the candidate against the previewed target and grounded again.

use std::sync::{Arc, Mutex};

use adk_core::{
    ActuationOutput, ComputerAction, ComputerActionKind, ComputerActuator, MouseButton,
    ScreenPoint, Screenshot,
};
use adk_vision::screen::{
    BrowserElements, CandidateSource, Deviation, FittedActuator, MarkSet, PlanRefusal,
    PreviewedTarget, ScreenPlan, ScreenshotLimits, ScreenshotScale, fit_screenshot,
};
use adk_vision::{
    BBox, BackendId, CoordFrame, DecodeLimits, DecodedImage, Digest, Region, RegionOrigin,
    ScreenSpace, Shape, SourceImage, TransformChain, Word,
};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/s5/s5.json");
const SCREEN_PNG: &[u8] = include_bytes!("fixtures/s5/s5-screen.png");

struct Truth {
    digest: Digest,
    screen: ScreenSpace,
    targets: Vec<(String, BBox)>,
}

fn truth() -> Truth {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let image = &fixture["image"];
    assert_eq!((image["width"].as_u64(), image["height"].as_u64()), (Some(3024), Some(1964)));
    Truth {
        digest: Digest::of(SCREEN_PNG),
        screen: ScreenSpace::new(
            image["display"].as_str().unwrap(),
            [0.0, 0.0],
            image["devicePixelRatio"].as_f64().unwrap() as f32,
        ),
        targets: fixture["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let b: Vec<f32> = t["box"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap() as f32)
                    .collect();
                (t["name"].as_str().unwrap().to_string(), BBox::new(b[0], b[1], b[2], b[3]))
            })
            .collect(),
    }
}

/// The widget's label as `render.py` draws it.
fn label(name: &str) -> String {
    name.replace('-', " ").to_uppercase()
}

/// A word box as a local reader reports it: the label's text, centred in its widget, about
/// 60% of the widget's width and half its height.
fn ocr_words(truth: &Truth) -> Vec<Word> {
    truth
        .targets
        .iter()
        .map(|(name, b)| {
            let (cx, cy) = (b.center()[0], b.center()[1]);
            let (hw, hh) = (b.width() * 0.3, b.height() * 0.25);
            Word {
                region: Region {
                    image: truth.digest,
                    shape: Shape::Box(BBox::new(cx - hw, cy - hh, cx + hw, cy + hh)),
                    label: None,
                    score: Some(0.97),
                    track: None,
                    origin: RegionOrigin {
                        backend: BackendId::new("pp-ocrv6"),
                        manifest: Some("pp-ocrv6-small".into()),
                        frame: CoordFrame::SentPixels,
                        chain: TransformChain::new(3024, 1964),
                        escalated_from: None,
                    },
                },
                text: label(name),
                confidence: Some(0.97),
            }
        })
        .collect()
}

/// `browser_elements` for the same page: CSS rectangles at device pixel ratio 2.
fn browser_page(truth: &Truth) -> BrowserElements {
    let elements: Vec<Value> = truth
        .targets
        .iter()
        .map(|(name, b)| {
            json!({
                "selector": format!("#{name}"),
                "role": "button",
                "name": label(name),
                "rect": { "x": b.x0 / 2.0, "y": b.y0 / 2.0, "width": b.width() / 2.0, "height": b.height() / 2.0 }
            })
        })
        .collect();
    serde_json::from_value(json!({ "devicePixelRatio": 2.0, "elements": elements })).unwrap()
}

fn target_points(truth: &Truth, name: &str) -> BBox {
    let (_, b) = truth.targets.iter().find(|(n, _)| n == name).unwrap();
    truth.screen.bbox_to_points(b)
}

#[test]
fn browser_marks_click_by_selector_inside_their_targets() {
    let truth = truth();
    let mut marks = MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone());
    let numbers = marks.add_browser_elements(&browser_page(&truth));
    assert_eq!(numbers.len(), 12);
    for (mark, (name, _)) in numbers.iter().zip(&truth.targets) {
        let target = marks.ground(*mark).unwrap();
        let points = target_points(&truth, name);
        assert!(points.contains(target.click), "{name}: {:?} misses {points:?}", target.click);
        // A browser mark is clicked by selector: no coordinate reaches the browser.
        assert_eq!(target.browser_click_args().unwrap(), json!({ "selector": format!("#{name}") }));
        assert_eq!(target.screenshot, truth.digest);
    }
}

#[test]
fn ocr_word_marks_map_to_points_inside_their_targets() {
    let truth = truth();
    let mut marks = MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone());
    let numbers = marks.add_words(&ocr_words(&truth));
    assert_eq!(numbers.len(), 12);
    for (mark, (name, _)) in numbers.iter().zip(&truth.targets) {
        let target = marks.ground(*mark).unwrap();
        let points = target_points(&truth, name);
        assert!(points.contains(target.click), "{name}: {:?} misses {points:?}", target.click);
        assert_eq!(target.label.as_deref(), Some(label(name).as_str()));
        assert!(target.browser_click_args().is_none());
    }
    // Words of another image are not candidates on this one.
    let mut other = MarkSet::new(Digest::of(b"other"), (3024, 1964), truth.screen.clone());
    assert!(other.add_words(&ocr_words(&truth)).is_empty());
}

#[test]
fn claude_toolset_points_on_the_fitted_screenshot_land_inside_their_targets() {
    let truth = truth();
    let (sent, scale) = fit_screenshot(SCREEN_PNG, &ScreenshotLimits::CLAUDE_HIGH).unwrap();
    assert_eq!(scale, ScreenshotScale::new((3024, 1964), (2380, 1546)));
    let sent = SourceImage::decode(&sent, &DecodeLimits::default()).unwrap();
    assert_eq!(sent.size(), (2380, 1546));
    assert!(ScreenshotLimits::CLAUDE_HIGH.fits(sent.size()));

    let mut marks = MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone());
    marks.add_words(&ocr_words(&truth));
    for (name, b) in &truth.targets {
        // Claude answers in whole pixels of the 2380×1546 copy it received.
        let at = scale.to_sent(b.center());
        let point = [at[0].round(), at[1].round()];
        let mark = marks.add_model_point(point, &scale, "claude-sonnet-5-5").unwrap();
        let target = marks.ground(mark).unwrap();
        let points = target_points(&truth, name);
        assert!(points.contains(target.click), "{name}: {:?} misses {points:?}", target.click);
        assert!(matches!(target.source, CandidateSource::ModelPoint { .. }));
        // The same target, back on the copy Claude saw, is a pixel inside the widget.
        let [x, y] = target.toolset_coordinate(&scale);
        let back = scale.to_source([x as f32 + 0.5, y as f32 + 0.5]);
        assert!(b.contains(back), "{name}: {back:?}");
    }
}

#[cfg(feature = "computer-use")]
#[test]
fn grounded_marks_fill_target_evidence_in_points() {
    let truth = truth();
    let mut marks =
        MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone()).with_window("Customer form");
    let numbers = marks.add_words(&ocr_words(&truth));
    for (mark, (name, _)) in numbers.iter().zip(&truth.targets) {
        let target = marks.ground(*mark).unwrap();
        let evidence = target.target_evidence(
            "darwin",
            "com.example.form",
            "observation-1",
            "2026-11-22T10:00:00Z",
        );
        evidence.check_screen_binding().unwrap();
        assert_eq!(evidence.screenshot_hash.as_deref(), Some(truth.digest.to_string().as_str()));
        assert_eq!(evidence.device_pixel_ratio, Some(2.0));
        let bounds = evidence.bounds.unwrap();
        assert_eq!(bounds.space, adk_computer_use::CoordinateSpace::LogicalPoints);
        let click = [f64::from(target.click[0]), f64::from(target.click[1])];
        assert!(bounds.contains(click), "{name}");
        let points = target_points(&truth, name);
        assert!(points.contains([bounds.center()[0] as f32, bounds.center()[1] as f32]));
    }
}

#[test]
fn marks_are_drawn_at_the_device_pixel_ratio() {
    let truth = truth();
    let pixels = SourceImage::decode(SCREEN_PNG, &DecodeLimits::default()).unwrap();
    let words = ocr_words(&truth);
    let badge = |ratio: f32| {
        let screen = ScreenSpace::new("built-in", [0.0, 0.0], ratio);
        let mut marks = MarkSet::new(truth.digest, (3024, 1964), screen);
        marks.add_words(&words[..1]);
        let out = marks.annotate(pixels.pixels()).unwrap();
        let corner = marks.candidates[0].region;
        // Badge pixels to the left of the word box's top-left corner, along its top edge.
        (1..60)
            .take_while(|dx| {
                let x = (corner.x0 as i64 - dx).max(0) as u32;
                let p = out.pixel(x, corner.y0 as u32);
                p == [230, 25, 75, 255] || p == [255, 255, 255, 255]
            })
            .count()
    };
    let (one, two) = (badge(1.0), badge(2.0));
    assert!(one >= 5, "a badge is drawn at ratio 1 ({one} px)");
    assert!(two >= 2 * one - 2, "the badge doubles at ratio 2: {one} px vs {two} px");
    // The arguments that make vision_annotate draw the same marks.
    let mut marks = MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone());
    marks.add_words(&words);
    let args = marks.annotate_args();
    assert_eq!(args["devicePixelRatio"], json!(2.0));
    assert_eq!(args["boxes"].as_array().unwrap().len(), 12);
    assert_eq!(args["boxes"][9]["mark"], json!(10));
}

/// The seeded mis-grounding of S14: the form's dialog Save and an identical Save in the
/// toolbar. The plan previewed the dialog's Save; an executor that grounds by label alone
/// picks the toolbar's, which the check against the preview catches, and grounding again
/// finds the previewed one.
#[test]
fn a_seeded_mis_grounding_is_caught_against_the_preview_and_grounded_again() {
    let truth = truth();
    let ocr = || CandidateSource::OcrWord { confidence: Some(0.95) };
    let dialog_save = BBox::new(1920.0, 1180.0, 2100.0, 1260.0);
    let toolbar_save = BBox::new(2620.0, 20.0, 2800.0, 100.0);

    let mut before =
        MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone()).with_window("Customer form");
    let name = before
        .push(BBox::new(400.0, 300.0, 1300.0, 380.0), Some("NAME FIELD".into()), ocr())
        .unwrap();
    let save = before.push(dialog_save, Some("SAVE".into()), ocr()).unwrap();
    let mut plan = ScreenPlan::new(&before).with_max_actions(3);
    plan.push(
        "c1",
        "computer.left_click",
        json!({ "mark": name }),
        Some(&before.ground(name).unwrap()),
    )
    .unwrap();
    plan.push("c2", "computer.type", json!({ "text": "Ada Lovelace" }), None).unwrap();
    plan.push(
        "c3",
        "computer.left_click",
        json!({ "mark": save }),
        Some(&before.ground(save).unwrap()),
    )
    .unwrap();
    assert!(plan.render().contains("\"SAVE\" at (1005, 610) pt"), "{}", plan.render());

    // After typing, a fresh screenshot shows a second, identical Save in the toolbar.
    let mut after = MarkSet::new(Digest::of(b"after typing"), (3024, 1964), truth.screen.clone())
        .with_window("Customer form");
    after.push(BBox::new(400.0, 300.0, 1300.0, 380.0), Some("NAME FIELD".into()), ocr());
    let wrong = after.push(toolbar_save, Some("SAVE".into()), ocr()).unwrap();
    after.push(dialog_save, Some("SAVE".into()), ocr()).unwrap();

    // A label-only grounder takes the first Save: the toolbar's.
    let naive = after.by_label("save")[0].mark;
    assert_eq!(naive, wrong);
    let preview = plan.step("c3").unwrap().target.clone().unwrap();
    let caught = preview.check(&after, naive).unwrap_err();
    assert!(matches!(caught, Deviation::WrongTarget { .. }), "{caught:?}");
    assert!(caught.to_string().contains("ground it again"));

    // Grounded again against the preview: the dialog's Save, inside its target in points.
    let args = plan.approved_args("c3").unwrap();
    let regrounded = plan.check_call("c3", &args, &after).unwrap().unwrap();
    assert_eq!(regrounded.pixels, dialog_save);
    assert!(target_points(&truth, "save").contains(regrounded.click));
    assert!(preview.check(&after, regrounded.mark).is_ok());
}

#[test]
fn a_plan_is_one_window_up_to_n_actions_and_rechecks_every_call() {
    let truth = truth();
    let ocr = || CandidateSource::OcrWord { confidence: None };
    let mut marks =
        MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone()).with_window("Customer form");
    let save = marks.push(BBox::new(1920.0, 1180.0, 2100.0, 1260.0), Some("SAVE".into()), ocr());
    let save = marks.ground(save.unwrap()).unwrap();

    let mut plan = ScreenPlan::new(&marks).with_max_actions(2);
    plan.push("c1", "computer.left_click", json!({ "mark": 1 }), Some(&save)).unwrap();
    plan.push("c2", "computer.key", json!({ "text": "Return" }), None).unwrap();
    assert_eq!(
        plan.push("c3", "computer.key", json!({ "text": "Tab" }), None).unwrap_err(),
        PlanRefusal::TooManyActions { max: 2 }
    );

    // A target in another window cannot join the plan.
    let mut crm = MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone()).with_window("CRM");
    let row = crm.push(BBox::new(0.0, 0.0, 100.0, 100.0), Some("ROW".into()), ocr()).unwrap();
    let mut other = ScreenPlan::new(&marks);
    assert!(matches!(
        other.push("c9", "computer.left_click", json!({}), Some(&crm.ground(row).unwrap())),
        Err(PlanRefusal::OtherWindow { .. })
    ));

    // The approval binds the plan digest; a changed call or another window holds again.
    let approved = plan.approved_args("c1").unwrap();
    assert_eq!(approved["expect_target"]["plan"], json!(plan.digest()));
    assert!(plan.check_call("c1", &approved, &marks).is_ok());
    assert!(matches!(
        plan.check_call("c1", &json!({ "mark": 2 }), &marks),
        Err(Deviation::ArgumentsChanged { .. })
    ));
    assert!(matches!(plan.check_call("c1", &approved, &crm), Err(Deviation::Window { .. })));
    assert!(matches!(plan.check_call("c7", &approved, &marks), Err(Deviation::NotInPlan { .. })));
    // The plan digest is the same with serde_json's preserve_order on or off: keys sorted.
    let reordered: ScreenPlan =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert_eq!(reordered.digest(), plan.digest());
}

#[test]
fn toolset_calls_preview_against_the_fitted_screenshot() {
    let truth = truth();
    let scale = ScreenshotScale::fitted((3024, 1964), &ScreenshotLimits::CLAUDE_HIGH);
    let mut marks = MarkSet::new(truth.digest, (3024, 1964), truth.screen.clone());
    marks.add_words(&ocr_words(&truth));
    let at = |name: &str| {
        let (_, b) = truth.targets.iter().find(|(n, _)| n == name).unwrap();
        let p = scale.to_sent(b.center());
        json!([p[0].round() as i64, p[1].round() as i64])
    };
    let calls = vec![
        (
            "c1".to_string(),
            "computer.left_click".to_string(),
            json!({ "coordinate": at("name-field") }),
        ),
        ("c2".to_string(), "computer.type".to_string(), json!({ "text": "Ada" })),
        ("c3".to_string(), "computer.left_click".to_string(), json!({ "coordinate": at("save") })),
    ];
    let plan = ScreenPlan::from_toolset_calls(&marks, &scale, &calls).unwrap();
    let labels: Vec<Option<&str>> =
        plan.steps.iter().map(|s| s.target.as_ref().and_then(|t| t.label.as_deref())).collect();
    assert_eq!(labels, vec![Some("NAME FIELD"), None, Some("SAVE")]);
    let PreviewedTarget { click, .. } = plan.steps[2].target.clone().unwrap();
    assert!(target_points(&truth, "save").contains(click));
}

/// A desktop whose screenshots are the S5 capture; it records the actions it receives.
struct Desktop {
    actions: Mutex<Vec<ComputerAction>>,
}

#[async_trait::async_trait]
impl ComputerActuator for Desktop {
    fn display_size(&self) -> Option<(u32, u32)> {
        Some((3024, 1964))
    }

    fn supports(&self, kind: ComputerActionKind) -> bool {
        kind != ComputerActionKind::Zoom
    }

    async fn perform(&self, action: ComputerAction) -> adk_core::Result<ActuationOutput> {
        self.actions.lock().unwrap().push(action.clone());
        Ok(match action {
            ComputerAction::Screenshot => {
                ActuationOutput::Image(Screenshot::png(SCREEN_PNG.to_vec()).unwrap())
            }
            ComputerAction::CursorPosition => ActuationOutput::Cursor(ScreenPoint::new(1500, 900)),
            _ => ActuationOutput::Done,
        })
    }
}

#[tokio::test]
async fn a_fitted_actuator_sends_fitting_screenshots_and_scales_actions_back() {
    let truth = truth();
    let desktop = Arc::new(Desktop { actions: Mutex::new(Vec::new()) });
    let fitted = FittedActuator::for_claude_model(desktop.clone(), "claude-sonnet-5-5");
    assert_eq!(fitted.display_size(), Some((2380, 1546)));
    assert!(fitted.supports(ComputerActionKind::Zoom), "zoom is cropped when the desktop has none");

    let ActuationOutput::Image(shot) = fitted.perform(ComputerAction::Screenshot).await.unwrap()
    else {
        panic!("a screenshot is an image");
    };
    assert_eq!((shot.width, shot.height), (2380, 1546));
    assert!(shot.fits_current_claude_limits());
    let scale = fitted.scale().unwrap();

    // Claude clicks Save on the copy it saw; the desktop receives a pixel inside Save.
    let (_, save) = truth.targets.iter().find(|(n, _)| n == "save").unwrap();
    let sent = scale.to_sent(save.center());
    let at = ScreenPoint::new(sent[0].round() as i64, sent[1].round() as i64);
    fitted
        .perform(ComputerAction::Click {
            button: MouseButton::Left,
            count: 1,
            at: Some(at),
            modifiers: Vec::new(),
        })
        .await
        .unwrap();
    let received = desktop.actions.lock().unwrap().last().cloned().unwrap();
    let ComputerAction::Click { at: Some(point), .. } = received else {
        panic!("a click reached the desktop: {received:?}");
    };
    assert!(save.contains([point.x as f32, point.y as f32]), "{point:?} misses {save:?}");

    // Zoom on the help button, in the full screenshot's pixel space: a full-resolution crop.
    let (_, help) = truth.targets.iter().find(|(n, _)| n == "help").unwrap();
    let a = scale.to_sent([help.x0, help.y0]);
    let b = scale.to_sent([help.x1, help.y1]);
    let region = [a[0].floor() as i64, a[1].floor() as i64, b[0].ceil() as i64, b[1].ceil() as i64];
    let ActuationOutput::Image(zoomed) =
        fitted.perform(ComputerAction::Zoom { region }).await.unwrap()
    else {
        panic!("a zoom is an image");
    };
    let source = scale.region_to_source(region);
    assert_eq!(
        (i64::from(zoomed.width), i64::from(zoomed.height)),
        (source[2] - source[0], source[3] - source[1]),
        "a small region is returned at full resolution"
    );
    let crop = SourceImage::decode(&zoomed.data, &DecodeLimits::default()).unwrap();
    let centre = crop.pixels().pixel(crop.size().0 / 2, 4);
    assert_eq!(centre, [60, 60, 160, 255], "the crop is the help button's colour");

    // The cursor position comes back in the sent copy's pixels.
    let ActuationOutput::Cursor(cursor) =
        fitted.perform(ComputerAction::CursorPosition).await.unwrap()
    else {
        panic!("a cursor position");
    };
    assert_eq!(cursor, scale.point_to_sent(ScreenPoint::new(1500, 900)));
}

#[test]
fn a_whole_screenshot_zoom_region_maps_to_the_whole_source() {
    let scale = ScreenshotScale::fitted((3024, 1964), &ScreenshotLimits::CLAUDE_HIGH);
    assert_eq!(scale.region_to_source([0, 0, 2380, 1546]), [0, 0, 3024, 1964]);
    let chain = scale.chain().unwrap();
    assert_eq!(chain.output_size(), (2380, 1546));
    let image = DecodedImage::filled(3024, 1964, [0, 0, 0, 255]);
    assert_eq!(image.size(), (chain.source_width, chain.source_height));
}
