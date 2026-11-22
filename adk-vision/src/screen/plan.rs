//! Previewed screen plans: up to N actions in one window, approved once, each action
//! grounded again and checked against its previewed target before it runs (spec 3.7 Q8,
//! S14).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::fit::ScreenshotScale;
use super::marks::{GroundedTarget, MarkSet, same_label};
use crate::digest::Digest;
use crate::geometry::BBox;

/// What an approved action expects to hit: the previewed target's label, region and click
/// point in logical points, its window and the screenshot it was grounded on.
///
/// # Example
///
/// ```
/// use adk_vision::{BBox, Digest, ScreenSpace};
/// use adk_vision::screen::{CandidateSource, MarkSet, PreviewedTarget};
///
/// let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
/// let ocr = CandidateSource::OcrWord { confidence: None };
/// let mut before = MarkSet::new(Digest::of(b"before"), (3024, 1964), screen.clone());
/// let save = before.push(BBox::new(1920.0, 1180.0, 2100.0, 1260.0), Some("Save".into()), ocr.clone()).unwrap();
/// let preview = PreviewedTarget::from(&before.ground(save).unwrap());
///
/// // A fresh screenshot with two "Save" buttons: the one under the previewed point is chosen.
/// let mut after = MarkSet::new(Digest::of(b"after"), (3024, 1964), screen);
/// after.push(BBox::new(40.0, 120.0, 220.0, 200.0), Some("Save".into()), ocr.clone());
/// after.push(BBox::new(1920.0, 1180.0, 2100.0, 1260.0), Some("Save".into()), ocr);
/// assert_eq!(preview.reground(&after).unwrap().mark, 2);
/// assert!(preview.check(&after, 1).is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewedTarget {
    /// The screenshot the preview was grounded on.
    pub screenshot: Digest,
    /// The window, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The target's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its region in logical points.
    pub points: BBox,
    /// The click point in logical points.
    pub click: [f32; 2],
}

impl From<&GroundedTarget> for PreviewedTarget {
    fn from(target: &GroundedTarget) -> Self {
        Self {
            screenshot: target.screenshot,
            window: target.window.clone(),
            label: target.label.clone(),
            points: target.points,
            click: target.click,
        }
    }
}

/// Why an action no longer matches its preview; the action is held again.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Deviation {
    /// The fresh screenshot shows another window than the plan's.
    Window {
        /// The plan's window.
        expected: Option<String>,
        /// The window now.
        found: Option<String>,
    },
    /// No candidate with the previewed label covers the previewed click point.
    Missing {
        /// The previewed label.
        label: Option<String>,
    },
    /// The candidate about to be acted on is not the previewed target.
    WrongTarget {
        /// The previewed region, in points.
        expected: BBox,
        /// The candidate's region, in points.
        found: BBox,
        /// The candidate's label.
        label: Option<String>,
    },
    /// The call's arguments differ from the previewed ones.
    ArgumentsChanged {
        /// The call's ID.
        call_id: String,
    },
    /// The call is not a step of the approved plan.
    NotInPlan {
        /// The call's ID.
        call_id: String,
    },
}

impl fmt::Display for Deviation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Window { expected, found } => write!(
                f,
                "the screen shows window {} but the plan was approved for {}; the action is held again",
                found.as_deref().unwrap_or("(unknown)"),
                expected.as_deref().unwrap_or("(unknown)")
            ),
            Self::Missing { label } => write!(
                f,
                "no {} is at the previewed point any more; the action is held again",
                label.as_deref().map_or("target".to_string(), |l| format!("\"{l}\""))
            ),
            Self::WrongTarget { expected, found, label } => write!(
                f,
                "the action would hit {} at ({:.0}, {:.0})-({:.0}, {:.0}) pt, not the previewed \
                 target at ({:.0}, {:.0})-({:.0}, {:.0}) pt; ground it again",
                label.as_deref().map_or("a target".to_string(), |l| format!("\"{l}\"")),
                found.x0,
                found.y0,
                found.x1,
                found.y1,
                expected.x0,
                expected.y0,
                expected.x1,
                expected.y1
            ),
            Self::ArgumentsChanged { call_id } => write!(
                f,
                "call {call_id} changed its arguments after the plan was approved; it is held again"
            ),
            Self::NotInPlan { call_id } => {
                write!(f, "call {call_id} is not in the approved plan; it is held on its own")
            }
        }
    }
}

