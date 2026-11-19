//! The NVIDIA NIM document reader, live on `ai.api.nvidia.com`, through the egress gate:
//! the report page as it is, and the invoice after `PersonalTextRedactor` over local
//! PP-OCRv6 found its labelled personal fields and the gate filled them before release
//! (spec 3.7 §7.7, §7.8, S1–S3).
//!
//! Needs `NVIDIA_API_KEY` and the PP-OCRv6 tiny weights (for the redactor):
//!
//! ```text
//! NVIDIA_API_KEY=... HF_HUB_CACHE=/path/to/cache cargo test -p adk-vision \
//!     --features nim,ocr,guardrail,fetch,conformance --test nim_live_tests -- --ignored --nocapture
//! ```

mod doc_support;

use std::sync::Arc;
use std::time::Instant;

use adk_core::egress::EgressPolicy;
use adk_vision::{
    BlockKind, CallOptions, EgressAction, EgressConfig, EgressGate, FetchConfig,
    InMemoryAcceptanceStore, InMemoryEgressRecorder, ModelFetcher, NimCvConfig, NimDocumentReader,
    PersonalTextRedactor, PpOcrReader, PpOcrV6Size, ReadOptions, RedactionClass, VisionCx,
    VisionRuntime,
};
use doc_support::{DOC_PAGE, INVOICE, box_error, char_accuracy, page, text_lines, truth};

fn reader() -> NimDocumentReader {
    let key = std::env::var("NVIDIA_API_KEY").expect("NVIDIA_API_KEY");
    NimDocumentReader::new(NimCvConfig::new("https://ai.api.nvidia.com/v1/cv").with_api_key(key))
        .unwrap()
}

fn record(name: &str, page: &adk_vision::DocumentPage) {
    if let Ok(dir) = std::env::var("ADK_VISION_RECORD_DIR") {
        let path = std::path::Path::new(&dir).join(format!("nim-document-reader.{name}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(page).unwrap()).unwrap();
    }
}

async fn runtime(redact: bool) -> (VisionRuntime, Arc<InMemoryEgressRecorder>) {
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let mut config = EgressConfig::default().with_default(EgressAction::Deny).with_rule(
        "ai.api.nvidia.com",
        &["document_page"],
        EgressAction::Allow,
    );
    let mut redactor = None;
    if redact {
        config = config.redacting(RedactionClass::personal_text());
        let fetcher =
            ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
        // The local reader finds the text; the redactor matches PII patterns and the fields
        // the invoice's schema labels personal.
        let ocr = PpOcrReader::fetch(&fetcher, PpOcrV6Size::Tiny).await.unwrap();
        redactor = Some(
            PersonalTextRedactor::new(Arc::new(ocr))
                .with_labelled_fields(["Name", "Address", "Email", "Phone"]),
        );
    }
    let mut gate = EgressGate::new(EgressPolicy::public_only(), config, recorder.clone());
    if let Some(redactor) = redactor {
        gate = gate.with_redactor(Arc::new(redactor));
    }
    let runtime = VisionRuntime::builder()
        .remote_reader(Arc::new(reader()))
        .egress_gate(gate)
        .build()
        .unwrap();
    (runtime, recorder)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "calls NVIDIA's hosted endpoints; needs NVIDIA_API_KEY"]
async fn nim_reads_the_report_page_through_the_gate() {
    let (runtime, recorder) = runtime(false).await;
    let source = page(DOC_PAGE);
    let options = CallOptions::new().with_data_class("document_page");
    let started = Instant::now();
    let out = runtime
        .read_with(&VisionCx::new(), &source, &ReadOptions::default(), &options)
        .await
        .unwrap();
    let elapsed = started.elapsed().as_millis();
    let truth = text_lines(DOC_PAGE);
    let accuracy = char_accuracy(&out.value, &truth);
    let boxes = box_error(&out.value, &truth);
    println!(
        "nim {DOC_PAGE}: read {elapsed} ms, {} blocks, char accuracy {accuracy:.4}, lines matched {}/{}, \
         edge error median {:.1} px max {:.1} px",
        out.value.blocks.len(),
        boxes.matched,
        boxes.total,
        boxes.median_px,
        boxes.max_px
    );
    for &i in &out.value.reading_order {
        let b = &out.value.blocks[i];
        println!(
            "    {:?} {:?} {:?}",
            b.kind,
            b.region.shape.bbox().unwrap(),
            b.text.replace('\n', " / ")
        );
    }
    record(DOC_PAGE, &out.value);
    let table = out.value.blocks.iter().find(|b| b.kind == BlockKind::Table).expect("a table");
    println!("    table cells {}", table.cells.len());
    assert!(accuracy >= 0.95, "{accuracy}");
    assert_eq!(table.cells.len(), 12);
    assert_eq!(recorder.released().len(), 1);
    assert_eq!(out.calls[0].egress, vec![recorder.released()[0].egress]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "calls NVIDIA's hosted endpoints; needs NVIDIA_API_KEY and the PP-OCRv6 weights"]
async fn nim_never_sees_the_invoices_personal_fields() {
    let (runtime, recorder) = runtime(true).await;
    let source = page(INVOICE);
    let options = CallOptions::new().with_data_class("document_page");
    let started = Instant::now();
    let out = runtime
        .read_with(&VisionCx::new(), &source, &ReadOptions::default(), &options)
        .await
        .unwrap();
    let elapsed = started.elapsed().as_millis();
    let text: String =
        out.value.blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n");
    let receipt = &recorder.released()[0];
    println!(
        "nim {INVOICE} redacted: read {elapsed} ms, {} blocks, redactions {:?}, egress {}",
        out.value.blocks.len(),
        receipt.redactions,
        receipt.egress
    );
    for &i in &out.value.reading_order {
        let b = &out.value.blocks[i];
        println!("    {:?} {:?}", b.kind, b.text.replace('\n', " / "));
    }
    let personal: Vec<String> = truth(INVOICE).into_iter().filter_map(|o| o.value).collect();
    assert_eq!(personal.len(), 4);
    for value in &personal {
        for part in value.split_whitespace().filter(|p| p.len() > 3) {
            assert!(!text.contains(part), "`{part}` of a personal field reached NVIDIA: {text}");
        }
    }
    for kept in ["INVOICE INV-4721", "Torque sensor", "58.20"] {
        assert!(text.contains(kept), "`{kept}` is missing: {text}");
    }
    // One box per covered word of the personal values.
    let covered = receipt.redactions.get(&RedactionClass::personal_text()).copied().unwrap_or(0);
    assert!(covered >= 4, "{covered}");
    assert_eq!(receipt.hosts, vec!["ai.api.nvidia.com".to_string()]);
}
