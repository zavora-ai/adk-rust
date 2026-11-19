//! Scenario S8, "same tool, any backend", for document reading: PP-OCRv6, PaddleOCR-VL-1.6
//! and NVIDIA NIM answer the same report page in one shape. The pages under
//! `tests/fixtures/documents/recorded/` are what `VisionRuntime::read` returned in the live
//! runs (`ocr_live_tests`, `ocr_vl_live_tests`, `nim_live_tests` with
//! `ADK_VISION_RECORD_DIR`); each validates against the published `DocumentPage` schema
//! and round-trips through the crate's type (spec 3.7 §7.5, §7.18, S8).

use std::collections::BTreeSet;
use std::path::PathBuf;

use adk_vision::{BlockKind, DocumentPage};
use serde_json::Value;

const PAGE_SCHEMA: &str = include_str!("../schema/document-page.json");
const REGION_SCHEMA: &str = include_str!("../schema/region-set.json");

/// `(backend id, recorded page)` for each backend, on the report page.
const RECORDED: [(&str, &str); 3] = [
    ("pp-ocrv6-tiny", "pp-ocrv6-tiny.doc-page-1000x1300.json"),
    ("paddleocr-vl-1.6", "paddleocr-vl-1.6.doc-page-1000x1300.json"),
    ("nim-document-reader", "nim-document-reader.doc-page-1000x1300.json"),
];

/// The published `region-set.json`, under its `$id`, for the page schema's `$ref`.
fn registry() -> jsonschema::Registry<'static> {
    let region: Value = serde_json::from_str(REGION_SCHEMA).unwrap();
    let id = region["$id"].as_str().unwrap().to_string();
    jsonschema::Registry::new().add(id, region).unwrap().prepare().unwrap()
}

fn validator(registry: &jsonschema::Registry<'_>) -> jsonschema::Validator {
    jsonschema::options()
        .with_registry(registry)
        .build(&serde_json::from_str(PAGE_SCHEMA).unwrap())
        .unwrap()
}

fn recorded(file: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/documents/recorded")
        .join(file);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn keys(value: &Value) -> BTreeSet<String> {
    value.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

#[test]
fn every_backend_answers_the_report_page_in_the_published_shape() {
    let registry = registry();
    let validator = validator(&registry);
    let mut digests = BTreeSet::new();
    for (backend, file) in RECORDED {
        let json = recorded(file);
        let errors: Vec<String> = validator.iter_errors(&json).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{backend}: {errors:?}");
        let page: DocumentPage = serde_json::from_value(json.clone()).unwrap();
        let again: DocumentPage =
            serde_json::from_str(&serde_json::to_string(&page).unwrap()).unwrap();
        assert_eq!(again, page, "{backend} does not round-trip");
        assert!(!page.blocks.is_empty(), "{backend} read nothing");
        let mut order = page.reading_order.clone();
        order.sort_unstable();
        assert_eq!(order, (0..page.blocks.len()).collect::<Vec<_>>(), "{backend}");
        for block in &page.blocks {
            assert_eq!(block.region.origin.backend.as_str(), backend);
            assert!(block.region.shape.is_within(1000, 1300), "{backend}");
            digests.insert(block.region.image);
        }
        // The title is the first block read, whatever the backend.
        let first = &page.blocks[page.reading_order[0]];
        assert_eq!(first.text, "Quarterly Parts Report", "{backend}");
    }
    assert_eq!(digests.len(), 1, "the backends read different images");
}

#[test]
fn the_shape_is_the_same_down_to_each_blocks_fields() {
    // Every block of every backend has kind, region and text; the optional fields are the
    // schema's, nothing else.
    let allowed: BTreeSet<String> =
        ["kind", "region", "text", "confidence", "words", "cells"].map(String::from).into();
    for (backend, file) in RECORDED {
        let json = recorded(file);
        let top: BTreeSet<String> = ["blocks", "readingOrder", "markdown"].map(String::from).into();
        assert!(keys(&json).is_subset(&top), "{backend}");
        for block in json["blocks"].as_array().unwrap() {
            let fields = keys(block);
            assert!(fields.is_superset(&["kind", "region", "text"].map(String::from).into()));
            assert!(fields.is_subset(&allowed), "{backend}: {fields:?}");
        }
    }
}

#[test]
fn the_schema_refuses_a_malformed_page() {
    let registry = registry();
    let validator = validator(&registry);
    let mut json = recorded(RECORDED[2].1);
    json["blocks"][0]["kind"] = Value::from("paragraph");
    assert!(!validator.is_valid(&json));
    let mut json = recorded(RECORDED[2].1);
    json["blocks"][0]["region"]["origin"]["frame"] = Value::from("pixels");
    assert!(!validator.is_valid(&json));
    let mut json = recorded(RECORDED[2].1);
    json.as_object_mut().unwrap().remove("readingOrder");
    assert!(!validator.is_valid(&json));
}

#[test]
fn the_backends_differ_in_granularity_not_in_shape() {
    // PP-OCRv6 answers lines; PaddleOCR-VL and NIM answer layout blocks with tables.
    let kinds = |file: &str| -> BTreeSet<String> {
        let page: DocumentPage = serde_json::from_value(recorded(file)).unwrap();
        page.blocks.iter().map(|b| format!("{:?}", b.kind)).collect()
    };
    assert_eq!(kinds(RECORDED[0].1), BTreeSet::from([format!("{:?}", BlockKind::Text)]));
    assert!(kinds(RECORDED[1].1).contains(&format!("{:?}", BlockKind::Table)));
    assert!(kinds(RECORDED[2].1).contains(&format!("{:?}", BlockKind::Table)));
}
