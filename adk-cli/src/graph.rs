//! CLI commands for graph time-travel debugging.
//!
//! `adk-rust graph steps` prints a thread's runs, each step's node records and a fork's
//! lineage from the records adk-graph keeps in every checkpoint; `graph fork` forks a thread at
//! a completed step, with `--set key=<json>` edits (spec 3.3 §12 developer track). `graph
//! replay` prints recorded states, and `graph resume` explains how to resume from code.
//!
//! Every value printed passes [`terminal_safe`] (spec 3.3 S-I9). The CLI has the checkpoints and not the graph, so a fork's edits are not checked
//! against the graph's channels: only keys the checkpoint holds can be set, and the lineage
//! records `checked: false`. CLI forks are not started and not audited.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use adk_graph::checkpoint::Checkpointer;
use adk_graph::record::{NodeRecord, NodeStatus};
use adk_graph::state::Checkpoint;
use adk_graph::time_travel::{ForkError, ForkSpec, fork_thread};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::cli::GraphCommands;

/// Escapes terminal control characters, as the library's `launcher::terminal_safe` does:
/// newlines and tabs stay, every other control character is written as its `\u{..}` escape,
/// so a printed value cannot move the cursor or rewrite the screen (spec 3.3 S-I9).
fn terminal_safe(text: &str) -> std::borrow::Cow<'_, str> {
    let unsafe_char = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !text.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if unsafe_char(c) {
            escaped.extend(c.escape_unicode());
        } else {
            escaped.push(c);
        }
    }
    std::borrow::Cow::Owned(escaped)
}

/// Execute a graph time-travel CLI command.
pub async fn run(command: GraphCommands) -> Result<()> {
    match command {
        GraphCommands::Steps { thread_id, db } => run_steps(&thread_id, db.as_deref()).await,
        GraphCommands::Replay { thread_id, from, to, db } => {
            run_replay(&thread_id, from, to, db.as_deref()).await
        }
        GraphCommands::Fork { thread_id, at, new_thread, set, db } => {
            run_fork(&thread_id, at, &new_thread, &set, db.as_deref()).await
        }
        GraphCommands::Resume { thread_id, from, db } => {
            run_resume(&thread_id, from, db.as_deref()).await
        }
    }
}

/// When a run started, as a time of day.
fn run_time(run: &adk_graph::record::RunRecord) -> String {
    time_of(&run.started_at)
}

/// A time of day in the host's time zone where the build reads it (chrono comes with
/// `harness`), else in UTC.
#[cfg(feature = "harness")]
fn time_of(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%H:%M:%S").to_string()
}

#[cfg(not(feature = "harness"))]
fn time_of<T: std::fmt::Display>(at: &T) -> String {
    // `DateTime<Utc>` displays as `2026-10-27 09:14:00.123 UTC`; its time of day is the second
    // field.
    let shown = at.to_string();
    shown.split(' ').nth(1).map_or(shown.clone(), |time| time.chars().take(8).collect())
}

/// Lists a thread's runs, the node records of each step and its lineage.
async fn run_steps(thread_id: &str, db: Option<&str>) -> Result<()> {
    let checkpointer = load_checkpointer(db).await?;
    let checkpoints =
        checkpointer.list(thread_id).await.context("failed to read the thread's checkpoints")?;
    if checkpoints.is_empty() {
        println!("No checkpoints found for thread '{}'.", terminal_safe(thread_id));
        return Ok(());
    }
    print!("{}", steps_text(&checkpoints, &|record| time_of(&record.started_at), &run_time));
    Ok(())
}

/// The columns of `graph steps`.
const STEP_HEADER: [&str; 6] = ["Step", "Time", "Node", "Status", "Duration", "Wrote"];

/// One row of `graph steps`.
type Row = [String; 6];