impl std::error::Error for Deviation {}

impl PreviewedTarget {
    fn window_matches(&self, fresh: &MarkSet) -> Result<(), Deviation> {
        match (&self.window, &fresh.window) {
            (Some(expected), Some(found)) if expected != found => Err(Deviation::Window {
                expected: Some(expected.clone()),
                found: Some(found.clone()),
            }),
            _ => Ok(()),
        }
    }

    /// Checks the candidate an executor is about to act on, on a fresh screenshot, against
    /// the preview: same window, same label, and the candidate's region covers the
    /// previewed click point while the previewed region covers the candidate's.
    ///
    /// # Errors
    ///
    /// The [`Deviation`] found; a mark that is not on `fresh` is [`Deviation::Missing`].
    pub fn check(&self, fresh: &MarkSet, mark: u32) -> Result<GroundedTarget, Deviation> {
        self.window_matches(fresh)?;
        let candidate =
            fresh.candidate(mark).ok_or(Deviation::Missing { label: self.label.clone() })?;
        let target = fresh.target_of(candidate);
        if same_label(target.label.as_deref(), self.label.as_deref())
            && target.points.contains(self.click)
            && self.points.contains(target.click)
        {
            Ok(target)
        } else {
            Err(Deviation::WrongTarget {
                expected: self.points,
                found: target.points,
                label: target.label,
            })
        }
    }

    /// Grounds the preview again on a fresh screenshot: the candidate with the previewed
    /// label whose region covers the previewed click point, the smallest when several do.
    /// A preview without a label is checked by window only and keeps its click point.
    ///
    /// # Errors
    ///
    /// [`Deviation::Window`] for another window, [`Deviation::Missing`] when no candidate
    /// with the label covers the point.
    pub fn reground(&self, fresh: &MarkSet) -> Result<GroundedTarget, Deviation> {
        self.window_matches(fresh)?;
        fresh
            .candidates
            .iter()
            .filter(|c| {
                self.label.is_none() || same_label(c.label.as_deref(), self.label.as_deref())
            })
            .filter(|c| fresh.screen.bbox_to_points(&c.region).contains(self.click))
            .min_by(|a, b| a.region.area().total_cmp(&b.region.area()))
            .map(|c| fresh.target_of(c))
            .ok_or(Deviation::Missing { label: self.label.clone() })
    }
}

/// Why a step cannot join a plan.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PlanRefusal {
    /// The plan already holds its maximum number of actions.
    TooManyActions {
        /// The maximum.
        max: usize,
    },
    /// The step's target is in another window than the plan's.
    OtherWindow {
        /// The plan's window.
        plan: Option<String>,
        /// The step's window.
        step: Option<String>,
    },
    /// The step's target was grounded on another screenshot than the plan's.
    OtherScreenshot,
}

impl fmt::Display for PlanRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyActions { max } => {
                write!(
                    f,
                    "a screen plan holds at most {max} actions; preview the rest as a new plan"
                )
            }
            Self::OtherWindow { plan, step } => write!(
                f,
                "a screen plan stays in one window: this step is in {}, the plan in {}",
                step.as_deref().unwrap_or("(unknown)"),
                plan.as_deref().unwrap_or("(unknown)")
            ),
            Self::OtherScreenshot => {
                write!(f, "every step of a screen plan is grounded on the plan's screenshot")
            }
        }
    }
}

impl std::error::Error for PlanRefusal {}

