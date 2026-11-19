//! Vision benchmarks (spec 3.7 §7.17): latency, accuracy and cost per `adk-vision` backend,
//! side by side, with the machine and versions they ran on.
//!
//! A [`VisionBackend`] is a backend behind a [`VisionEvalAgent`]: the agent answers the
//! committed vision eval sets, so the accuracy columns are the same scores `cargo adk eval`
//! reports for that backend ([`adk_eval::vision`]). For each backend the bench measures:
//!
//! | Column | How |
//! |--------|-----|
//! | Cold load | the backend factory's time to verify the weights and build its sessions |
//! | p50, p95 | wall time per image through the runtime at three input sizes (long edge 512, 1024, 2048 px by default), one changed pixel per run so no embedding cache answers |
//! | Throughput | images per second, sequential, at the middle size |
//! | Peak RSS | the process's resident set after the timed runs |
//! | Accuracy | each configured vision score's mean over the eval set's cases |
//! | Cost | the run's metered spend per 1,000 images: zero for local backends, catalog prices for cloud ones |
//!
//! Cloud backends replay recorded responses ([`RunMode::Recorded`]): accuracy and cost are
//! what the live call produced, and latency is not measured. One live call gives a
//! [`RunMode::Live`] row with a single timing.
//!
//! # Example
//!
//! ```
//! use adk_bench::vision::{MachineInfo, VisionBenchReport};
//!
//! let report = VisionBenchReport::new(MachineInfo::detect(), Vec::new());
//! assert!(report.to_markdown().contains("| CPU |"));
//! ```

pub mod backends;
mod embed;
mod fixtures;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{BudgetTracker, ModelCallScope, RunBudget, UsageUnit};
use adk_eval::vision::VisionCriteria;
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator, TestFile};
use adk_vision::eval::VisionEvalAgent;
use adk_vision::{DecodedImage, SourceImage, VisionCx};
use serde::{Deserialize, Serialize};

pub use embed::{EmbedWorkload, run_embedder};
pub use fixtures::{FixtureSet, eval_dir, scaled};

use crate::error::{BenchError, Result};
use crate::metrics::compute_stats;

/// How a row's numbers were produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunMode {
    /// On this machine.
    Local,
    /// Recorded provider responses served to the real client: accuracy and cost only.
    Recorded,
    /// Live provider calls.
    Live,
}

impl RunMode {
    fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Recorded => "recorded",
            Self::Live => "live",
        }
    }
}

/// A model a backend loads, for the report's provenance table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// The manifest id, or the provider's model id.
    pub id: String,
    /// The pinned revision, for weights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The licence.
    pub licence: String,
}

/// A backend ready to measure.
pub struct VisionBackend {
    /// The row name, such as `rf-detr-n`.
    pub id: String,
    /// The runtime and execution provider, such as `ort 2.0.0-rc.13, CPU`.
    pub runtime: String,
    /// The models it loads.
    pub models: Vec<ModelRef>,
    /// How its numbers are produced.
    pub mode: RunMode,
    /// Time to verify weights and build sessions.
    pub cold_load: Option<Duration>,
    /// The agent answering eval cases, and the budget its calls are metered against.
    pub agent: Arc<VisionEvalAgent>,
    budget: Arc<BudgetTracker>,
}

impl VisionBackend {
    /// A backend whose eval agent is built from `make`, metered against a fresh budget.
    pub fn new(
        id: impl Into<String>,
        runtime: impl Into<String>,
        mode: RunMode,
        make: impl FnOnce(VisionCx) -> VisionEvalAgent,
    ) -> Self {
        let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
        let cx = VisionCx::new().with_scope(ModelCallScope::new().with_budget(budget.clone()));
        Self {
            id: id.into(),
            runtime: runtime.into(),
            models: Vec::new(),
            mode,
            cold_load: None,
            agent: Arc::new(make(cx)),
            budget,
        }
    }

    /// Records the models it loads.
    pub fn with_models(mut self, models: Vec<ModelRef>) -> Self {
        self.models = models;
        self
    }

    /// Records the cold-load time.
    pub fn with_cold_load(mut self, cold_load: Duration) -> Self {
        self.cold_load = Some(cold_load);
        self
    }
}

/// What a backend is measured on: an eval set for accuracy and one image for latency.
pub struct VisionWorkload {
    /// The eval set's name, such as `shapes-segment`.
    pub name: String,
    /// The loaded eval set, its fixtures verified.
    pub eval_set: TestFile,
    /// The scores to report: each with a threshold of 0, so it is computed and never fails.
    pub scores: Vec<&'static str>,
    /// The image and turn text the latency runs send.
    pub latency: Option<(SourceImage, String)>,
}

