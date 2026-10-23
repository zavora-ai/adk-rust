//! Any change to a pinned driver tool's declaration makes it stale (spec 3.2 §7.7, K-I14):
//! its digest differs from the pin, and the kernel's driver policy asks before every call
//! of it until the driver is re-approved, whatever its class.

#![cfg(feature = "drivers")]

use std::sync::Arc;

use adk_core::{PolicyDecision, ToolOrigin};
use adk_kernel::drivers::{
    DriverManifest, DriverPolicy, DriverRecord, DriverStore, DriverSupervisor, EffectClass,
    driver_source,
};
use adk_kernel::stores::InMemoryDriverStore;
use proptest::prelude::*;
use serde_json::{Value, json};

/// A tool declaration and its annotations, as an MCP server publishes them.
fn declaration() -> impl Strategy<Value = (Value, Option<Value>)> {
    (
        "[a-z][a-z_]{0,12}",
        ".{0,40}",
        proptest::collection::btree_map(
            "[a-z]{1,8}",
            prop_oneof![Just("string"), Just("integer"), Just("boolean")],
            0..4,
        ),
        proptest::option::of((any::<bool>(), any::<bool>())),
    )
        .prop_map(|(name, description, properties, hints)| {
            let properties: serde_json::Map<String, Value> =
                properties.into_iter().map(|(key, kind)| (key, json!({ "type": kind }))).collect();
            let declaration = json!({
                "name": name,
                "description": description,
                "parameters": { "type": "object", "properties": properties },
            });
            let annotations = hints.map(|(read_only, idempotent)| {
                json!({ "readOnlyHint": read_only, "idempotentHint": idempotent })
            });
            (declaration, annotations)
        })
}

/// One change a server could make to what it declares; each one alters the JSON.
fn mutate(
    declaration: &Value,
    annotations: &Option<Value>,
    which: u8,
    salt: &str,
) -> (Value, Option<Value>) {
    let mut declaration = declaration.clone();
    let mut annotations = annotations.clone();
    match which % 6 {
        0 => {
            let text = declaration["description"].as_str().unwrap_or_default().to_string();
            declaration["description"] = json!(format!("{text} {salt}"));
        }
        1 => {
            declaration["parameters"]["properties"][format!("added_{salt}")] =
                json!({ "type": "string" });
        }
        2 => {
            declaration["parameters"]["required"] = json!([format!("must_{salt}")]);
        }
        3 => {
            annotations = Some(match annotations {
                Some(mut hints) => {
                    let flipped = !hints["readOnlyHint"].as_bool().unwrap_or(false);
                    hints["readOnlyHint"] = json!(flipped);
                    hints
                }
                None => json!({ "destructiveHint": true }),
            });
        }
        4 => {
            annotations = match annotations {
                Some(_) => None,
                None => Some(json!({ "openWorldHint": true })),
            };
        }
        _ => {
            declaration[format!("x_{salt}")] = json!(salt);
        }
    }
    (declaration, annotations)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn any_declaration_mutation_makes_a_pinned_tool_stale(
        (declaration, annotations) in declaration(),
        which in any::<u8>(),
        salt in "[a-z0-9]{1,6}",
        class in proptest::sample::select(EffectClass::ALL.to_vec()),
    ) {
        let tool = declaration["name"].as_str().unwrap().to_string();
        let source = driver_source("notes");
        let pinned = ToolOrigin::new(&source, &declaration, annotations.as_ref());
        let (changed, changed_annotations) = mutate(&declaration, &annotations, which, &salt);
        let observed = ToolOrigin::new(&source, &changed, changed_annotations.as_ref());
        prop_assert_ne!(&pinned.declaration_digest, &observed.declaration_digest);

        let decisions = runtime().block_on(async {
            let store = Arc::new(InMemoryDriverStore::new());
            let mut manifest = DriverManifest::from_toml("id = \"notes\"\ncommand = \"mcp-notes\"\n")
                .unwrap()
                .with_tool(tool.clone(), class);
            manifest.declaration_digests.insert(tool.clone(), pinned.declaration_digest.clone());
            store.put(DriverRecord::new(manifest, "alice", chrono::Utc::now())).await.unwrap();
            let drivers = DriverSupervisor::builder(store).build();
            drivers.start().await.unwrap();
            let policy = DriverPolicy::new(drivers);
            (
                policy.decide("notes", &tool, &pinned.declaration_digest),
                policy.decide("notes", &tool, &observed.declaration_digest),
            )
        });
        let (as_pinned, as_changed) = decisions;
        prop_assert!(matches!(as_changed, PolicyDecision::RequireApproval { .. }), "{as_changed:?}");
        if class.is_never_auto_allowed() {
            prop_assert!(matches!(as_pinned, PolicyDecision::RequireApproval { .. }), "{:?}", as_pinned);
        } else {
            prop_assert_eq!(as_pinned, PolicyDecision::Allow);
        }
    }
}
