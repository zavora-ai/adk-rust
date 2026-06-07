//! Result output formatting (JSON, table, markdown).
//!
//! Provides formatters for [`BenchmarkResult`] and [`ComparisonResult`] in three
//! output formats: JSON (machine-readable), table (human-readable terminal), and
//! markdown (publishable documentation).
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::{format_result, format_comparison, OutputFormat};
//!
//! let output = format_result(&result, OutputFormat::Table);
//! println!("{output}");
//!
//! let comparison_output = format_comparison(&comparison, OutputFormat::Markdown);
//! println!("{comparison_output}");
//! ```

use crate::config::OutputFormat;
use crate::external::ExternalMetricsOutput;
use crate::metrics::BenchmarkResult;

/// Comparison result combining ADK-Rust and external framework metrics.
///
/// Used by [`format_comparison`] to produce side-by-side framework comparison
/// output in any supported format.
#[derive(Debug, Clone)]
pub struct ComparisonResult {
    /// ADK-Rust benchmark result.
    pub adk_result: BenchmarkResult,
    /// External framework metrics collected via the EBP protocol.
    pub external_results: Vec<ExternalMetricsOutput>,
}

/// Formats benchmark results into the specified output format.
///
/// # Arguments
///
/// * `result` - The benchmark result to format.
/// * `format` - The desired output format (JSON, Table, or Markdown).
///
/// # Returns
///
/// A formatted string representation of the benchmark result.
pub fn format_result(result: &BenchmarkResult, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => format_json(result),
        OutputFormat::Table => format_table(result),
        OutputFormat::Markdown => format_markdown(result),
    }
}

/// Formats comparison results including external frameworks.
///
/// # Arguments
///
/// * `comparison` - The comparison result containing ADK-Rust and external metrics.
/// * `format` - The desired output format (JSON, Table, or Markdown).
///
/// # Returns
///
/// A formatted string with framework comparison data.
pub fn format_comparison(comparison: &ComparisonResult, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => format_comparison_json(comparison),
        OutputFormat::Table => format_comparison_table(comparison),
        OutputFormat::Markdown => format_comparison_markdown(comparison),
    }
}

// ─── JSON Formatters ─────────────────────────────────────────────────────────

fn format_json(result: &BenchmarkResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

fn format_comparison_json(comparison: &ComparisonResult) -> String {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ComparisonJson<'a> {
        adk_result: &'a BenchmarkResult,
        external_results: &'a [ExternalMetricsOutput],
    }

    let json = ComparisonJson {
        adk_result: &comparison.adk_result,
        external_results: &comparison.external_results,
    };

    serde_json::to_string_pretty(&json).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

// ─── Table Formatters ────────────────────────────────────────────────────────

fn format_table(result: &BenchmarkResult) -> String {
    let mut out = String::new();

    // Header
    out.push_str(&format!(
        "Benchmark: {} | Model: {} | Iterations: {}\n",
        result.workload_name, result.model, result.iterations
    ));
    out.push_str(&format!(
        "Date: {} | ADK: {} | Rust: {} | OS: {} ({})\n",
        result.metadata.timestamp,
        result.metadata.adk_version,
        result.metadata.rust_version,
        result.metadata.os,
        result.metadata.arch,
    ));
    out.push_str(&separator_line(72));

    // Cold Start
    out.push_str("\n  Cold Start\n");
    out.push_str(&format_duration_stats_table(&result.cold_start));

    // Agent Loop Overhead
    out.push_str("\n  Agent Loop Overhead\n");
    out.push_str(&format_duration_stats_table(&result.agent_loop_overhead));

    // Throughput (if present)
    if let Some(ref throughput) = result.throughput {
        out.push_str("\n  Throughput\n");
        out.push_str(&format!("    {:>12}  {:>14}\n", "Concurrency", "Agents/s"));
        out.push_str(&format!("    {}  {}\n", "-".repeat(12), "-".repeat(14)));
        for level in &throughput.levels {
            out.push_str(&format!(
                "    {:>12}  {:>11.2} /s\n",
                level.concurrency, level.agents_per_second
            ));
        }
    }

    // Memory (if present)
    if let Some(ref memory) = result.memory {
        out.push_str("\n  Memory\n");
        out.push_str(&format!("    Peak RSS:        {}\n", format_bytes(memory.peak_rss_bytes)));
        if let Some(per_agent) = memory.per_agent_bytes {
            out.push_str(&format!("    Per-Agent:       {}\n", format_bytes(per_agent)));
        }
        out.push_str(&format!("    Samples:         {}\n", memory.sample_count));
    }

    // Token Overhead (if present)
    if let Some(ref tokens) = result.token_overhead {
        out.push_str("\n  Token Overhead\n");
        out.push_str(&format!("    Total tokens:    {}\n", tokens.total_tokens));
        out.push_str(&format!("    User content:    {}\n", tokens.user_content_tokens));
        out.push_str(&format!(
            "    Overhead:        {} ({:.1}%)\n",
            tokens.overhead_tokens, tokens.overhead_percentage
        ));
        out.push_str(&format!(
            "    Breakdown:       system={}, tools={}, wrapper={}\n",
            tokens.breakdown.system_prompt_tokens,
            tokens.breakdown.tool_schema_tokens,
            tokens.breakdown.framework_wrapper_tokens,
        ));
    }

    out
}

fn format_duration_stats_table(stats: &crate::metrics::DurationStats) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "    {:>12}  {:>12}  {:>12}  {:>12}  {:>8}\n",
        "Mean", "P95", "Min", "Max", "CV"
    ));
    out.push_str(&format!(
        "    {}  {}  {}  {}  {}\n",
        "-".repeat(12),
        "-".repeat(12),
        "-".repeat(12),
        "-".repeat(12),
        "-".repeat(8)
    ));
    out.push_str(&format!(
        "    {:>12}  {:>12}  {:>12}  {:>12}  {:>7.1}%\n",
        format_us(stats.mean_us),
        format_us(stats.p95_us),
        format_us(stats.min_us),
        format_us(stats.max_us),
        stats.coefficient_of_variation * 100.0,
    ));
    out
}