/// How long and how often to measure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionBenchConfig {
    /// Untimed runs per input size.
    pub warmup: usize,
    /// Timed runs per input size.
    pub runs: usize,
    /// Long edges, in pixels, the latency image is scaled to.
    pub long_edges: Vec<u32>,
}

impl Default for VisionBenchConfig {
    fn default() -> Self {
        Self { warmup: 2, runs: 10, long_edges: vec![512, 1024, 2048] }
    }
}

/// Latency at one input size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeLatency {
    /// The input's long edge in pixels.
    pub long_edge: u32,
    /// Median milliseconds per image.
    pub p50_ms: f64,
    /// 95th-percentile milliseconds per image.
    pub p95_ms: f64,
    /// Images per second, sequential.
    pub per_second: f64,
    /// Timed runs.
    pub runs: usize,
}

/// One backend's results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionBenchRow {
    /// The backend.
    pub backend: String,
    /// The task, such as `segment`.
    pub task: String,
    /// The eval set its accuracy is measured on.
    pub workload: String,
    /// How the numbers were produced.
    pub mode: RunMode,
    /// The runtime and execution provider.
    pub runtime: String,
    /// The models it loads.
    pub models: Vec<ModelRef>,
    /// Cold load in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_load_ms: Option<f64>,
    /// Latency per input size.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latency: Vec<SizeLatency>,
    /// Peak resident set in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    /// Score name to mean over the eval set's cases.
    pub accuracy: BTreeMap<String, f64>,
    /// Cases scored.
    pub cases: usize,
    /// Metered spend per 1,000 images in US dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_images_usd: Option<f64>,
    /// What else a reader needs to know about the row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The machine a report was measured on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineInfo {
    /// When the run started, RFC 3339.
    pub timestamp: String,
    /// Operating system and version.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
    /// CPU model.
    pub cpu: String,
    /// Logical cores.
    pub cores: usize,
    /// Physical memory in bytes, when it can be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// `adk-bench`'s version.
    pub adk_version: String,
    /// Runtime library versions, by name.
    #[serde(default)]
    pub runtimes: BTreeMap<String, String>,
}

impl MachineInfo {
    /// Reads this machine's OS, CPU and memory.
    pub fn detect() -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            os: os_version(),
            arch: std::env::consts::ARCH.to_string(),
            cpu: cpu_model(),
            cores: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
            memory_bytes: memory_bytes(),
            adk_version: env!("CARGO_PKG_VERSION").to_string(),
            runtimes: BTreeMap::new(),
        }
    }

    /// Records a runtime library's version.
    pub fn with_runtime(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.runtimes.insert(name.into(), version.into());
        self
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn os_version() -> String {
    let os = std::env::consts::OS;
    let version = match os {
        "macos" => command_output("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}")),
        "linux" => std::fs::read_to_string("/etc/os-release").ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                .map(|name| name.trim_matches('"').to_string())
        }),
        _ => None,
    };
    version.unwrap_or_else(|| os.to_string())
}

fn cpu_model() -> String {
    let model = match std::env::consts::OS {
        "macos" => command_output("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "linux" => std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, name)| name.trim().to_string())
        }),
        _ => None,
    };
    model.unwrap_or_else(|| "unknown".to_string())
}

fn memory_bytes() -> Option<u64> {
    match std::env::consts::OS {
        "macos" => command_output("sysctl", &["-n", "hw.memsize"])?.parse().ok(),
        "linux" => std::fs::read_to_string("/proc/meminfo").ok().and_then(|text| {
            let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
            let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            Some(kib * 1024)
        }),
        _ => None,
    }
}

