//! The workflow observer's words (spec 3.3 §8, §12): a developer panel, so technical words
//! such as "checkpoint" and "HTTP call" are allowed; the words a decision card carries stay
//! the engine's, in [`crate::words`].

use adk_graph::shape::NodeKind;

use super::view::{Change, NodeState, RunStatus};

/// "Asks first": a node that waits for a person.
pub const ASKS_FIRST: &str = "Asks first";
/// "Spends money": a governed node marked as spending money.
pub const SPENDS_MONEY: &str = "Spends money";
/// "Can't be undone": a node declared non-idempotent.
pub const CANT_BE_UNDONE: &str = "Can't be undone";
/// The input step's text: the person's message started the run.
pub const YOUR_MESSAGE: &str = "Your message";
/// A step that waits and wrote nothing.
pub const NOTHING_WRITTEN_YET: &str = "nothing written yet";
/// The link from a decision card to the run view.
pub const OPEN_IN_RUNS: &str = "Open in Runs";
/// A fork made and started.
pub const FORKED: &str = "Forked. The original run is unchanged.";
/// A fork made while everything is stopped.
pub const FORKED_STOPPED: &str = "Forked. Everything is stopped — start it after Resume all.";
/// A fork made whose run could not start.
pub const FORKED_NOT_STARTED: &str = "Forked, but its run couldn't start — start it from its run.";
/// In the fork dialog, when a call that runs again asks a person first.
pub const CHANGED_CALL_WAITS: &str = "A changed call waits for you.";
/// In the fork dialog, when the calls that run again are decided by the host's rules.
pub const CHANGED_CALL_RULES: &str = "A changed call is a new call, decided by your rules.";
/// A fork refused while a decision of the session waits.
pub const ANSWER_THE_WAITING_DECISION_FIRST: &str = "Answer the waiting decision first";
/// A fork refused at a checkpoint with nothing pending.
pub const NOTHING_RUNS_AFTER_THE_LAST_STEP: &str = "Nothing runs after the last step";
/// A fork refused at a pause.
pub const FORK_FROM_THE_COMPLETED_STEP: &str =
    "A node waits in this step; fork from the step before it";
/// A value with a secret pattern.
pub const LOOKS_LIKE_A_SECRET: &str = "Hidden: looks like a secret";
/// A value under a key the host hides.
pub const HIDDEN_KEY: &str = "Hidden: this home hides this key";
/// A value over the size cap.
pub const TOO_LARGE: &str = "Too large to show in full";
/// What a redacted value reads.
pub const REDACTED: &str = "[redacted]";
/// What a hidden value reads.
pub const HIDDEN: &str = "[hidden]";

/// "Step 2".
pub fn step_label(step: usize) -> String {
    format!("Step {step}")
}

/// The words of a run's status.
pub fn run_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::NoRun => "No run yet",
        RunStatus::NotStarted => "Not started",
        RunStatus::Working => "Working",
        RunStatus::Waiting => crate::words::WAITING_FOR_YOU,
        RunStatus::Paused => "Paused",
        RunStatus::Stopped => "Stopped",
        RunStatus::Done => "Done",
        RunStatus::Failed => "Failed",
    }
}

/// The words of a node's state.
pub fn node_state(state: NodeState) -> &'static str {
    match state {
        NodeState::Idle => "Idle",
        NodeState::Working => "Working",
        NodeState::Done => "Done",
        NodeState::Waiting => crate::words::WAITING_FOR_YOU,
        NodeState::Paused => "Paused",
        NodeState::Failed => "Failed",
        NodeState::Recorded => "Not sent again",
        NodeState::NotRun => "Not run",
    }
}

/// The words of a change.
pub fn change(change: Change) -> &'static str {
    match change {
        Change::Added => "Added",
        Change::Changed => "Changed",
        Change::Removed => "Removed",
    }
}

