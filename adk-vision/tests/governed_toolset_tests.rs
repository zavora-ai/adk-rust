//! The governed vision toolset through `Runner` and `LlmAgent` with a scripted planner:
//! S4 (approve with the image), S7 (a budget per image), S1 and S2 (local first, escalate the
//! crop), the egress rules' three actions, the kill switch and edits.

mod governed;
mod support;

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest};
use adk_core::audit_log::{AuditRecordDraft, InMemoryAuditRecorder};
use adk_core::egress::EgressPolicy;
use adk_core::{
    ApprovalScope, ApprovalStore, Evidence, GovernanceControl, InMemoryApprovalStore,
    InMemorySpendLedger, Part, RunBudget, RunConfig, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod, ToolApproval,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::{
    BBox, DeterministicEditor, Digest, EgressRecord, Escalation, InMemoryEgressRecorder,
    MediaStore, PersonRedactor, PersonalTextRedactor, Route, Trigger, VisionConfig, VisionRuntime,
    VisionTask, VisionToolset,
};
use governed::*;
use serde_json::{Value, json};

const ASK_TOML: &str = r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document_crop"]
action = "ask"
"#;

struct Desk {
    artifacts: Arc<InMemoryArtifactService>,
    recorder: Arc<InMemoryEgressRecorder>,
    describer: Arc<RecordingDescriber>,
    toolset: VisionToolset,
    card: Card,
}

async fn desk(toml: &str) -> Desk {
    let card = identity_card();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    save_artifact(&artifacts, CARD, &card.png).await;
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = adk_vision::EgressGate::new(
        EgressPolicy::public_only(),
        VisionConfig::from_toml_str(toml).unwrap().egress,
        recorder.clone(),
    )
    .with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))))
    .with_redactor(Arc::new(
        PersonalTextRedactor::new(Arc::new(FixtureReader::for_card(&card)))
            .with_labelled_fields(["Name"]),
    ))
    .with_media_store(store.clone());
    let describer = Arc::new(RecordingDescriber::claude());
    let runtime = VisionRuntime::builder()
        .remote_describer(describer.clone())
        .editor(Arc::new(DeterministicEditor::new()))
        .egress_gate(gate)
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime))
        .data_classes("document", "document_crop")
        .build()
        .unwrap();
    Desk { artifacts, recorder, describer, toolset, card }
}

async fn sessions() -> Arc<InMemorySessionService> {
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
    sessions
}

fn agent(toolset: &VisionToolset, model: Arc<PlannerModel>) -> Arc<dyn adk_core::Agent> {
    Arc::new(
        LlmAgentBuilder::new("desk")
            .model(model)
            .toolset(Arc::new(toolset.clone()))
            .before_tool_callback(toolset.evidence_callback())
            .build()
            .unwrap(),
    )
}

fn records(recorder: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    recorder.records().into_iter().filter(|r| r.action == action).collect()
}

const CROP: [f32; 4] = [20.0, 10.0, 620.0, 320.0];