fn format_comparison_table(comparison: &ComparisonResult) -> String {
    let mut out = String::new();
    let adk = &comparison.adk_result;

    out.push_str(&format!("Framework Comparison: {} | Model: {}\n", adk.workload_name, adk.model));
    out.push_str(&separator_line(80));

    // Header row
    out.push_str(&format!(
        "\n  {:>20}  {:>14}  {:>14}  {:>14}\n",
        "Metric", "ADK-Rust", "Framework", "Δ"
    ));
    out.push_str(&format!(
        "  {}  {}  {}  {}\n",
        "-".repeat(20),
        "-".repeat(14),
        "-".repeat(14),
        "-".repeat(14)
    ));

    for ext in &comparison.external_results {
        out.push_str(&format!("\n  vs {}\n", ext.framework));
        out.push_str(&format!(
            "  {}  {}  {}  {}\n",
            "-".repeat(20),
            "-".repeat(14),
            "-".repeat(14),
            "-".repeat(14)
        ));

        // Cold Start (mean)
        let adk_cold = adk.cold_start.mean_us;
        let ext_cold = ext.cold_start_us;
        out.push_str(&format!(
            "  {:>20}  {:>14}  {:>14}  {:>14}\n",
            "Cold Start (mean)",
            format_us(adk_cold),
            format_us(ext_cold),
            format_delta(adk_cold, ext_cold),
        ));

        // Loop Overhead (mean)
        let adk_loop = adk.agent_loop_overhead.mean_us;
        let ext_loop = ext.loop_overhead.mean_us;
        out.push_str(&format!(
            "  {:>20}  {:>14}  {:>14}  {:>14}\n",
            "Loop Overhead (mean)",
            format_us(adk_loop),
            format_us(ext_loop),
            format_delta(adk_loop, ext_loop),
        ));

        // Loop Overhead (P95)
        let adk_p95 = adk.agent_loop_overhead.p95_us;
        let ext_p95 = ext.loop_overhead.p95_us;
        out.push_str(&format!(
            "  {:>20}  {:>14}  {:>14}  {:>14}\n",
            "Loop Overhead (P95)",
            format_us(adk_p95),
            format_us(ext_p95),
            format_delta(adk_p95, ext_p95),
        ));

        // Throughput (if available)
        if let (Some(adk_tp), Some(ext_tp)) = (&adk.throughput, ext.throughput_agents_per_sec)
            && let Some(first_level) = adk_tp.levels.first()
        {
            out.push_str(&format!(
                "  {:>20}  {:>11.2} /s  {:>11.2} /s  {:>14}\n",
                "Throughput",
                first_level.agents_per_second,
                ext_tp,
                format_throughput_delta(first_level.agents_per_second, ext_tp),
            ));
        }

        // Memory (if available)
        if let (Some(adk_mem), Some(ext_mem)) = (&adk.memory, ext.peak_rss_bytes) {
            out.push_str(&format!(
                "  {:>20}  {:>14}  {:>14}  {:>14}\n",
                "Peak RSS",
                format_bytes(adk_mem.peak_rss_bytes),
                format_bytes(ext_mem),
                format_bytes_delta(adk_mem.peak_rss_bytes, ext_mem),
            ));
        }
    }

    out
}

