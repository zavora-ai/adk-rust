//! Scenarios for the agent kernel (spec 3.2 §5, §11).
//!
//! Scripted, no keys: K1 and K5 on `ScriptedLlm` with a real kernel — durable workers, the
//! scheduler, SQLite stores and the control plane on a socket and loopback HTTP.
//! Live (`--live`): K3 and K5 through the `adk-rust up` binary on a real model.

mod live;
mod scripted;

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
];

/// The live scenarios.
const LIVE: &[&str] = &[
    "live_k5_approve_from_another_process",
    "live_k3_file_change_wakes",
    "live_k4_a_proposal_waits_for_approval",
];

fn verdict(result: &Result<Checks>) -> String {
    match result {
        Ok(checks) if checks.passed() => "PASS".to_string(),
        Ok(checks) => format!("FAIL ({})", checks.failures().join("; ")),
        Err(error) => format!("ERROR ({error})"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
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
        let bin = match bin.or_else(|| std::env::var("ADK_RUST_BIN").ok()) {
            Some(bin) => PathBuf::from(bin),
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
                let result = live::run(scenario, &bin, provider, model).await;
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
            .copied()
            .filter(|(name, _)| selected.is_empty() || selected.contains(name))
        {
            println!("  {name}");
            let result = scripted::run(name).await;
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