#[tokio::test]
async fn s4_a_remote_call_is_held_with_its_egress_image_and_releases_that_digest() {
    let desk = desk(ASK_TOML).await;
    let model = Arc::new(PlannerModel::calling(vec![("vision_describe", args_describe(CROP))]));
    let approver = Arc::new(RecordingApprover::default());
    let audit = InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model.clone()))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .run_config(RunConfig::builder().tool_confirmation_handler(approver.clone()).build())
        .tool_policy(desk.toolset.egress_policy())
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();

    let events = run_turn(&runner, "What kind of document is id-card.png?").await;

    // The call was held with the exact egress image.
    let seen = approver.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "one held call");
    assert_eq!(seen[0].tool_name, "vision_describe");
    let Evidence::Image(evidence) = &seen[0].evidence[0] else { panic!("image evidence") };
    let egress: Digest = evidence.digest.parse().unwrap();
    assert_eq!(
        evidence.caption.as_deref(),
        Some(
            "to api.anthropic.com as document_crop · 1 face · the whole person is covered, 4 personal details covered"
        )
    );
    let overlay = evidence.regions.clone().expect("the redactions are outlined");
    let labels: Vec<&str> = overlay["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        vec!["faces", "personal_text", "personal_text", "personal_text", "personal_text"]
    );

    // Approval released that digest, and the provider received exactly those bytes.
    let received = desk.describer.received();
    assert_eq!(received.len(), 1);
    assert_eq!(Digest::of(&received[0].bytes), egress);
    assert_eq!(received[0].receipt.egress, egress);
    assert_eq!(received[0].receipt.source, desk.card.digest());

    // The bytes hold no face and no personal string; the ordinary words are still there.
    let (sent, _) =
        adk_vision::DecodedImage::decode(&received[0].bytes, &Default::default()).unwrap();
    let chain = &received[0].receipt.chain;
    let reader = FixtureReader::for_card(&desk.card);
    let read: Vec<String> = reader.read_pixels(&sent, chain).into_iter().map(|(t, _)| t).collect();
    assert_eq!(read, vec!["IDENTITY", "CARD", "Name:", "Email:", "ID:", "Issued", "2026"]);
    assert!(all_black(&sent, &chain.bbox_to_target(&desk.card.portrait)));
    for word in desk.card.personal() {
        assert!(all_black(&sent, &chain.bbox_to_target(&word.bbox)), "{} is covered", word.text);
    }

    // The egress copy is an artifact with the same digest.
    let stored = desk
        .artifacts
        .load(LoadRequest {
            app_name: evidence.artifact.app_name.clone(),
            user_id: evidence.artifact.user_id.clone(),
            session_id: evidence.artifact.session_id.clone(),
            file_name: evidence.artifact.file_name.clone(),
            version: Some(evidence.artifact.version),
        })
        .await
        .unwrap();
    let Part::InlineData { data, .. } = stored.part else { panic!("inline image") };
    assert_eq!(Digest::of(&data), egress);

    // The audit trail links decision, source and egress digests.
    let released = records(&audit, "vision.egress.released");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].ext["sourceDigest"], json!(desk.card.digest()));
    assert_eq!(released[0].ext["egressDigest"], json!(egress));
    assert_eq!(released[0].ext["decisionKind"]["kind"], "approved");
    assert_eq!(released[0].ext["redactions"], json!({ "faces": 1, "personal_text": 4 }));
    assert_eq!(released[0].payload_ref.as_deref(), Some(egress.to_string().as_str()));
    let decided: Vec<Value> = audit
        .records()
        .iter()
        .filter(|r| r.action == "vision_describe")
        .filter_map(|r| r.ext.get("evidenceDigests").cloned())
        .collect();
    assert!(!decided.is_empty() && decided.iter().all(|d| d == &json!([egress])), "{decided:?}");
    let calls = records(&audit, "vision.call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].ext["egressDigests"], json!([egress]));
    assert_eq!(calls[0].ext["imageDigests"], json!([desk.card.digest()]));

    // The model got the answer, and nothing else left the machine.
    let answer = &tool_responses(&events, "vision_describe")[0];
    assert_eq!(answer["text"], "An identity card.");
    assert_eq!(desk.recorder.released().len(), 1);
}

