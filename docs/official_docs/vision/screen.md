# Screen Grounding: Set-of-Marks

A model that names a numbered mark cannot miss a pixel. `adk_vision::screen` draws numbered
marks on the candidates of one screenshot, takes the number the model chooses, and maps it to
something an actuator understands: a `browser_click` selector in a browser, or a click point in
logical screen points on a desktop. It implements spec 3.7 §7.14 and the screen invariant V-I14:
a screen action's target is derived through `ScreenSpace` from a region of a screenshot whose
digest the action records.

| Piece | What it does |
|-------|--------------|
| `browser_elements` (`adk-browser`) | The page's visible interactive elements, each with its rectangle in CSS pixels and a selector that matches only it |
| `vision_annotate` | Numbered marks drawn on a screenshot at its device pixel ratio |
| `MarkSet` | The candidates of one screenshot, from browser elements, local OCR words or a model's point |
| `GroundedTarget` | A chosen mark: region in pixels and points, click point, screenshot digest, and the `browser_click` arguments or the `TargetEvidence` for it |
| `ScreenshotLimits`, `ScreenshotScale`, `fit_screenshot` | A screenshot resized to the model's image limits before it is sent, with the scale kept |
| `FittedActuator` | A `ComputerActuator` whose screenshots fit the model and whose clicks and `zoom` regions are scaled back |
| `ScreenPlan`, `PreviewedTarget` | One approval for a previewed plan of up to N actions in one window, each action grounded again and checked before it runs |

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["computer-use"] }
```

The `computer-use` feature adds `GroundedTarget::target_evidence`; everything else on this page
is in the default build.

## In a Browser

`browser_elements` lists the links, buttons, inputs and controls in the viewport. Each element
carries its rectangle from `getBoundingClientRect` in CSS pixels and a selector checked to match
exactly one element: its id, then a unique `data-testid`, `name` or `aria-label`, then a
structural path from the nearest unique id. Hidden, disabled and off-screen elements are left
out.

```json
{
  "url": "http://127.0.0.1:8080/form.html",
  "devicePixelRatio": 2,
  "viewport": { "width": 1512, "height": 982 },
  "elements": [
    { "index": 1, "selector": "#toolbar-save", "role": "button", "name": "Save", "tag": "button",
      "rect": { "x": 1310, "y": 10, "width": 90, "height": 40 } },
    { "index": 4, "selector": "#dialog-save", "role": "button", "name": "Save", "tag": "button",
      "rect": { "x": 960, "y": 590, "width": 90, "height": 40 } }
  ],
  "truncated": false
}
```

A `browser_screenshot` of that page is 3024×1964 pixels, so the rectangles are scaled by the
device pixel ratio before marks are drawn. `vision_annotate` does this with `units: "css"`, and
draws each badge and outline `devicePixelRatio` times larger so they read the same size in
points:

```json
{
  "image": "page.png",
  "units": "css",
  "devicePixelRatio": 2,
  "boxes": [
    { "box": [1310, 10, 1400, 50], "label": "Save", "mark": 1 },
    { "box": [960, 590, 1050, 630], "label": "Save", "mark": 4 }
  ]
}
```

When the model answers "4", the chosen element becomes a `browser_click` by its selector. No
coordinate reaches the browser:

```rust
use adk_vision::screen::{BrowserElements, MarkSet};
use adk_vision::{Digest, ScreenSpace};
use serde_json::json;

fn main() {
    let page: BrowserElements = serde_json::from_value(json!({
        "devicePixelRatio": 2,
        "elements": [
            { "selector": "#toolbar-save", "name": "Save",
              "rect": { "x": 1310, "y": 10, "width": 90, "height": 40 } },
            { "selector": "#dialog-save", "name": "Save",
              "rect": { "x": 960, "y": 590, "width": 90, "height": 40 } }
        ]
    }))
    .unwrap();
    let screenshot = Digest::of(b"the browser_screenshot bytes");
    let viewport = ScreenSpace::new("viewport", [0.0, 0.0], 2.0);
    let mut marks = MarkSet::new(screenshot, (3024, 1964), viewport);
    let numbers = marks.add_browser_elements(&page);

    let chosen = marks.ground(numbers[1]).unwrap();
    assert_eq!(chosen.browser_click_args().unwrap(), json!({ "selector": "#dialog-save" }));
    assert_eq!(chosen.click, [1005.0, 610.0]);
}
```

## On a Desktop

A desktop has no DOM. Candidates come from the words a local reader recognised on the
screenshot (`DocumentPage` words from PP-OCRv6), or from a point a model gave, such as a
coordinate from Claude's computer use toolset. Each region is in screenshot pixels;
`ScreenSpace` maps it to logical points with the display's origin and device pixel ratio. A
3024×1964 screenshot of a 1512×982-point display at device pixel ratio 2 halves every
coordinate.

```rust
use adk_vision::screen::{CandidateSource, MarkSet};
use adk_vision::{BBox, Digest, ScreenSpace};

