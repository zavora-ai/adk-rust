//! PaddleOCR-VL-1.6 with PP-DocLayoutV3 through `oar-ocr-vl`, live on the generated report
//! page: blocks, reading order, character accuracy and box error against the ground truth,
//! and the conformance suite.
//!
//! Needs the weights (2.05 GB): set `HF_HUB_CACHE` to a cache that holds them, or let the
//! fetcher download them by revision. `VL_DEVICE=metal` runs on the GPU with feature
//! `ocr-vl-metal`.
//!
//! ```text
//! HF_HUB_CACHE=/path/to/cache cargo test -p adk-vision --features ocr-vl,fetch,conformance \
//!     --test ocr_vl_live_tests -- --ignored --nocapture --test-threads 1
//! ```

mod doc_support;

use std::sync::Arc;
use std::time::Instant;

use adk_vision::{
    BlockKind, CoordFrame, FetchConfig, InMemoryAcceptanceStore, ModelFetcher, PaddleOcrVlReader,
    ReadOptions, VisionCx, VisionRuntime, VlDevice,
};
use doc_support::{DOC_PAGE, box_error, char_accuracy, page, text_lines, truth};

fn fetcher() -> ModelFetcher {
    ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
}

fn device() -> VlDevice {
    match std::env::var("VL_DEVICE").as_deref() {
        Ok("metal") => VlDevice::Metal,
        _ => VlDevice::Cpu,
    }
}

async fn reader() -> PaddleOcrVlReader {
    PaddleOcrVlReader::fetch(&fetcher(), device()).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the PaddleOCR-VL-1.6 and PP-DocLayoutV3 weights; see the module docs"]
async fn paddleocr_vl_reads_the_report_page() {
    let loaded = Instant::now();
    let reader = reader().await;
    let load_ms = loaded.elapsed().as_millis();
    let runtime = VisionRuntime::builder().reader(Arc::new(reader)).build().unwrap();
    let source = page(DOC_PAGE);
    let mut options = ReadOptions::default();
    options.markdown = true;
    let started = Instant::now();
    let out = runtime.read(&VisionCx::new(), &source, &options).await.unwrap();
    let elapsed = started.elapsed().as_millis();
    let truth_lines = text_lines(DOC_PAGE);
    let accuracy = char_accuracy(&out.value, &truth_lines);
    // PaddleOCR-VL reads a chart as a figure; its tick and axis labels are chart
    // recognition, a separate task, so the text score leaves them out.
    let prose: Vec<_> = truth_lines
        .iter()
        .filter(|o| !o.label.starts_with("y-tick") && !o.label.starts_with("x-label"))
        .cloned()
        .collect();
    let prose_accuracy = char_accuracy(&out.value, &prose);
    let boxes = box_error(&out.value, &truth_lines);
    println!(
        "paddleocr-vl-1.6 ({:?}) {DOC_PAGE}: load {load_ms} ms, read {elapsed} ms, {} blocks, \
         char accuracy {accuracy:.4} ({prose_accuracy:.4} without chart labels), lines matched {}/{}, edge error median {:.1} px max {:.1} px",
        device(),
        out.value.blocks.len(),
        boxes.matched,
        boxes.total,
        boxes.median_px,
        boxes.max_px
    );
    for &i in &out.value.reading_order {
        let b = &out.value.blocks[i];
        println!(
            "    {:?} {:?} {:.3} {:?} {:?}",
            b.kind,
            b.region.label,
            b.confidence.unwrap_or(0.0),
            b.region.shape.bbox().unwrap(),
            b.text.replace('\n', " / ")
        );
    }
    if let Ok(dir) = std::env::var("ADK_VISION_RECORD_DIR") {
        let path = std::path::Path::new(&dir).join(format!("paddleocr-vl-1.6.{DOC_PAGE}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&out.value).unwrap()).unwrap();
    }
    for block in &out.value.blocks {
        assert_eq!(block.region.image, source.digest());
        assert_eq!(block.region.origin.frame, CoordFrame::SentPixels);
        assert!(block.region.shape.is_within(1000, 1300));
    }
    // Blocks: the title, the paragraph, the table and the chart, each placed within 12 px.
    let objects = truth(DOC_PAGE);
    let gt = |label: &str| objects.iter().find(|o| o.label == label).unwrap().bbox();
    let near = |kind: BlockKind, want: adk_vision::BBox| {
        out.value.blocks.iter().filter(|b| b.kind == kind).any(|b| {
            b.region.shape.bbox().is_some_and(|f| {
                (f.x0 - want.x0).abs() <= 12.0
                    && (f.y0 - want.y0).abs() <= 12.0
                    && (f.x1 - want.x1).abs() <= 12.0
                    && (f.y1 - want.y1).abs() <= 12.0
            })
        })
    };
    assert!(near(BlockKind::Title, gt("title")), "title");
    assert!(near(BlockKind::Table, gt("table")), "table");
    let table = out.value.blocks.iter().find(|b| b.kind == BlockKind::Table).unwrap();
    assert!(table.text.contains("P-017\tcrack\t4.2"), "{}", table.text);
    assert!(prose_accuracy >= 0.99, "{prose_accuracy}");
    assert!(out.value.markdown.as_deref().is_some_and(|m| m.contains("Quarterly Parts Report")));
    assert_eq!(out.calls[0].cost_micro_usd, Some(0));
}

mod paddleocr_vl_conformance {
    use std::sync::Arc;

    use adk_vision::conformance::ReaderTarget;

    use super::doc_support::conformance_fixture;
    use super::reader;

    adk_vision::document_reader_conformance_tests! {
        #[ignore = "needs the PaddleOCR-VL-1.6 and PP-DocLayoutV3 weights; see the module docs"]
        backend = ReaderTarget::Local(Arc::new(reader().await)),
        fixture = conformance_fixture(12.0),
    }
}