#[tokio::test]
async fn s4_a_swapped_image_is_held_again_and_the_decision_stays_unspent() {
    let desk = desk(ASK_TOML).await;
    let model = Arc::new(PlannerModel::calling(vec![("vision_describe", args_describe(CROP))]));
    let store = Arc::new(InMemoryApprovalStore::new());
    let audit = InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model.clone()))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .approval_store(store.clone())
        .tool_policy(desk.toolset.egress_policy())
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let scope = ApprovalScope::new(APP, USER, SESSION);

    // Held with the egress image; nothing leaves.
    run_turn(&runner, "Describe id-card.png").await;
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    let held = pending[0].request.clone();
    let first = held.evidence[0].digest().to_string();
    assert!(desk.describer.received().is_empty());

    // The approver approves that image.
    store.decide(&scope, &held.fingerprint(), ToolApproval::approve()).await.unwrap();

    // The image is swapped before the call runs again: a different egress image, held again.
    let swapped = identity_card_shifted(12.0);
    save_artifact(&desk.artifacts, CARD, &swapped.png).await;
    run_turn(&runner, "Describe id-card.png").await;
    let again: Vec<_> = store
        .pending(&scope)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.request)
        .filter(|r| r.evidence[0].digest() != first)
        .collect();
    assert_eq!(again.len(), 1, "held again with the new image");
    assert_ne!(again[0].fingerprint(), held.fingerprint());
    assert!(desk.describer.received().is_empty(), "the swapped image did not leave");
    assert!(store.has_decision(&scope, &held.fingerprint()).await.unwrap(), "not spent");

    // The approved image restored: the decision applies, and exactly that digest leaves.
    save_artifact(&desk.artifacts, CARD, &desk.card.png).await;
    run_turn(&runner, "Describe id-card.png").await;
    let received = desk.describer.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].receipt.egress.to_string(), first);

    let released = records(&audit, "vision.egress.released");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].ext["egressDigest"], json!(first));
}

#[tokio::test]
async fn a_run_without_the_egress_policy_fails_closed() {
    let desk = desk(ASK_TOML).await;
    let model = Arc::new(PlannerModel::calling(vec![("vision_describe", args_describe(CROP))]));
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .build()
        .unwrap();

    let events = run_turn(&runner, "Describe id-card.png").await;

    let answer = tool_responses(&events, "vision_describe");
    let error = answer[0]["error"].as_str().unwrap();
    assert!(error.contains("install VisionToolset::egress_policy"), "{error}");
    assert!(desk.describer.received().is_empty());
    assert!(desk.recorder.released().is_empty());
}

#[tokio::test]
async fn a_deny_rule_refuses_before_anyone_is_asked() {
    let desk = desk(
        r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
action = "deny"
"#,
    )
    .await;
    let model = Arc::new(PlannerModel::calling(vec![("vision_describe", args_describe(CROP))]));
    let approver = Arc::new(RecordingApprover::default());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .run_config(RunConfig::builder().tool_confirmation_handler(approver.clone()).build())
        .tool_policy(desk.toolset.egress_policy())
        .build()
        .unwrap();

    let events = run_turn(&runner, "Describe id-card.png").await;

    assert!(approver.seen.lock().unwrap().is_empty(), "nobody is asked");
    assert!(desk.describer.received().is_empty());
    let refused: Vec<_> = desk
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r {
            EgressRecord::Refused(refusal) => Some(refusal),
            _ => None,
        })
        .collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].step, "decide");
    assert_eq!(refused[0].reason, "egress rule 0 denies `document_crop`");
    let error = tool_responses(&events, "vision_describe")[0].to_string();
    assert!(error.contains("denies"), "{error}");
}

#[tokio::test]
async fn an_allow_rule_releases_the_redacted_copy_without_asking() {
    let desk = desk(
        r#"
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document_crop"]
action = "allow"
"#,
    )
    .await;
    let model = Arc::new(PlannerModel::calling(vec![("vision_describe", args_describe(CROP))]));
    let approver = Arc::new(RecordingApprover::default());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .run_config(RunConfig::builder().tool_confirmation_handler(approver.clone()).build())
        .tool_policy(desk.toolset.egress_policy())
        .build()
        .unwrap();

    run_turn(&runner, "Describe id-card.png").await;

    assert!(approver.seen.lock().unwrap().is_empty());
    let received = desk.describer.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].receipt.decision, adk_vision::EgressDecision::Allowed { rule: Some(0) });
    let (sent, _) =
        adk_vision::DecodedImage::decode(&received[0].bytes, &Default::default()).unwrap();
    assert!(all_black(&sent, &received[0].receipt.chain.bbox_to_target(&desk.card.portrait)));
}