fn main() {
    let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
    let mut marks = MarkSet::new(Digest::of(b"screenshot"), (3024, 1964), screen)
        .with_window("Customer form");
    let save = marks
        .push(
            BBox::new(1920.0, 1180.0, 2100.0, 1260.0),
            Some("Save".into()),
            CandidateSource::OcrWord { confidence: Some(0.97) },
        )
        .unwrap();

    let target = marks.ground(save).unwrap();
    // Region and click point in logical points, for the actuator.
    assert_eq!((target.points.x0, target.points.y0), (960.0, 590.0));
    assert_eq!(target.click, [1005.0, 610.0]);
    // The screenshot the target came from, which the action records.
    assert_eq!(target.screenshot, Digest::of(b"screenshot"));
}
```

`MarkSet::add_words` takes the words of a `DocumentPage` and skips words of another image;
`MarkSet::add_model_point` maps a point from the copy a model received back to the screenshot
and takes the smallest candidate that covers it.

### Target Evidence

With the `computer-use` feature, a grounded target fills `adk_computer_use::TargetEvidence` for
`computer_use_preview` and the held `computer_use_execute`. The bounds are in logical points and
state their device pixel ratio; the evidence names the screenshot's digest and its ratio:

```rust
use adk_vision::screen::{CandidateSource, MarkSet};
use adk_vision::{BBox, Digest, ScreenSpace};

fn main() {
    let mut marks = MarkSet::new(
        Digest::of(b"screenshot"),
        (3024, 1964),
        ScreenSpace::new("built-in", [0.0, 0.0], 2.0),
    );
    let save = marks
        .push(
            BBox::new(1920.0, 1180.0, 2100.0, 1260.0),
            Some("Save".into()),
            CandidateSource::OcrWord { confidence: Some(0.97) },
        )
        .unwrap();
    let evidence = marks.ground(save).unwrap().target_evidence(
        "darwin",
        "com.example.form",
        "observation-1",
        "2026-11-22T10:00:00Z",
    );
    let bounds = evidence.bounds.as_ref().unwrap();
    assert_eq!((bounds.x, bounds.y, bounds.width, bounds.height), (960.0, 590.0, 90.0, 40.0));
    assert_eq!(bounds.device_pixel_ratio, 2.0);
    assert_eq!(evidence.device_pixel_ratio, Some(2.0));
    assert!(evidence.check_screen_binding().is_ok());
}
```

See [Governed Desktop Automation](../computer-use/index.md#target-bounds) for the `Bounds`
contract.

## Claude's Computer Use Toolset

`computer_toolset_20260801` — the computer use version Claude Sonnet 5.5, Opus 5.5, Haiku 5.5
and Fable 5.2 accept — takes no display size, and the API refuses a screenshot over the model's
image limits instead of resizing it. Claude answers in pixels of the image it received, and its
`zoom` member takes `[x0, y0, x1, y1]` in the full screenshot's pixel space, also after a zoom.
So the screenshot is resized before it is sent and the scale is kept.

`ScreenshotLimits::fitted_size` computes the size the provider would choose: the largest
aspect-preserving size whose sides, padded to multiples of 28, stay within the long-edge limit
and whose `⌈w/28⌉ × ⌈h/28⌉` visual tokens stay within the token limit.

| Limits | Models | Long edge | Visual tokens | 3024×1964 becomes |
|--------|--------|-----------|---------------|-------------------|
| `ScreenshotLimits::CLAUDE_HIGH` | Opus 4.7 and later, Sonnet 5 and later, Haiku 5.5, Fable, Mythos | 2,576 px | 4,784 | 2380×1546 |
| `ScreenshotLimits::CLAUDE_STANDARD` | Earlier Claude models | 1,568 px | 1,568 | 1372×891 |

`FittedActuator` applies this to any `ComputerActuator`. Each screenshot is resized and its
`ScreenshotScale` recorded; clicks, drags, scrolls and moves are mapped back to the inner
actuator's pixels; a `zoom` region is mapped back and cropped from a full-resolution capture
(or passed to the inner actuator's own zoom), and the zoom image is downscaled only when it is
larger than the sent screenshot:

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_computer_use::ComputerUseToolset;
use adk_core::{ComputerActuator, Llm};
use adk_vision::screen::FittedActuator;

fn build(
    desktop: Arc<dyn ComputerActuator>,
    model: Arc<dyn Llm>,
) -> Result<adk_agent::LlmAgent, adk_core::AdkError> {
    let fitted = Arc::new(FittedActuator::for_claude_model(desktop, "claude-sonnet-5-5"));
    LlmAgentBuilder::new("operator")
        .model(model)
        .toolset(Arc::new(ComputerUseToolset::new(fitted)))
        .build()
}
```

`zoom` is always offered through `FittedActuator`, since it can crop from a screenshot when the
inner actuator has no zoom of its own.

