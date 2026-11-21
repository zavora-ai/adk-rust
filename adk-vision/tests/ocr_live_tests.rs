//! PP-OCRv6 through `oar-ocr`, live on the generated document pages: character accuracy
//! and box error in source pixels against the ground truth, and the conformance suite.
//!
//! Needs the weights: set `HF_HUB_CACHE` to a cache that holds them, or let the fetcher
//! download them by revision (31 MB for small, 6 MB for tiny).
//!
//! ```text
//! HF_HUB_CACHE=/path/to/cache cargo test -p adk-vision --features ocr,fetch,conformance \
//!     --test ocr_live_tests -- --ignored --nocapture
//! ```

mod doc_support;

use std::sync::Arc;
use std::time::Instant;

use adk_vision::{
    BlockKind, CoordFrame, DocumentPage, FetchConfig, InMemoryAcceptanceStore, ModelFetcher,
    PpOcrReader, PpOcrV6Size, ReadOptions, Shape, VisionCx, VisionRuntime,
};
use doc_support::{DOC_PAGE, INVOICE, box_error, char_accuracy, page, text_lines};

fn fetcher() -> ModelFetcher {
    ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
}

/// Writes a page's JSON under `ADK_VISION_RECORD_DIR` when it is set, for the S8 schema
/// fixtures.
fn record(backend: &str, name: &str, page: &DocumentPage) {
    if let Ok(dir) = std::env::var("ADK_VISION_RECORD_DIR") {
        let path = std::path::Path::new(&dir).join(format!("{backend}.{name}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(page).unwrap()).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the PP-OCRv6 weights; see the module docs"]
async fn pp_ocrv6_reads_the_generated_pages() {
    for size in [PpOcrV6Size::Tiny, PpOcrV6Size::Small] {
        let loaded = Instant::now();
        let reader = PpOcrReader::fetch(&fetcher(), size).await.unwrap();
        let load_ms = loaded.elapsed().as_millis();
        let id = reader_id(size);
        let runtime = VisionRuntime::builder().reader(Arc::new(reader)).build().unwrap();
        for name in [DOC_PAGE, INVOICE] {
            let source = page(name);
            let started = Instant::now();
            let mut options = ReadOptions::default();
            options.markdown = true;
            let out = runtime.read(&VisionCx::new(), &source, &options).await.unwrap();
            let elapsed = started.elapsed().as_millis();
            let truth = text_lines(name);
            let accuracy = char_accuracy(&out.value, &truth);
            let boxes = box_error(&out.value, &truth);
            println!(
                "{id} {name}: load {load_ms} ms, read {elapsed} ms, {} lines, char accuracy {:.4}, \
                 lines matched {}/{}, edge error median {:.1} px max {:.1} px",
                out.value.blocks.len(),
                accuracy,
                boxes.matched,
                boxes.total,
                boxes.median_px,
                boxes.max_px,
            );
            for i in &out.value.reading_order {
                let block = &out.value.blocks[*i];
                println!(
                    "    {:?} {:.3} {:?}",
                    block.region.shape.bbox().unwrap(),
                    block.confidence.unwrap_or(0.0),
                    block.text
                );
            }
            record(&id, name, &out.value);
            // Every block is a text line in source pixels, from this backend, in its frame.
            for block in &out.value.blocks {
                assert_eq!(block.kind, BlockKind::Text);
                assert!(matches!(block.region.shape, Shape::Polygon(_)));
                assert_eq!(block.region.image, source.digest());
                assert_eq!(block.region.origin.backend.as_str(), id);
                assert_eq!(block.region.origin.frame, CoordFrame::SentPixels);
                assert!(block.region.shape.is_within(1000, 1300));
            }
            assert_eq!(out.value.reading_order.len(), out.value.blocks.len());
            assert!(out.value.markdown.is_some());
            assert!(accuracy >= 0.95, "{id} read {name} at {accuracy:.4}");
            assert!(boxes.matched * 10 >= boxes.total * 9, "{id} matched {boxes:?}");
            // onnxruntime 1.31 draws PP-OCRv6 small's line boxes about 2 px wider than 1.28 did
            // (median 9 px against 7 px on the report page), with the same text.
            assert!(boxes.median_px <= 10.0, "{id} edge error {boxes:?}");
            // Local: one image and one page recorded at zero cost, nothing released.
            let call = &out.calls[0];
            assert_eq!(call.cost_micro_usd, Some(0));
            assert!(call.egress.is_empty());
        }
    }
}

fn reader_id(size: PpOcrV6Size) -> String {
    match size {
        PpOcrV6Size::Tiny => "pp-ocrv6-tiny",
        PpOcrV6Size::Small => "pp-ocrv6-small",
        _ => "pp-ocrv6-medium",
    }
    .to_string()
}

mod pp_ocrv6_tiny_conformance {
    use std::sync::Arc;

    use adk_vision::conformance::ReaderTarget;
    use adk_vision::{PpOcrReader, PpOcrV6Size};

    use super::doc_support::conformance_fixture;
    use super::fetcher;

    adk_vision::document_reader_conformance_tests! {
        #[ignore = "needs the PP-OCRv6 weights; see the module docs"]
        backend = ReaderTarget::Local(Arc::new(
            PpOcrReader::fetch(&fetcher(), PpOcrV6Size::Tiny).await.unwrap(),
        )),
        fixture = conformance_fixture(10.0),
    }
}