fn table(out: &mut String, rows: &[Row]) {
    let mut widths = STEP_HEADER.map(str::len);
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: &[String]| -> String {
        let mut line = String::new();
        for (index, (cell, width)) in cells.iter().zip(widths).enumerate() {
            if index + 1 == cells.len() {
                line.push_str(cell);
            } else {
                let _ = write!(line, "{cell:<width$}  ");
            }
        }
        line.trim_end().to_string()
    };
    let _ = writeln!(out, "{}", line(&STEP_HEADER.map(str::to_string)));
    for row in rows {
        let _ = writeln!(out, "{}", line(row));
    }
}

/// A node record's status in the terminal's words.
fn status_word(record: &NodeRecord) -> &'static str {
    match record.status {
        NodeStatus::Done => "done",
        // A governed node's pause waits for a decision.
        NodeStatus::Paused if record.effect_key.is_some() => "waiting",
        NodeStatus::Paused => "paused",
        NodeStatus::Failed => "failed",
        NodeStatus::Recorded => "not sent again",
        NodeStatus::Refused => "refused",
        NodeStatus::NotRun => "not run",
    }
}

fn duration(record: &NodeRecord) -> String {
    match record.status {
        NodeStatus::Done | NodeStatus::Failed => {
            format!("{:.1} s", record.duration_ms as f64 / 1000.0)
        }
        _ => "-".to_string(),
    }
}

fn list_or_dash(items: &[String]) -> String {
    if items.is_empty() { "-".to_string() } else { items.join(", ") }
}

/// The text `graph steps` prints for a thread's checkpoints, oldest first: a block per run,
/// with a lineage line for a fork and the input that started a run from its entry.
/// `time` and `started` give a record's and a run's time of day.
fn steps_text(
    checkpoints: &[Checkpoint],
    time: &dyn Fn(&NodeRecord) -> String,
    started: &dyn Fn(&adk_graph::record::RunRecord) -> String,
) -> String {
    let mut out = String::new();
    if let Some(oldest) = checkpoints.first()
        && oldest.version > 1
    {
        let first =
            oldest.step_records().ok().and_then(|steps| steps.first().map(|step| step.step));
        let _ = writeln!(out, "steps before {} were removed", first.unwrap_or(oldest.step));
    }
    let lineage = checkpoints.last().and_then(|newest| newest.lineage().ok().flatten());

    // Group consecutive checkpoints by run; checkpoints with no run record are kept apart.
    let mut blocks: Vec<(
        Option<adk_graph::record::RunRecord>,
        Vec<&Checkpoint>,
        Option<&Checkpoint>,
    )> = Vec::new();
    let mut previous: Option<&Checkpoint> = None;
    for checkpoint in checkpoints {
        let run = checkpoint.run_record().ok().flatten();
        let fork_start = run.is_none() && checkpoint.lineage().ok().flatten().is_some();
        if !fork_start {
            match blocks.last_mut() {
                Some((known, members, _)) if *known == run => members.push(checkpoint),
                _ => blocks.push((run, vec![checkpoint], previous)),
            }
        }
        previous = Some(checkpoint);
    }
    if blocks.is_empty() {
        if let Some(lineage) = &lineage {
            let _ = writeln!(out, "{}", lineage_line(lineage));
            let _ = writeln!(out, "not started");
        }
        return out;
    }

    let mut lineage_printed = false;
    for (index, (run, members, before)) in blocks.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let mut rows: Vec<Row> = Vec::new();
        match run {
            Some(run) => {
                let _ = writeln!(out, "run {} ({})", started(run), terminal_safe(&run.run_id));
                if let Some(lineage) = &lineage
                    && !lineage_printed
                {
                    let _ = writeln!(out, "{}", lineage_line(lineage));
                    lineage_printed = true;
                }
                // A run from its entry starts with the person's input; a fork's does not.
                let from_fork = before.is_some_and(|before| {
                    before.run_record().ok().flatten().is_none()
                        && before.lineage().ok().flatten().is_some()
                });
                if let Some(first) = members.first().filter(|_| !from_fork) {
                    let records = first.step_records().unwrap_or_default();
                    let written: BTreeSet<&String> = records
                        .iter()
                        .flat_map(|step| &step.nodes)
                        .flat_map(|node| &node.writes)
                        .collect();
                    let input: Vec<String> = first
                        .state
                        .iter()
                        .filter(|(key, value)| {
                            !written.contains(key)
                                && before.and_then(|before| before.state.get(*key)) != Some(*value)
                        })
                        .map(|(key, _)| terminal_safe(key).into_owned())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    let step =
                        records.first().map_or(first.step, |step| step.step).saturating_sub(1);
                    let at = started(run);
                    rows.push([
                        step.to_string(),
                        at,
                        "-".into(),
                        "input".into(),
                        "-".into(),
                        list_or_dash(&input),
                    ]);
                }
                for checkpoint in members {
                    for step in checkpoint.step_records().unwrap_or_default() {
                        for node in &step.nodes {
                            let writes: Vec<String> = node
                                .writes
                                .iter()
                                .map(|key| terminal_safe(key).into_owned())
                                .collect();
                            rows.push([
                                step.step.to_string(),
                                time(node),
                                terminal_safe(&node.node).into_owned(),
                                status_word(node).to_string(),
                                duration(node),
                                list_or_dash(&writes),
                            ]);
                        }
                    }
                }
            }
            None => {
                let _ = writeln!(out, "run - (this thread has no run records)");
                for checkpoint in members {
                    rows.push([
                        checkpoint.step.to_string(),
                        "-".into(),
                        "-".into(),
                        "-".into(),
                        "-".into(),
                        "-".into(),
                    ]);
                }
            }
        }
        table(&mut out, &rows);
    }
    out
}

