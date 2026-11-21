//! Vision team members in the harness (spec 3.7 §7.12): the perceiver, reasoner and verifier
//! presets, and the build refusing a perceiver that could send what it sees off the machine.

#![cfg(all(feature = "vision", feature = "team"))]

use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::egress::ToolEgress;
use adk_core::{DataLocality, Llm, Tool, ToolContext};
use adk_harness::packs::vision::VisionPack;
use adk_harness::{HarnessAgent, MemberKind, MemberSpec, MemberTools};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_vision::{
    BackendDescriptor, CoordFrame, DetectQuery, Detector, Locality, MediaStore, RawRegion,
    SentImage, VisionConfig, VisionCx, VisionRuntime,
};
use serde_json::Value;

struct Nothing(BackendDescriptor);

#[async_trait::async_trait]
impl Detector for Nothing {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        _image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        Ok(Vec::new())
    }
}

struct Upload;

#[async_trait::async_trait]
impl Tool for Upload {
    fn name(&self) -> &str {
        "upload_frame"
    }

    fn description(&self) -> &str {
        "Uploads a frame"
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        ToolEgress::for_urls(["https://uploads.example.com"])
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(Value::Null)
    }
}

fn model(name: &str, locality: DataLocality) -> Arc<dyn Llm> {
    Arc::new(ScriptedLlm::new(name, [ScriptedResponse::text("done")]).with_data_locality(locality))
}

fn build(members: Vec<MemberSpec>) -> adk_core::Result<HarnessAgent> {
    let dir = tempfile::tempdir().unwrap();
    let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
    let pack = VisionPack::new(store, |gate, store| {
        VisionRuntime::builder()
            .detector(Arc::new(Nothing(BackendDescriptor::new(
                "rf-detr-n",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ))))
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    })
    .config(VisionConfig::default());
    let mut builder = HarnessAgent::builder("harness")
        .model(model("claude-sonnet-5-5", DataLocality::Remote))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(adk_harness::InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..Default::default()
        })
        .pack_vision(pack)
        .without_compaction();
    for member in members {
        builder = builder.member(member);
    }
    builder.build()
}

#[test]
fn a_perceiver_on_a_remote_model_is_refused() {
    let error = build(vec![MemberSpec::perceiver(model("claude-haiku-5-5", DataLocality::Remote))])
        .unwrap_err();
    assert!(error.to_string().contains("remote model 'claude-haiku-5-5'"), "{error}");
}

#[test]
fn a_perceiver_holding_a_remote_destination_tool_is_refused() {
    let mut perceiver = MemberSpec::perceiver(model("qwen3-vl-local", DataLocality::InProcess));
    perceiver.tools = MemberTools::LocalPerception(vec![Arc::new(Upload)]);
    let error = build(vec![perceiver]).unwrap_err();
    assert!(error.to_string().contains("`upload_frame`"), "{error}");

    let mut full = MemberSpec::perceiver(model("qwen3-vl-local", DataLocality::InProcess));
    full.tools = MemberTools::Full;
    let error = build(vec![full]).unwrap_err();
    assert!(error.to_string().contains("on-device tools"), "{error}");
}

#[test]
fn the_three_roles_build_as_members() {
    let harness = build(vec![
        MemberSpec::perceiver(model("qwen3-vl-local", DataLocality::InProcess)),
        MemberSpec::reasoner(model("claude-haiku-5-5", DataLocality::Remote)),
        MemberSpec::verifier(model("nvidia/nemotron-3.5-super-vl-120b-a12b", DataLocality::Remote)),
    ])
    .unwrap();
    let team = harness.runtime().team().expect("the harness has members");
    let names = team.member_names();
    for role in ["perceiver", "reasoner", "verifier"] {
        assert!(names.iter().any(|name| name == role), "{names:?}");
    }
    let reasoner = MemberSpec::reasoner(model("claude-haiku-5-5", DataLocality::Remote));
    assert_eq!(reasoner.output_schema, Some(adk_agent::opinion_schema()));
    assert!(matches!(reasoner.tools, MemberTools::Custom(ref tools) if tools.is_empty()));
    assert!(matches!(
        MemberSpec::perceiver(model("qwen3-vl-local", DataLocality::InProcess)).kind,
        MemberKind::Perceiver { .. }
    ));
}