/// Measures one backend on one workload.
///
/// # Errors
///
/// Returns [`BenchError::Vision`] when the eval set cannot be scored or a latency run fails.
pub async fn run_backend(
    config: &VisionBenchConfig,
    backend: &VisionBackend,
    workload: &VisionWorkload,
) -> Result<VisionBenchRow> {
    let mut notes = Vec::new();
    let (accuracy, cases) = accuracy(backend, workload).await?;
    let usage = backend.budget.usage();
    let images = backend.budget.units().get(UsageUnit::Images);
    let cost_per_1k_images_usd =
        (images > 0).then(|| usage.cost_micro_usd as f64 / 1_000_000.0 / images as f64 * 1000.0);

    let mut latency = Vec::new();
    let mut peak_rss_bytes = None;
    match (&workload.latency, backend.mode) {
        (Some((image, text)), RunMode::Local) => {
            for &edge in &config.long_edges {
                let scaled = scaled(image, edge)?;
                let (size, peak) = time_answers(config, backend, &scaled, text, edge).await?;
                latency.push(size);
                peak_rss_bytes = peak_rss_bytes.max(peak);
            }
        }
        (Some(_), RunMode::Recorded) => {
            notes.push("latency not measured: recorded responses".to_string());
        }
        (Some(_), RunMode::Live) | (None, _) => {}
    }
    if cost_per_1k_images_usd.is_none() && backend.mode != RunMode::Local {
        notes.push("no metered images".to_string());
    }
    Ok(VisionBenchRow {
        backend: backend.id.clone(),
        task: backend.agent.task().tool_name().trim_start_matches("vision_").to_string(),
        workload: workload.name.clone(),
        mode: backend.mode,
        runtime: backend.runtime.clone(),
        models: backend.models.clone(),
        cold_load_ms: backend.cold_load.map(|d| d.as_secs_f64() * 1000.0),
        latency,
        peak_rss_bytes,
        accuracy,
        cases,
        cost_per_1k_images_usd,
        notes,
    })
}

/// Each configured score's mean over the eval set, scored by `adk-eval`.
async fn accuracy(
    backend: &VisionBackend,
    workload: &VisionWorkload,
) -> Result<(BTreeMap<String, f64>, usize)> {
    let mut criteria = VisionCriteria::default();
    for name in &workload.scores {
        let slot = match *name {
            "region_iou" => &mut criteria.region_iou,
            "box_hit_rate" => &mut criteria.box_hit_rate,
            "mask_iou" => &mut criteria.mask_iou,
            "ocr_char_accuracy" => &mut criteria.ocr_char_accuracy,
            "ocr_word_accuracy" => &mut criteria.ocr_word_accuracy,
            "field_exact_match" => &mut criteria.field_exact_match,
            "field_normalized_match" => &mut criteria.field_normalized_match,
            "detection_precision" => &mut criteria.detection_precision,
            "detection_recall" => &mut criteria.detection_recall,
            "detection_ap" => &mut criteria.detection_ap,
            "detection_map" => &mut criteria.detection_map,
            "grounding_hit_rate" => &mut criteria.grounding_hit_rate,
            "count_accuracy" => &mut criteria.count_accuracy,
            other => return Err(BenchError::Vision(format!("no vision score named {other}"))),
        };
        *slot = Some(0.0);
    }
    let config =
        EvaluationConfig::with_criteria(EvaluationCriteria::default().with_vision(criteria));
    let evaluator = Evaluator::new(config);
    let agent: Arc<dyn adk_core::Agent> = backend.agent.clone();
    let report = evaluator
        .evaluate_test_file(agent, &workload.eval_set)
        .await
        .map_err(|e| BenchError::Vision(format!("{} on {}: {e}", backend.id, workload.name)))?;
    let mut sums: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for result in &report.results {
        if let Some(failure) = result.failures.iter().find(|f| f.criterion == "execution") {
            return Err(BenchError::Vision(format!(
                "{} could not answer {} case {}: {}",
                backend.id,
                workload.name,
                result.eval_id,
                failure.details.as_deref().unwrap_or("no details")
            )));
        }
        for (name, value) in &result.scores {
            let entry = sums.entry(name.clone()).or_insert((0.0, 0));
            entry.0 += value;
            entry.1 += 1;
        }
    }
    let means = sums.into_iter().map(|(k, (sum, n))| (k, sum / n as f64)).collect();
    Ok((means, report.results.len()))
}

/// Times `runs` answers on `image`, each with one pixel changed so no cache answers.
async fn time_answers(
    config: &VisionBenchConfig,
    backend: &VisionBackend,
    image: &SourceImage,
    text: &str,
    edge: u32,
) -> Result<(SizeLatency, Option<u64>)> {
    let cx = VisionCx::new();
    let mut timings = Vec::with_capacity(config.runs);
    let mut peak = None;
    for run in 0..config.warmup + config.runs {
        let variant = nudged(image, run)?;
        let started = Instant::now();
        backend
            .agent
            .answer(&cx, &[variant], text)
            .await
            .map_err(|e| BenchError::Vision(format!("{} at {edge} px: {e}", backend.id)))?;
        if run >= config.warmup {
            timings.push(started.elapsed());
            peak = peak.max(crate::memory::current_rss_bytes().ok());
        }
    }
    let stats = compute_stats(&timings);
    let ms = |us: u64| us as f64 / 1000.0;
    let per_second = if stats.mean_us == 0 { 0.0 } else { 1_000_000.0 / stats.mean_us as f64 };
    let size = SizeLatency {
        long_edge: edge,
        p50_ms: ms(stats.median_us),
        p95_ms: ms(stats.p95_us),
        per_second,
        runs: timings.len(),
    };
    Ok((size, peak))
}

