//! Properties of the Screen copilot's coordinate mapping (spec 3.7 §7.3, §7.14, V-I14):
//! `ScreenSpace` round trips between screenshot pixels and logical points, fitted
//! screenshots stay within the model's limits and map back within a pixel, and a grounded
//! mark's click point always lies inside its target in both spaces.

use adk_core::ScreenPoint;
use adk_vision::screen::{CandidateSource, MarkSet, ScreenshotLimits, ScreenshotScale};
use adk_vision::{BBox, Digest, ScreenSpace};
use proptest::prelude::*;

fn ratio() -> impl Strategy<Value = f32> {
    prop_oneof![Just(1.0f32), Just(1.25), Just(1.5), Just(2.0), Just(3.0), 0.5f32..4.0]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Pixels to points and back returns the point within 0.01 px, for any display origin
    /// and device pixel ratio.
    #[test]
    fn screen_space_round_trips(
        ox in -4000.0f32..4000.0,
        oy in -2000.0f32..2000.0,
        r in ratio(),
        x in 0.0f32..8000.0,
        y in 0.0f32..5000.0,
    ) {
        let screen = ScreenSpace::new("display", [ox, oy], r);
        let back = screen.to_pixels(screen.to_points([x, y]));
        prop_assert!((back[0] - x).abs() < 0.01 && (back[1] - y).abs() < 0.01, "{back:?} vs ({x}, {y})");
        let points = screen.to_points([x, y]);
        let again = screen.to_points(screen.to_pixels(points));
        prop_assert!((again[0] - points[0]).abs() < 0.01 && (again[1] - points[1]).abs() < 0.01);
    }

    /// A mark's click point is inside its region in pixels and in points.
    #[test]
    fn a_grounded_click_lies_inside_its_target(
        r in ratio(),
        x in 0.0f32..2900.0,
        y in 0.0f32..1900.0,
        w in 2.0f32..120.0,
        h in 2.0f32..60.0,
    ) {
        let screen = ScreenSpace::new("built-in", [0.0, 0.0], r);
        let mut marks = MarkSet::new(Digest::of(b"shot"), (3024, 1964), screen.clone());
        let mark = marks
            .push(BBox::new(x, y, x + w, y + h), Some("Target".into()), CandidateSource::OcrWord { confidence: None })
            .unwrap();
        let target = marks.ground(mark).unwrap();
        prop_assert!(target.points.contains(target.click));
        prop_assert!(target.pixels.contains(target.click_pixel()));
        prop_assert_eq!(target.points, screen.bbox_to_points(&target.pixels));
    }

    /// A fitted screenshot fits the limits, keeps its aspect ratio within a pixel, and
    /// every sent pixel maps to a source pixel that maps back to it.
    #[test]
    fn fitted_screenshots_fit_and_map_back(
        w in 64u32..8000,
        h in 64u32..8000,
        high in any::<bool>(),
        fx in 0.0f64..1.0,
        fy in 0.0f64..1.0,
    ) {
        let limits = if high { ScreenshotLimits::CLAUDE_HIGH } else { ScreenshotLimits::CLAUDE_STANDARD };
        let scale = ScreenshotScale::fitted((w, h), &limits);
        let [sw, sh] = scale.sent;
        prop_assert!(limits.fits((sw, sh)), "{w}x{h} -> {sw}x{sh}");
        prop_assert!(sw <= w && sh <= h);
        // The short side follows the long one, rounded.
        let (long, short, src_long, src_short) =
            if w >= h { (sw, sh, w, h) } else { (sh, sw, h, w) };
        let expected = f64::from(long) * f64::from(src_short) / f64::from(src_long);
        prop_assert!(
            (f64::from(short) - expected.max(1.0)).abs() <= 1.0,
            "{sw}x{sh} for {w}x{h}"
        );
        let sent = ScreenPoint::new(
            ((f64::from(sw) - 1.0) * fx).floor() as i64,
            ((f64::from(sh) - 1.0) * fy).floor() as i64,
        );
        let source = scale.point_to_source(sent);
        prop_assert!(source.x >= 0 && source.x < i64::from(w) && source.y >= 0 && source.y < i64::from(h));
        prop_assert_eq!(scale.point_to_sent(source), sent);
        let region = scale.region_to_source([sent.x, sent.y, sent.x + 1, sent.y + 1]);
        prop_assert!(region[0] <= source.x && source.x < region[2]);
        prop_assert!(region[1] <= source.y && source.y < region[3]);
    }
}
