//! Screen plans (spec 3.7 Q8): up to N previewed desktop actions in one window, approved
//! once, as spec 3.1 §7.26 does for change sets.
//!
//! `computer_use_preview { actions }` previews a list of actions on one observation and
//! groups consecutive steps the runtime holds for approval into plans of up to N actions in
//! one window. The approval binds the plan's digest — its window, steps and previewed
//! targets — and answers its steps one at a time, in order; before each step runs, the
//! reference graph previews it again on a fresh observation and the pack checks the fresh
//! target against the previewed one. A deviation, a step that fails or does not verify, or
//! a step run out of order ends the plan: that step and every later one need a fresh
//! approval of their own. A plan is never a standing rule.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use adk_computer_use::{Bounds, TargetEvidence};
use serde::Serialize;
use serde_json::{Value, json};

use crate::approvals::escape_control;

/// Actions one screen plan holds unless set with
/// [`ComputerUsePack::with_plan_max_actions`](super::ComputerUsePack::with_plan_max_actions)
/// (spec 3.7 Q8).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::computer_use::DEFAULT_PLAN_MAX_ACTIONS, 10);
/// ```
pub const DEFAULT_PLAN_MAX_ACTIONS: usize = 10;

/// Most actions one `computer_use_preview { actions }` call previews.
pub(super) const PREVIEW_MAX_ACTIONS: usize = 64;

/// Most plans kept per process; the oldest is dropped first.
const PLANS_CAP: usize = 256;

/// The window a screen plan acts in: the application, window and display of its steps'
/// target evidence.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::computer_use::PlanWindow;
/// use serde_json::json;
///
/// let window = PlanWindow {
///     app_id: "com.example.forms".into(),
///     window_id: Some(json!("Customer form")),
///     window_title_digest: None,
///     display_id: Some("built-in".into()),
/// };
/// assert_eq!(window.to_string(), "com.example.forms, window \"Customer form\"");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanWindow {
    /// The application or bundle identifier.
    pub app_id: String,
    /// The platform's window identifier, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Value>,
    /// The digest of the window's title, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title_digest: Option<String>,
    /// The display the window is on, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
}

impl PlanWindow {
    /// The window `target` was observed in.
    pub fn of(target: &TargetEvidence) -> Self {
        Self {
            app_id: target.app_id.clone(),
            window_id: target.window_id.clone(),
            window_title_digest: target.window_title_digest.clone(),
            display_id: target.display_id.clone(),
        }
    }
}

impl std::fmt::Display for PlanWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", escape_control(&self.app_id))?;
        match &self.window_id {
            Some(Value::String(id)) => write!(f, ", window \"{}\"", escape_control(id)),
            Some(id) => write!(f, ", window {}", escape_control(&id.to_string())),
            None => Ok(()),
        }
    }
}

/// One step of a [`PreviewedPlan`]: the preview a `computer_use_execute` call runs.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// The runtime's digest of the action's arguments.
    pub action_digest: String,
    /// The runtime's policy digest for the action.
    pub policy_digest: String,
    /// The action summary `computer_use_execute` echoes.
    pub action: Value,
    /// The action as the model proposed it.
    pub proposed: Value,
    /// The target the action was previewed against; a step without one, such as typing,
    /// acts where the step before it put the focus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetEvidence>,
}

impl PlanStep {
    /// The arguments of the `computer_use_execute` call that runs this step.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::packs::computer_use::PlanStep;
    /// use serde_json::json;
    ///
    /// let step = PlanStep {
    ///     action_digest: "a1".into(),
    ///     policy_digest: "p1".into(),
    ///     action: json!({ "operation": "type" }),
    ///     proposed: json!({ "operation": "type", "text": "Ada" }),
    ///     target: None,
    /// };
    /// assert_eq!(
    ///     step.execute_args(),
    ///     json!({ "actionDigest": "a1", "policyDigest": "p1", "action": { "operation": "type" } })
    /// );
    /// ```
    pub fn execute_args(&self) -> Value {
        json!({
            "actionDigest": self.action_digest,
            "policyDigest": self.policy_digest,
            "action": self.action,
        })
    }
}

