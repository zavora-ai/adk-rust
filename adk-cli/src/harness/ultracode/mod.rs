//! Adaptive `ultracode` (spec 3.1 §7.10 "Adaptive review", §8.1, §13 question 4; feature
//! `harness`, Beta).
//!
//! The implementer runs one turn in the user's mode; the review is then routed once, on the
//! actual change, to the parallel graph or a Blackboard board, and the plan holds for the
//! run. Reviewers are ReadOnly and shell-less in both architectures (I61), the plan is a
//! pure function of its `harness.ultracode.routed` record (I62), and every step starts only
//! when its budget fits what remains of the run's (I63).
//!
//! | Item | What |
//! |---|---|
//! | [`route`], [`replay`], [`heuristic`], [`router_schema`] | `ultracode-route/1`: override, `unchanged`, `sensitive`, the closed-schema model answer, the heuristic |
//! | [`ReviewPlan::escalated`], [`replay_escalation`] | the `security` seat a revise turn that makes the change sensitive adds |
//! | [`TaskFeatures`], [`WorkspaceSnapshot`], [`ChangeSignals`], [`RepoSignals`] | the signals: task keywords and a metadata snapshot, no file contents, no `git` |
//! | [`UltracodeLimits`] | the `[ultracode]` table: defaults, ceilings, tighten-only project file, flags |
//! | [`ReviewBoard`], [`IndependentReviewer`] | the board and the independent reviewer |
//! | [`UltracodeAudit`] | the `harness.ultracode.*` records |
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_cli::harness::ultracode::{
//!     RepoSignals, RouteInputs, TaskFeatures, UltracodeFlags, UltracodeLimits, WorkspaceSnapshot, route,
//! };
//!
//! # async fn run(router: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
//! let root = std::path::Path::new(".");
//! let limits = UltracodeLimits::resolve(None, root, &UltracodeFlags::default())?;
//! let before = WorkspaceSnapshot::capture(root);
//! // ... the implementer's first turn ...
//! let after = WorkspaceSnapshot::capture(root);
//! let task = "add a /health endpoint";
//! let inputs = RouteInputs {
//!     features: TaskFeatures::extract(task),
//!     change: before.diff(&after, &limits.sensitive_paths),
//!     repo: RepoSignals::from_snapshot(&after),
//!     limits,
//! };
//! // The router call carries the session's `members` effort (spec 3.1 §7.23).
//! let routed = route(&router, Some(adk_core::Effort::Low), task, inputs).await;
//! println!("{} via {}", routed.plan.architecture, routed.rule);
//! # Ok(())
//! # }
//! ```

mod board;
mod config;
mod route;
mod signals;

use std::sync::Arc;

use adk_core::Result;
use adk_core::audit_log::{
    AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecorder, AuditTrail,
};
use serde_json::Value;

pub use board::{
    BoardOutcome, BoardStatus, IndependentReviewer, ReviewBoard, ReviewerRun, SeatVerdict, Verdict,
    parse_verdict,
};
pub use config::{SettingSource, UltracodeFlags, UltracodeLimits};
pub use route::{
    Architecture, ArchitectureChoice, FLOOR, Fallback, Focus, HeuristicTrace, ROUTER_VERSION,
    ReplayError, ReviewPlan, RouteDecision, RouteInputs, RouteSource, RouterAnswer, RouterCall,
    RouterMode, Rule, SPECIALISTS, heuristic, replay, replay_escalation, route, router_schema,
};
pub use signals::{
    ChangeSignals, FileStamp, LanguageCount, RepoSignals, SNAPSHOT_MAX_FILES, TaskFeatures,
    TurnStatus, WorkspaceSnapshot,
};

/// The reviewer instruction for `focus`, with project `guidance` (spec 3.1 §8.1).
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::{Focus, review_instruction};
///
/// let text = review_instruction(Focus::Style.name(), Focus::Style.desc(), Some("Use tabs."));
/// assert!(text.starts_with("You are a strict but fair senior code reviewer. Focus ONLY on style:"));
/// assert!(text.ends_with("Project guidance the code must follow:\nUse tabs."));
/// ```
pub fn review_instruction(focus: &str, desc: &str, guidance: Option<&str>) -> String {
    let mut instruction = format!(
        "You are a strict but fair senior code reviewer. Focus ONLY on {focus}: {desc}. \
         Use read_file/glob/grep to inspect the code in the workspace. Reply with a single \
         final line 'VERDICT: approve' if it meets the bar, or 'VERDICT: changes' followed by \
         a short bullet list of required changes. Do not request removing behavior the task \
         requires. Do not modify files."
    );
    if let Some(guidance) = guidance {
        instruction.push_str(&format!("\n\nProject guidance the code must follow:\n{guidance}"));
    }
    instruction
}

/// The prompt every reviewer of `task` gets, independent or on a board.
///
/// # Example
///
/// ```rust
/// let prompt = adk_cli::harness::ultracode::review_prompt("add a /health endpoint");
/// assert!(prompt.starts_with("The task being implemented is:\nadd a /health endpoint"));
/// ```
pub fn review_prompt(task: &str) -> String {
    format!(
        "The task being implemented is:\n{task}\n\nReview the code in the workspace against \
         this task now."
    )
}

/// `sha256:<hex>` of `bytes`.
pub(crate) fn sha256_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The `harness.ultracode.*` records of one run (spec 3.1 §9.3): kind `Other("harness")`,
/// actor `AuditActor::for_invocation("adk-rust", "ultracode", user, run_id)`, `ext.sessionId`
/// of the implementer's session, written in Block mode: a record that cannot be written is
/// an error, and the step it precedes does not run.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_cli::harness::ultracode::UltracodeAudit;
/// use adk_core::audit_log::InMemoryAuditRecorder;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let recorder = InMemoryAuditRecorder::new();
/// let audit = UltracodeAudit::new(Arc::new(recorder.clone()), "alice", "run-1", "session-1");
/// audit.record("harness.ultracode.started", serde_json::json!({ "runId": "run-1" })).await.unwrap();
/// let record = &recorder.records()[0];
/// assert_eq!(record.ext["sessionId"], "session-1");
/// assert_eq!(record.actor.invocation_id.as_deref(), Some("run-1"));
/// # });
/// ```
#[derive(Clone, Debug)]
pub struct UltracodeAudit {
    trail: AuditTrail,
    actor: AuditActor,
    session_id: String,
}

impl UltracodeAudit {
    /// The records of run `run_id` by `user` in the implementer's session `session_id`.
    pub fn new(
        recorder: Arc<dyn AuditRecorder>,
        user: &str,
        run_id: &str,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            trail: AuditTrail::new(recorder),
            actor: AuditActor::for_invocation("adk-rust", "ultracode", user, run_id),
            session_id: session_id.into(),
        }
    }

    /// Records `action` with the fields of `ext` (an object); null fields are left out.
    ///
    /// # Errors
    ///
    /// `audit.record_failed` when the record cannot be written.
    pub async fn record(&self, action: &str, ext: Value) -> Result<()> {
        let mut draft =
            AuditRecordDraft::new(AuditRecordKind::Other("harness".to_string()), action);
        if let Value::Object(fields) = ext {
            for (key, value) in fields {
                if !value.is_null() {
                    draft = draft.ext(key, value);
                }
            }
        }
        draft = draft
            .actor(self.actor.clone())
            .ext("sessionId", Value::String(self.session_id.clone()));
        self.trail.record(draft).await
    }
}