/// `image` with its top-left pixel's red channel set to `run`, so each run has its own digest.
fn nudged(image: &SourceImage, run: usize) -> Result<SourceImage> {
    let (w, h) = image.size();
    let mut rgba = image.pixels().rgba().to_vec();
    if let Some(red) = rgba.first_mut() {
        *red = (run % 256) as u8;
    }
    let pixels = DecodedImage::from_rgba(w, h, rgba).map_err(vision_error)?;
    SourceImage::from_pixels(pixels).map_err(vision_error)
}

fn vision_error(e: adk_vision::VisionError) -> BenchError {
    BenchError::Vision(e.to_string())
}

/// Results for every backend, with the machine they ran on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionBenchReport {
    /// The machine.
    pub machine: MachineInfo,
    /// The run settings.
    #[serde(default)]
    pub config: VisionBenchConfig,
    /// One row per backend and workload.
    pub rows: Vec<VisionBenchRow>,
}

impl VisionBenchReport {
    /// A report.
    pub fn new(machine: MachineInfo, rows: Vec<VisionBenchRow>) -> Self {
        Self { machine, config: VisionBenchConfig::default(), rows }
    }

    /// The report as Markdown: machine, latency, accuracy and cost, and model provenance.
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write;

        let m = &self.machine;
        let mut out = String::new();
        let _ = writeln!(out, "| Machine | |\n|---|---|");
        let _ = writeln!(out, "| Run | {} |", m.timestamp);
        let _ = writeln!(out, "| OS | {} ({}) |", m.os, m.arch);
        let _ = writeln!(out, "| CPU | {}, {} logical cores |", m.cpu, m.cores);
        if let Some(bytes) = m.memory_bytes {
            let _ = writeln!(out, "| Memory | {:.0} GiB |", bytes as f64 / (1u64 << 30) as f64);
        }
        let _ = writeln!(out, "| adk-rust | {} |", m.adk_version);
        for (name, version) in &m.runtimes {
            let _ = writeln!(out, "| {name} | {version} |");
        }
        let c = &self.config;
        let _ = writeln!(
            out,
            "| Settings | {} warm-up and {} timed runs per size; sizes are the long edge |",
            c.warmup, c.runs
        );

        let edges: Vec<u32> = c.long_edges.clone();
        let _ = write!(out, "\n| Backend | Task | Runtime | Cold load |");
        for edge in &edges {
            let _ = write!(out, " {edge} px p50 / p95 |");
        }
        let _ = writeln!(out, " Throughput | Peak RSS |");
        let _ = writeln!(out, "|{}", "---|".repeat(6 + edges.len()));
        for row in self.rows.iter().filter(|r| !r.latency.is_empty() || r.mode == RunMode::Live) {
            let cold =
                row.cold_load_ms.map_or("—".to_string(), |ms| format!("{:.2} s", ms / 1000.0));
            let _ = write!(out, "| {} | {} | {} | {cold} |", row.backend, row.task, row.runtime);
            for edge in &edges {
                match row.latency.iter().find(|l| l.long_edge == *edge) {
                    Some(l) => {
                        let _ = write!(out, " {} / {} |", fmt_ms(l.p50_ms), fmt_ms(l.p95_ms));
                    }
                    None => out.push_str(" — |"),
                }
            }
            let middle = edges.get(edges.len() / 2).copied();
            let throughput = row
                .latency
                .iter()
                .find(|l| Some(l.long_edge) == middle)
                .map_or("—".to_string(), |l| format!("{:.1} img/s", l.per_second));
            let rss = row.peak_rss_bytes.map_or("—".to_string(), |b| {
                format!("{:.0} MiB", b as f64 / (1u64 << 20) as f64)
            });
            let _ = writeln!(out, " {throughput} | {rss} |");
        }

