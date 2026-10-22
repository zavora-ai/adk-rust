//! `Pack::Managed` on the builder (spec 3.1 §7.18): with feature `managed` it marks the
//! harness for the managed runtime; without it the build names the feature. The voice pack
//! of §7.17 is still refused as not yet implemented.

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

/// The voice surface (spec 3.1 §7.17) is a pack like the managed form: marked on the
/// harness, served by `packs::voice::VoiceSurface`, refused without its feature.
#[cfg(feature = "voice")]
#[test]
fn pack_voice_marks_the_harness() {
    let workspace = tempfile::tempdir().unwrap();
    let harness = builder(workspace.path()).pack(Pack::Voice).build().unwrap();
    assert!(harness.info().packs.iter().any(|pack| pack == "voice"), "{:?}", harness.info().packs);
}

#[cfg(not(feature = "voice"))]
#[test]
fn pack_voice_needs_the_feature() {
    let workspace = tempfile::tempdir().unwrap();
    let error = builder(workspace.path()).pack(Pack::Voice).build().unwrap_err();
    assert!(error.to_string().contains("feature `voice`"), "{error}");
}
