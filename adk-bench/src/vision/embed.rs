//! Embedders: latency per image and cross-modal retrieval on captioned fixtures.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_vision::{CallOptions, SourceImage, VisionCx, VisionRuntime};

use super::{
    FixtureSet, ModelRef, RunMode, SizeLatency, VisionBenchConfig, VisionBenchRow, nudged, scaled,
};
use crate::error::{BenchError, Result};
use crate::metrics::compute_stats;

/// Images and the caption that describes each, for retrieval accuracy.
pub struct EmbedWorkload {
    /// The workload's name.
    pub name: String,
    /// `(image, caption)` pairs; caption `i` describes image `i` and no other.
    pub pairs: Vec<(SourceImage, String)>,
}

impl EmbedWorkload {
    /// The four committed generated fixtures with a caption each: drawn shapes, the S5 form
    /// screenshot, the report page and the invoice.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Vision`] when a fixture cannot be read.
    pub fn captioned_fixtures(set: &FixtureSet) -> Result<Self> {
        let pairs = [
            ("../cloud/shapes-960x540.png", "coloured geometric shapes on a white background"),
            ("../s5/s5-screen.png", "a screenshot of a web form with text fields and buttons"),
            ("../documents/doc-page-1000x1300.png", "a report page with a table and a bar chart"),
            (
                "../documents/invoice-personal-1000x1300.png",
                "an invoice with line items and a total",
            ),
        ];
        let pairs = pairs
            .into_iter()
            .map(|(path, caption)| Ok((set.image(path)?, caption.to_string())))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { name: "captioned-fixtures".to_string(), pairs })
    }
}

/// The share of rows whose highest dot product is on the diagonal.
fn top1(rows: &[Vec<f32>], columns: &[Vec<f32>]) -> f64 {
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    let hits = rows
        .iter()
        .enumerate()
        .filter(|(i, row)| {
            columns
                .iter()
                .enumerate()
                .max_by(|a, b| dot(row, a.1).total_cmp(&dot(row, b.1)))
                .is_some_and(|(j, _)| j == *i)
        })
        .count();
    hits as f64 / rows.len().max(1) as f64
}

/// Measures an embedder: image→text and text→image top-1 retrieval on `workload`, and the
/// latency of embedding one image at each configured size.
///
/// # Errors
///
/// Returns [`BenchError::Vision`] when an embedding call fails.
pub async fn run_embedder(
    config: &VisionBenchConfig,
    id: &str,
    runtime_label: &str,
    models: Vec<ModelRef>,
    cold_load: Option<Duration>,
    runtime: Arc<VisionRuntime>,
    workload: &EmbedWorkload,
) -> Result<VisionBenchRow> {
    let cx = VisionCx::new();
    let options = CallOptions::default();
    let failed = |e: adk_vision::VisionError| BenchError::Vision(format!("{id}: {e}"));
    let images: Vec<&SourceImage> = workload.pairs.iter().map(|(image, _)| image).collect();
    let captions: Vec<&str> = workload.pairs.iter().map(|(_, caption)| caption.as_str()).collect();
    let image_vectors = runtime.embed_images(&cx, &images, &options).await.map_err(failed)?.value;
    let text_vectors = runtime.embed_texts(&cx, &captions, &options).await.map_err(failed)?.value;
    let accuracy = BTreeMap::from([
        ("retrieval_image_to_text_top1".to_string(), top1(&image_vectors, &text_vectors)),
        ("retrieval_text_to_image_top1".to_string(), top1(&text_vectors, &image_vectors)),
    ]);

    let mut latency = Vec::new();
    let mut peak = None;
    let (first, _) = workload
        .pairs
        .first()
        .ok_or_else(|| BenchError::Vision(format!("{} has no images", workload.name)))?;
    for &edge in &config.long_edges {
        let image = scaled(first, edge)?;
        let mut timings = Vec::with_capacity(config.runs);
        for run in 0..config.warmup + config.runs {
            let variant = nudged(&image, run)?;
            let started = Instant::now();
            runtime.embed_images(&cx, &[&variant], &options).await.map_err(failed)?;
            if run >= config.warmup {
                timings.push(started.elapsed());
                peak = peak.max(crate::memory::current_rss_bytes().ok());
            }
        }
        let stats = compute_stats(&timings);
        latency.push(SizeLatency {
            long_edge: edge,
            p50_ms: stats.median_us as f64 / 1000.0,
            p95_ms: stats.p95_us as f64 / 1000.0,
            per_second: if stats.mean_us == 0 { 0.0 } else { 1e6 / stats.mean_us as f64 },
            runs: timings.len(),
        });
    }
    Ok(VisionBenchRow {
        backend: id.to_string(),
        task: "embed".to_string(),
        workload: workload.name.clone(),
        mode: RunMode::Local,
        runtime: runtime_label.to_string(),
        models,
        cold_load_ms: cold_load.map(|d| d.as_secs_f64() * 1000.0),
        latency,
        peak_rss_bytes: peak,
        accuracy,
        cases: workload.pairs.len(),
        cost_per_1k_images_usd: Some(0.0),
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top1_counts_the_diagonal() {
        let rows = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.9, 0.2]];
        let columns = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.6, 0.6]];
        // Rows 0 and 1 match their own columns; row 2 scores 0.9 on column 0 against 0.66 on
        // its own.
        assert_eq!(top1(&rows[..2], &columns[..2]), 1.0);
        assert_eq!(top1(&rows, &columns), 2.0 / 3.0);
    }
}
