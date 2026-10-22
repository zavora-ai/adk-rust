//! The two kinds of `ultracode` reviewer (spec 3.1 §7.10, §9.2 I61): an independent
//! reviewer — a fresh ReadOnly, shell-less harness per focus — and a Blackboard board of
//! ReadOnly members of a ReadOnly, shell-less harness, run as a governed runner root.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{BlackboardHistoryPolicy, BlackboardPolicy, TeamBudget};
use adk_core::audit_log::AuditRecorder;
use adk_core::{Content, Llm, RunBudget, SessionId, UserId};
use adk_devtools::Workspace;
use adk_harness::{
    EffortPolicy, HarnessAgent, HarnessMode, MEMBER_TIMEOUT, MemberKind, MemberSpec, MemberTools,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use super::route::Focus;
use crate::console::ConsoleIo;
use crate::harness::HarnessLauncher;

const APP_NAME: &str = "adk-rust";
/// The name of the board's harness and of its review round.
const BOARD_NAME: &str = "ultracode_board";
/// The longest a board may run.
const BOARD_WALL_TIME: Duration = Duration::from_secs(20 * 60);
/// What each board seat is told about the board, after its review instruction.
pub(crate) const SEAT_SUFFIX: &str = "Other reviewers' notes earlier in this conversation are \
     context; do not repeat a point already made; end with your own VERDICT line.";

/// One reviewer's verdict (spec 3.1 §9.2 I63): only `Approve` approves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// `VERDICT: approve`.
    Approve,
    /// `VERDICT: changes`.
    Changes,
    /// No verdict: the reviewer failed, never spoke, or wrote no VERDICT line.
    Failed,
}

impl Verdict {
    /// The verdict as records name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Changes => "changes",
            Self::Failed => "failed",
        }
    }
}

/// The verdict of a reviewer's text and its notes: the last `VERDICT:` line decides; text
/// without one is [`Verdict::Failed`]. Notes are the lines after the first line naming the
/// verdict, or the whole text, at most 180 characters.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::{Verdict, parse_verdict};
///
/// assert_eq!(parse_verdict("Looks right.\nVERDICT: approve").0, Verdict::Approve);
/// let (verdict, notes) = parse_verdict("VERDICT: changes\n- handle the empty input");
/// assert_eq!((verdict, notes.as_str()), (Verdict::Changes, "- handle the empty input"));
/// assert_eq!(parse_verdict("I could not finish.").0, Verdict::Failed);
/// ```
pub fn parse_verdict(text: &str) -> (Verdict, String) {
    let verdict = text
        .lines()
        .rev()
        .find_map(|line| {
            let line: String =
                line.to_lowercase().chars().filter(|c| !c.is_whitespace() && *c != '*').collect();
            if line.contains("verdict:approve") {
                Some(Verdict::Approve)
            } else if line.contains("verdict:changes") {
                Some(Verdict::Changes)
            } else {
                None
            }
        })
        .unwrap_or(Verdict::Failed);
    if verdict == Verdict::Approve {
        return (verdict, String::new());
    }
    let notes = text
        .lines()
        .skip_while(|line| !line.to_lowercase().contains("verdict"))
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let notes = if notes.is_empty() { text.trim().to_string() } else { notes };
    (verdict, notes.chars().take(180).collect())
}

/// One reviewer run on its own: what it wrote and the model calls it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewerRun {
    /// The reviewer's text, or `review failed: ...` when it could not run.
    pub text: String,
    /// Its model calls.
    pub model_calls: u64,
}

