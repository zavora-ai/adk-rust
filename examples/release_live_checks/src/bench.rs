//! Item 5: a `BenchRunner` run reports the memory it measured, and no metric it did not.

use adk_bench::{BenchConfig, BenchRunner};

use crate::report::{Mode, Report, Verdict, check, fail, key, skip};

const MODEL: &str = "gemini-3.5-flash-lite";

pub async fn run(report: &mut Report) {
    let verdict = match key("GOOGLE_API_KEY") {
        Some(_) => measured().await,
        None => Ok(skip("GOOGLE_API_KEY is not set")),
    };
    report.record_result("bench_reports_measured_metrics", 5, Mode::Live, verdict);
}

async fn measured() -> anyhow::Result<Verdict> {
    let config = BenchConfig {
        model: MODEL.to_string(),
        workload: Some("simple_tool_call".to_string()),
        runs: 1,
        warmup: 0,
        confirm_cost: true,
        ..BenchConfig::default()
    };
    let results = match BenchRunner::new(config).run().await {
        Ok(results) => results,
        Err(error) => return Ok(fail(format!("run: {error}"))),
    };
    let Some(result) = results.first() else {
        return Ok(fail("the run returned no result"));
    };
    let json = serde_json::to_value(result)?;
    let unmeasured: Vec<&str> = ["toolInvocation", "tokenOverhead"]
        .into_iter()
        .filter(|field| json.get(field).is_some())
        .collect();
    let memory = result.memory.as_ref();
    Ok(check(
        memory.is_some_and(|memory| memory.peak_rss_bytes > 0 && memory.sample_count > 0)
            && unmeasured.is_empty()
            && result.model == MODEL,
        format!(
            "model={} memory={:?} unmeasured fields present={unmeasured:?}",
            result.model,
            memory.map(|memory| (memory.peak_rss_bytes, memory.sample_count)),
        ),
    ))
}
