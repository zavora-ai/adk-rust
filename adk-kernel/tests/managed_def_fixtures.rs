//! Golden fixtures for the kernel block of a managed agent definition (spec 3.2 §7.13,
//! §10 M1), in the managed runtime's fixture style: each file in `tests/fixtures/managed`
//! holds a definition, its kernel block and what the block must read as — its canonical
//! form with the defaults explicit, whether the consolidation tick is on, and the triggers a
//! tenant kernel registers — or the refusal it must meet. On `origin/main` there is no kernel
//! block: `adk-kernel` has no `managed` module.

#![cfg(feature = "managed")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use adk_core::Agent;
use adk_kernel::KernelError;
use adk_kernel::managed::{KERNEL_DEF_METADATA_KEY, KernelDef, TenantKernel};
use adk_managed::ManagedStores;
use adk_managed::types::ManagedAgentDef;
use adk_model::ScriptedLlm;
use adk_session::InMemorySessionService;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    name: String,
    #[allow(dead_code)]
    description: String,
    agent_def: Value,
    kernel: Value,
    expect: Expect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    #[serde(default)]
    canonical: Option<Value>,
    #[serde(default)]
    consolidation: Option<bool>,
    #[serde(default)]
    triggers: Option<Vec<String>>,
    #[serde(default)]
    metadata_kept: Option<BTreeMap<String, String>>,
    #[serde(default)]
    error_contains: Vec<String>,
}

fn fixtures() -> Vec<(PathBuf, Fixture)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let fixture = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            (path, fixture)
        })
        .collect()
}

/// The fixture's definition with its block stored under `adk.kernel`, after a JSON round
/// trip as a session record crosses the managed state store.
fn definition(fixture: &Fixture) -> ManagedAgentDef {
    let mut def: ManagedAgentDef = serde_json::from_value(fixture.agent_def.clone()).unwrap();
    def.metadata
        .get_or_insert_with(BTreeMap::new)
        .insert(KERNEL_DEF_METADATA_KEY.to_string(), fixture.kernel.to_string());
    serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap()
}

/// The trigger names a tenant kernel registers for `def`.
async fn triggers(def: &ManagedAgentDef) -> Vec<String> {
    let agent: Arc<dyn Agent> = Arc::new(
        adk_agent::LlmAgentBuilder::new(def.name.clone())
            .model(Arc::new(ScriptedLlm::new(
                "scripted",
                Vec::<adk_model::ScriptedResponse>::new(),
            )))
            .build()
            .unwrap(),
    );
    let tenant = TenantKernel::builder(
        "tenant-a",
        ManagedStores::in_memory(),
        Arc::new(InMemorySessionService::new()),
    )
    .build()
    .unwrap();
    assert!(tenant.host(def, agent).await.unwrap(), "the definition carries a kernel block");
    tenant.start().await.unwrap();
    let kernel = tenant.kernel().await.unwrap();
    let names = kernel.wake_router().triggers().iter().map(|t| t.name.clone()).collect();
    tenant.stop().await.unwrap();
    names
}

#[tokio::test]
async fn every_fixture_reads_as_its_golden_form_or_meets_its_refusal() {
    let fixtures = fixtures();
    assert!(fixtures.len() >= 10, "the corpus has {} fixtures", fixtures.len());
    for (path, fixture) in fixtures {
        let def = definition(&fixture);
        let read = KernelDef::from_agent_def(&def);
        if !fixture.expect.error_contains.is_empty() {
            let reason = match read {
                Err(KernelError::Config { reason }) => reason,
                other => panic!("{}: expected a refusal, got {other:?}", fixture.name),
            };
            for needle in &fixture.expect.error_contains {
                assert!(reason.contains(needle), "{}: `{needle}` not in: {reason}", fixture.name);
            }
            continue;
        }
        let kernel = read
            .unwrap_or_else(|error| panic!("{}: {error}", fixture.name))
            .unwrap_or_else(|| panic!("{}: no kernel block", path.display()));
        // `to_string` writes an `f32` share as the number it was written as.
        let canonical: Value =
            serde_json::from_str(&serde_json::to_string(&kernel).unwrap()).unwrap();
        assert_eq!(
            Some(&canonical),
            fixture.expect.canonical.as_ref(),
            "{}: canonical form",
            fixture.name
        );
        let again: KernelDef = serde_json::from_value(canonical).unwrap();
        assert_eq!(again, kernel, "{}: the canonical form reads back as the block", fixture.name);
        if let Some(on) = fixture.expect.consolidation {
            assert_eq!(kernel.consolidation().is_some(), on, "{}: consolidation", fixture.name);
        }
        if let Some(kept) = &fixture.expect.metadata_kept {
            let metadata = def.metadata.clone().unwrap();
            for (key, value) in kept {
                assert_eq!(metadata.get(key), Some(value), "{}: metadata `{key}`", fixture.name);
            }
        }
        if let Some(expected) = &fixture.expect.triggers {
            assert_eq!(&triggers(&def).await, expected, "{}: triggers", fixture.name);
        }
    }
}
