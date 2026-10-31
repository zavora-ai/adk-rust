//! The model sees why a devtools call failed (spec 3.1 §7.33 fix 6).

#![cfg(feature = "runner")]

use crate::common;

use adk_harness::{HarnessAgentBuilder, HarnessMode};
use adk_model::ScriptedResponse;
use common::{fixture, response_of};
use serde_json::json;

#[tokio::test]
async fn an_edit_before_a_read_tells_the_model_to_read_first() {
    let f = fixture(
        vec![
            ScriptedResponse::function_call(
                "edit_file",
                json!({ "path": "notes.txt", "old_string": "a", "new_string": "b" }),
            ),
            ScriptedResponse::text("done"),
        ],
        |b: HarnessAgentBuilder| b.mode(HarnessMode::Auto),
    )
    .await;
    std::fs::write(f.path("notes.txt"), "a\n").unwrap();
    let events = f.send("change a to b in notes.txt").await;
    let error = response_of(&events, "edit_file")["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("devtools.not_read"), "{error}");
    assert!(error.contains("must be read with read_file before it can be edited"), "{error}");
    assert!(!error.contains("tool.legacy"), "{error}");
}

#[tokio::test]
async fn a_missing_string_tells_the_model_it_was_not_found() {
    let f = fixture(
        vec![
            ScriptedResponse::function_call("read_file", json!({ "path": "notes.txt" })),
            ScriptedResponse::function_call(
                "edit_file",
                json!({ "path": "notes.txt", "old_string": "zzz", "new_string": "b" }),
            ),
            ScriptedResponse::text("done"),
        ],
        |b: HarnessAgentBuilder| b.mode(HarnessMode::Auto),
    )
    .await;
    std::fs::write(f.path("notes.txt"), "a\n").unwrap();
    let events = f.send("change zzz to b in notes.txt").await;
    let error = response_of(&events, "edit_file")["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("devtools.no_match"), "{error}");
    assert!(error.contains("no occurrence of the target string"), "{error}");
}