fn lineage_line(lineage: &adk_graph::record::Lineage) -> String {
    let edited: Vec<String> =
        lineage.edited.iter().map(|key| terminal_safe(key).into_owned()).collect();
    let mut line =
        format!("forked from {} at step {}", terminal_safe(&lineage.parent), lineage.parent_step);
    if !edited.is_empty() {
        let _ = write!(line, " (edited: {})", edited.join(", "));
    }
    line
}

/// Print the recorded state at each checkpointed step in a range.
///
/// Reads checkpoints; nothing is executed.
async fn run_replay(
    thread_id: &str,
    from: usize,
    to: Option<usize>,
    db: Option<&str>,
) -> Result<()> {
    let checkpointer = load_checkpointer(db).await?;
    let mut checkpoints =
        checkpointer.list(thread_id).await.context("failed to read state history")?;
    checkpoints.sort_by_key(|cp| cp.step);
    let transitions: Vec<(usize, adk_graph::state::State)> = checkpoints
        .into_iter()
        .filter(|cp| cp.step >= from && to.is_none_or(|end| cp.step <= end))
        .map(|cp| (cp.step, cp.state))
        .collect();
    if transitions.first().is_none_or(|(step, _)| *step != from) {
        bail!("no checkpoint found at step {from} for thread '{}'", terminal_safe(thread_id));
    }

    let range_desc = match to {
        Some(end) => format!("{from}..={end}"),
        None => format!("{from}..end"),
    };
    println!("Recorded state for thread '{}' (steps {range_desc}):\n", terminal_safe(thread_id));
    for (step, state) in &transitions {
        println!("── Step {step} ──");
        let json = serde_json::to_string_pretty(state).unwrap_or_else(|_| format!("{state:?}"));
        println!("{}\n", terminal_safe(&json));
    }
    println!("{} recorded state(s) shown; nothing was re-executed.", transitions.len());
    Ok(())
}