/// An independent reviewer (spec 3.1 §7.10 graph architecture, §9.2 I61): a fresh harness
/// per review in [`HarnessMode::ReadOnly`] with no shell, under a `RunBudget` of its model
/// calls and 15 minutes, recording into the run's audit log. Each of its model calls carries
/// the [`effort`](Self::effort) policy's `read_only` level (spec 3.1 §7.23).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_cli::harness::ultracode::{Focus, IndependentReviewer};
/// use adk_harness::EffortPolicy;
///
/// # async fn review(model: Arc<dyn adk_core::Llm>) {
/// let reviewer =
///     IndependentReviewer::new(model, ".").effort(EffortPolicy::default()).model_calls(20);
/// let run = reviewer.run(Focus::Style.name(), Focus::Style.desc(), "Review the change now.").await;
/// println!("{} ({} model calls)", run.text, run.model_calls);
/// # }
/// ```
#[derive(Clone)]
pub struct IndependentReviewer {
    model: Arc<dyn Llm>,
    workspace: PathBuf,
    effort: EffortPolicy,
    guidance: Option<String>,
    audit: Option<Arc<dyn AuditRecorder>>,
    user: String,
    model_calls: u64,
}

impl std::fmt::Debug for IndependentReviewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndependentReviewer")
            .field("workspace", &self.workspace)
            .field("model_calls", &self.model_calls)
            .finish_non_exhaustive()
    }
}

impl IndependentReviewer {
    /// A reviewer on `model` in `workspace`, as user `user`, with 20 model calls at the
    /// default effort policy.
    pub fn new(model: Arc<dyn Llm>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            model,
            workspace: workspace.into(),
            effort: EffortPolicy::default(),
            guidance: None,
            audit: None,
            user: "user".to_string(),
            model_calls: 20,
        }
    }

    /// Project guidance the reviewed code must follow (`--instruction`).
    pub fn guidance(mut self, guidance: Option<String>) -> Self {
        self.guidance = guidance;
        self
    }

    /// The run's audit log.
    pub fn audit_log(mut self, recorder: Arc<dyn AuditRecorder>) -> Self {
        self.audit = Some(recorder);
        self
    }

    /// The user the reviewer runs as.
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// The most model calls of one review.
    pub fn model_calls(mut self, max: u64) -> Self {
        self.model_calls = max;
        self
    }

    /// The session's effort policy (spec 3.1 §7.23): each review request carries its
    /// `read_only` level as `GenerateContentConfig::effort`.
    pub fn effort(mut self, policy: EffortPolicy) -> Self {
        self.effort = policy;
        self
    }

    /// The model-call cap of one review.
    pub fn max_model_calls(&self) -> u64 {
        self.model_calls
    }

    /// Reviews the workspace with one `focus` described by `desc`.
    pub async fn run(&self, focus: &str, desc: &str, prompt: &str) -> ReviewerRun {
        let instruction = super::review_instruction(focus, desc, self.guidance.as_deref());
        let mut builder = HarnessAgent::builder(format!("review-{focus}"))
            .model(self.model.clone())
            .effort(self.effort)
            .workspace(Workspace::new(&self.workspace))
            .mode(HarnessMode::ReadOnly)
            .without_shell()
            .instruction(instruction)
            .budget(
                RunBudget::new().max_model_calls(self.model_calls).max_wall_time(MEMBER_TIMEOUT),
            );
        if let Some(recorder) = &self.audit {
            builder = builder.audit_log(recorder.clone());
        }
        let agent = match builder.build() {
            Ok(agent) => agent,
            Err(error) => {
                return ReviewerRun { text: format!("review failed: {error}"), model_calls: 0 };
            }
        };
        let runtime = agent.runtime().clone();
        let mut launcher = HarnessLauncher::new(agent)
            .app_name(APP_NAME)
            .with_user_id(self.user.clone())
            .with_session_service(Arc::new(InMemorySessionService::new()))
            .with_console(ConsoleIo::scripted([], false))
            .quiet();
        let text = match launcher.run_once(prompt).await {
            Ok(outcome) => outcome.text,
            Err(error) => format!("review failed: {error}"),
        };
        ReviewerRun { text, model_calls: runtime.spend(None).model_calls }
    }
}

