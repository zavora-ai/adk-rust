//! Scenarios for the agent kernel (spec 3.2 §5, §11).
//!
//! Scripted, no keys: K1, K4, K5, K6, K7 and K8 on `ScriptedLlm` with a real kernel — durable
//! workers, the scheduler, SQLite stores, drivers and the control plane on a socket and
//! loopback HTTP.
//! Live (`--live`): K3, K4, K5, K6, K7, a parked driver question answered after a restart, and
//! K10, through the `adk-rust up` binary on a real model; K2, K8 and K9 on a real model with
//! the kernel in process, under a manual clock, held model calls and two replicas.
//! The managed form (spec 3.2 §7.13) and voice decisions on parked processes (§7.11) run in
//! process, scripted and live.

mod live;
mod live_in_process;
mod managed;
mod notes_driver;
mod scripted;
mod voice;

use std::path::PathBuf;

use anyhow::{Result, bail};

/// The outcome of one scenario: named checks, each passed or failed.
#[derive(Default)]
pub struct Checks {
    results: Vec<(String, bool)>,
}

impl Checks {
    /// Records `name` as passed or failed and prints it.
    pub fn check(&mut self, name: &str, passed: bool) {
        println!("    {} {name}", if passed { "ok  " } else { "FAIL" });
        self.results.push((name.to_string(), passed));
    }

    fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|(_, passed)| *passed)
    }

    fn failures(&self) -> Vec<String> {
        self.results.iter().filter(|(_, passed)| !passed).map(|(name, _)| name.clone()).collect()
    }

    /// `passed/total checks`.
    fn tally(&self) -> String {
        let passed = self.results.iter().filter(|(_, passed)| *passed).count();
        format!("{passed}/{} checks", self.results.len())
    }
}

/// The scripted scenarios, and what `main` does instead: it has no kernel.
const SCRIPTED: &[(&str, &str)] = &[
    (
        "k1_leave_it_running",
        "a schedule fires only while `adk-rust serve` runs; nothing keeps it across a restart",
    ),
    (
        "k4_it_proposes_its_own_work",
        "there is no agenda: a follow-up the agent mentions is prose nobody schedules or approves",
    ),
    (
        "k8_a_message_preempts_background_work",
        "a typed message queues behind background work; nothing bounds a partition or parks a run for a human turn",
    ),
    (
        "k5_one_job_many_windows",
        "a turn belongs to the terminal that runs it; there is no control plane to attach to",
    ),
    (
        "k6_drivers_you_can_trust",
        "an MCP server runs unsandboxed as configured, its `readOnlyHint` decides its class, and a changed tool keeps its rules",
    ),
    (
        "k7_partitions",
        "one harness serves everything: every agent reads every fact, and nothing is a crossing",
    ),
];

/// The live scenarios.
const LIVE: &[&str] = &[
    "live_k5_approve_from_another_process",
    "live_k2_sleep_is_free_for_an_hour",
    "live_k3_file_change_wakes",
    "live_k4_a_proposal_waits_for_approval",
    "live_k6_supervised_driver",
    "live_parked_question_after_restart",
    "live_managed_tenant_approval",
    "live_voice_parked_process",
    "live_k10_proc",
    "live_k8_a_message_preempts_background_work",
    "live_k9_one_switch_stops_everything",
    "live_k7_partitions",
];

fn verdict(result: &Result<Checks>) -> String {
    match result {
        Ok(checks) if checks.passed() => format!("PASS ({})", checks.tally()),
        Ok(checks) => format!("FAIL ({}: {})", checks.tally(), checks.failures().join("; ")),
        Err(error) => format!("ERROR ({error})"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Mounted as a driver, this binary is the notes MCP server; it prints nothing else.
    if std::env::args().nth(1).as_deref() == Some("serve-notes-driver") {
        return notes_driver::serve().await;
    }
    match std::env::var("HARNESS_ENV_FILE") {
        Ok(path) => {
            dotenvy::from_path_override(path).ok();
        }
        Err(_) => {
            dotenvy::dotenv().ok();
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value =
        |flag: &str| args.iter().position(|arg| arg == flag).and_then(|i| args.get(i + 1)).cloned();
    let live = args.iter().any(|arg| arg == "--live");
    let provider = value("--provider");
    let bin = value("--bin");
    let selected: Vec<&str> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .filter(|arg| Some(*arg) != provider.as_deref() && Some(*arg) != bin.as_deref())
        .collect();

    println!("adk-kernel scenarios (spec 3.2 §5, §11)\n");
    let mut failed = false;
    if live {
        let in_process = |name: &str| {
            managed::LIVE.contains(&name)
                || voice::LIVE.contains(&name)
                || live_in_process::LIVE.contains(&name)
        };
        let needs_bin = LIVE
            .iter()
            .copied()
            .filter(|name| selected.is_empty() || selected.contains(name))
            .any(|name| !in_process(name));
        let bin = match bin.or_else(|| std::env::var("ADK_RUST_BIN").ok()) {
            Some(bin) => PathBuf::from(bin),
            None if !needs_bin => PathBuf::new(),
            None => bail!(
                "the live scenarios run the `adk-rust` binary: pass --bin PATH or set ADK_RUST_BIN \
                 (build it with `cargo build -p adk-cli --bin adk-rust`)"
            ),
        };
        let providers: Vec<&str> = match &provider {
            Some(provider) => vec![provider.as_str()],
            None => vec!["anthropic", "gemini"],
        };
        let mut rows = Vec::new();
        for provider in providers {
            let Some(model) = live::model_for(provider) else {
                rows.push(format!("| — | {provider} | — | skipped: no key |"));
                continue;
            };
            for scenario in
                LIVE.iter().copied().filter(|name| selected.is_empty() || selected.contains(name))
            {
                println!("  {scenario} on {model}");
                let result = match (
                    managed::run_live(scenario, provider, model).await,
                    voice::run_live(scenario, provider, model).await,
                    live_in_process::run_live(scenario, provider, model).await,
                ) {
                    (Some(result), _, _) | (None, Some(result), _) | (None, None, Some(result)) => {
                        result
                    }
                    (None, None, None) => live::run(scenario, &bin, provider, model).await,
                };
                let shown = verdict(&result);
                failed |= !shown.starts_with("PASS");
                rows.push(format!("| `{scenario}` | {provider} | {model} | {shown} |"));
            }
        }
        println!("\n| Scenario | Provider | Model | Result |\n|---|---|---|---|");
        for row in rows {
            println!("{row}");
        }
    } else {
        let mut rows = Vec::new();
        for (name, on_main) in SCRIPTED
            .iter()
            .chain(managed::SCRIPTED)
            .chain(voice::SCRIPTED)
            .copied()
            .filter(|(name, _)| selected.is_empty() || selected.contains(name))
        {
            println!("  {name}");
            let result = match (managed::run_scripted(name).await, voice::run_scripted(name).await)
            {
                (Some(result), _) | (None, Some(result)) => result,
                (None, None) => scripted::run(name).await,
            };
            let shown = verdict(&result);
            failed |= !shown.starts_with("PASS");
            rows.push(format!("| `{name}` | not run, expected: {on_main} | {shown} |"));
        }
        println!("\n| Scenario | main (no kernel) | branch (`adk-kernel`) |\n|---|---|---|");
        for row in rows {
            println!("{row}");
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
