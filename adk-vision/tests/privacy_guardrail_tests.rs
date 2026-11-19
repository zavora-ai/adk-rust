//! S3, "redact before it leaves": `ImagePrivacyGuardrail` on the model calls of an agent
//! whose model is remote, on tool results and on user input.

mod governed;

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::egress::EgressPolicy;
use adk_core::{Content, DataLocality, Part};
use adk_guardrail::{Guardrail, GuardrailResult, ToolGuardrail, ToolResultGuardrailResult};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::{
    DecodedImage, Digest, EgressGate, EgressRecord, ImagePrivacyGuardrail, InMemoryEgressRecorder,
    MediaStore, PersonRedactor, PersonalTextRedactor, VisionConfig,
};
use base64::Engine as _;
use governed::*;
use serde_json::json;

const ALLOW_CONTEXT: &str = r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["model_context"]
action = "allow"
"#;

struct Guarded {
    guardrail: Arc<ImagePrivacyGuardrail>,
    recorder: Arc<InMemoryEgressRecorder>,
    artifacts: Arc<InMemoryArtifactService>,
    audit: InMemoryAuditRecorder,
    card: Card,
}

fn guarded(toml: &str, locality: DataLocality) -> Guarded {
    let card = identity_card();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = Arc::new(
        EgressGate::new(
            EgressPolicy::public_only(),
            VisionConfig::from_toml_str(toml).unwrap().egress,
            recorder.clone(),
        )
        .with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))))
        .with_redactor(Arc::new(
            PersonalTextRedactor::new(Arc::new(FixtureReader::for_card(&card)))
                .with_labelled_fields(["Name"]),
        ))
        .with_media_store(MediaStore::new(artifacts.clone())),
    );
    let model = ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new())
        .with_data_locality(locality);
    let audit = InMemoryAuditRecorder::new();
    let guardrail = Arc::new(
        ImagePrivacyGuardrail::for_model(gate, &model, ["api.anthropic.com"])
            .with_audit(AuditTrail::new(Arc::new(audit.clone()))),
    );
    Guarded { guardrail, recorder, artifacts, audit, card }
}

fn card_part(card: &Card) -> Part {
    Part::InlineData {
        mime_type: "image/png".into(),
        data: card.png.clone(),
        uri: None,
        annotations: None,
    }
}

fn with_part(mut content: Content, part: Part) -> Content {
    content.parts.push(part);
    content
}

async fn runner(guarded: &Guarded, model: Arc<ScriptedLlm>) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let agent = LlmAgentBuilder::new("desk")
        .model(model)
        .before_model_callback(guarded.guardrail.before_model_callback())
        .build()
        .unwrap();
    Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions)
        .artifact_service(guarded.artifacts.clone())
        .build()
        .unwrap()
}

