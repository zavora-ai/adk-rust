//! `Pack::Managed` on the builder (spec 3.1 §7.18): with feature `managed` it marks the
//! harness for the managed runtime; without it the build names the feature. The other
//! packs of §7.17 are still refused as not yet implemented.

use std::sync::Arc;

use adk_harness::{HarnessAgent, HarnessAgentBuilder, InstructionsConfig, Pack};
use adk_model::{ScriptedLlm, ScriptedResponse};

fn builder(workspace: &std::path::Path) -> HarnessAgentBuilder {
    HarnessAgent::builder("h")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(workspace))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
}

#[cfg(feature = "managed")]
#[test]
fn pack_managed_marks_the_harness_for_the_managed_runtime() {
    let workspace = tempfile::tempdir().unwrap();
    let harness = builder(workspace.path()).pack(Pack::Managed).build().unwrap();
    assert_eq!(harness.info().packs, ["managed"]);
}

#[cfg(not(feature = "managed"))]
#[test]
fn pack_managed_needs_the_feature() {
    let workspace = tempfile::tempdir().unwrap();
    let error = builder(workspace.path()).pack(Pack::Managed).build().unwrap_err();
    assert!(error.to_string().contains("feature `managed`"), "{error}");
}

#[test]
fn the_other_packs_are_still_not_implemented() {
    let workspace = tempfile::tempdir().unwrap();
    let error = builder(workspace.path()).pack(Pack::Browser).build().unwrap_err();
    assert!(error.to_string().contains("not yet implemented"), "{error}");
}