/// Up to N previewed desktop actions in one window, approved once (spec 3.7 Q8).
///
/// The [`digest`](Self::digest) is the plan's fingerprint: SHA-256 over the canonical JSON
/// of its window and steps, targets included. The one approval binds it, so it answers no
/// step of another plan, another window or another target.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewedPlan {
    /// The plan's fingerprint, `sha256:<hex>`.
    pub digest: String,
    /// The window every step acts in.
    pub window: PlanWindow,
    /// The steps, in the order they run.
    pub steps: Vec<PlanStep>,
}

impl PreviewedPlan {
    pub(super) fn new(window: PlanWindow, steps: Vec<PlanStep>) -> Self {
        let digest = Self::digest_of(&window, &steps);
        Self { digest, window, steps }
    }

    /// The fingerprint of a plan with `window` and `steps`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::packs::computer_use::{PlanStep, PlanWindow, PreviewedPlan};
    /// use serde_json::json;
    ///
    /// let window = PlanWindow { app_id: "forms".into(), window_id: None, window_title_digest: None, display_id: None };
    /// let step = |digest: &str| PlanStep {
    ///     action_digest: digest.into(),
    ///     policy_digest: "p".into(),
    ///     action: json!({}),
    ///     proposed: json!({}),
    ///     target: None,
    /// };
    /// let one = PreviewedPlan::digest_of(&window, &[step("a"), step("b")]);
    /// assert_eq!(one, PreviewedPlan::digest_of(&window, &[step("a"), step("b")]));
    /// assert_ne!(one, PreviewedPlan::digest_of(&window, &[step("b"), step("a")]));
    /// assert!(one.starts_with("sha256:"));
    /// ```
    pub fn digest_of(window: &PlanWindow, steps: &[PlanStep]) -> String {
        let value = json!({ "window": window, "steps": steps });
        // Keys sorted at every level, so the digest does not depend on how a runtime or
        // serde_json's `preserve_order` built the JSON.
        let canonical = adk_core::tool_call_fingerprint("screen_plan", &value);
        format!("sha256:{}", crate::fsutil::hex_digest(canonical.as_bytes()))
    }

    /// The plan as the lines of an approval card: the window, then each step's operation,
    /// target and proposed action, escaped.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{} desktop action(s) in {}\nplan {}",
            self.steps.len(),
            self.window,
            self.digest
        );
        for (i, step) in self.steps.iter().enumerate() {
            let operation = step.action.get("operation").and_then(Value::as_str).unwrap_or("action");
            out.push_str(&format!("\n{}. {}", i + 1, escape_control(operation)));
            match step.target.as_ref().and_then(|target| target.bounds.as_ref()) {
                Some(bounds) => {
                    let p = bounds.to_points();
                    out.push_str(&format!(
                        " at ({:.0}, {:.0})-({:.0}, {:.0}) pt",
                        p.x,
                        p.y,
                        p.x + p.width,
                        p.y + p.height
                    ));
                }
                None if step.target.is_none() => out.push_str(" where the focus is"),
                None => {}
            }
            out.push_str(&format!("\n   {}", escape_control(&step.proposed.to_string())));
        }
        out
    }
}

/// Why a step no longer matches its preview.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Deviation {
    /// `window`, `label`, `missing`, `target` or `wrongTarget`.
    pub(super) kind: &'static str,
    pub(super) reason: String,
}

impl Deviation {
    fn new(kind: &'static str, reason: impl Into<String>) -> Self {
        Self { kind, reason: reason.into() }
    }
}

fn points(bounds: &Bounds) -> String {
    let p = bounds.to_points();
    format!("({:.0}, {:.0})-({:.0}, {:.0}) pt", p.x, p.y, p.x + p.width, p.y + p.height)
}

