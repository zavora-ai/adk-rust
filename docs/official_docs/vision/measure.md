# Calibration and Measurement

A fiducial of known size lying in the measured plane gives an image's scale in pixels per
millimetre. Under that calibration, `adk-vision` measures a region's maximum length and area
from its mask, each with an uncertainty, and the governed `vision_measure` tool does the same
for an agent. This page implements the calibration and measurement bullet of spec 3.7 §7.13
([spec](../../design/spec-3.7-vision.md)).

```toml
[dependencies]
adk-vision = "3.0.0"
```

> **Important:** a measurement is valid only for an object lying in the fiducial's plane, seen
> by the same camera at the same image size. One image holds no depth, so anything nearer to
> or farther from the camera than the fiducial measures wrongly; depth arrives with the
> spatial outputs of phase D. Every result says so (`MEASUREMENT_VALIDITY`).

## Calibrating

`Calibration::from_fiducial` takes the fiducial's region — a mask from a segmenter, a polygon
or a box — and its size (`FiducialSpec::square(20.0)`, `FiducialSpec::rectangle(85.6, 54.0)`,
with an optional printed tolerance). It fits the smallest rectangle enclosing the region and
matches its sides long to long, so a fiducial at any rotation in the plane calibrates.

| Refused | Because |
|---------|---------|
| A side under 10 px | Locating each side to ±1 px would be more than a tenth of the scale |
| Sides whose scales differ by more than 20% | The region is not the fiducial, or the plane is tilted too far to measure in |
| A region outside the image, or a mask of another size | Regions are in the source image's pixels |

`Calibration::from_scale` takes a scale directly, such as one an earlier fiducial gave for
the same fixed camera. A calibration holds for images of its own size only.

## Measuring

`Calibration::measure` returns a `Measurement`:

| Field | Meaning |
|-------|---------|
| `longest_mm`, `longest_uncertainty_mm` | The maximum Feret diameter — the longest distance across the region — and its uncertainty |
| `line` | The two points that distance joins, in source pixels |
| `area_mm2`, `area_uncertainty_mm2` | The area and its uncertainty |
| `longest_px`, `area_px` | The same in pixels |
| `geometry` | `mask` (the set pixels as unit squares), `polygon`, or `box` (an upper bound on the object's extent) |

```rust
use adk_vision::{BBox, Calibration, FiducialSpec, Mask, Shape};

// A 20 mm fiducial drawn 150 px wide at 7.5 px/mm in a 1600x1000 frame.
let fiducial = Shape::Mask(Mask::from_box(1600, 1000, &BBox::new(100.0, 100.0, 250.0, 250.0)));
let calibration =
    Calibration::from_fiducial(&fiducial, FiducialSpec::square(20.0), (1600, 1000)).unwrap();

// A scratch 300 px long and 3 px wide: 40 mm.
let scratch = Shape::Mask(Mask::from_box(1600, 1000, &BBox::new(400.0, 500.0, 700.0, 503.0)));
let measured = calibration.measure(&scratch, (1600, 1000)).unwrap();
assert!((measured.longest_mm - 40.0).abs() < 0.05);
println!("{:.1} ± {:.1} mm", measured.longest_mm, measured.longest_uncertainty_mm);
```

### Uncertainty

Each figure is one standard uncertainty, from these sources added in quadrature:

| Quantity | Sources |
|----------|---------|
| Scale `k` (px/mm) | Each fiducial side located within ±1 px; the printed tolerance; half the difference between the scales the two sides give, which perspective and a tilted plane produce |
| Maximum length | ±1 px over the length (each end within half a pixel), and the scale's relative uncertainty times the length |
| Area | Half a pixel along the whole boundary, and twice the scale's relative uncertainty times the area |

## `vision_measure`

`vision_measure` joins the [governed vision tools](governed-tools.md) whenever the runtime has
a segmenter. It is `ReadOnly`, runs on the governed path like `vision_segment`, and with a
remote segmenter its image is checked, held and released through the egress gate the same way.

| Argument | Meaning |
|----------|---------|
| `image` | An artifact name or a `sha256:` digest |
| `boxes`, `concepts` | The objects to measure; each box is segmented to its mask |
| `fiducial` | `{ "box": [x0, y0, x1, y1], "widthMm": 20, "heightMm": 20, "toleranceMm": 0.1 }`: the fiducial is segmented from its box and calibrates the scale; `heightMm` defaults to `widthMm` |
| `calibration` | `{ "pxPerMm": 7.5, "uncertaintyPxPerMm": 0.04 }`: a scale an earlier call returned, instead of a fiducial |
| `region`, `backend` | As for the other perception tools; a region must hold the fiducial too |

One of `fiducial` and `calibration` is required. The response carries the calibration, one
measurement per region, and the validity statement. This is the first of the seven
measurements on the calibration image of the tests below (a 6 mm defect 0.5 mm wide, so a
6.02 mm diagonal), with the test's on-device segmenter:

```json
{
  "image": "sha256:64fe0215…",
  "width": 1600,
  "height": 1000,
  "calibration": {
    "pxPerMm": 7.5585, "uncertaintyPxPerMm": 0.0354,
    "referenceMm": 20.0, "referencePx": 151.2,
    "plane": "the fiducial's plane", "from": "the fiducial", "region": 7
  },
  "measurements": [
    { "region": 0, "box": [537.0, 158.0, 583.0, 162.0], "label": "defect", "score": 0.95,
      "geometry": "mask", "longestMm": 6.11, "longestUncertaintyMm": 0.14,
      "areaMm2": 3.22, "areaUncertaintyMm2": 0.88, "line": [[537.0, 158.0], [583.0, 162.0]] }
  ],
  "validity": "Valid only for objects lying in the plane of the calibration fiducial, …",
  "calls": [{ "backend": "colour", "remote": false }, { "backend": "colour", "remote": false }]
}
```

A region that cannot be measured, such as a point, carries an `error` instead of figures.
The call's [answer record](../surfaces/annotated-viewer.md) lists the measured regions and
then the fiducial, with each measurement and its calibration in the form the annotated viewer
draws as a measurement tile.

## Tests

`adk-vision/tests/measure_tests.rs` draws a calibration image: a 20 mm fiducial turned 12° in
the plane at 7.5 px/mm, six seeded defects 6 to 45 mm long at six angles, and an ellipse of
known area. The scale comes out within 1% (7.5585 against 7.5 px/mm), every length and the
area within 5% (S9; the worst length is 1.5% off, the 6 mm defect), each error inside three of
its standard uncertainties, both directly and through `vision_measure` on the governed path
with its answer record.

## Related

- [Watch Conditions and Visual Wakes](wake.md): the Inspector wakes on a defect, then measures it
- [Governed Vision Tools](governed-tools.md)
- [Vision overview](index.md)