// ─── Markdown Formatters ─────────────────────────────────────────────────────

fn format_markdown(result: &BenchmarkResult) -> String {
    let mut out = String::new();

    out.push_str(&format!("## Benchmark Results: {}\n\n", result.workload_name));
    out.push_str(&format!(
        "**Model:** {} | **Iterations:** {} | **Date:** {}\n\n",
        result.model, result.iterations, result.metadata.timestamp
    ));

    // Core metrics table
    out.push_str("### Latency Metrics\n\n");
    out.push_str("| Metric | Mean | P95 | P99 | Min | Max | CV |\n");
    out.push_str("|--------|------|-----|-----|-----|-----|----|\n");
    out.push_str(&format!(
        "| Cold Start | {} | {} | {} | {} | {} | {:.1}% |\n",
        format_us(result.cold_start.mean_us),
        format_us(result.cold_start.p95_us),
        format_us(result.cold_start.p99_us),
        format_us(result.cold_start.min_us),
        format_us(result.cold_start.max_us),
        result.cold_start.coefficient_of_variation * 100.0,
    ));
    out.push_str(&format!(
        "| Agent Loop Overhead | {} | {} | {} | {} | {} | {:.1}% |\n",
        format_us(result.agent_loop_overhead.mean_us),
        format_us(result.agent_loop_overhead.p95_us),
        format_us(result.agent_loop_overhead.p99_us),
        format_us(result.agent_loop_overhead.min_us),
        format_us(result.agent_loop_overhead.max_us),
        result.agent_loop_overhead.coefficient_of_variation * 100.0,
    ));

    // Throughput table (if present)
    if let Some(ref throughput) = result.throughput {
        out.push_str("\n### Throughput\n\n");
        out.push_str("| Concurrency | Agents/s | Mean Completion |\n");
        out.push_str("|-------------|----------|------------------|\n");
        for level in &throughput.levels {
            out.push_str(&format!(
                "| {} | {:.2} | {} |\n",
                level.concurrency,
                level.agents_per_second,
                format_us(level.completion_time.mean_us),
            ));
        }
    }

    // Memory (if present)
    if let Some(ref memory) = result.memory {
        out.push_str("\n### Memory\n\n");
        out.push_str("| Metric | Value |\n");
        out.push_str("|--------|-------|\n");
        out.push_str(&format!("| Peak RSS | {} |\n", format_bytes(memory.peak_rss_bytes)));
        if let Some(per_agent) = memory.per_agent_bytes {
            out.push_str(&format!("| Per-Agent | {} |\n", format_bytes(per_agent)));
        }
        out.push_str(&format!("| Samples | {} |\n", memory.sample_count));
    }

    // Token Overhead (if present)
    if let Some(ref tokens) = result.token_overhead {
        out.push_str("\n### Token Overhead\n\n");
        out.push_str("| Metric | Value |\n");
        out.push_str("|--------|-------|\n");
        out.push_str(&format!("| Total Tokens | {} |\n", tokens.total_tokens));
        out.push_str(&format!("| User Content | {} |\n", tokens.user_content_tokens));
        out.push_str(&format!(
            "| Framework Overhead | {} ({:.1}%) |\n",
            tokens.overhead_tokens, tokens.overhead_percentage
        ));
        out.push_str(&format!("| System Prompt | {} |\n", tokens.breakdown.system_prompt_tokens));
        out.push_str(&format!("| Tool Schema | {} |\n", tokens.breakdown.tool_schema_tokens));
        out.push_str(&format!(
            "| Framework Wrapper | {} |\n",
            tokens.breakdown.framework_wrapper_tokens
        ));
    }

    out
}