/// Parses `--set key=<json>` edits; a value that is not JSON is refused, shown escaped.
fn parse_sets(sets: &[String]) -> Result<BTreeMap<String, Value>> {
    let mut edits = BTreeMap::new();
    for set in sets {
        let Some((key, json)) = set.split_once('=').filter(|(key, _)| !key.trim().is_empty())
        else {
            bail!("--set takes key=<json>: {}", terminal_safe(set));
        };
        let value: Value = serde_json::from_str(json)
            .map_err(|_| anyhow!("not valid JSON: {}", terminal_safe(set)))?;
        edits.insert(key.trim().to_string(), value);
    }
    Ok(edits)
}

/// Forks `thread_id` at the completed checkpoint of step `at` into `new_thread`, with `sets`
/// written over existing keys (spec 3.3 §12). The edits are not checked against the graph's
/// channels, which the CLI does not have; the fork's lineage records that. The fork is not
/// started.
async fn run_fork(
    thread_id: &str,
    at: usize,
    new_thread: &str,
    sets: &[String],
    db: Option<&str>,
) -> Result<()> {
    let edits = parse_sets(sets)?;
    let checkpointer = load_checkpointer(db).await?;
    let text = fork_text(checkpointer.as_ref(), thread_id, at, new_thread, edits).await?;
    println!("{text}");
    Ok(())
}

/// Forks and returns what `graph fork` prints.
async fn fork_text(
    checkpointer: &dyn Checkpointer,
    thread_id: &str,
    at: usize,
    new_thread: &str,
    edits: BTreeMap<String, Value>,
) -> Result<String> {
    let checkpoints = checkpointer.list(thread_id).await.context("failed to read the thread")?;
    let Some(source) =
        checkpoints.iter().find(|checkpoint| checkpoint.step == at && !checkpoint.is_paused())
    else {
        bail!("no checkpoint {at} in {}", terminal_safe(thread_id));
    };
    let spec = ForkSpec {
        from: source.checkpoint_id.clone(),
        new_thread_id: new_thread.to_string(),
        set: edits,
    };
    let fork = fork_thread(checkpointer, spec, None).await.map_err(|error| match error {
        ForkError::UnknownKey { key } => {
            anyhow!("not a key of this checkpoint: {}", terminal_safe(&key))
        }
        ForkError::NothingToRun { .. } => anyhow!("nothing runs after the last step"),
        ForkError::ThreadExists { thread } => {
            anyhow!(
                "thread {} already has checkpoints; fork into a new thread",
                terminal_safe(&thread)
            )
        }
        other => anyhow!("{}", terminal_safe(&other.to_string())),
    })?;
    let edited = fork.lineage().ok().flatten().map(|lineage| lineage.edited).unwrap_or_default();

    // Governed calls after the fork point that already happened: answered from their record
    // when the fork makes them unchanged.
    let mut happened: Vec<String> = Vec::new();
    for later in checkpoints.iter().filter(|later| later.version > source.version) {
        for step in later.step_records().unwrap_or_default() {
            for node in step.nodes {
                let done = matches!(node.status, NodeStatus::Done | NodeStatus::Recorded);
                if done && node.effect_key.is_some() && !happened.contains(&node.node) {
                    happened.push(node.node);
                }
            }
        }
    }

    let mut text =
        format!("Forked {} at step {at} → {}", terminal_safe(thread_id), terminal_safe(new_thread));
    if !edited.is_empty() {
        let keys: Vec<String> = edited.iter().map(|key| terminal_safe(key).into_owned()).collect();
        let _ = write!(
            text,
            "; edited: {} (not checked against the graph's channels)",
            keys.join(", ")
        );
    }
    if !happened.is_empty() {
        let nodes: Vec<String> =
            happened.iter().map(|node| terminal_safe(node).into_owned()).collect();
        let _ = write!(
            text,
            "; already happened (answered from the record if unchanged): {}",
            nodes.join(", ")
        );
    }
    Ok(text)
}

