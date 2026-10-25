//! The AWP 2.0 test vectors regenerate byte-identically, match the committed copies in
//! `tests/fixtures/vectors/`, and each case evaluates to its expected outcome (spec 3.5 §7.7,
//! §11).
//!
//! When a vector drifts, this test rewrites the committed copy and fails; review and commit
//! it, then copy the directory into the protocol repository's `vectors/`.

#![cfg(feature = "verify")]

use std::path::Path;

use adk_awp::conformance::vectors::{case_bytes, check_case, generate};
use serde_json::Value;

#[tokio::test]
async fn vectors_are_deterministic_and_committed() {
    let first = generate().await.unwrap();
    let second = generate().await.unwrap();
    assert_eq!(
        first.iter().map(|(n, c)| (n.clone(), case_bytes(c))).collect::<Vec<_>>(),
        second.iter().map(|(n, c)| (n.clone(), case_bytes(c))).collect::<Vec<_>>(),
        "two runs produce the same bytes"
    );

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vectors");
    let mut drifted = Vec::new();
    for (name, case) in &first {
        let path = dir.join(name);
        let bytes = case_bytes(case);
        if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            drifted.push(name.clone());
        }
    }
    assert!(
        drifted.is_empty(),
        "vectors drifted and were rewritten; review and commit: {drifted:#?}"
    );
}

#[tokio::test]
async fn every_case_evaluates_to_its_expectation() {
    let cases = generate().await.unwrap();
    assert!(cases.len() >= 18);
    for (name, case) in &cases {
        let vector = name.split('/').next().unwrap();
        let outcome = check_case(vector, case).unwrap_or_else(|e| panic!("{name}: {e}"));
        let expected = &case["expected"];
        if expected["valid"] == Value::Bool(true) {
            assert_eq!(outcome, None, "{name}");
        } else {
            assert_eq!(outcome, expected["failure"].as_str(), "{name}");
        }
    }
}