/// How a board ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardStatus {
    /// Every round ran.
    Completed,
    /// The board could not be built or failed with an error other than a budget stop.
    Failed,
    /// The board stopped on its budget; seats that had not spoken are `Failed`.
    BudgetExhausted,
}

/// One seat's verdict on a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatVerdict {
    /// The seat's focus.
    pub focus: Focus,
    /// Its verdict, from its last turn.
    pub verdict: Verdict,
    /// Its notes.
    pub notes: String,
}

/// What a board decided (spec 3.1 §7.10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardOutcome {
    /// One verdict per seat, in roster order.
    pub seats: Vec<SeatVerdict>,
    /// How the board ended.
    pub status: BoardStatus,
    /// Model calls of the board and of any fallback reviews.
    pub model_calls: u64,
    /// The error code of a failed board.
    pub error_code: Option<String>,
    /// Seats a failed board left without a verdict that independent reviewers then took.
    pub fallback: Vec<Focus>,
}

impl BoardOutcome {
    /// Whether every seat approved.
    pub fn approved(&self) -> bool {
        !self.seats.is_empty() && self.seats.iter().all(|seat| seat.verdict == Verdict::Approve)
    }
}

/// A Blackboard review board (spec 3.1 §7.10, §9.2 I61): one round-robin round of
/// `review_{focus}` seats with history `Full`, the ReadOnly members of a ReadOnly,
/// shell-less harness, run as [`HarnessAgent::governed_root`] so every seat's call is
/// decided by the harness policy and recorded in the run's audit log.
///
/// Each seat's verdict is its last turn's `VERDICT` line; a seat that never spoke or wrote
/// none is [`Verdict::Failed`]. A board that cannot be built, or fails with an error other
/// than a budget stop, hands the seats without a verdict to independent reviewers in
/// parallel when the [`fallback`](Self::fallback) budget covers them; a budget stop never
/// runs anything again. Each seat's model call carries the [`effort`](Self::effort)
/// policy's `members` level, as every harness team member's does (spec 3.1 §7.23).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_cli::harness::ultracode::{Focus, ReviewBoard};
/// use adk_harness::EffortPolicy;
///
/// # async fn review(model: Arc<dyn adk_core::Llm>) {
/// let outcome = ReviewBoard::new(model, ".")
///     .effort(EffortPolicy::default())
///     .roster(vec![Focus::Correctness, Focus::EdgeCases, Focus::Security, Focus::Style])
///     .rounds(2)
///     .model_calls(48)
///     .run("Review the change now.")
///     .await;
/// println!("{:?}: approved {}", outcome.status, outcome.approved());
/// # }
/// ```
#[derive(Clone)]
pub struct ReviewBoard {
    model: Arc<dyn Llm>,
    workspace: PathBuf,
    effort: EffortPolicy,
    guidance: Option<String>,
    audit: Option<Arc<dyn AuditRecorder>>,
    roster: Vec<Focus>,
    rounds: u32,
    model_calls: u64,
    user: String,
    fallback: Option<(IndependentReviewer, u64)>,
}

impl std::fmt::Debug for ReviewBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewBoard")
            .field("roster", &self.roster)
            .field("rounds", &self.rounds)
            .field("model_calls", &self.model_calls)
            .finish_non_exhaustive()
    }
}

