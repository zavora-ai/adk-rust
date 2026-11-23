//! S14's desktop and the copilot runtime that acts on it.
//!
//! [`Desktop`] stands in for a computer-use-mcp display at device pixel ratio 2: three windows
//! drawn by `fixtures/screen/generate.py`, and a customer form app that focuses the input it
//! is clicked in, takes typed text and saves the customer from its footer's Save. It draws
//! what is typed from the fixture's glyph strip, so every screenshot shows the form's state.
//!
//! [`ScreenRuntime`] is the `ComputerUseRuntime` the harness's computer-use pack drives
//! through adk-computer-use's reference graph. It is built from adk-vision's Screen copilot
//! pieces: PP-OCRv6 reads each screenshot on the device into set-of-marks candidates per
//! window (`MarkSet`); the copilot's coordinates, in the pixels of the screenshot fitted to
//! Claude's limits (`FittedActuator`), are grounded on those candidates and mapped through
//! `ScreenSpace` into `TargetEvidence`; each window's previewed steps form a `ScreenPlan`, and
//! every step is checked against it on a fresh screenshot before it runs. A step previewed
//! again — as the reference graph does before it executes one — is grounded again on a fresh
//! screenshot, so the harness can check the fresh target against the one its screen plan
//! showed (spec 3.7 Q8); after every action
//! a fresh screenshot goes to the verifier (`check_change`), which compares regions with the
//! change the action declared.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPostcondition, ActionPreview, ComputerUseError,
    ComputerUseRuntime, ControlLease, ExecutionCapability, ExecutionMode, ExecutionReceipt,
    LeaseBoundaries, PolicyDecision, ReceiptStatus, VerificationOutcome,
};
use adk_core::{
    ActuationOutput, AdkError, ComputerAction, ComputerActionKind, ComputerActuator, MouseButton,
    ScreenPoint, Screenshot,
};
use adk_vision::screen::{
    CandidateSource, FittedActuator, GroundedTarget, MarkSet, PreviewedTarget, ScreenPlan,
};
use adk_vision::{
    BBox, DecodeLimits, DecodedImage, Digest, DocumentReader, ExpectedChange, ReadOptions, Region,
    RegionExpectation, RegionSet, ScreenSpace, SourceImage, VerificationReport, VisionCx,
    VisionRuntime, check_change,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::fixtures::fixtures;

/// Text as the verifier compares it: letters and digits, lower case, so a reader's spacing
/// and punctuation do not decide a match.
pub fn norm(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn bbox(b: [f32; 4]) -> BBox {
    BBox::new(b[0], b[1], b[2], b[3])
}

/// One window on the display, its frame in screenshot pixels.
#[derive(Clone, Debug, Deserialize)]
pub struct Window {
    pub title: String,
    pub app: String,
    pub frame: [f32; 4],
}

impl Window {
    pub fn frame(&self) -> BBox {
        bbox(self.frame)
    }
}

/// One widget of a window, its box in screenshot pixels.
#[derive(Clone, Debug, Deserialize)]
pub struct Widget {
    pub id: String,
    pub window: String,
    pub kind: String,
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
}

impl Widget {
    pub fn bbox(&self) -> BBox {
        bbox(self.bbox)
    }
}

#[derive(Deserialize)]
struct Display {
    name: String,
    size: [u32; 2],
    #[serde(rename = "devicePixelRatio")]
    device_pixel_ratio: f32,
}

#[derive(Deserialize)]
struct GlyphStrip {
    file: String,
    height: u32,
    cells: HashMap<char, [u32; 2]>,
}

/// `fixtures/screen/layout.json`.
#[derive(Deserialize)]
pub struct Layout {
    display: Display,
    pub windows: Vec<Window>,
    pub widgets: Vec<Widget>,
    glyphs: GlyphStrip,
    /// The record the CRM holds, as the browser window draws it.
    pub record: BTreeMap<String, String>,
}

impl Layout {
    pub fn load() -> Result<Self> {
        let path = screen_dir().join("layout.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn screen(&self) -> ScreenSpace {
        ScreenSpace::new(&self.display.name, [0.0, 0.0], self.display.device_pixel_ratio)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.display.size[0], self.display.size[1])
    }

    pub fn window_at(&self, pixel: [f32; 2]) -> Option<&Window> {
        self.windows.iter().find(|w| w.frame().contains(pixel))
    }

    pub fn window(&self, title: &str) -> Option<&Window> {
        self.windows.iter().find(|w| w.title == title)
    }

    pub fn widget_at(&self, pixel: [f32; 2]) -> Option<&Widget> {
        self.widgets.iter().find(|w| w.bbox().contains(pixel))
    }

    pub fn widget(&self, id: &str) -> Option<&Widget> {
        self.widgets.iter().find(|w| w.id == id)
    }
}

pub fn screen_dir() -> PathBuf {
    fixtures().join("screen")
}

/// What the desktop received: a click at a pixel, or typed text.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopAction {
    /// `click` or `type`.
    pub kind: &'static str,
    /// Where a click landed, in screenshot pixels.
    pub pixel: Option<[f32; 2]>,
    /// The window under the click, or holding the focus for typed text.
    pub window: Option<String>,
    /// The widget under the click, or holding the focus for typed text.
    pub widget: Option<String>,
    pub text: Option<String>,
    /// The governed action this happened inside, if any.
    pub governed_by: Option<String>,
}

#[derive(Default)]
struct FormState {
    focused: Option<String>,
    values: BTreeMap<String, String>,
    note: String,
    saved: Option<BTreeMap<String, String>>,
    saves: usize,
    governing: Option<String>,
    actions: Vec<DesktopAction>,
    version: u64,
    drawn: Option<(u64, Vec<u8>)>,
}

/// The stand-in display: the drawn desktop and the customer form app on it.
pub struct Desktop {
    pub layout: Layout,
    base: DecodedImage,
    glyphs: DecodedImage,
    state: Mutex<FormState>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Desktop {
    pub fn load() -> Result<Self> {
        let layout = Layout::load()?;
        let read = |name: &str| -> Result<DecodedImage> {
            let path = screen_dir().join(name);
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            Ok(SourceImage::decode(&bytes, &DecodeLimits::default())?.pixels().clone())
        };
        let base = read("desktop.png")?;
        let glyphs = read(&layout.glyphs.file)?;
        anyhow::ensure!(base.size() == layout.size(), "desktop.png is not the layout's size");
        Ok(Self { layout, base, glyphs, state: Mutex::new(FormState::default()) })
    }

    /// Marks the actions that follow as part of the governed action `id`, or of none.
    pub fn govern(&self, id: Option<String>) {
        lock(&self.state).governing = id;
    }

    pub fn actions(&self) -> Vec<DesktopAction> {
        lock(&self.state).actions.clone()
    }

    /// The customer the form saved, field by field, and how many times it saved one.
    pub fn saved(&self) -> (Option<BTreeMap<String, String>>, usize) {
        let state = lock(&self.state);
        (state.saved.clone(), state.saves)
    }

    /// The display as a PNG, drawn from the form's state.
    pub fn render(&self) -> Result<Vec<u8>> {
        let mut state = lock(&self.state);
        if let Some((version, png)) = &state.drawn
            && *version == state.version
        {
            return Ok(png.clone());
        }
        let (width, height) = self.base.size();
        let mut rgba = self.base.rgba().to_vec();
        for widget in &self.layout.widgets {
            let value = match widget.kind.as_str() {
                "input" => state.values.get(&widget.id).cloned().unwrap_or_default(),
                "textarea" => state.note.clone(),
                _ => continue,
            };
            if value.is_empty() {
                continue;
            }
            let b = widget.bbox();
            fill(&mut rgba, width, BBox::new(b.x0 + 2.0, b.y0 + 2.0, b.x1 - 2.0, b.y1 - 2.0));
            let middle = if widget.kind == "textarea" { b.y0 + 40.0 } else { b.center()[1] };
            self.draw_text(&mut rgba, width, &value, b.x0 + 24.0, middle);
        }
        if state.saved.is_some()
            && let Some(status) = self.layout.widget("status")
        {
            let b = status.bbox();
            self.draw_text(&mut rgba, width, "Customer saved", b.x0, b.center()[1]);
        }
        let png = DecodedImage::from_rgba(width, height, rgba)?.encode_png()?;
        state.drawn = Some((state.version, png.clone()));
        Ok(png)
    }

    /// Draws `text` from the glyph strip, its cells' vertical middle at `middle`.
    fn draw_text(&self, rgba: &mut [u8], width: u32, text: &str, left: f32, middle: f32) {
        let strip = &self.layout.glyphs;
        let top = (middle - strip.height as f32 / 2.0).round() as i64;
        let mut pen = left.round() as i64;
        let height = (rgba.len() / 4) as i64 / i64::from(width);
        for ch in text.chars() {
            let Some([cx, cw]) = strip.cells.get(&ch).or_else(|| strip.cells.get(&'?')).copied()
            else {
                continue;
            };
            for dy in 0..strip.height {
                for dx in 0..cw {
                    let (x, y) = (pen + i64::from(dx), top + i64::from(dy));
                    if x < 0 || y < 0 || x >= i64::from(width) || y >= height {
                        continue;
                    }
                    let ink = self.glyphs.pixel(cx + dx, dy);
                    let at = ((y * i64::from(width) + x) * 4) as usize;
                    for c in 0..3 {
                        rgba[at + c] = rgba[at + c].min(ink[c]);
                    }
                }
            }
            pen += i64::from(cw);
        }
    }

    fn record(
        &self,
        state: &mut FormState,
        kind: &'static str,
        pixel: Option<[f32; 2]>,
        text: Option<String>,
    ) {
        let (window, widget) = match pixel {
            Some(p) => (
                self.layout.window_at(p).map(|w| w.title.clone()),
                self.layout.widget_at(p).map(|w| w.id.clone()),
            ),
            None => {
                let widget = state.focused.clone();
                let window = widget
                    .as_deref()
                    .and_then(|id| self.layout.widget(id))
                    .map(|w| w.window.clone());
                (window, widget)
            }
        };
        let governed_by = state.governing.clone();
        state.actions.push(DesktopAction { kind, pixel, window, widget, text, governed_by });
    }

    fn click(&self, pixel: [f32; 2]) {
        let mut state = lock(&self.state);
        self.record(&mut state, "click", Some(pixel), None);
        let Some(widget) = self.layout.widget_at(pixel) else {
            state.focused = None;
            return;
        };
        match (widget.kind.as_str(), widget.id.as_str()) {
            ("input" | "textarea", id) => state.focused = Some(id.to_string()),
            // The notes' Save keeps the note; with no note there is nothing to keep.
            (_, "note-save") => {}
            (_, "save") => {
                state.saved = Some(state.values.clone());
                state.saves += 1;
                state.version += 1;
            }
            (_, "cancel") => {
                state.values.clear();
                state.version += 1;
            }
            _ => {}
        }
    }

    fn type_text(&self, text: &str) {
        let mut state = lock(&self.state);
        self.record(&mut state, "type", None, Some(text.to_string()));
        let focused = state.focused.clone();
        match focused.as_deref().and_then(|id| self.layout.widget(id)).map(|w| w.kind.as_str()) {
            Some("input") => {
                let id = focused.unwrap_or_default();
                state.values.entry(id).or_default().push_str(text);
                state.version += 1;
            }
            Some("textarea") => {
                state.note.push_str(text);
                state.version += 1;
            }
            _ => {}
        }
    }
}

fn fill(rgba: &mut [u8], width: u32, b: BBox) {
    let height = rgba.len() as u32 / 4 / width;
    for y in (b.y0.max(0.0) as u32)..(b.y1.max(0.0) as u32).min(height) {
        for x in (b.x0.max(0.0) as u32)..(b.x1.max(0.0) as u32).min(width) {
            let at = ((y * width + x) * 4) as usize;
            rgba[at..at + 3].copy_from_slice(&[255, 255, 255]);
        }
    }
}

#[async_trait::async_trait]
impl ComputerActuator for Desktop {
    fn display_size(&self) -> Option<(u32, u32)> {
        Some(self.layout.size())
    }

    fn supports(&self, kind: ComputerActionKind) -> bool {
        matches!(
            kind,
            ComputerActionKind::Screenshot | ComputerActionKind::Click | ComputerActionKind::Type
        )
    }

    async fn perform(&self, action: ComputerAction) -> adk_core::Result<ActuationOutput> {
        match action {
            ComputerAction::Screenshot => {
                let png = self.render().map_err(|e| AdkError::tool(e.to_string()))?;
                let shot = Screenshot::png(png).ok_or_else(|| AdkError::tool("not a PNG"))?;
                Ok(ActuationOutput::Image(shot))
            }
            ComputerAction::Click { button: MouseButton::Left, count: 1, at: Some(at), .. } => {
                self.click([at.x as f32 + 0.5, at.y as f32 + 0.5]);
                Ok(ActuationOutput::Done)
            }
            ComputerAction::Type { text } => {
                self.type_text(&text);
                Ok(ActuationOutput::Done)
            }
            other => Err(AdkError::tool(format!("the form desktop does not support {other:?}"))),
        }
    }
}

/// One screenshot as the copilot saw it: OCR regions and each window's marks.
#[derive(Clone)]
struct Observed {
    screenshot: Digest,
    scale: adk_vision::screen::ScreenshotScale,
    marks: BTreeMap<String, MarkSet>,
    regions: Vec<Region>,
}

impl Observed {
    fn regions_in(&self, frame: BBox, size: (u32, u32)) -> RegionSet {
        let inside = self
            .regions
            .iter()
            .filter(|r| r.shape.bbox().is_some_and(|b| frame.contains(b.center())))
            .cloned()
            .collect();
        RegionSet::new(self.screenshot, size.0, size.1, inside)
    }
}

/// What the runtime did, for the scenario's checks.
#[derive(Clone, Debug)]
pub enum ScreenEvent {
    /// A step previewed: its route, and for a pointing step the target it was grounded to.
    Previewed {
        step: String,
        route: &'static str,
        reason: Option<String>,
        target: Option<Box<GroundedTarget>>,
        /// Whether the target evidence binds the screenshot and states points at its ratio.
        evidence_bound: bool,
    },
    /// A step checked against its plan on a fresh screenshot and run, or held.
    Executed {
        step: String,
        action_id: String,
        target: Option<Box<GroundedTarget>>,
        deviation: Option<String>,
        actions: std::ops::Range<usize>,
    },
    /// The verifier's check after a step.
    Verified { step: String, report: Box<VerificationReport> },
}

struct Kept {
    preview: ActionPreview,
    /// The target the step was previewed against, grounded again at each later preview.
    target: Option<PreviewedTarget>,
    app: String,
    step: String,
    window: String,
    plan: (Digest, String),
    args: Value,
    expect_appear: Vec<String>,
    expect_disappear: Vec<String>,
}

struct Pending {
    step: String,
    window: String,
    before: RegionSet,
    expected: ExpectedChange,
}

#[derive(Default)]
struct RuntimeState {
    observed: HashMap<Digest, Observed>,
    plans: HashMap<(Digest, String), ScreenPlan>,
    kept: HashMap<String, Kept>,
    receipts: HashMap<String, ExecutionReceipt>,
    pending: HashMap<String, Pending>,
    /// The last field clicked, which typed text goes into.
    focus: Option<BBox>,
    events: Vec<ScreenEvent>,
}

/// The copilot's `ComputerUseRuntime` over the desktop, under a list of windows it may act in.
pub struct ScreenRuntime {
    desktop: Arc<Desktop>,
    fitted: FittedActuator,
    reader: VisionRuntime,
    allowed: Vec<String>,
    state: Mutex<RuntimeState>,
}

const SESSION: &str = "s14-desktop";
const PRINCIPAL: &str = "ana";

fn decode_error(message: impl Into<String>) -> ComputerUseError {
    ComputerUseError::Decode(message.into())
}

impl ScreenRuntime {
    pub fn new(
        desktop: Arc<Desktop>,
        reader: Arc<dyn DocumentReader>,
        allowed: Vec<String>,
    ) -> Result<Self> {
        let fitted = FittedActuator::for_claude_model(desktop.clone(), "claude-sonnet-5-5");
        let reader = VisionRuntime::builder().reader(reader).build()?;
        Ok(Self { desktop, fitted, reader, allowed, state: Mutex::new(RuntimeState::default()) })
    }

    pub fn events(&self) -> Vec<ScreenEvent> {
        lock(&self.state).events.clone()
    }

    /// Every plan the runtime previewed steps into.
    pub fn plans(&self) -> Vec<ScreenPlan> {
        lock(&self.state).plans.values().cloned().collect()
    }

    /// A fresh screenshot: the fitted copy a model would see (which records the scale), and
    /// the full-resolution capture read on the device into regions and per-window marks.
    async fn observe(&self) -> Result<Observed, ComputerUseError> {
        let fitted = self
            .fitted
            .perform(ComputerAction::Screenshot)
            .await
            .map_err(|e| decode_error(format!("screenshot: {e}")))?;
        let ActuationOutput::Image(_) = fitted else {
            return Err(decode_error("the fitted screenshot is not an image"));
        };
        let scale = self.fitted.scale().ok_or_else(|| decode_error("no screenshot scale"))?;
        let png = self.desktop.render().map_err(|e| decode_error(e.to_string()))?;
        let screenshot = Digest::of(&png);
        if let Some(seen) = lock(&self.state).observed.get(&screenshot) {
            return Ok(seen.clone());
        }
        let image = SourceImage::decode(&png, &DecodeLimits::default())
            .map_err(|e| decode_error(e.to_string()))?;
        let page = self
            .reader
            .read(&VisionCx::new(), &image, &ReadOptions::default())
            .await
            .map_err(|e| decode_error(format!("reading the screenshot: {e}")))?
            .value;
        let layout = &self.desktop.layout;
        let screen = layout.screen();
        let mut marks: BTreeMap<String, MarkSet> = layout
            .windows
            .iter()
            .map(|w| {
                let set = MarkSet::new(screenshot, layout.size(), screen.clone())
                    .with_window(w.title.clone());
                (w.title.clone(), set)
            })
            .collect();
        let mut regions = Vec::new();
        for &index in &page.reading_order {
            let block = &page.blocks[index];
            let Some(b) = block.region.shape.bbox() else { continue };
            let mut region = block.region.clone();
            region.image = screenshot;
            region.label = Some(norm(&block.text));
            regions.push(region);
            if let Some(window) = layout.window_at(b.center())
                && let Some(set) = marks.get_mut(&window.title)
            {
                set.push(
                    b,
                    Some(block.text.clone()),
                    CandidateSource::OcrWord { confidence: block.confidence },
                );
            }
        }
        let seen = Observed { screenshot, scale, marks, regions };
        let mut state = lock(&self.state);
        state.observed.insert(screenshot, seen.clone());
        Ok(seen)
    }

    fn capability(operation: &str) -> ExecutionCapability {
        ExecutionCapability {
            app_id: "*".into(),
            operation: operation.into(),
            backend: "stand-in".into(),
            supported_modes: vec![ExecutionMode::Background],
            interference: "none".into(),
            confidence: 1.0,
            verified_at: None,
            verification_source: "platform_rule".into(),
        }
    }
}

/// The strings of a JSON array.
pub fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl ComputerUseRuntime for ScreenRuntime {
    async fn discover_capabilities(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "operations": ["left_click", "type"], "modes": ["background"] }))
    }

    async fn observe_visual(&self) -> Result<Value, ComputerUseError> {
        let seen = self.observe().await?;
        let layout = &self.desktop.layout;
        let windows: Vec<Value> = layout
            .windows
            .iter()
            .map(|w| {
                json!({
                    "title": w.title,
                    "app": w.app,
                    "frame": w.frame,
                    "allowed": self.allowed.contains(&w.title),
                })
            })
            .collect();
        let marks: Vec<Value> = seen
            .marks
            .iter()
            .flat_map(|(window, set)| {
                set.candidates.iter().map(move |c| {
                    json!({
                        "window": window,
                        "mark": c.mark,
                        "label": c.label,
                        "box": [c.region.x0, c.region.y0, c.region.x1, c.region.y1],
                    })
                })
            })
            .collect();
        Ok(json!({
            "screenshot": seen.screenshot,
            "size": layout.size(),
            "devicePixelRatio": layout.screen().device_pixel_ratio,
            "sent": seen.scale.sent,
            "windows": windows,
            "marks": marks,
        }))
    }

    async fn observe_semantic(&self) -> Result<Value, ComputerUseError> {
        let screen = self.desktop.layout.screen();
        let windows: Vec<Value> = self
            .desktop
            .layout
            .windows
            .iter()
            .map(|w| {
                let p = screen.bbox_to_points(&w.frame());
                json!({ "title": w.title, "app": w.app, "points": [p.x0, p.y0, p.x1, p.y1] })
            })
            .collect();
        Ok(json!({ "windows": windows }))
    }

    async fn preview_action(&self, action: Value) -> Result<ActionPreview, ComputerUseError> {
        let args_digest = Digest::of_json(&action).to_string();
        let again = lock(&self.state).kept.get(&args_digest).map(|kept| {
            (kept.preview.clone(), kept.target.clone(), kept.window.clone(), kept.app.clone())
        });
        if let Some((mut preview, target, window, app)) = again {
            // The same step previewed again: its target grounded on a fresh screenshot, or
            // none when nothing with its label covers the previewed point any more.
            if let Some(shown) = target {
                let fresh = self.observe().await?;
                let now = chrono::Utc::now().to_rfc3339();
                preview.envelope.target = fresh
                    .marks
                    .get(&window)
                    .and_then(|marks| shown.reground(marks).ok())
                    .map(|mut found| {
                        // `reground` matched the label ignoring case and spacing; the
                        // evidence names the label the plan showed.
                        found.label = shown.label.clone();
                        found.target_evidence("stand-in", &app, &fresh.screenshot.to_string(), &now)
                    });
            }
            return Ok(preview);
        }
        let field = |name: &str| action.get(name).and_then(Value::as_str).unwrap_or_default();
        let operation = field("operation").to_string();
        let step = field("step").to_string();
        let claimed = field("window").to_string();
        let screenshot: Digest = field("screenshot")
            .parse()
            .map_err(|_| decode_error("an action names the screenshot it was grounded on"))?;
        let seen = lock(&self.state)
            .observed
            .get(&screenshot)
            .cloned()
            .ok_or_else(|| decode_error("the action's screenshot was not observed here"))?;
        let layout = &self.desktop.layout;

        // A pointing step is grounded on the screenshot the copilot saw, in the pixels of the
        // fitted copy it gave its coordinate in.
        let (target, window, args) = match operation.as_str() {
            "left_click" => {
                let c = action.get("coordinate").and_then(Value::as_array);
                let point = c
                    .and_then(|c| Some([c.first()?.as_f64()? as f32, c.get(1)?.as_f64()? as f32]))
                    .ok_or_else(|| decode_error("left_click needs a coordinate"))?;
                let at = seen.scale.to_source([point[0] + 0.5, point[1] + 0.5]);
                let window = layout.window_at(at).map(|w| w.title.clone()).unwrap_or_default();
                let target = seen.marks.get(&window).cloned().and_then(|mut marks| {
                    let mark = marks.add_model_point(
                        [point[0] + 0.5, point[1] + 0.5],
                        &seen.scale,
                        "copilot",
                    )?;
                    marks.ground(mark).ok()
                });
                (target, window, json!({ "coordinate": [point[0], point[1]] }))
            }
            "type" => (None, claimed.clone(), json!({ "text": field("text") })),
            other => return Err(decode_error(format!("unknown operation {other}"))),
        };

        let mut refusal: Option<(&str, String)> = None;
        if !self.allowed.contains(&window) {
            refusal = Some((
                "window_not_allowed",
                format!(
                    "{} is not one of the windows this task may act in ({})",
                    if window.is_empty() { "the desktop" } else { &window },
                    self.allowed.join(", ")
                ),
            ));
        } else if window != claimed {
            refusal = Some((
                "window_mismatch",
                format!("the action names {claimed} but its point is in {window}"),
            ));
        }
        let plan_key = (screenshot, window.clone());
        if refusal.is_none() {
            let mut state = lock(&self.state);
            let plan = state.plans.entry(plan_key.clone()).or_insert_with(|| {
                let marks = seen.marks.get(&window).cloned().unwrap_or_else(|| {
                    MarkSet::new(screenshot, layout.size(), layout.screen())
                        .with_window(window.clone())
                });
                ScreenPlan::new(&marks).with_max_actions(12)
            });
            if let Err(refused) =
                plan.push(step.clone(), operation.clone(), args.clone(), target.as_ref())
            {
                refusal = Some(("plan_refused", refused.to_string()));
            }
        }

        let app = layout.window(&window).map(|w| w.app.clone()).unwrap_or_default();
        let now = chrono::Utc::now();
        let evidence = target.as_ref().map(|t| {
            t.target_evidence("stand-in", &app, &screenshot.to_string(), &now.to_rfc3339())
        });
        let evidence_bound = evidence.as_ref().is_some_and(|e| {
            e.check_screen_binding().is_ok()
                && e.device_pixel_ratio == Some(2.0)
                && e.screenshot_hash.as_deref() == Some(screenshot.to_string().as_str())
        });
        let appear = strings(action.pointer("/expect/appear").unwrap_or(&Value::Null));
        let disappear = strings(action.pointer("/expect/disappear").unwrap_or(&Value::Null));
        let postcondition = (!appear.is_empty()).then(|| ActionPostcondition::UiElement {
            role: Some("text".into()),
            label: None,
            exists: true,
            value_digest: Some(Digest::of(appear.join("\n").as_bytes()).to_string()),
        });
        let policy_digest = Digest::of_json(&json!({
            "allowedWindows": self.allowed,
            "window": window,
            "screenshot": screenshot,
        }))
        .to_string();
        let preview = ActionPreview {
            envelope: ActionEnvelope {
                action_id: format!("act-{}", &args_digest[args_digest.len() - 16..]),
                session_id: SESSION.into(),
                execution_group_id: None,
                principal_id: PRINCIPAL.into(),
                agent_id: Some("copilot".into()),
                tool: "computer".into(),
                operation: operation.clone(),
                action_class: ActionClass::EditReversible,
                requested_mode: ExecutionMode::Background,
                target: evidence,
                target_sensitivity: None,
                resource: None,
                provenance: None,
                data_labels: vec!["customer_record".into()],
                postcondition,
                reversible: true,
                external_side_effect: false,
                proposed_at: now.to_rfc3339(),
                expires_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
                args_digest: args_digest.clone(),
            },
            executable: false,
            blocker: Some(refusal.as_ref().map_or("approval_required", |r| r.0).to_string()),
            policy: PolicyDecision {
                decision: if refusal.is_some() { "deny" } else { "confirm" }.into(),
                policy_digest,
                reasons: vec![refusal.as_ref().map_or_else(
                    || format!("step {step} of the plan for {window}; a person approves it"),
                    |r| r.1.clone(),
                )],
                grant_id: None,
            },
            capability: Self::capability(&operation),
        };
        let route = if refusal.is_some() { "blocked" } else { "approval" };
        let shown = target.as_ref().map(PreviewedTarget::from);
        let mut state = lock(&self.state);
        state.events.push(ScreenEvent::Previewed {
            step: step.clone(),
            route,
            reason: refusal.map(|r| r.1),
            target: target.map(Box::new),
            evidence_bound,
        });
        state.kept.insert(
            args_digest,
            Kept {
                preview: preview.clone(),
                target: shown,
                app,
                step,
                window,
                plan: plan_key,
                args,
                expect_appear: appear,
                expect_disappear: disappear,
            },
        );
        Ok(preview)
    }

    async fn acquire_lease(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<ControlLease, ComputerUseError> {
        Ok(ControlLease {
            lease_id: format!("lease-{}", envelope.action_id),
            revision: 1,
            session_id: envelope.session_id.clone(),
            principal_id: envelope.principal_id.clone(),
            agent_id: envelope.agent_id.clone(),
            kind: "cooperative".into(),
            execution_mode: envelope.requested_mode,
            state: "active".into(),
            acquired_at: None,
            expires_at: envelope.expires_at.clone(),
            action_budget: 1,
            actions_used: 0,
            boundaries: LeaseBoundaries::default(),
        })
    }

    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        _lease: &ControlLease,
        _approval_grant_id: Option<&str>,
    ) -> Result<ExecutionReceipt, ComputerUseError> {
        if let Some(receipt) = lock(&self.state).receipts.get(&envelope.action_id) {
            return Ok(receipt.clone());
        }
        let (step, window, plan, args, appear, disappear) = {
            let state = lock(&self.state);
            let kept = state
                .kept
                .get(&envelope.args_digest)
                .ok_or_else(|| decode_error("no preview is kept for this action"))?;
            (
                kept.step.clone(),
                kept.window.clone(),
                state.plans.get(&kept.plan).cloned(),
                kept.args.clone(),
                kept.expect_appear.clone(),
                kept.expect_disappear.clone(),
            )
        };
        let plan = plan.ok_or_else(|| decode_error("the action's plan is gone"))?;
        let layout = &self.desktop.layout;
        let frame =
            layout.window(&window).map(Window::frame).unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0));

        // Grounded again on a fresh screenshot and checked against the previewed step (Q8).
        let fresh = self.observe().await?;
        let marks = fresh
            .marks
            .get(&window)
            .cloned()
            .ok_or_else(|| decode_error("the action's window is not on the screen"))?;
        let checked = plan.check_call(&step, &args, &marks);
        let first = self.desktop.actions().len();
        let (status, error, target, deviation) = match checked {
            Err(deviation) => (
                ReceiptStatus::Rejected,
                Some(json!(deviation.to_string())),
                None,
                Some(deviation.to_string()),
            ),
            Ok(target) => {
                self.desktop.govern(Some(envelope.action_id.clone()));
                let acted = match (&target, args.get("text").and_then(Value::as_str)) {
                    (Some(target), _) => {
                        let [x, y] = target.toolset_coordinate(&fresh.scale);
                        lock(&self.state).focus = Some(target.pixels);
                        self.fitted
                            .perform(ComputerAction::Click {
                                button: MouseButton::Left,
                                count: 1,
                                at: Some(ScreenPoint::new(x, y)),
                                modifiers: Vec::new(),
                            })
                            .await
                    }
                    (None, Some(text)) => {
                        self.fitted.perform(ComputerAction::Type { text: text.to_string() }).await
                    }
                    (None, None) => Err(AdkError::tool("a step with no target types no text")),
                };
                self.desktop.govern(None);
                match acted {
                    Ok(_) => (ReceiptStatus::Committed, None, target, None),
                    Err(e) => (ReceiptStatus::Rejected, Some(json!(e.to_string())), target, None),
                }
            }
        };
        let last = self.desktop.actions().len();

        // What the step should change, for the verifier: inside the window, or along the
        // focused field's row for typed text.
        let within = match (operation_of(&args), lock(&self.state).focus) {
            ("type", Some(field)) => {
                BBox::new(field.x0 - 16.0, field.y0 - 16.0, frame.x1, field.y1 + 16.0)
            }
            _ => frame,
        };
        let mut expected = ExpectedChange::none();
        for text in &appear {
            expected = expected.appears(RegionExpectation::label(norm(text)).within(within));
        }
        for text in &disappear {
            expected = expected.disappears(RegionExpectation::label(norm(text)).within(within));
        }
        let receipt = ExecutionReceipt {
            receipt_id: format!("receipt-{}", envelope.action_id),
            session_id: envelope.session_id.clone(),
            action_id: envelope.action_id.clone(),
            action_digest: envelope.args_digest.clone(),
            attempt: 1,
            status,
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            updated_at: None,
            result: Some(json!({ "step": step, "actions": last - first })),
            error,
        };
        let mut state = lock(&self.state);
        state.events.push(ScreenEvent::Executed {
            step: step.clone(),
            action_id: envelope.action_id.clone(),
            target: target.map(Box::new),
            deviation,
            actions: first..last,
        });
        state.pending.insert(
            receipt.receipt_id.clone(),
            Pending {
                step,
                window: window.clone(),
                before: fresh.regions_in(frame, layout.size()),
                expected,
            },
        );
        state.receipts.insert(envelope.action_id.clone(), receipt.clone());
        Ok(receipt)
    }

    async fn verify(
        &self,
        receipt: &ExecutionReceipt,
        _postcondition: Option<&ActionPostcondition>,
    ) -> Result<VerificationOutcome, ComputerUseError> {
        if receipt.status != ReceiptStatus::Committed {
            let reason = receipt.error.as_ref().map_or("not committed".into(), Value::to_string);
            return Ok(VerificationOutcome::Failed { reason });
        }
        let pending = lock(&self.state)
            .pending
            .remove(&receipt.receipt_id)
            .ok_or_else(|| decode_error("nothing to verify for this receipt"))?;
        let layout = &self.desktop.layout;
        let frame = layout
            .window(&pending.window)
            .map(Window::frame)
            .unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0));
        let after = self.observe().await?.regions_in(frame, layout.size());
        let report = check_change(&pending.before, &after, &pending.expected);
        let outcome = if report.passed {
            VerificationOutcome::Verified
        } else {
            let unexpected: Vec<String> =
                report.unexpected.iter().filter_map(|r| r.label.clone()).collect();
            let mut reason = report.missing.join("; ");
            if !unexpected.is_empty() {
                reason.push_str(&format!("; unexpected: {}", unexpected.join(", ")));
            }
            VerificationOutcome::Failed { reason }
        };
        lock(&self.state)
            .events
            .push(ScreenEvent::Verified { step: pending.step, report: Box::new(report) });
        Ok(outcome)
    }
}

fn operation_of(args: &Value) -> &'static str {
    if args.get("text").is_some() { "type" } else { "left_click" }
}
