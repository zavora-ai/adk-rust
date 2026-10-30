//! Loading a skill records `harness.skill.activated` (spec 3.1 §9.3).

#![cfg(all(feature = "runner", feature = "skills"))]

use crate::common;

use adk_model::ScriptedResponse;
use serde_json::json;

#[tokio::test]
async fn loading_a_skill_records_its_activation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".skills/release")).unwrap();
    std::fs::write(
        dir.path().join(".skills/release/SKILL.md"),
        "---\nname: release\ndescription: Cut a release of this crate.\n---\n\nBump the version, then tag.\n",
    )
    .unwrap();
    let f = common::fixture_in(
        dir,
        vec![
            ScriptedResponse::function_call("load_skill", json!({ "skill_name": "release" })),
            ScriptedResponse::text("done"),
        ],
        |b| b,
    )
    .await;
    let events = f.send("release it").await;
    let loaded = common::response_of(&events, "load_skill");
    assert!(loaded.to_string().contains("Bump the version"), "{loaded}");

    let records = f.records("harness.skill.activated");
    assert_eq!(records.len(), 1);
    let ext = &records[0].ext;
    assert_eq!(ext["skillId"], loaded["skillId"]);
    assert_eq!(ext["hash"], loaded["hash"]);
    // The source path renders the platform separator, like `Workspace::display`.
    let source = std::path::Path::new(".skills").join("release").join("SKILL.md");
    assert_eq!(ext["sourcePath"], source.display().to_string());
    assert_eq!(ext["allowedTools"], json!([]));
}