impl ReviewBoard {
    /// A board on `model` in `workspace`: the floor seats, one round, 48 model calls, the
    /// default effort policy.
    pub fn new(model: Arc<dyn Llm>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            model,
            workspace: workspace.into(),
            effort: EffortPolicy::default(),
            guidance: None,
            audit: None,
            roster: super::route::FLOOR.to_vec(),
            rounds: 1,
            model_calls: 48,
            user: "user".to_string(),
            fallback: None,
        }
    }

    /// Project guidance the reviewed code must follow (`--instruction`).
    pub fn guidance(mut self, guidance: Option<String>) -> Self {
        self.guidance = guidance;
        self
    }

    /// The run's audit log.
    pub fn audit_log(mut self, recorder: Arc<dyn AuditRecorder>) -> Self {
        self.audit = Some(recorder);
        self
    }

    /// The seats, in speaking order.
    pub fn roster(mut self, roster: Vec<Focus>) -> Self {
        self.roster = roster;
        self
    }

    /// The board's rounds.
    pub fn rounds(mut self, rounds: u32) -> Self {
        self.rounds = rounds;
        self
    }

    /// The board's most model calls: its `RunBudget` and its `TeamBudget`.
    pub fn model_calls(mut self, max: u64) -> Self {
        self.model_calls = max;
        self
    }

    /// The session's effort policy (spec 3.1 §7.23): each seat's request carries its
    /// `members` level as `GenerateContentConfig::effort`.
    pub fn effort(mut self, policy: EffortPolicy) -> Self {
        self.effort = policy;
        self
    }

    /// The user the board runs as.
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// Independent reviewers for the seats a failed board leaves without a verdict, run
    /// only when `budget` — the run's model calls left before the board — still covers
    /// them after the board's own calls.
    pub fn fallback(mut self, reviewer: IndependentReviewer, budget: u64) -> Self {
        self.fallback = Some((reviewer, budget));
        self
    }

    fn seat_name(focus: Focus) -> String {
        format!("review_{}", focus.name())
    }

    fn harness(&self) -> adk_core::Result<HarnessAgent> {
        let mut builder = HarnessAgent::builder(BOARD_NAME)
            .model(self.model.clone())
            .effort(self.effort)
            .workspace(Workspace::new(&self.workspace))
            .mode(HarnessMode::ReadOnly)
            .without_shell()
            .without_default_members()
            .budget(
                RunBudget::new().max_model_calls(self.model_calls).max_wall_time(BOARD_WALL_TIME),
            );
        if let Some(recorder) = &self.audit {
            builder = builder.audit_log(recorder.clone());
        }
        for focus in &self.roster {
            let instruction = format!(
                "{}\n\n{SEAT_SUFFIX}",
                super::review_instruction(focus.name(), focus.desc(), self.guidance.as_deref())
            );
            builder = builder.member(MemberSpec {
                name: Self::seat_name(*focus),
                description: format!("Reviews the change for {}.", focus.name()),
                kind: MemberKind::Local { instruction, model: None },
                tools: MemberTools::ReadOnly,
                skills: false,
                max_iterations: 8,
                timeout: Duration::from_secs(10 * 60),
                output_schema: None,
            });
        }
        builder.build()
    }

    /// Runs the board on `prompt`.
    pub async fn run(&self, prompt: &str) -> BoardOutcome {
        let mut outcome = match self.harness() {
            Ok(harness) => self.run_board(&harness, prompt).await,
            Err(error) => BoardOutcome {
                seats: self.failed_seats(),
                status: BoardStatus::Failed,
                model_calls: 0,
                error_code: Some(error.code.to_string()),
                fallback: Vec::new(),
            },
        };
        if outcome.status == BoardStatus::Failed
            && let Some((reviewer, budget)) = &self.fallback
        {
            let missing: Vec<Focus> = outcome
                .seats
                .iter()
                .filter(|seat| seat.verdict == Verdict::Failed)
                .map(|seat| seat.focus)
                .collect();
            let cost = missing.len() as u64 * reviewer.max_model_calls();
            if !missing.is_empty() && budget.saturating_sub(outcome.model_calls) >= cost {
                let runs = futures::future::join_all(
                    missing.iter().map(|focus| reviewer.run(focus.name(), focus.desc(), prompt)),
                )
                .await;
                for (focus, run) in missing.iter().zip(runs) {
                    outcome.model_calls += run.model_calls;
                    let (verdict, notes) = parse_verdict(&run.text);
                    if let Some(seat) = outcome.seats.iter_mut().find(|seat| seat.focus == *focus) {
                        *seat = SeatVerdict { focus: *focus, verdict, notes };
                    }
                }
                outcome.fallback = missing;
            }
        }
        outcome
    }

    fn failed_seats(&self) -> Vec<SeatVerdict> {
        self.roster
            .iter()
            .map(|focus| SeatVerdict {
                focus: *focus,
                verdict: Verdict::Failed,
                notes: format!("the {} seat gave no verdict", focus.name()),
            })
            .collect()
    }

    async fn run_board(&self, harness: &HarnessAgent, prompt: &str) -> BoardOutcome {
        let failed = |code: &str| BoardOutcome {
            seats: self.failed_seats(),
            status: BoardStatus::Failed,
            model_calls: harness.runtime().spend(None).model_calls,
            error_code: Some(code.to_string()),
            fallback: Vec::new(),
        };
        let Some(team) = harness.runtime().team() else {
            return failed("ultracode.board_without_seats");
        };
        let names: Vec<String> = self.roster.iter().map(|focus| Self::seat_name(*focus)).collect();
        let seats: Vec<&str> = names.iter().map(String::as_str).collect();
        let policy = BlackboardPolicy {
            max_rounds: self.rounds,
            history: BlackboardHistoryPolicy::Full,
            budget: TeamBudget {
                max_model_requests: Some(self.model_calls),
                max_events: Some(4 * self.model_calls),
                max_wall_time_ms: Some(1_200_000),
                ..TeamBudget::default()
            },
            ..BlackboardPolicy::default()
        };
        let round = match team.review_round_with(BOARD_NAME, &seats, policy) {
            Ok(round) => round,
            Err(error) => return failed(error.code),
        };
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let created = sessions
            .create(CreateRequest {
                app_name: APP_NAME.to_string(),
                user_id: self.user.clone(),
                session_id: Some(BOARD_NAME.to_string()),
                state: Default::default(),
            })
            .await;
        if let Err(error) = created {
            return failed(error.code);
        }
        let runner = harness
            .configure_runner(
                Runner::builder()
                    .app_name(APP_NAME)
                    .agent(harness.governed_root(round))
                    .session_service(sessions),
            )
            .build();
        let runner = match runner {
            Ok(runner) => runner,
            Err(error) => return failed(error.code),
        };
        let (user, session) = match (UserId::new(self.user.clone()), SessionId::new(BOARD_NAME)) {
            (Ok(user), Ok(session)) => (user, session),
            (Err(_), _) | (_, Err(_)) => return failed("ultracode.invalid_identity"),
        };
        let mut events =
            match runner.run(user, session, Content::new("user").with_text(prompt)).await {
                Ok(events) => events,
                Err(error) => return failed(error.code),
            };
        let mut last: BTreeMap<String, String> = BTreeMap::new();
        let mut status = BoardStatus::Completed;
        let mut error_code = None;
        while let Some(event) = events.next().await {
            match event {
                Ok(event) => {
                    if event
                        .llm_response
                        .error_code
                        .as_deref()
                        .is_some_and(|code| code.starts_with("budget."))
                    {
                        status = BoardStatus::BudgetExhausted;
                    }
                    if event.llm_response.partial || !names.contains(&event.author) {
                        continue;
                    }
                    let text: String = event
                        .llm_response
                        .content
                        .iter()
                        .flat_map(|content| content.parts.iter())
                        .filter_map(adk_core::Part::text)
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.trim().is_empty() {
                        last.insert(event.author.clone(), text);
                    }
                }
                Err(error) if error.code.starts_with("budget.") => {
                    status = BoardStatus::BudgetExhausted;
                    error_code = Some(error.code.to_string());
                }
                Err(error) => {
                    if status != BoardStatus::BudgetExhausted {
                        status = BoardStatus::Failed;
                    }
                    error_code = Some(error.code.to_string());
                }
            }
        }
        drop(events);
        let seats = self
            .roster
            .iter()
            .map(|focus| match last.get(&Self::seat_name(*focus)) {
                Some(text) => {
                    let (verdict, notes) = parse_verdict(text);
                    SeatVerdict { focus: *focus, verdict, notes }
                }
                None => SeatVerdict {
                    focus: *focus,
                    verdict: Verdict::Failed,
                    notes: format!("the {} seat never spoke", focus.name()),
                },
            })
            .collect();
        BoardOutcome {
            seats,
            status,
            model_calls: harness.runtime().spend(None).model_calls,
            error_code,
            fallback: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{AdkError, UsageMetadata};
    use adk_model::{ScriptedLlm, ScriptedResponse};

    fn text(text: &str) -> ScriptedResponse {
        ScriptedResponse::text(text).with_usage(UsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 10,
            total_token_count: 20,
            ..UsageMetadata::default()
        })
    }

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "a project\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn a_board_reads_each_other_and_takes_each_seats_last_turn() {
        let dir = workspace();
        let model = Arc::new(ScriptedLlm::new(
            "board",
            [
                text("The empty-input path panics.\nVERDICT: changes\n- handle empty input"),
                text("Same concern as correctness.\nVERDICT: changes"),
                text("VERDICT: approve"),
            ],
        ));
        let outcome =
            ReviewBoard::new(model.clone(), dir.path()).model_calls(8).run("Review now.").await;
        assert_eq!(outcome.status, BoardStatus::Completed);
        let verdicts: Vec<Verdict> = outcome.seats.iter().map(|seat| seat.verdict).collect();
        assert_eq!(verdicts, [Verdict::Changes, Verdict::Changes, Verdict::Approve]);
        assert_eq!(outcome.seats[0].notes, "- handle empty input");
        assert_eq!(outcome.model_calls, 3);
        assert!(!outcome.approved());
        let second = &model.requests()[1];
        let context: Vec<&str> = second
            .contents
            .iter()
            .filter(|content| content.role == "user")
            .flat_map(|content| content.parts.iter())
            .filter_map(adk_core::Part::text)
            .collect();
        assert!(
            context.iter().any(|text| text.starts_with("[review_correctness] said:")),
            "{context:#?}"
        );
        let system: String = second
            .contents
            .iter()
            .filter(|content| content.role == "system")
            .flat_map(|content| content.parts.iter())
            .filter_map(adk_core::Part::text)
            .collect();
        assert!(system.contains(SEAT_SUFFIX), "the seat knows it is on a board: {system}");
    }

    #[tokio::test]
    async fn a_failed_board_falls_back_to_independent_reviews() {
        let dir = workspace();
        let board = Arc::new(ScriptedLlm::new(
            "board",
            [text("VERDICT: approve"), ScriptedResponse::error(AdkError::model("provider down"))],
        ));
        let independent = Arc::new(ScriptedLlm::new(
            "independent",
            [text("VERDICT: approve"), text("VERDICT: changes\n- rename x")],
        ));
        let reviewer = IndependentReviewer::new(independent.clone(), dir.path()).model_calls(5);
        let outcome = ReviewBoard::new(board.clone(), dir.path())
            .model_calls(8)
            .fallback(reviewer.clone(), 100)
            .run("Review now.")
            .await;
        assert_eq!(outcome.status, BoardStatus::Failed);
        assert!(outcome.error_code.is_some());
        assert_eq!(outcome.fallback, [Focus::EdgeCases, Focus::Style]);
        assert_eq!(outcome.seats[0].verdict, Verdict::Approve, "the board's own verdict stays");
        let mut fallback: Vec<Verdict> =
            outcome.seats[1..].iter().map(|seat| seat.verdict).collect();
        fallback.sort_by_key(|verdict| verdict.as_str());
        assert_eq!(fallback, [Verdict::Approve, Verdict::Changes]);
        assert_eq!(independent.requests().len(), 2, "one independent review per missing seat");
        assert_eq!(outcome.model_calls, 2 + 2);

        // Without the budget for them, the missing seats stay failed.
        let board =
            Arc::new(ScriptedLlm::new("board", [ScriptedResponse::error(AdkError::model("down"))]));
        let idle = Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new()));
        let outcome = ReviewBoard::new(board, dir.path())
            .fallback(IndependentReviewer::new(idle.clone(), dir.path()).model_calls(5), 14)
            .run("Review now.")
            .await;
        assert!(outcome.fallback.is_empty());
        assert!(outcome.seats.iter().all(|seat| seat.verdict == Verdict::Failed));
        assert!(idle.requests().is_empty());
    }

    #[tokio::test]
    async fn a_board_budget_stop_fails_unspoken_seats_closed() {
        let dir = workspace();
        let board = Arc::new(ScriptedLlm::new(
            "board",
            [text("VERDICT: approve"), text("VERDICT: approve"), text("VERDICT: approve")],
        ));
        let idle = Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new()));
        let outcome = ReviewBoard::new(board.clone(), dir.path())
            .model_calls(2)
            .fallback(IndependentReviewer::new(idle.clone(), dir.path()), 1_000)
            .run("Review now.")
            .await;
        assert_eq!(outcome.status, BoardStatus::BudgetExhausted);
        let verdicts: Vec<Verdict> = outcome.seats.iter().map(|seat| seat.verdict).collect();
        assert_eq!(verdicts, [Verdict::Approve, Verdict::Approve, Verdict::Failed]);
        assert!(!outcome.approved(), "an unspoken seat is never an approval");
        assert!(outcome.fallback.is_empty() && idle.requests().is_empty(), "nothing runs again");
        assert_eq!(board.requests().len(), 2);
        assert_eq!(outcome.model_calls, 2);
    }

    #[tokio::test]
    async fn reviewers_and_seats_carry_the_sessions_effort() {
        let dir = workspace();
        let policy = EffortPolicy {
            read_only: adk_core::Effort::Low,
            members: adk_core::Effort::High,
            ..EffortPolicy::default()
        };
        let efforts = |model: &ScriptedLlm| {
            model
                .requests()
                .iter()
                .map(|request| request.config.as_ref().and_then(|config| config.effort))
                .collect::<Vec<_>>()
        };
        let model = Arc::new(ScriptedLlm::new("reviewer", [text("VERDICT: approve")]));
        let run = IndependentReviewer::new(model.clone(), dir.path())
            .effort(policy)
            .run(Focus::Style.name(), Focus::Style.desc(), "Review now.")
            .await;
        assert!(run.text.contains("VERDICT: approve"), "{}", run.text);
        assert_eq!(efforts(&model), [Some(adk_core::Effort::Low)], "the read_only level");

        let board = Arc::new(ScriptedLlm::new(
            "board",
            [text("VERDICT: approve"), text("VERDICT: approve"), text("VERDICT: approve")],
        ));
        let outcome = ReviewBoard::new(board.clone(), dir.path())
            .effort(policy)
            .model_calls(8)
            .run("Review now.")
            .await;
        assert_eq!(outcome.status, BoardStatus::Completed);
        assert_eq!(efforts(&board), [Some(adk_core::Effort::High); 3], "the members level");
    }

    #[test]
    fn verdicts_need_a_verdict_line() {
        assert_eq!(parse_verdict("**VERDICT: approve**").0, Verdict::Approve);
        assert_eq!(parse_verdict("VERDICT: changes\nVERDICT: approve").0, Verdict::Approve);
        assert_eq!(parse_verdict("VERDICT:changes").0, Verdict::Changes);
        assert_eq!(
            parse_verdict("I cannot give VERDICT: approve yet.\nVERDICT: changes").0,
            Verdict::Changes
        );
        let (verdict, notes) = parse_verdict("review failed: provider down");
        assert_eq!((verdict, notes.as_str()), (Verdict::Failed, "review failed: provider down"));
    }
}
