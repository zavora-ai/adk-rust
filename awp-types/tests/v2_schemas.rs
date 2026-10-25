//! The committed AWP 2.0 JSON Schemas match the types (spec 3.5 §7.2–§7.8.3).
//!
//! The protocol repository vendors `awp-types/schemas/2.0/`. When a schema drifts, this test
//! writes the new one and fails; review the diff and commit it.

#![cfg(feature = "json-schema")]

use std::path::Path;

use serde_json::{Map, Value};

/// Sorts object keys and `required` lists so the file is the same whatever
/// `serde_json/preserve_order` the build unifies.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut sorted = Map::new();
            for (key, child) in entries {
                let child = match (key.as_str(), child) {
                    ("required", Value::Array(mut items)) => {
                        items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                        Value::Array(items)
                    }
                    (_, child) => canonical(child),
                };
                sorted.insert(key, child);
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

#[test]
fn committed_schemas_match_the_types() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/2.0");
    std::fs::create_dir_all(&dir).unwrap();
    let mut drifted = Vec::new();
    for (stem, schema) in awp_types::v2::schema::wire_schemas() {
        let value = canonical(serde_json::to_value(&schema).unwrap());
        let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
        let path = dir.join(format!("{stem}.schema.json"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&path, &text).unwrap();
            drifted.push(path.display().to_string());
        }
    }
    let mut committed: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    committed.sort();
    let mut expected: Vec<String> = awp_types::v2::schema::wire_schemas()
        .into_iter()
        .map(|(stem, _)| format!("{stem}.schema.json"))
        .collect();
    expected.sort();
    assert_eq!(committed, expected, "schemas/2.0 holds exactly the generated schemas");
    assert!(
        drifted.is_empty(),
        "schemas drifted from the types and were rewritten; review and commit: {drifted:#?}"
    );
}

#[test]
fn schemas_describe_the_wire_spelling() {
    let schemas = awp_types::v2::schema::wire_schemas();
    let find = |stem: &str| {
        serde_json::to_value(&schemas.iter().find(|(s, _)| *s == stem).unwrap().1).unwrap()
    };
    let contract = find("site-contract");
    assert_eq!(contract["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(contract["required"], serde_json::json!(["awp", "site"]));
    assert!(contract["properties"]["surfaces"].is_object());
    let manifest = find("release-manifest");
    assert!(manifest["properties"]["charterVersion"].is_object());
    assert!(manifest["properties"]["createdAt"].is_object());
}