/// The image parts of every request the model received.
fn sent_images(model: &ScriptedLlm) -> Vec<Vec<u8>> {
    model
        .requests()
        .iter()
        .flat_map(|r| r.contents.iter())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| match p {
            Part::InlineData { mime_type, data, .. } if mime_type.starts_with("image/") => {
                Some(data.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn s3_the_provider_receives_no_face_and_no_personal_string() {
    let guarded = guarded(ALLOW_CONTEXT, DataLocality::Remote);
    let model = Arc::new(
        ScriptedLlm::new("claude-sonnet-5-5", [ScriptedResponse::text("An identity card.")])
            .with_data_locality(DataLocality::Remote),
    );
    let runner = runner(&guarded, model.clone()).await;
    let content = with_part(
        Content::new("user").with_text("What kind of document is this?"),
        card_part(&guarded.card),
    );

    run_content(&runner, content).await;

    let sent = sent_images(&model);
    assert_eq!(sent.len(), 1);
    let receipt = guarded.recorder.released()[0].clone();
    assert_eq!(Digest::of(&sent[0]), receipt.egress, "the provider got the released bytes");
    assert_eq!(receipt.source, guarded.card.digest());
    // The local detector finds no person and the local reader no personal string.
    let (pixels, _) = DecodedImage::decode(&sent[0], &Default::default()).unwrap();
    let read: Vec<String> = FixtureReader::for_card(&guarded.card)
        .read_pixels(&pixels, &receipt.chain)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    for word in guarded.card.personal() {
        assert!(!read.contains(&word.text.to_string()), "{} still reads", word.text);
    }
    assert!(read.contains(&"IDENTITY".to_string()));
    assert!(all_black(&pixels, &receipt.chain.bbox_to_target(&guarded.card.portrait)));
    // The egress copy is an artifact, and the audit record links both digests.
    let artifact = receipt.artifact.clone().expect("stored");
    assert_eq!(artifact.file_name, format!("vision-{}.png", receipt.egress.hex()));
    let audited: Vec<_> = guarded
        .audit
        .records()
        .into_iter()
        .filter(|r| r.action == "vision.egress.released")
        .collect();
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0].ext["sourceDigest"], json!(guarded.card.digest()));
    assert_eq!(audited[0].ext["egressDigest"], json!(receipt.egress));
    assert_eq!(audited[0].ext["dataClass"], "model_context");
}

#[tokio::test]
async fn an_image_the_rule_asks_about_is_withheld_with_a_placeholder() {
    let guarded = guarded(
        "[egress]\ndefault = \"ask\"\nredact = [\"faces\", \"personal_text\"]",
        DataLocality::Remote,
    );
    let model = Arc::new(
        ScriptedLlm::new("claude-sonnet-5-5", [ScriptedResponse::text("I cannot see it.")])
            .with_data_locality(DataLocality::Remote),
    );
    let runner = runner(&guarded, model.clone()).await;
    let content =
        with_part(Content::new("user").with_text("What is this?"), card_part(&guarded.card));

    run_content(&runner, content).await;

    assert!(sent_images(&model).is_empty(), "no image reached the model");
    let text: String = model.requests()[0]
        .contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(text.contains(&format!("[image {} withheld", guarded.card.digest())), "{text}");
    let refused = guarded.recorder.records();
    assert!(matches!(&refused[0], EgressRecord::Refused(r) if r.step == "decide"));
}

#[tokio::test]
async fn a_model_on_the_device_is_not_guarded() {
    let guarded = guarded(ALLOW_CONTEXT, DataLocality::InProcess);
    assert!(!guarded.guardrail.guards());
    let model = Arc::new(
        ScriptedLlm::new("qwen3-vl", [ScriptedResponse::text("An identity card.")])
            .with_data_locality(DataLocality::InProcess),
    );
    let runner = runner(&guarded, model.clone()).await;
    let content =
        with_part(Content::new("user").with_text("What is this?"), card_part(&guarded.card));

    run_content(&runner, content).await;

    assert_eq!(sent_images(&model), vec![guarded.card.png.clone()]);
    assert!(guarded.recorder.is_empty());
}

#[tokio::test]
async fn a_screenshot_in_a_tool_result_is_released_or_withheld() {
    let guarded = guarded(ALLOW_CONTEXT, DataLocality::Remote);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&guarded.card.png);
    let result = json!({ "screenshot": format!("data:image/png;base64,{encoded}"), "url": "https://crm.example" });

    let ToolResultGuardrailResult::Revise { result: revised, .. } =
        guarded.guardrail.validate_result("browser_screenshot", &json!({}), &result).await
    else {
        panic!("the screenshot is gated");
    };
    let payload =
        revised["screenshot"].as_str().unwrap().strip_prefix("data:image/png;base64,").unwrap();
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).unwrap();
    assert_eq!(Digest::of(&bytes), guarded.recorder.released()[0].egress);
    assert_eq!(revised["url"], "https://crm.example");
}