> **Note:** On 2026-11-21 `claude-sonnet-5-5`, given the 3024×1964 S5 screenshot through
> `FittedActuator`, took a screenshot (sent as 2380×1546), zoomed on `[0, 1460, 300, 1546]` to
> read the small button in the corner ("HELP"), and clicked `[1580, 960]`, which maps to
> source pixel (2008, 1220) inside Save. The calls are recorded in
> `adk-vision/tests/fixtures/screen/claude-zoom.json` and replayed by the PR tier.

## One Approval per Previewed Plan

No standing rule may cover `computer_use_execute`, so a copilot that asks once per click is
tiring. A `ScreenPlan` previews up to N actions (10 by default) in one window on one screenshot,
so a person approves them once. The approval binds the plan's digest, computed from its
canonical JSON with keys sorted: `approved_args` adds `expect_target` — the plan digest and the
previewed target — to each call's arguments, so the call's fingerprint binds what was shown.

Before each action runs, a fresh screenshot is grounded again and `ScreenPlan::check_call`
checks the call against the plan:

| Check | Deviation, and the call is held again |
|-------|---------------------------------------|
| The call is a step of the plan | `NotInPlan` |
| Its arguments are the previewed ones | `ArgumentsChanged` |
| The screen shows the plan's window | `Window` |
| A candidate with the previewed label covers the previewed click point | `Missing` |

`PreviewedTarget::check` checks a candidate an executor is about to act on: same label, its
region covers the previewed click point, and the previewed region covers its click point. A
mis-grounding is caught there and grounded again with `PreviewedTarget::reground`:

```rust
use adk_vision::screen::{CandidateSource, Deviation, MarkSet, ScreenPlan};
use adk_vision::{BBox, Digest, ScreenSpace};
use serde_json::json;

fn main() {
    let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
    let ocr = || CandidateSource::OcrWord { confidence: None };
    let dialog_save = BBox::new(1920.0, 1180.0, 2100.0, 1260.0);

    let mut before = MarkSet::new(Digest::of(b"before"), (3024, 1964), screen.clone())
        .with_window("Customer form");
    let save = before.push(dialog_save, Some("Save".into()), ocr()).unwrap();
    let mut plan = ScreenPlan::new(&before);
    plan.push("c1", "computer.left_click", json!({ "mark": save }), Some(&before.ground(save).unwrap()))
        .unwrap();

    // The fresh screenshot shows a second, identical Save in the toolbar.
    let mut after = MarkSet::new(Digest::of(b"after"), (3024, 1964), screen)
        .with_window("Customer form");
    let toolbar = after
        .push(BBox::new(2620.0, 20.0, 2800.0, 100.0), Some("Save".into()), ocr())
        .unwrap();
    after.push(dialog_save, Some("Save".into()), ocr()).unwrap();

    // A grounder that matches the label alone picks the toolbar's Save: caught.
    let preview = plan.step("c1").unwrap().target.clone().unwrap();
    assert!(matches!(preview.check(&after, toolbar), Err(Deviation::WrongTarget { .. })));

    // Grounded again against the preview: the dialog's Save.
    let args = plan.approved_args("c1").unwrap();
    let target = plan.check_call("c1", &args, &after).unwrap().unwrap();
    assert_eq!(target.pixels, dialog_save);
}
```

`ScreenPlan::from_toolset_calls` previews a batch of Claude's toolset calls — a batch action —
on the screenshot it was given, grounding each `coordinate` to the candidate under it.

> **Note:** `ScreenPlan` is the plan, its digest and the per-call re-check. The harness card
> that shows a plan and answers its calls from one decision, as the change-set card of spec 3.1
> §7.26 does for edits, is not wired yet; until then each held `computer_use_execute` is
> approved on its own.

## Tests

| Test | What it proves |
|------|----------------|
| `adk-vision/tests/screen_tests.rs` | On the 3024×1964 DPR-2 screenshot, browser, OCR and toolset candidates for the twelve S5 targets map to click points inside their targets in points; the two-Save mis-grounding is caught and grounded again; a plan stays in one window and holds at most N actions; `FittedActuator` sends 2380×1546 screenshots and scales clicks and zoom regions back |
| `adk-vision/tests/screen_property_tests.rs` | `ScreenSpace` round trips within 0.01 px; a grounded click lies inside its target in both spaces; fitted screenshots fit the limits and map back within a pixel |
| `adk-vision/tests/screen_live_tests.rs` | `claude-sonnet-5-5` with `computer_toolset_20260801` zooms and clicks inside Save (live, `#[ignore]`), and the recorded calls replayed without a key |
| `examples/vision`, scenario S14 | A harness copies a CRM record into a desktop form at ratio 2: browser marks clicked by selector, PP-OCRv6 marks on the desktop, coordinates in the fitted screenshot, one `ScreenPlan` re-checked before every step, every action a governed browser call or an approved `computer_use_execute`, the two-Save mis-grounding caught by the verifier and grounded again ([scenarios](scenarios.md#phase-c-the-screen-copilot)) |
| `adk-browser/tests/elements_tests.rs` | `browser_elements` against the recorded WebDriver server, and against a real browser through `ADK_WEBDRIVER_URL` (default `http://localhost:9515`) when one answers |