fn format_comparison_markdown(comparison: &ComparisonResult) -> String {
    let mut out = String::new();
    let adk = &comparison.adk_result;

    out.push_str(&format!("## Framework Comparison: {}\n\n", adk.workload_name));
    out.push_str(&format!("**Model:** {} | **Iterations:** {}\n\n", adk.model, adk.iterations));

    // Build header: Framework | Cold Start | Loop Overhead (mean) | Loop Overhead (P95) | Throughput | Peak RSS
    out.push_str("| Framework | Cold Start | Loop Overhead (mean) | Loop Overhead (P95) |");

    let has_throughput = adk.throughput.is_some()
        || comparison.external_results.iter().any(|e| e.throughput_agents_per_sec.is_some());
    let has_memory = adk.memory.is_some()
        || comparison.external_results.iter().any(|e| e.peak_rss_bytes.is_some());

    if has_throughput {
        out.push_str(" Throughput |");
    }
    if has_memory {
        out.push_str(" Peak RSS |");
    }
    out.push('\n');

    // Separator
    out.push_str("|-----------|------------|----------------------|---------------------|");
    if has_throughput {
        out.push_str("------------|");
    }
    if has_memory {
        out.push_str("----------|");
    }
    out.push('\n');

    // ADK-Rust row
    let adk_throughput = adk
        .throughput
        .as_ref()
        .and_then(|t| t.levels.first())
        .map(|l| format!("{:.2} /s", l.agents_per_second))
        .unwrap_or_else(|| "—".to_string());
    let adk_memory = adk
        .memory
        .as_ref()
        .map(|m| format_bytes(m.peak_rss_bytes))
        .unwrap_or_else(|| "—".to_string());

    out.push_str(&format!(
        "| **ADK-Rust** | {} | {} | {} |",
        format_us(adk.cold_start.mean_us),
        format_us(adk.agent_loop_overhead.mean_us),
        format_us(adk.agent_loop_overhead.p95_us),
    ));
    if has_throughput {
        out.push_str(&format!(" {adk_throughput} |"));
    }
    if has_memory {
        out.push_str(&format!(" {adk_memory} |"));
    }
    out.push('\n');

    // External framework rows
    for ext in &comparison.external_results {
        let ext_throughput = ext
            .throughput_agents_per_sec
            .map(|t| format!("{t:.2} /s"))
            .unwrap_or_else(|| "—".to_string());
        let ext_memory = ext.peak_rss_bytes.map(format_bytes).unwrap_or_else(|| "—".to_string());

        out.push_str(&format!(
            "| {} | {} | {} | {} |",
            ext.framework,
            format_us(ext.cold_start_us),
            format_us(ext.loop_overhead.mean_us),
            format_us(ext.loop_overhead.p95_us),
        ));
        if has_throughput {
            out.push_str(&format!(" {ext_throughput} |"));
        }
        if has_memory {
            out.push_str(&format!(" {ext_memory} |"));
        }
        out.push('\n');
    }

    out
}

// ─── Formatting Helpers ──────────────────────────────────────────────────────

/// Formats microseconds into a human-readable string with appropriate units.
/// - < 1000 μs: displays as "XXX μs"
/// - >= 1000 μs: displays as "X.XX ms"
fn format_us(us: u64) -> String {
    if us < 1_000 { format!("{us} μs") } else { format!("{:.2} ms", us as f64 / 1_000.0) }
}

/// Formats bytes into a human-readable string with appropriate units.
fn format_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else if bytes < 1_024 * 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

/// Formats a delta between ADK and external framework values.
/// Negative values indicate ADK is faster (better), positive means slower.
fn format_delta(adk_us: u64, ext_us: u64) -> String {
    if ext_us == 0 {
        return "—".to_string();
    }
    let ratio = adk_us as f64 / ext_us as f64;
    if ratio < 1.0 {
        format!("{:.1}x faster", 1.0 / ratio)
    } else if ratio > 1.0 {
        format!("{:.1}x slower", ratio)
    } else {
        "equal".to_string()
    }
}

/// Formats a throughput delta (higher is better for throughput).
fn format_throughput_delta(adk_agents_per_sec: f64, ext_agents_per_sec: f64) -> String {
    if ext_agents_per_sec == 0.0 {
        return "—".to_string();
    }
    let ratio = adk_agents_per_sec / ext_agents_per_sec;
    if ratio > 1.0 {
        format!("{ratio:.1}x higher")
    } else if ratio < 1.0 {
        format!("{:.1}x lower", 1.0 / ratio)
    } else {
        "equal".to_string()
    }
}

/// Formats a memory delta (lower is better for memory).
fn format_bytes_delta(adk_bytes: u64, ext_bytes: u64) -> String {
    if ext_bytes == 0 {
        return "—".to_string();
    }
    let ratio = adk_bytes as f64 / ext_bytes as f64;
    if ratio < 1.0 {
        format!("{:.1}x less", 1.0 / ratio)
    } else if ratio > 1.0 {
        format!("{ratio:.1}x more")
    } else {
        "equal".to_string()
    }
}