#[tokio::test]
async fn user_input_is_transformed_and_a_released_part_passes_again() {
    let guarded = guarded(ALLOW_CONTEXT, DataLocality::Remote);
    let content = with_part(Content::new("user").with_text("Here it is"), card_part(&guarded.card));

    let GuardrailResult::Transform { new_content, .. } = guarded.guardrail.validate(&content).await
    else {
        panic!("the image is released");
    };
    let released = new_content.parts[1].clone();
    let Part::InlineData { data, annotations, .. } = &released else { panic!("an image") };
    assert_eq!(Digest::of(data), guarded.recorder.released()[0].egress);
    assert!(annotations.as_ref().unwrap()["adkVision"]["release"]["id"].is_string());

    // The released part is recognised and not released twice.
    assert!(matches!(guarded.guardrail.validate(&new_content).await, GuardrailResult::Pass));
    assert_eq!(guarded.recorder.released().len(), 1);

    // A forged annotation on the original bytes does not pass.
    let Part::InlineData { annotations, .. } = released else { unreachable!() };
    let forged = with_part(
        Content::new("user"),
        Part::InlineData {
            mime_type: "image/png".into(),
            data: guarded.card.png.clone(),
            uri: None,
            annotations,
        },
    );
    assert!(matches!(guarded.guardrail.validate(&forged).await, GuardrailResult::Transform { .. }));
}

/// A reader that returns fixed blocks, without words.
struct BlocksReader {
    descriptor: adk_vision::BackendDescriptor,
    blocks: Vec<(&'static str, adk_vision::BBox)>,
}

#[async_trait::async_trait]
impl adk_vision::DocumentReader for BlocksReader {
    fn descriptor(&self) -> &adk_vision::BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &adk_vision::VisionCx,
        _page: &adk_vision::SentImage,
        _options: &adk_vision::ReadOptions,
    ) -> adk_vision::Result<adk_vision::DocumentPage<adk_vision::RawRegion>> {
        let blocks: Vec<adk_vision::Block<adk_vision::RawRegion>> = self
            .blocks
            .iter()
            .map(|(text, bbox)| adk_vision::Block {
                kind: adk_vision::BlockKind::Text,
                region: adk_vision::RawRegion::shape(adk_vision::Shape::Box(*bbox)),
                text: (*text).to_string(),
                confidence: Some(0.9),
                words: Vec::new(),
                cells: Vec::new(),
            })
            .collect();
        let reading_order = (0..blocks.len()).collect();
        Ok(adk_vision::DocumentPage { blocks, reading_order, markdown: None })
    }
}

/// A form whose reader returns a label and its value as separate blocks: the value beside
/// the bare label is covered, the label and the next line are not.
#[tokio::test]
async fn the_value_beside_a_bare_label_is_covered() {
    use adk_vision::{
        BBox, BackendDescriptor, CoordFrame, DataClass, EgressConfig, Locality, ReleaseRequest,
        SourceImage, VisionCx,
    };
    let value = BBox::new(300.0, 20.0, 420.0, 44.0);
    let reader = BlocksReader {
        descriptor: BackendDescriptor::new("blocks", Locality::InProcess, CoordFrame::SentPixels),
        blocks: vec![
            ("Name:", BBox::new(20.0, 22.0, 80.0, 42.0)),
            ("Ana Diaz", value),
            ("Issued: 2026", BBox::new(20.0, 70.0, 160.0, 90.0)),
            ("Wanjiru", BBox::new(300.0, 70.0, 380.0, 90.0)),
        ],
    };
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().redacting(adk_vision::RedactionClass::personal_text()),
        Arc::new(InMemoryEgressRecorder::new()),
    )
    .with_redactor(Arc::new(
        PersonalTextRedactor::new(Arc::new(reader)).with_labelled_fields(["Name"]),
    ));
    let page = SourceImage::from_pixels(DecodedImage::filled(480, 120, [255; 4])).unwrap();
    let backend = BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
        CoordFrame::SentPixels,
    );
    let request = ReleaseRequest::new(&page, &backend, DataClass::new("document"));

    let preview = gate.preview(&VisionCx::new(), &request).await.unwrap();

    let filled: Vec<BBox> = preview.filled().iter().map(|(_, bbox)| *bbox).collect();
    assert_eq!(filled, vec![value.expanded(adk_vision::REDACTION_MARGIN).clamped(480, 120)]);
}