#[tokio::test]
async fn a_frozen_kill_switch_refuses_every_vision_call() {
    let desk = desk(ASK_TOML).await;
    let model = Arc::new(PlannerModel::calling(vec![("vision_describe", args_describe(CROP))]));
    let control = GovernanceControl::new();
    control.freeze("incident 7");
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .tool_policy(desk.toolset.egress_policy())
        .governance(control)
        .build()
        .unwrap();

    let mut stream = runner
        .run(
            adk_core::UserId::try_from(USER).unwrap(),
            adk_core::SessionId::try_from(SESSION).unwrap(),
            adk_core::Content::new("user").with_text("go"),
        )
        .await;
    if let Ok(stream) = stream.as_mut() {
        use futures::StreamExt;
        while stream.next().await.is_some() {}
    }

    assert!(desk.describer.received().is_empty());
    assert!(desk.recorder.released().is_empty());
}

/// S7 through the toolset: a run capped at 20 images makes 30 `sam-3.1` segmentation calls.
#[tokio::test]
async fn s7_the_twenty_first_image_is_refused_before_its_release() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let page = support::boxes_image(320, 240, &[("red", BBox::new(40.0, 40.0, 120.0, 120.0))]);
    let png = page.pixels().encode_png().unwrap();
    save_artifact(&artifacts, "part.png", &png).await;
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = adk_vision::EgressGate::new(
        EgressPolicy::public_only(),
        VisionConfig::from_toml_str("[egress]\ndefault = \"allow\"").unwrap().egress,
        recorder.clone(),
    );
    let sam = Arc::new(RemoteSegmenter::sam());
    let runtime = VisionRuntime::builder()
        .remote_segmenter(sam.clone())
        .egress_gate(gate)
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    let calls: Vec<(&str, Value)> = (0..30)
        .map(|i| {
            ("vision_segment", json!({ "image": "part.png", "concepts": [format!("part {i}")] }))
        })
        .collect();
    let model = Arc::new(PlannerModel::calling(calls));
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&toolset, model))
        .session_service(sessions().await)
        .artifact_service(artifacts.clone())
        .tool_policy(toolset.egress_policy())
        .budget(RunBudget::new().max_units(adk_core::UsageUnit::Images, 20))
        .spend_ledger(ledger.clone())
        .build()
        .unwrap();

    let events = run_turn(&runner, "Segment the part thirty times").await;

    let answers = tool_responses(&events, "vision_segment");
    assert_eq!(answers.len(), 30);
    let refused: Vec<&Value> = answers.iter().filter(|a| a.get("error").is_some()).collect();
    assert_eq!(answers.len() - refused.len(), 20);
    assert_eq!(sam.calls(), 20, "the 21st call was refused before its request");
    let reason = refused[0]["error"].as_str().unwrap();
    assert!(reason.contains("images") && reason.contains("20"), "names the unit and cap: {reason}");
    // Refused before release: only the twenty calls that ran left the machine.
    assert_eq!(recorder.released().len(), 20);
    let sam_spend = SpendKey::org(APP).with_vendor("meta");
    assert_eq!(ledger.spent(&sam_spend, SpendPeriod::Day).await.unwrap(), 20 * 2_500);
}