/// Resume execution from a specific step.
///
/// Note: Resuming requires a compiled graph, which is not available from the CLI
/// alone. This command prints guidance on how to use the API programmatically.
async fn run_resume(thread_id: &str, from: usize, db: Option<&str>) -> Result<()> {
    let _checkpointer = load_checkpointer(db).await?;
    let thread_id = terminal_safe(thread_id);

    println!("Resume is not yet supported from the CLI.");
    println!();
    println!("Resuming execution requires a compiled graph, which cannot be");
    println!("constructed from CLI arguments alone.");
    println!();
    println!("To resume programmatically, use the TimeTravelHandle API:");
    println!();
    println!("    let handle = graph.time_travel(\"{thread_id}\");");
    println!("    let config = ExecutionConfig::new(\"{thread_id}\");");
    println!("    let state = handle.resume_from({from}, config).await?;");
    println!();
    println!("See: https://docs.rs/adk-graph/latest/adk_graph/time_travel/");
    Ok(())
}

/// Opens the SQLite checkpoint database at `db`, or an in-memory checkpointer without one.
async fn load_checkpointer(db: Option<&str>) -> Result<Arc<dyn Checkpointer>> {
    match db {
        Some(_path) => {
            // adk-graph's SQLite checkpointer: `graph-sqlite`, or the default build's harness
            // store, which compiles it.
            #[cfg(any(feature = "graph-sqlite", all(feature = "harness", feature = "persist")))]
            {
                let url = if _path.starts_with("sqlite:") {
                    _path.to_string()
                } else {
                    format!("sqlite://{_path}")
                };
                let checkpointer = adk_graph::checkpoint::SqliteCheckpointer::new(&url)
                    .await
                    .context("failed to open SQLite checkpoint database")?;
                Ok(Arc::new(checkpointer))
            }
            #[cfg(not(any(
                feature = "graph-sqlite",
                all(feature = "harness", feature = "persist")
            )))]
            {
                bail!(
                    "SQLite checkpoint support requires the `graph-sqlite` feature.\n\
                     Rebuild with: cargo install adk-cli --features graph-sqlite\n\
                     \n\
                     Without --db, an in-memory checkpointer is used (useful for testing)."
                );
            }
        }
        // In memory: useful for trying the commands, with no checkpoints.
        None => Ok(Arc::new(adk_graph::checkpoint::MemoryCheckpointer::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_graph::checkpoint::MemoryCheckpointer;
    use adk_graph::record::{
        LINEAGE_METADATA_KEY, Lineage, RUN_METADATA_KEY, RunRecord, STEPS_METADATA_KEY, StepKind,
        StepRecord,
    };
    use adk_graph::state::State;
    use serde_json::json;

    fn at(seconds: u32) -> String {
        format!("2026-10-27T06:14:{seconds:02}Z")
    }

    fn run_record(id: &str, seconds: u32) -> RunRecord {
        serde_json::from_value(json!({ "runId": id, "startedAt": at(seconds) })).unwrap()
    }

    fn record(
        node: &str,
        seconds: u32,
        status: NodeStatus,
        writes: &[&str],
        ms: u64,
    ) -> NodeRecord {
        let mut record: NodeRecord = serde_json::from_value(json!({
            "node": node,
            "startedAt": at(seconds),
            "durationMs": ms,
            "status": "done",
            "writes": writes,
            "attempt": 1,
        }))
        .unwrap();
        record.status = status;
        record.effect_key = (node == "pay").then(|| "graph/a/t1/pay/1/d".to_string());
        record
    }

    fn checkpoint(
        thread: &str,
        version: u64,
        state: Value,
        step: usize,
        pending: &[&str],
        steps: Vec<StepRecord>,
        run: Option<&RunRecord>,
    ) -> Checkpoint {
        let state: State = serde_json::from_value(state).unwrap();
        let mut checkpoint =
            Checkpoint::new(thread, state, step, pending.iter().map(|n| n.to_string()).collect())
                .with_version(version);
        checkpoint.checkpoint_id = format!("{thread}-c{version}");
        if let Some(run) = run {
            checkpoint =
                checkpoint.with_metadata(RUN_METADATA_KEY, serde_json::to_value(run).unwrap());
        }
        if !steps.is_empty() {
            checkpoint =
                checkpoint.with_metadata(STEPS_METADATA_KEY, serde_json::to_value(steps).unwrap());
        }
        checkpoint
    }

    /// invoice-run on t1: draft, pay waiting then done, receipt.
    fn invoice_run() -> Vec<Checkpoint> {
        let run = run_record("inv-1", 0);
        let drafted = json!({ "note": "Bill Acme", "invoice": { "amountCents": 4200 } });
        let charged =
            json!({ "note": "Bill Acme", "invoice": { "amountCents": 4200 }, "charge": "ch_1" });
        vec![
            checkpoint(
                "t1",
                1,
                drafted.clone(),
                1,
                &["pay"],
                vec![StepRecord {
                    step: 1,
                    kind: StepKind::Completed,
                    nodes: vec![record("draft", 1, NodeStatus::Done, &["invoice"], 1200)],
                }],
                Some(&run),
            ),
            checkpoint(
                "t1",
                2,
                drafted,
                1,
                &["pay"],
                vec![StepRecord {
                    step: 2,
                    kind: StepKind::Paused,
                    nodes: vec![record("pay", 3, NodeStatus::Paused, &[], 0)],
                }],
                Some(&run),
            )
            .with_metadata("adk.graph.paused_nodes", json!(["pay"])),
            checkpoint(
                "t1",
                3,
                charged.clone(),
                2,
                &["receipt"],
                vec![StepRecord {
                    step: 2,
                    kind: StepKind::Completed,
                    nodes: vec![record("pay", 52, NodeStatus::Done, &["charge"], 300)],
                }],
                Some(&run),
            ),
            checkpoint(
                "t1",
                4,
                charged,
                3,
                &[],
                vec![StepRecord {
                    step: 3,
                    kind: StepKind::Completed,
                    nodes: vec![record("receipt", 53, NodeStatus::Done, &["receipt"], 100)],
                }],
                Some(&run),
            ),
        ]
    }

    fn utc(record: &NodeRecord) -> String {
        record.started_at.format("%H:%M:%S").to_string()
    }

    fn run_utc(run: &RunRecord) -> String {
        run.started_at.format("%H:%M:%S").to_string()
    }

    #[test]
    fn steps_print_each_runs_input_and_node_records() {
        assert_eq!(
            steps_text(&invoice_run(), &utc, &run_utc),
            "run 06:14:00 (inv-1)\n\
             Step  Time      Node     Status   Duration  Wrote\n\
             0     06:14:00  -        input    -         note\n\
             1     06:14:01  draft    done     1.2 s     invoice\n\
             2     06:14:03  pay      waiting  -         -\n\
             2     06:14:52  pay      done     0.3 s     charge\n\
             3     06:14:53  receipt  done     0.1 s     receipt\n"
        );
    }

    #[test]
    fn a_forks_steps_carry_its_lineage_and_an_old_thread_shows_dashes() {
        let lineage = Lineage {
            root: "t1".into(),
            parent: "t1".into(),
            parent_checkpoint: "t1-c1".into(),
            parent_step: 1,
            edited: vec!["note".into()],
            checked: false,
        };
        let run = run_record("inv-2", 31);
        let lineage_value = serde_json::to_value(&lineage).unwrap();
        let fork = vec![
            checkpoint("t2", 1, json!({ "note": "re-run" }), 1, &["pay"], vec![], None)
                .with_metadata(LINEAGE_METADATA_KEY, lineage_value.clone()),
            checkpoint(
                "t2",
                2,
                json!({ "note": "re-run" }),
                2,
                &[],
                vec![StepRecord {
                    step: 2,
                    kind: StepKind::Completed,
                    nodes: vec![record("pay", 31, NodeStatus::Recorded, &["charge"], 0)],
                }],
                Some(&run),
            )
            .with_metadata(LINEAGE_METADATA_KEY, lineage_value),
        ];
        let old = vec![
            checkpoint("t3", 1, json!({}), 0, &["a"], vec![], None),
            checkpoint("t3", 2, json!({}), 1, &[], vec![], None),
        ];
        assert_eq!(
            (steps_text(&fork, &utc, &run_utc), steps_text(&old, &utc, &run_utc)),
            (
                "run 06:14:31 (inv-2)\n\
                 forked from t1 at step 1 (edited: note)\n\
                 Step  Time      Node  Status          Duration  Wrote\n\
                 2     06:14:31  pay   not sent again  -         charge\n"
                    .to_string(),
                "run - (this thread has no run records)\n\
                 Step  Time  Node  Status  Duration  Wrote\n\
                 0     -     -     -       -         -\n\
                 1     -     -     -       -         -\n"
                    .to_string(),
            )
        );
    }

    #[test]
    fn sets_parse_as_json_and_refuse_anything_else_escaped() {
        assert_eq!(
            parse_sets(&["note=\"re-run\"".into(), "invoice={\"amountCents\":5000}".into()])
                .unwrap(),
            BTreeMap::from([
                ("invoice".to_string(), json!({ "amountCents": 5000 })),
                ("note".to_string(), json!("re-run")),
            ])
        );
        let refused = parse_sets(&["note=re-run\u{1b}[2J".into()]).unwrap_err();
        assert_eq!(refused.to_string(), "not valid JSON: note=re-run\\u{1b}[2J");
        assert_eq!(
            parse_sets(&["=1".into()]).unwrap_err().to_string(),
            "--set takes key=<json>: =1"
        );
    }

    #[tokio::test]
    async fn a_fork_is_unchecked_and_says_what_already_happened() {
        let checkpointer = MemoryCheckpointer::new();
        for checkpoint in invoice_run() {
            checkpointer.save(&checkpoint).await.unwrap();
        }
        let text = fork_text(
            &checkpointer,
            "t1",
            1,
            "t2",
            BTreeMap::from([("note".to_string(), json!("re-run"))]),
        )
        .await
        .unwrap();
        assert_eq!(
            text,
            "Forked t1 at step 1 → t2; edited: note (not checked against the graph's channels); \
             already happened (answered from the record if unchanged): pay"
        );
        let fork = checkpointer.load("t2").await.unwrap().unwrap();
        assert_eq!(
            fork.lineage().unwrap(),
            Some(Lineage {
                root: "t1".into(),
                parent: "t1".into(),
                parent_checkpoint: "t1-c1".into(),
                parent_step: 1,
                edited: vec!["note".into()],
                checked: false,
            })
        );

        let mut errors = Vec::new();
        for (step, key, thread) in [(1, "amount", "t4"), (9, "note", "t5"), (3, "note", "t6")] {
            let edits = BTreeMap::from([(key.to_string(), json!(1))]);
            errors.push(
                fork_text(&checkpointer, "t1", step, thread, edits).await.unwrap_err().to_string(),
            );
        }
        assert_eq!(
            errors,
            vec![
                "not a key of this checkpoint: amount".to_string(),
                "no checkpoint 9 in t1".to_string(),
                "nothing runs after the last step".to_string(),
            ]
        );
    }

    #[test]
    fn control_characters_in_records_are_shown_escaped() {
        let run = run_record("inv-\u{1b}]0;x\u{7}", 0);
        let checkpoints = vec![checkpoint(
            "t1",
            1,
            json!({ "k": 1 }),
            1,
            &[],
            vec![StepRecord {
                step: 1,
                kind: StepKind::Completed,
                nodes: vec![record("dr\u{1b}[2Jaft", 1, NodeStatus::Done, &["k\u{9b}"], 0)],
            }],
            Some(&run),
        )];
        let text = steps_text(&checkpoints, &utc, &run_utc);
        assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "{text:?}");
        assert!(text.contains("dr\\u{1b}[2Jaft") && text.contains("k\\u{9b}"), "{text}");
    }
}