/// One step of a [`ScreenPlan`]: a call, its arguments and the target it was previewed
/// against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// The call's ID.
    pub call_id: String,
    /// The tool or toolset member, such as `computer.left_click`.
    pub action: String,
    /// The call's arguments as previewed.
    pub args: Value,
    /// The previewed target, for an action that points at something.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PreviewedTarget>,
}

/// Up to N actions in one window, previewed on one screenshot and approved once (spec 3.7
/// Q8). The approval binds the plan's digest; each step is grounded again on a fresh
/// screenshot and checked against its previewed target before it runs, and any deviation
/// holds it again. A plan is never a standing rule.
///
/// # Example
///
/// ```
/// use adk_vision::{BBox, Digest, ScreenSpace};
/// use adk_vision::screen::{CandidateSource, MarkSet, ScreenPlan};
/// use serde_json::json;
///
/// let ocr = CandidateSource::OcrWord { confidence: None };
/// let mut marks = MarkSet::new(Digest::of(b"form"), (3024, 1964), ScreenSpace::new("built-in", [0.0, 0.0], 2.0))
///     .with_window("Customer form");
/// let name = marks.push(BBox::new(400.0, 300.0, 1300.0, 380.0), Some("Name".into()), ocr.clone()).unwrap();
/// let save = marks.push(BBox::new(1920.0, 1180.0, 2100.0, 1260.0), Some("Save".into()), ocr).unwrap();
///
/// let mut plan = ScreenPlan::new(&marks);
/// plan.push("c1", "computer.left_click", json!({ "mark": name }), Some(&marks.ground(name).unwrap())).unwrap();
/// plan.push("c2", "computer.type", json!({ "text": "Ada Lovelace" }), None).unwrap();
/// plan.push("c3", "computer.left_click", json!({ "mark": save }), Some(&marks.ground(save).unwrap())).unwrap();
/// assert_eq!(plan.steps.len(), 3);
/// // The same steps always give the same digest, which the one approval binds.
/// assert_eq!(plan.digest(), plan.clone().digest());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenPlan {
    /// The screenshot every step was grounded on.
    pub screenshot: Digest,
    /// The one window the plan acts in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The most actions the plan holds.
    pub max_actions: usize,
    /// The steps, in call order.
    pub steps: Vec<PlanStep>,
}

impl ScreenPlan {
    /// Actions one plan holds unless set otherwise.
    pub const DEFAULT_MAX_ACTIONS: usize = 10;

    /// An empty plan on the screenshot and window of `marks`.
    pub fn new(marks: &MarkSet) -> Self {
        Self {
            screenshot: marks.screenshot,
            window: marks.window.clone(),
            max_actions: Self::DEFAULT_MAX_ACTIONS,
            steps: Vec::new(),
        }
    }

    /// Sets the most actions the plan holds.
    #[must_use]
    pub fn with_max_actions(mut self, max: usize) -> Self {
        self.max_actions = max.max(1);
        self
    }

    /// Appends a step.
    ///
    /// # Errors
    ///
    /// The [`PlanRefusal`] when the plan is full or the target is in another window or on
    /// another screenshot.
    pub fn push(
        &mut self,
        call_id: impl Into<String>,
        action: impl Into<String>,
        args: Value,
        target: Option<&GroundedTarget>,
    ) -> Result<(), PlanRefusal> {
        if self.steps.len() >= self.max_actions {
            return Err(PlanRefusal::TooManyActions { max: self.max_actions });
        }
        if let Some(target) = target {
            if target.window != self.window {
                return Err(PlanRefusal::OtherWindow {
                    plan: self.window.clone(),
                    step: target.window.clone(),
                });
            }
            if target.screenshot != self.screenshot {
                return Err(PlanRefusal::OtherScreenshot);
            }
        }
        self.steps.push(PlanStep {
            call_id: call_id.into(),
            action: action.into(),
            args,
            target: target.map(PreviewedTarget::from),
        });
        Ok(())
    }

