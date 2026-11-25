//! Scenarios S1–S19 of spec 3.7 §5 for `adk-vision`: the Document desk and phase A (S1–S8); the
//! Inspector, visual wakes, frame budgets, the live camera surface and native video of phase B
//! (S9–S13); the Screen copilot, the Inspector as a team, visual memory and the video analyst of
//! phase C (S14–S17); face blur on streams and the on-device profile (S18, S19).
//!
//! On the device, for real: PP-OCRv6 tiny reads, RF-DETR Nano and YuNet detect, SAM 2.1 Tiny
//! segments and SigLIP 2 embeds, each fetched by revision and SHA-256 through the model fetcher
//! on the first run (`HF_HUB_CACHE`); the conveyor clip streams over an in-process RTSP server.
//! In the cloud, on recorded responses: Claude, Gemini, Meta `sam-3.1` and NVIDIA NIM answer
//! through their real clients from a local server, so no key is needed. Every pass condition is
//! asserted on stores, egress records, ledgers, approvals and audit records, never on model
//! prose.

mod conveyor;
mod crm;
mod fixtures;
mod replay;
mod s10;
mod s11;
mod s12;
mod s13;
mod s14;
mod s15;
mod s16;
mod s17;
mod s18;
mod s19;
mod s1_s2;
mod s3_s4;
mod s5;
mod s6;
mod s7;
mod s8;
mod s9;
mod screen;

use std::path::PathBuf;

use anyhow::{Result, bail};

/// The outcome of one scenario: named checks, each passed or failed, and what it ran on.
#[derive(Default)]
pub struct Checks {
    results: Vec<(String, bool)>,
    notes: Vec<String>,
}

impl Checks {
    /// Records `name` as passed or failed and prints it.
    pub fn check(&mut self, name: &str, passed: bool) {
        println!("    {} {name}", if passed { "ok  " } else { "FAIL" });
        self.results.push((name.to_string(), passed));
    }

    /// Records what the scenario ran on, for the report.
    pub fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        println!("    ·    {note}");
        self.notes.push(note);
    }

    fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|(_, passed)| *passed)
    }

    fn failures(&self) -> Vec<String> {
        self.results.iter().filter(|(_, passed)| !passed).map(|(name, _)| name.clone()).collect()
    }

    fn tally(&self) -> String {
        let passed = self.results.iter().filter(|(_, passed)| *passed).count();
        format!("{passed}/{} checks", self.results.len())
    }
}

/// The scenarios, with the capability each proves (spec 3.7 §5).
const SCENARIOS: &[(&str, &str)] = &[
    ("s1_document_desk_local_first", "S1 Document desk, local first"),
    ("s2_escalate_the_crop_not_the_page", "S2 Escalate the crop, not the page"),
    ("s3_redact_before_it_leaves", "S3 Redact before it leaves"),
    ("s4_approve_with_the_image", "S4 Approve with the image"),
    ("s5_one_region_four_frames", "S5 One region, four frames"),
    ("s6_weights_you_can_trust", "S6 Weights you can trust"),
    ("s7_a_budget_per_image", "S7 A budget per image"),
    ("s8_same_tool_any_backend", "S8 Same tool, any backend"),
    ("s9_inspector", "S9 Inspector"),
    ("s10_wake_on_what_matters", "S10 Wake on what matters"),
    ("s11_a_budget_per_frame", "S11 A budget per frame"),
    ("s12_live_camera_surface", "S12 Live camera surface"),
    ("s13_native_video_two_ways", "S13 Native video, two ways"),
    ("s14_screen_copilot", "S14 Screen copilot"),
    ("s15_perceiver_reasoner_verifier", "S15 Perceiver, reasoner, verifier"),
    ("s16_visual_memory", "S16 Visual memory"),
    ("s17_video_analyst", "S17 Video analyst"),
    ("s18_blur_faces_on_streams", "S18 Blur faces on streams"),
    ("s19_off_the_network", "S19 Off the network"),
];

async fn run(name: &str, desk: &s1_s2::Shared) -> Result<Checks> {
    match name {
        "s1_document_desk_local_first" => s1_s2::s1(desk).await,
        "s2_escalate_the_crop_not_the_page" => s1_s2::s2(desk).await,
        "s3_redact_before_it_leaves" => s3_s4::s3().await,
        "s4_approve_with_the_image" => s3_s4::s4().await,
        "s5_one_region_four_frames" => s5::run().await,
        "s6_weights_you_can_trust" => s6::run().await,
        "s7_a_budget_per_image" => s7::run().await,
        "s8_same_tool_any_backend" => s8::run().await,
        "s9_inspector" => s9::run().await,
        "s10_wake_on_what_matters" => s10::run().await,
        "s11_a_budget_per_frame" => s11::run().await,
        "s12_live_camera_surface" => s12::run().await,
        "s13_native_video_two_ways" => s13::run().await,
        "s14_screen_copilot" => s14::run().await,
        "s15_perceiver_reasoner_verifier" => s15::run().await,
        "s16_visual_memory" => s16::run().await,
        "s17_video_analyst" => s17::run().await,
        "s18_blur_faces_on_streams" => s18::run().await,
        "s19_off_the_network" => s19::run().await,
        other => bail!("no scenario `{other}`"),
    }
}

fn verdict(result: &Result<Checks>) -> String {
    match result {
        Ok(checks) if checks.passed() => format!("PASS ({})", checks.tally()),
        Ok(checks) => format!("FAIL ({}: {})", checks.tally(), checks.failures().join("; ")),
        Err(error) => format!("ERROR ({error:#})"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // S19's sandboxed run of S1: this binary again, inside the operating system's sandbox.
    if let Some(report) = s19::child_report(&args) {
        return s19::child(std::path::Path::new(report)).await;
    }
    if args.iter().any(|arg| arg == "--write-s11-clips") {
        return s11::write_clips();
    }
    let report = args
        .iter()
        .position(|arg| arg == "--report")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let selected: Vec<&str> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .filter(|arg| report.as_ref().is_none_or(|r| r.as_os_str() != *arg))
        .collect();
    for name in &selected {
        if !SCENARIOS.iter().any(|(id, _)| id == name) {
            let names: Vec<&str> = SCENARIOS.iter().map(|s| s.0).collect();
            bail!("no scenario `{name}`; the scenarios are {names:?}");
        }
    }

    println!("adk-vision scenarios (spec 3.7 §5, §11)\n");
    let desk = s1_s2::Shared::default();
    let mut rows = Vec::new();
    for (name, title) in SCENARIOS {
        if !selected.is_empty() && !selected.contains(name) {
            continue;
        }
        println!("{title} ({name})");
        let result = run(name, &desk).await;
        let line = verdict(&result);
        println!("  {line}\n");
        let notes = result.as_ref().map(|c| c.notes.join("; ")).unwrap_or_default();
        rows.push((*title, line, notes, result.as_ref().is_ok_and(Checks::passed)));
    }

    println!("| Scenario | Result |\n|---|---|");
    for (title, line, _, _) in &rows {
        println!("| {title} | {line} |");
    }
    if let Some(path) = report {
        let mut text = String::from(
            "## adk-vision scenarios (spec 3.7)\n\n| Scenario | Result | Ran on |\n|---|---|---|\n",
        );
        for (title, line, notes, _) in &rows {
            text.push_str(&format!("| {title} | {line} | {notes} |\n"));
        }
        std::fs::write(&path, text)?;
    }
    if rows.iter().any(|row| !row.3) {
        std::process::exit(1);
    }
    Ok(())
}