/// A node kind in words.
pub fn node_kind(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::ModelStep => "Model step",
        NodeKind::Function => "Function",
        NodeKind::HttpCall => "HTTP call",
        NodeKind::Trigger => "Trigger",
        NodeKind::Set => "Set",
        NodeKind::Transform => "Transform",
        NodeKind::Switch => "Switch",
        NodeKind::Loop => "Loop",
        NodeKind::Merge => "Merge",
        NodeKind::Wait => "Wait",
        NodeKind::File => "File",
        NodeKind::Code => "Code",
        NodeKind::Database => "Database",
        NodeKind::Email => "Email",
        NodeKind::Notification => "Notification",
        NodeKind::Rss => "RSS",
        NodeKind::Join => "Join",
        NodeKind::Subgraph => "Subgraph",
        _ => "Step",
    }
}

/// "Started 09:14 · 4.2 s · 3 steps".
pub fn run_summary(started: &str, duration: &str, steps: usize) -> String {
    let unit = if steps == 1 { "step" } else { "steps" };
    format!("Started {started} · {duration} · {steps} {unit}")
}

/// A fork made and not started: "Made 09:31 · forked at step 1".
pub fn made_summary(made: &str, step: usize) -> String {
    format!("Made {made} · forked at step {step}")
}

/// Before the first run.
pub const NO_RUN_YET: &str = "No run yet — send a message to start one.";

/// "Fork of the 09:14 run at step 1 · edited note".
pub fn fork_of(run_started: Option<&str>, step: usize, edited: &[String]) -> String {
    let run = run_started.map_or_else(|| "a run".to_string(), |at| format!("the {at} run"));
    let mut text = format!("Fork of {run} at step {step}");
    if !edited.is_empty() {
        text.push_str(&format!(" · edited {}", edited.join(", ")));
    }
    text
}

/// "from step 1 · edited note".
pub fn fork_summary(step: usize, edited: &[String]) -> String {
    let mut text = format!("from step {step}");
    if !edited.is_empty() {
        text.push_str(&format!(" · edited {}", edited.join(", ")));
    }
    text
}

/// "from 09:14": a node a fork did not run again, done by the run it was forked from.
pub fn inherited(at: &str) -> String {
    format!("from {at}")
}

/// "From the invoice-run workflow".
pub fn from_the_workflow(graph: &str) -> String {
    format!("From the {graph} workflow")
}

/// "pay — POST 127.0.0.1/charges at 09:14".
pub fn effect(node: &str, call: Option<&str>, at: &str) -> String {
    match call {
        Some(call) => format!("{node} — {call} at {at}"),
        None => format!("{node} at {at}"),
    }
}

/// "Not sent again — the record from 09:14 answered it".
pub fn not_sent_again(at: &str) -> String {
    format!("Not sent again — the record from {at} answered it")
}

/// A governed call that failed before: never sent again.
pub const FAILED_EARLIER: &str = "This step failed earlier — not sent again";

/// "Asked at 09:14".
pub fn asked_at(at: &str) -> String {
    crate::words::asked_at(at)
}

/// "invoice still holds a hidden value; replace it or reset it".
pub fn still_hidden(key: &str) -> String {
    format!("{key} still holds a hidden value; replace it or reset it")
}

/// "document is too large to edit here".
pub fn too_large_to_edit(key: &str) -> String {
    format!("{key} is too large to edit here")
}

/// A duration in words: `0.3 s`, `4.2 s`, `2 min 5 s`.
pub fn duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        let seconds = ms / 1000;
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_in_seconds_then_minutes() {
        let cases = [(300, "0.3 s"), (1_200, "1.2 s"), (30_200, "30.2 s"), (125_000, "2 min 5 s")];
        let words: Vec<(u64, String)> = cases.iter().map(|(ms, _)| (*ms, duration(*ms))).collect();
        let expected: Vec<(u64, String)> =
            cases.iter().map(|(ms, text)| (*ms, (*text).to_string())).collect();
        assert_eq!(words, expected);
    }

    #[test]
    fn a_fork_names_its_run_step_and_edits() {
        assert_eq!(
            fork_of(Some("09:14"), 1, &["note".to_string()]),
            "Fork of the 09:14 run at step 1 · edited note"
        );
        assert_eq!(fork_summary(1, &[]), "from step 1");
    }
}