    /// Previews Claude computer use toolset calls on `marks`: each `coordinate` (in the
    /// pixels of the copy Claude received under `scale`) is grounded to the smallest
    /// candidate that covers it.
    ///
    /// # Errors
    ///
    /// The [`PlanRefusal`] of the first step that cannot join.
    pub fn from_toolset_calls(
        marks: &MarkSet,
        scale: &ScreenshotScale,
        calls: &[(String, String, Value)],
    ) -> Result<Self, PlanRefusal> {
        let mut plan = Self::new(marks);
        for (call_id, member, args) in calls {
            let target = args
                .get("coordinate")
                .and_then(Value::as_array)
                .and_then(|pair| {
                    Some([pair.first()?.as_f64()? as f32, pair.get(1)?.as_f64()? as f32])
                })
                .and_then(|point| {
                    let at = scale.to_source([point[0] + 0.5, point[1] + 0.5]);
                    marks
                        .candidates
                        .iter()
                        .filter(|c| c.region.contains(at))
                        .min_by(|a, b| a.region.area().total_cmp(&b.region.area()))
                        .map(|c| marks.target_of(c))
                });
            plan.push(call_id.clone(), member.clone(), args.clone(), target.as_ref())?;
        }
        Ok(plan)
    }

    /// The plan's identity, which the one approval binds: the digest of its canonical JSON,
    /// keys sorted.
    pub fn digest(&self) -> Digest {
        Digest::of_json(self)
    }

    /// The step for `call_id`.
    pub fn step(&self, call_id: &str) -> Option<&PlanStep> {
        self.steps.iter().find(|step| step.call_id == call_id)
    }

    /// The arguments a step is approved with: its own, with `expect_target` naming the plan
    /// digest and the previewed target, so the call's fingerprint binds what was shown.
    pub fn approved_args(&self, call_id: &str) -> Option<Value> {
        let step = self.step(call_id)?;
        let mut args = step.args.clone();
        if let Value::Object(map) = &mut args {
            map.insert(
                "expect_target".into(),
                json!({ "plan": self.digest(), "target": step.target }),
            );
        }
        Some(args)
    }

    /// Checks a call before it runs: it is a step of the plan with the previewed
    /// arguments, and — for a step with a target — the target grounded again on `fresh`
    /// is the previewed one.
    ///
    /// # Errors
    ///
    /// The [`Deviation`] found; the call is held again.
    pub fn check_call(
        &self,
        call_id: &str,
        args: &Value,
        fresh: &MarkSet,
    ) -> Result<Option<GroundedTarget>, Deviation> {
        let step = self.step(call_id).ok_or(Deviation::NotInPlan { call_id: call_id.into() })?;
        let mut bare = args.clone();
        if let Value::Object(map) = &mut bare {
            map.remove("expect_target");
        }
        if bare != step.args {
            return Err(Deviation::ArgumentsChanged { call_id: call_id.into() });
        }
        if let (Some(expected), Some(found)) = (&self.window, &fresh.window)
            && expected != found
        {
            return Err(Deviation::Window {
                expected: Some(expected.clone()),
                found: Some(found.clone()),
            });
        }
        step.target.as_ref().map(|target| target.reground(fresh)).transpose()
    }

    /// The plan as the lines of an approval card.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{} action(s) in {} on screenshot {}:",
            self.steps.len(),
            self.window.as_deref().unwrap_or("the current window"),
            self.screenshot
        );
        for (i, step) in self.steps.iter().enumerate() {
            out.push_str(&format!("\n{}. {}", i + 1, step.action));
            if let Some(target) = &step.target {
                out.push_str(&format!(
                    " on {} at ({:.0}, {:.0}) pt",
                    target.label.as_deref().map_or("a point".to_string(), |l| format!("\"{l}\"")),
                    target.click[0],
                    target.click[1]
                ));
            } else if let Some(text) = step.args.get("text").and_then(Value::as_str) {
                out.push_str(&format!(" \"{text}\""));
            }
        }
        out
    }
}
