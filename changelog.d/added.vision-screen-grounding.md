- **Set-of-marks screen grounding** (`adk-vision`, `adk-browser`, `adk-computer-use`):
  `adk_vision::screen` numbers the candidates of a screenshot — `browser_elements`
  rectangles, local OCR words or a model's point — and maps the chosen mark through
  `ScreenSpace` to a click point in logical points with the screenshot's digest, or to a
  `browser_click` by selector. `vision_annotate` takes `devicePixelRatio`, `units: "css"` and
  per-box `mark` numbers. `ScreenshotLimits`, `fit_screenshot` and `FittedActuator` resize
  screenshots to Claude's image limits for `computer_toolset_20260801`, keep the scale, and
  map clicks and `zoom` regions back. `ScreenPlan` previews up to N actions in one window for
  one approval and re-checks each call against its preview. The `computer-use` feature fills
  `TargetEvidence` from a grounded mark (spec 3.7 §7.14, X7, X16, V-I14, Q8).
- **`browser_elements`** (`adk-browser`): the visible interactive elements of the viewport
  with their rectangles in CSS pixels, role, name and a selector that matches only them; in
  every `BrowserToolset` profile and classed `Read` by the harness browser pack.