/// Checks the target a fresh preview grounded a step to against the target the step was
/// previewed and approved with: same window, same label, and each box covers the other's
/// centre.
pub(super) fn deviation(
    planned: Option<&TargetEvidence>,
    fresh: Option<&TargetEvidence>,
) -> Option<Deviation> {
    let (planned, fresh) = match (planned, fresh) {
        (None, None) => return None,
        (None, Some(_)) => {
            return Some(Deviation::new(
                "target",
                "the step was previewed without a target and now has one",
            ));
        }
        (Some(_), None) => {
            return Some(Deviation::new("missing", "the previewed target is no longer on the screen"));
        }
        (Some(planned), Some(fresh)) => (planned, fresh),
    };
    let (expected, found) = (PlanWindow::of(planned), PlanWindow::of(fresh));
    if expected != found {
        return Some(Deviation::new(
            "window",
            format!("the target is in {found}, not in {expected}, the plan's window"),
        ));
    }
    if planned.label_digest != fresh.label_digest {
        return Some(Deviation::new("label", "the target's label differs from the previewed one"));
    }
    match (&planned.bounds, &fresh.bounds) {
        (None, None) => None,
        (Some(expected), Some(found)) => {
            let (e, f) = (expected.to_points(), found.to_points());
            (!(f.contains(e.center()) && e.contains(f.center()))).then(|| {
                Deviation::new(
                    "wrongTarget",
                    format!(
                        "the target is at {}, not at the previewed {}",
                        points(found),
                        points(expected)
                    ),
                )
            })
        }
        (Some(_), None) | (None, Some(_)) => {
            Some(Deviation::new("wrongTarget", "the target's bounds differ from the previewed ones"))
        }
    }
}

/// Groups previewed actions into plans: consecutive steps the runtime holds for approval,
/// in one window, at most `max` at a time. A step with a target starts a plan or joins the
/// one before it in the same window; a step without one joins the plan before it; a step
/// that cannot join stands alone, and a step the runtime allows or blocks ends the plan
/// before it. Plans of one step are dropped: a lone step is approved on its own.
///
/// Returns the plans, and for each step its (plan, step) position when it is in one.
pub(super) fn group(
    steps: Vec<(PlanStep, bool)>,
    max: usize,
) -> (Vec<PreviewedPlan>, Vec<Option<(usize, usize)>>) {
    let max = max.max(2);
    let total = steps.len();
    let mut runs: Vec<(PlanWindow, Vec<(usize, PlanStep)>)> = Vec::new();
    let mut open: Option<(PlanWindow, Vec<(usize, PlanStep)>)> = None;
    for (index, (step, held)) in steps.into_iter().enumerate() {
        if !held {
            runs.extend(open.take());
            continue;
        }
        let window = step.target.as_ref().map(PlanWindow::of);
        match (&mut open, window) {
            (Some((current, members)), Some(window))
                if *current == window && members.len() < max =>
            {
                members.push((index, step));
            }
            (Some((_, members)), None) if members.len() < max => members.push((index, step)),
            (_, Some(window)) => {
                runs.extend(open.take());
                open = Some((window, vec![(index, step)]));
            }
            (_, None) => runs.extend(open.take()),
        }
    }
    runs.extend(open);

    let mut plans = Vec::new();
    let mut positions = HashMap::new();
    for (window, members) in runs.into_iter().filter(|(_, members)| members.len() > 1) {
        let at = plans.len();
        for (position, (index, _)) in members.iter().enumerate() {
            positions.insert(*index, (at, position));
        }
        plans.push(PreviewedPlan::new(window, members.into_iter().map(|(_, s)| s).collect()));
    }
    let positions = (0..total).map(|index| positions.get(&index).copied()).collect();
    (plans, positions)
}