/// S1 and S2 through the toolset: a page whose fields all clear the local threshold sends
/// nothing; five low-confidence fields escalate five crops, never the page.
#[tokio::test]
async fn s1_s2_local_first_and_five_crops_not_the_page() {
    let (page, fields) = form();
    let printed: Vec<Field> = fields.iter().filter(|f| f.colour == GREEN).cloned().collect();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let clean = support::boxes_image(800, 600, &[("green", printed[0].bbox)]);
    save_artifact(&artifacts, "clean.png", &clean.pixels().encode_png().unwrap()).await;
    save_artifact(&artifacts, "form.png", &page.pixels().encode_png().unwrap()).await;
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = adk_vision::EgressGate::new(
        EgressPolicy::public_only(),
        VisionConfig::from_toml_str(
            "[egress]\ndefault = \"deny\"\n[[egress.rule]]\nhost = \"ocr.example\"\nclasses = [\"document_crop\"]\naction = \"allow\"",
        )
        .unwrap()
        .egress,
        recorder.clone(),
    )
    .with_media_store(store.clone());
    let runtime = VisionRuntime::builder()
        .reader(Arc::new(FieldReader::local(&fields, 0.3)))
        .remote_reader(Arc::new(FieldReader::cloud(&fields)))
        .route(
            VisionTask::Read,
            Route::to("pp-ocrv6").escalate(
                Escalation::to("cloud-ocr")
                    .on(Trigger::LowConfidence { threshold: 0.5 })
                    .with_margin(8.0)
                    .with_data_class("document_crop"),
            ),
        )
        .egress_gate(gate)
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let sessions = sessions().await;
    let runner = |model: Arc<PlannerModel>| {
        Runner::builder()
            .app_name(APP)
            .agent(agent(&toolset, model))
            .session_service(sessions.clone())
            .artifact_service(artifacts.clone())
            .tool_policy(toolset.egress_policy())
            .spend_ledger(ledger.clone())
            .audit_log(Arc::new(audit.clone()))
            .build()
            .unwrap()
    };

    // S1: every field clears the threshold, so nothing leaves the machine.
    let reads =
        Arc::new(PlannerModel::calling(vec![("vision_read", json!({ "image": "clean.png" }))]));
    run_turn(&runner(reads), "Read clean.png").await;
    assert!(recorder.is_empty(), "zero bytes off the machine");

    // S2: five handwritten fields read below the threshold; each escalates as its crop.
    let reads =
        Arc::new(PlannerModel::calling(vec![("vision_read", json!({ "image": "form.png" }))]));
    let events = run_turn(&runner(reads), "Read form.png").await;

    let released = recorder.released();
    assert_eq!(released.len(), 5, "exactly five egress records");
    for (receipt, field) in released.iter().zip(fields.iter().filter(|f| f.colour == BLUE)) {
        // The field's crop plus margin, never the page.
        let expected = (field.bbox.width() as u32 + 16, field.bbox.height() as u32 + 16);
        assert_eq!(receipt.chain.output_size(), expected);
        assert_eq!(receipt.source, page.digest());
        assert_eq!(receipt.hosts, vec!["ocr.example".to_string()]);
        assert_eq!(receipt.estimated_cost_micro_usd, Some(1_500));
        assert!(receipt.artifact.is_some(), "the egress copy is stored");
    }
    let answer = &tool_responses(&events, "vision_read")[0];
    let blocks = answer["blocks"].as_array().unwrap();
    let escalated: Vec<&Value> =
        blocks.iter().filter(|b| b.get("escalatedFrom").is_some()).collect();
    assert_eq!(escalated.len(), 5);
    for block in &escalated {
        assert_eq!(block["escalatedFrom"], "pp-ocrv6");
        assert!(block["text"].as_str().unwrap().ends_with("(cloud)"));
    }
    let audited = records(&audit, "vision.egress.released");
    assert_eq!(audited.len(), 5);
    for (record, receipt) in audited.iter().zip(&released) {
        assert_eq!(record.ext["sourceDigest"], json!(page.digest()));
        assert_eq!(record.ext["egressDigest"], json!(receipt.egress));
        assert_eq!(record.ext["destination"], json!(["ocr.example"]));
        assert_eq!(record.ext["estimatedCostMicroUsd"], json!(1_500));
    }
    // The reading spend equals the sum of its ledger entries: five pages at $0.0015.
    let calls = records(&audit, "vision.call");
    let costs: u64 = calls.iter().filter_map(|c| c.ext.get("costMicroUsd")?.as_u64()).sum();
    assert_eq!(costs, 5 * 1_500);
    let ocr_spend = SpendKey::org(APP).with_vendor("ocr");
    assert_eq!(ledger.spent(&ocr_spend, SpendPeriod::Day).await.unwrap(), costs);

    // Each read left its answer record beside the image for the annotated viewer: the
    // regions in source pixels with their origin, the blocks in reading order, every call.
    let names = artifacts
        .list(adk_artifact::ListRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
        })
        .await
        .unwrap()
        .file_names;
    let answers: Vec<&String> =
        names.iter().filter(|n| n.starts_with(adk_vision::ANSWER_ARTIFACT_PREFIX)).collect();
    assert_eq!(answers.len(), 2, "{names:?}");
    let mut form_record = None;
    for name in answers {
        let part = artifacts
            .load(LoadRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                file_name: name.clone(),
                version: None,
            })
            .await
            .unwrap()
            .part;
        let Part::InlineData { mime_type, data, .. } = part else { panic!("{name} holds no JSON") };
        assert_eq!(mime_type, "application/json");
        let record: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(record["kind"], "adkVisionAnswer");
        assert_eq!(record["tool"], "vision_read");
        assert_eq!(*name, adk_vision::answer_artifact_name(record["callId"].as_str().unwrap()));
        if record["image"]["name"] == "form.png" {
            form_record = Some(record);
        }
    }
    let record = form_record.expect("the form's answer record");
    assert_eq!(record["image"]["digest"], json!(page.digest()));
    assert_eq!(record["image"]["artifact"], json!(format!("vision-{}.png", page.digest().hex())));
    let regions = record["regionSet"]["regions"].as_array().unwrap();
    assert_eq!(regions.len(), blocks.len());
    assert_eq!(record["page"]["blocks"].as_array().unwrap().len(), blocks.len());
    assert!(regions.iter().all(|r| r["origin"]["frame"]["kind"].is_string()
        && r["origin"]["chain"]["sourceWidth"] == json!(800)));
    let escalated: Vec<&Value> =
        regions.iter().filter(|r| r["origin"].get("escalatedFrom").is_some()).collect();
    assert_eq!(escalated.len(), 5);
    let egress: Vec<Value> = record["calls"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|call| call["egress"].as_array().cloned().unwrap_or_default())
        .collect();
    let receipts: Vec<Value> = released.iter().map(|r| json!(r.egress)).collect();
    assert_eq!(egress, receipts, "the record names exactly the released crops");
    let schema: Value = serde_json::from_str(include_str!("../schema/region-set.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&record["regionSet"]), "the record's RegionSet matches the schema");
}

#[tokio::test]
async fn edits_are_new_images_with_a_vision_edit_record() {
    let desk = desk(ASK_TOML).await;
    let model = Arc::new(PlannerModel::calling(vec![(
        "vision_redact",
        json!({ "image": CARD, "regions": [[30, 80, 200, 300]] }),
    )]));
    let audit = InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent(&desk.toolset, model))
        .session_service(sessions().await)
        .artifact_service(desk.artifacts.clone())
        .tool_policy(desk.toolset.egress_policy())
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();

    let events = run_turn(&runner, "Cover the portrait").await;

    let answer = &tool_responses(&events, "vision_redact")[0];
    assert_eq!(answer["from"], json!(desk.card.digest()));
    let edits = records(&audit, "vision.edit");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].ext["inputDigest"], json!(desk.card.digest()));
    assert_eq!(edits[0].ext["outputDigest"], answer["image"]);
    assert!(desk.recorder.is_empty(), "an edit stays on the device");
}