/// Creates a horizontal separator line of the given width.
fn separator_line(width: usize) -> String {
    format!("{}\n", "─".repeat(width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::{ExternalDurationStats, ExternalTokenOverhead};
    use crate::metrics::{
        BenchmarkResult, ConcurrencyLevel, DurationStats, MemoryMetrics, RunMetadata,
        ThroughputMetrics, TokenBreakdown, TokenOverheadMetrics,
    };

    fn sample_result() -> BenchmarkResult {
        BenchmarkResult {
            schema_version: 1,
            workload_name: "simple_tool_call".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-15T10:30:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: DurationStats {
                min_us: 1000,
                max_us: 5000,
                mean_us: 2500,
                median_us: 2400,
                p95_us: 4800,
                p99_us: 4950,
                std_dev_us: 800,
                count: 5,
                coefficient_of_variation: 0.32,
            },
            agent_loop_overhead: DurationStats {
                min_us: 100,
                max_us: 500,
                mean_us: 250,
                median_us: 240,
                p95_us: 480,
                p99_us: 495,
                std_dev_us: 80,
                count: 10,
                coefficient_of_variation: 0.32,
            },
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }
    }

    fn sample_result_full() -> BenchmarkResult {
        let mut result = sample_result();
        result.throughput = Some(ThroughputMetrics {
            levels: vec![ConcurrencyLevel {
                concurrency: 4,
                agents_per_second: 12.5,
                completion_time: DurationStats {
                    min_us: 800_000,
                    max_us: 1_200_000,
                    mean_us: 1_000_000,
                    median_us: 980_000,
                    p95_us: 1_150_000,
                    p99_us: 1_190_000,
                    std_dev_us: 100_000,
                    count: 4,
                    coefficient_of_variation: 0.1,
                },
            }],
        });
        result.memory = Some(MemoryMetrics {
            peak_rss_bytes: 52_428_800,
            per_agent_bytes: Some(2_097_152),
            sample_count: 50,
        });
        result.token_overhead = Some(TokenOverheadMetrics {
            total_tokens: 1200,
            user_content_tokens: 950,
            overhead_tokens: 250,
            overhead_percentage: 20.83,
            breakdown: TokenBreakdown {
                system_prompt_tokens: 100,
                tool_schema_tokens: 100,
                framework_wrapper_tokens: 50,
            },
        });
        result
    }

    fn sample_external() -> ExternalMetricsOutput {
        ExternalMetricsOutput {
            framework: "langgraph".to_string(),
            cold_start_us: 45_000,
            first_llm_call_epoch_ns: 1_705_312_800_000_045_000,
            loop_overhead: ExternalDurationStats {
                min_us: 120,
                max_us: 890,
                mean_us: 340,
                median_us: 310,
                p95_us: 780,
                p99_us: 870,
                count: 10,
            },
            peak_rss_bytes: Some(104_857_600),
            throughput_agents_per_sec: Some(6.2),
            token_overhead: Some(ExternalTokenOverhead {
                total_tokens: 1400,
                user_content_tokens: 950,
                overhead_tokens: 450,
            }),
        }
    }

    #[test]
    fn test_format_json_contains_all_fields() {
        let result = sample_result();
        let json = format_result(&result, OutputFormat::Json);

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["workloadName"], "simple_tool_call");
        assert_eq!(parsed["model"], "gemini-2.5-flash");
        assert_eq!(parsed["iterations"], 5);
        assert_eq!(parsed["schemaVersion"], 1);
        assert!(parsed["metadata"]["timestamp"].is_string());
        assert!(parsed["metadata"]["adkVersion"].is_string());
        assert!(parsed["coldStart"]["meanUs"].is_number());
        assert!(parsed["agentLoopOverhead"]["p95Us"].is_number());
    }

    #[test]
    fn test_format_json_round_trip() {
        let result = sample_result_full();
        let json = format_result(&result, OutputFormat::Json);
        let deserialized: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, deserialized);
    }

    #[test]
    fn test_format_table_contains_metrics() {
        let result = sample_result();
        let table = format_result(&result, OutputFormat::Table);

        assert!(table.contains("simple_tool_call"));
        assert!(table.contains("gemini-2.5-flash"));
        assert!(table.contains("Cold Start"));
        assert!(table.contains("Agent Loop Overhead"));
        assert!(table.contains("Mean"));
        assert!(table.contains("P95"));
    }

    #[test]
    fn test_format_table_shows_units() {
        let result = sample_result();
        let table = format_result(&result, OutputFormat::Table);

        // Cold start mean is 2500 μs = 2.50 ms
        assert!(table.contains("2.50 ms"));
        // Agent loop overhead mean is 250 μs
        assert!(table.contains("250 μs"));
    }

    #[test]
    fn test_format_table_with_throughput() {
        let result = sample_result_full();
        let table = format_result(&result, OutputFormat::Table);

        assert!(table.contains("Throughput"));
        assert!(table.contains("12.50"));
        assert!(table.contains("/s"));
    }

    #[test]
    fn test_format_table_with_memory() {
        let result = sample_result_full();
        let table = format_result(&result, OutputFormat::Table);

        assert!(table.contains("Memory"));
        assert!(table.contains("Peak RSS"));
        assert!(table.contains("50.0 MB"));
        assert!(table.contains("2.0 MB"));
    }

    #[test]
    fn test_format_markdown_structure() {
        let result = sample_result();
        let md = format_result(&result, OutputFormat::Markdown);

        assert!(md.contains("## Benchmark Results:"));
        assert!(md.contains("| Metric | Mean | P95 |"));
        assert!(md.contains("| Cold Start |"));
        assert!(md.contains("| Agent Loop Overhead |"));
    }

    #[test]
    fn test_format_markdown_with_optional_sections() {
        let result = sample_result_full();
        let md = format_result(&result, OutputFormat::Markdown);

        assert!(md.contains("### Throughput"));
        assert!(md.contains("### Memory"));
        assert!(md.contains("### Token Overhead"));
        assert!(md.contains("| Peak RSS |"));
    }

    #[test]
    fn test_format_comparison_json_structure() {
        let comparison = ComparisonResult {
            adk_result: sample_result_full(),
            external_results: vec![sample_external()],
        };
        let json = format_comparison(&comparison, OutputFormat::Json);

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["adkResult"].is_object());
        assert!(parsed["externalResults"].is_array());
        assert_eq!(parsed["externalResults"][0]["framework"], "langgraph");
    }

    #[test]
    fn test_format_comparison_table_shows_frameworks() {
        let comparison = ComparisonResult {
            adk_result: sample_result_full(),
            external_results: vec![sample_external()],
        };
        let table = format_comparison(&comparison, OutputFormat::Table);

        assert!(table.contains("ADK-Rust"));
        assert!(table.contains("langgraph"));
        assert!(table.contains("Cold Start"));
        assert!(table.contains("Loop Overhead"));
    }

    #[test]
    fn test_format_comparison_table_shows_deltas() {
        let comparison = ComparisonResult {
            adk_result: sample_result_full(),
            external_results: vec![sample_external()],
        };
        let table = format_comparison(&comparison, OutputFormat::Table);

        // ADK cold start 2500 vs langgraph 45000, so ADK is faster
        assert!(table.contains("faster"));
    }

    #[test]
    fn test_format_comparison_markdown_table() {
        let comparison = ComparisonResult {
            adk_result: sample_result_full(),
            external_results: vec![sample_external()],
        };
        let md = format_comparison(&comparison, OutputFormat::Markdown);

        assert!(md.contains("## Framework Comparison:"));
        assert!(md.contains("| **ADK-Rust** |"));
        assert!(md.contains("| langgraph |"));
        assert!(md.contains("| Framework |"));
    }

    #[test]
    fn test_format_us_microseconds() {
        assert_eq!(format_us(100), "100 μs");
        assert_eq!(format_us(999), "999 μs");
    }

    #[test]
    fn test_format_us_milliseconds() {
        assert_eq!(format_us(1000), "1.00 ms");
        assert_eq!(format_us(2500), "2.50 ms");
        assert_eq!(format_us(45000), "45.00 ms");
    }

    #[test]
    fn test_format_bytes_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GB");
    }

    #[test]
    fn test_format_delta_faster() {
        let delta = format_delta(100, 1000);
        assert!(delta.contains("10.0x faster"));
    }

    #[test]
    fn test_format_delta_slower() {
        let delta = format_delta(1000, 100);
        assert!(delta.contains("10.0x slower"));
    }

    #[test]
    fn test_format_delta_equal() {
        let delta = format_delta(100, 100);
        assert_eq!(delta, "equal");
    }

    #[test]
    fn test_format_delta_zero_external() {
        let delta = format_delta(100, 0);
        assert_eq!(delta, "—");
    }
}