/// Where a plan stands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlanState {
    /// Previewed; no one decided it yet.
    Previewed,
    /// A person approved it: the handler answered its steps before `answered`, and the
    /// steps before `ran` ran and verified.
    Approved { answered: usize, ran: usize },
    /// A person denied it.
    Denied { message: Option<String> },
    /// Its approval covers no further step.
    Ended { reason: String },
}

/// What the handler sees of the plan a held `computer_use_execute` belongs to.
#[derive(Clone, Debug)]
pub(crate) struct Membership {
    pub(crate) plan: PreviewedPlan,
    pub(crate) state: PlanState,
    pub(crate) instance: u64,
}

#[derive(Debug)]
struct Record {
    plan: PreviewedPlan,
    state: PlanState,
    instance: u64,
}

#[derive(Debug, Default)]
struct Book {
    plans: HashMap<(String, String), Record>,
    /// The plan each kept action digest last joined.
    steps: HashMap<(String, String), String>,
    order: VecDeque<(String, String)>,
    next: u64,
}

/// The screen plans of a process, shared by the pack's tools and the harness's prompting
/// handler, keyed by (app, user, session).
#[derive(Debug, Default)]
pub(crate) struct ScreenPlans {
    book: Mutex<Book>,
}

/// The key that scopes plans to one session of one user of one app.
pub(crate) fn scope(app: &str, user: &str, session: &str) -> String {
    format!("{app}\u{1f}{user}\u{1f}{session}")
}

impl ScreenPlans {
    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps `plan`, previewed now: a plan previewed again starts over and needs a new
    /// approval.
    pub(super) fn keep(&self, scope: &str, plan: PreviewedPlan) {
        let mut book = self.book();
        book.next += 1;
        let instance = book.next;
        let key = (scope.to_string(), plan.digest.clone());
        for step in &plan.steps {
            book.steps.insert((scope.to_string(), step.action_digest.clone()), plan.digest.clone());
        }
        if book.plans.insert(key.clone(), Record { plan, state: PlanState::Previewed, instance }).is_none() {
            book.order.push_back(key);
        }
        while book.order.len() > PLANS_CAP {
            let Some(oldest) = book.order.pop_front() else { break };
            if let Some(record) = book.plans.remove(&oldest) {
                for step in record.plan.steps {
                    let key = (oldest.0.clone(), step.action_digest);
                    if book.steps.get(&key) == Some(&oldest.1) {
                        book.steps.remove(&key);
                    }
                }
            }
        }
    }

    /// The plan the kept action `action_digest` last joined.
    pub(crate) fn member(&self, scope: &str, action_digest: &str) -> Option<Membership> {
        let book = self.book();
        let digest = book.steps.get(&(scope.to_string(), action_digest.to_string()))?;
        let record = book.plans.get(&(scope.to_string(), digest.clone()))?;
        Some(Membership {
            plan: record.plan.clone(),
            state: record.state.clone(),
            instance: record.instance,
        })
    }

    /// Sets the state of the plan `digest`, when it is still the instance a decision was
    /// taken on.
    pub(crate) fn settle(&self, scope: &str, digest: &str, instance: u64, state: PlanState) -> bool {
        let mut book = self.book();
        match book.plans.get_mut(&(scope.to_string(), digest.to_string())) {
            Some(record) if record.instance == instance => {
                record.state = state;
                true
            }
            _ => false,
        }
    }

    /// Takes the approval of step `step` of the plan `digest` for the handler: the plan is
    /// approved and `step` is the next step it has not answered.
    pub(crate) fn answer(&self, scope: &str, digest: &str, step: usize) -> bool {
        let mut book = self.book();
        match book.plans.get_mut(&(scope.to_string(), digest.to_string())) {
            Some(Record { state: PlanState::Approved { answered, .. }, plan, .. })
                if *answered == step && step < plan.steps.len() =>
            {
                *answered += 1;
                true
            }
            _ => false,
        }
    }