        let _ =
            writeln!(out, "\n| Backend | Mode | Eval set | Cases | Scores | Cost / 1k images |");
        let _ = writeln!(out, "|---|---|---|---|---|---|");
        for row in &self.rows {
            let scores = row
                .accuracy
                .iter()
                .map(|(name, value)| format!("{name} {value:.3}"))
                .collect::<Vec<_>>()
                .join(", ");
            let cost =
                row.cost_per_1k_images_usd.map_or("—".to_string(), |usd| format!("${usd:.2}"));
            let mut notes = row.notes.clone();
            if let Some(runs) = row.latency.first().map(|l| l.runs).filter(|r| *r != c.runs) {
                notes.push(format!("{runs} timed runs per size"));
            }
            let mut mode = row.mode.label().to_string();
            if !notes.is_empty() {
                mode = format!("{mode} ({})", notes.join("; "));
            }
            let _ = writeln!(
                out,
                "| {} | {mode} | {} | {} | {scores} | {cost} |",
                row.backend, row.workload, row.cases
            );
        }

        let _ = writeln!(out, "\n| Backend | Model | Revision | Licence |\n|---|---|---|---|");
        for row in &self.rows {
            for model in &row.models {
                let revision = model
                    .revision
                    .as_deref()
                    .map_or("—".to_string(), |r| format!("`{}`", r.get(..12).unwrap_or(r)));
                let _ = writeln!(
                    out,
                    "| {} | `{}` | {revision} | {} |",
                    row.backend, model.id, model.licence
                );
            }
        }
        out
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 { format!("{:.2} s", ms / 1000.0) } else { format!("{ms:.1} ms") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(backend: &str, mode: RunMode) -> VisionBenchRow {
        VisionBenchRow {
            backend: backend.to_string(),
            task: "segment".to_string(),
            workload: "shapes-segment".to_string(),
            mode,
            runtime: "ort, CPU".to_string(),
            models: vec![ModelRef {
                id: "efficientsam-ti".to_string(),
                revision: Some("1cf49585c39567bfc49e991ab8eb31f491ad4877".to_string()),
                licence: "Apache-2.0".to_string(),
            }],
            cold_load_ms: Some(1500.0),
            latency: vec![
                SizeLatency {
                    long_edge: 512,
                    p50_ms: 80.0,
                    p95_ms: 95.5,
                    per_second: 12.0,
                    runs: 10,
                },
                SizeLatency {
                    long_edge: 1024,
                    p50_ms: 1200.0,
                    p95_ms: 1300.0,
                    per_second: 0.8,
                    runs: 10,
                },
            ],
            peak_rss_bytes: Some(512 << 20),
            accuracy: BTreeMap::from([("mask_iou".to_string(), 0.9512)]),
            cases: 1,
            cost_per_1k_images_usd: Some(0.0),
            notes: Vec::new(),
        }
    }

    #[test]
    fn markdown_has_a_row_per_backend_in_each_table() {
        let mut machine = MachineInfo::detect().with_runtime("onnxruntime", "1.23.0");
        machine.cpu = "Test CPU".to_string();
        let mut report = VisionBenchReport::new(
            machine,
            vec![row("efficientsam-ti", RunMode::Local), {
                let mut r = row("sam-3.1", RunMode::Recorded);
                r.latency.clear();
                r.notes.push("latency not measured: recorded responses".to_string());
                r.cost_per_1k_images_usd = Some(2.5);
                r
            }],
        );
        report.config.long_edges = vec![512, 1024];
        let md = report.to_markdown();
        assert!(md.contains("| CPU | Test CPU,"), "{md}");
        assert!(md.contains("| onnxruntime | 1.23.0 |"), "{md}");
        assert!(
            md.contains(
                "| efficientsam-ti | segment | ort, CPU | 1.50 s | 80.0 ms / 95.5 ms | 1.20 s / 1.30 s | 0.8 img/s | 512 MiB |"
            ),
            "{md}"
        );
        // A recorded row has no latency line, only accuracy and cost.
        assert!(!md.contains("| sam-3.1 | segment |"), "{md}");
        assert!(
            md.contains(
                "| sam-3.1 | recorded (latency not measured: recorded responses) | shapes-segment | 1 | mask_iou 0.951 | $2.50 |"
            ),
            "{md}"
        );
        assert!(
            md.contains("| efficientsam-ti | `efficientsam-ti` | `1cf49585c395` | Apache-2.0 |")
        );
    }

    #[test]
    fn nudged_images_have_their_own_digests() {
        let image = SourceImage::from_pixels(DecodedImage::filled(4, 4, [255; 4])).unwrap();
        let a = nudged(&image, 1).unwrap();
        let b = nudged(&image, 2).unwrap();
        assert_ne!(a.digest(), b.digest());
        assert_eq!(a.size(), (4, 4));
    }
}