    /// Ends the plan `digest`: its approval covers no further step.
    pub(crate) fn end(&self, scope: &str, digest: &str, reason: impl Into<String>) {
        let mut book = self.book();
        if let Some(record) = book.plans.get_mut(&(scope.to_string(), digest.to_string()))
            && matches!(record.state, PlanState::Approved { .. } | PlanState::Previewed)
        {
            record.state = PlanState::Ended { reason: reason.into() };
        }
    }

    /// Starts step `step` of the plan `digest` under its approval: the plan is approved,
    /// the steps before `step` ran, and `step` is the action of `args`. Returns the step.
    ///
    /// # Errors
    ///
    /// Why the plan's approval does not cover the call; a step run out of order ends the
    /// plan.
    pub(super) fn begin(
        &self,
        scope: &str,
        digest: &str,
        step: usize,
        args: &Value,
    ) -> Result<PlanStep, String> {
        let mut book = self.book();
        let Some(record) = book.plans.get_mut(&(scope.to_string(), digest.to_string())) else {
            return Err(format!("no plan {digest} is kept for this session in this process"));
        };
        let ran = match &record.state {
            PlanState::Approved { ran, .. } => *ran,
            PlanState::Previewed => return Err(format!("plan {digest} was not approved")),
            PlanState::Denied { .. } => return Err(format!("plan {digest} was denied")),
            PlanState::Ended { reason } => {
                return Err(format!("the approval of plan {digest} ended: {reason}"));
            }
        };
        let Some(planned) = record.plan.steps.get(step) else {
            return Err(format!("plan {digest} has no step {}", step + 1));
        };
        let mut bare = args.clone();
        if let Value::Object(map) = &mut bare {
            map.remove("plan");
        }
        if bare != planned.execute_args() {
            return Err(format!("the call is not step {} of plan {digest}", step + 1));
        }
        if step != ran {
            let reason = format!(
                "step {} was asked to run before step {} of the plan ran",
                step + 1,
                ran + 1
            );
            record.state = PlanState::Ended { reason: reason.clone() };
            return Err(reason);
        }
        Ok(planned.clone())
    }

    /// Records how step `step` of the plan `digest` ended: a step that ran and verified
    /// lets the next one run; any other outcome ends the plan.
    pub(super) fn finish(&self, scope: &str, digest: &str, step: usize, verified: bool) {
        let mut book = self.book();
        let Some(record) = book.plans.get_mut(&(scope.to_string(), digest.to_string())) else {
            return;
        };
        match &mut record.state {
            PlanState::Approved { ran, .. } if *ran == step && verified => *ran += 1,
            PlanState::Approved { .. } => {
                record.state = PlanState::Ended {
                    reason: format!("step {} did not run and verify as previewed", step + 1),
                };
            }
            PlanState::Previewed | PlanState::Denied { .. } | PlanState::Ended { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(window: &str, label: &str, x: f64) -> TargetEvidence {
        TargetEvidence {
            platform: "test".into(),
            app_id: "forms".into(),
            pid: None,
            window_id: Some(json!(window)),
            window_title_digest: None,
            display_id: Some("built-in".into()),
            role: None,
            label_digest: Some(format!("sha256:{label}")),
            bounds: Some(Bounds::points(x, 100.0, 90.0, 40.0, 2.0)),
            observation_id: "o1".into(),
            screenshot_hash: Some("sha256:shot".into()),
            device_pixel_ratio: Some(2.0),
            ui_tree_revision: None,
            confidence: 1.0,
            captured_at: "2026-11-23T10:00:00Z".into(),
        }
    }

    fn step(digest: &str, target: Option<TargetEvidence>) -> PlanStep {
        PlanStep {
            action_digest: digest.into(),
            policy_digest: "p".into(),
            action: json!({ "operation": "left_click" }),
            proposed: json!({ "digest": digest }),
            target,
        }
    }

    #[test]
    fn a_moved_or_relabelled_target_deviates_and_a_fresh_capture_does_not() {
        let planned = target("Form", "save", 960.0);
        let mut fresh = planned.clone();
        fresh.observation_id = "o2".into();
        fresh.screenshot_hash = Some("sha256:later".into());
        fresh.bounds = Some(Bounds::points(962.0, 101.0, 90.0, 40.0, 2.0));
        assert_eq!(deviation(Some(&planned), Some(&fresh)), None);

        let moved = target("Form", "save", 40.0);
        assert_eq!(deviation(Some(&planned), Some(&moved)).unwrap().kind, "wrongTarget");
        let relabelled = target("Form", "cancel", 960.0);
        assert_eq!(deviation(Some(&planned), Some(&relabelled)).unwrap().kind, "label");
        let elsewhere = target("Mail", "save", 960.0);
        assert_eq!(deviation(Some(&planned), Some(&elsewhere)).unwrap().kind, "window");
        assert_eq!(deviation(Some(&planned), None).unwrap().kind, "missing");
        assert_eq!(deviation(None, None), None);
    }

    #[test]
    fn steps_group_into_plans_of_at_most_n_in_one_window() {
        let form = |d: &str| (step(d, Some(target("Form", d, 100.0))), true);
        let typing = |d: &str| (step(d, None), true);
        let mail = |d: &str| (step(d, Some(target("Mail", d, 100.0))), true);
        let blocked = |d: &str| (step(d, Some(target("Mail", d, 100.0))), false);

        let (plans, at) = group(
            vec![
                form("a"),
                typing("b"),
                form("c"),
                typing("d"),
                form("e"),
                typing("f"),
                blocked("m"),
            ],
            4,
        );
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].steps.len(), 4);
        assert_eq!(plans[1].steps.len(), 2);
        let expected =
            [Some((0, 0)), Some((0, 1)), Some((0, 2)), Some((0, 3)), Some((1, 0)), Some((1, 1)), None];
        assert_eq!(at, expected);

        // A change of window starts a new plan; a lone step forms none.
        let (plans, at) = group(vec![form("a"), typing("b"), mail("c"), form("d")], 10);
        assert_eq!(plans.len(), 1);
        assert_eq!(at, vec![Some((0, 0)), Some((0, 1)), None, None]);

        // A plan never starts with a step that has no target.
        let (plans, _) = group(vec![typing("a"), typing("b")], 10);
        assert!(plans.is_empty());
    }

    #[test]
    fn an_approved_plan_answers_each_step_once_in_order_and_ends_on_failure() {
        let plans = ScreenPlans::default();
        let plan = PreviewedPlan::new(
            PlanWindow::of(&target("Form", "x", 1.0)),
            vec![step("a", Some(target("Form", "a", 1.0))), step("b", None), step("c", None)],
        );
        let digest = plan.digest.clone();
        plans.keep("s", plan.clone());
        let member = plans.member("s", "b").unwrap();
        assert_eq!(member.state, PlanState::Previewed);
        assert!(!plans.answer("s", &digest, 0), "not approved yet");
        assert!(plans.settle(
            "s",
            &digest,
            member.instance,
            PlanState::Approved { answered: 0, ran: 0 }
        ));
        assert!(!plans.answer("s", &digest, 1), "out of order");
        assert!(plans.answer("s", &digest, 0));
        assert!(!plans.answer("s", &digest, 0), "taken once");
        assert!(plans.member("t", "b").is_none(), "another session");

        let args = plan.steps[0].execute_args();
        assert!(plans.begin("s", &digest, 0, &args).is_ok());
        plans.finish("s", &digest, 0, true);
        assert!(plans.begin("s", &digest, 2, &plan.steps[2].execute_args()).is_err());
        assert!(matches!(plans.member("s", "a").unwrap().state, PlanState::Ended { .. }));

        // Previewed again: a new instance, so the old decision cannot settle it.
        plans.keep("s", plan);
        let again = plans.member("s", "a").unwrap();
        assert_eq!(again.state, PlanState::Previewed);
        assert!(!plans.settle("s", &digest, member.instance, PlanState::Denied { message: None }));
    }
}
