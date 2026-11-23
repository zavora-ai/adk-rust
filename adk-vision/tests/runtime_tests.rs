//! `VisionRuntime`: local-first routing, image preparation, the egress gate, metering,
//! cancellation, the kill switch and crop escalation (spec 3.7 §7.5, §7.8, §7.9, S1, S2, S7).

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_artifact::InMemoryArtifactService;
use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, BudgetTracker, ErrorCategory, GovernanceControl, InMemorySpendLedger, ModelCallScope,
    RunBudget, SpendKey, SpendLedger, SpendPeriod, UsageUnit,
};
use adk_vision::{
    BBox, CallOptions, DescribeSpec, DetectQuery, DeterministicEditor, EditOp, EgressAction,
    EgressConfig, EgressGate, EgressRecord, Escalation, EscalationReason, InMemoryEgressRecorder,
    Locality, MediaScope, MediaStore, Provenance, ReadOptions, RedactionClass, Rgba, Route,
    SegmentPrompt, Shape, Transform, Trigger, VisionCx, VisionError, VisionRuntime, VisionTask,
};
use support::{
    ColourDescriber, ColourDetector, ColourReader, ColourRedactor, ColourSegmenter, MeanEmbedder,
    RemoteReader, arc, boxes_image,
};

const RED: BBox = BBox { x0: 100.0, y0: 80.0, x1: 260.0, y1: 200.0 };
const BLUE: BBox = BBox { x0: 380.0, y0: 260.0, x1: 560.0, y1: 420.0 };

fn page() -> adk_vision::SourceImage {
    boxes_image(640, 480, &[("red", RED), ("blue", BLUE)])
}

fn query() -> DetectQuery {
    DetectQuery::Concepts(vec!["red".into(), "blue".into()])
}

fn near(a: BBox, b: BBox, tolerance: f32) -> bool {
    (a.x0 - b.x0).abs() <= tolerance
        && (a.y0 - b.y0).abs() <= tolerance
        && (a.x1 - b.x1).abs() <= tolerance
        && (a.y1 - b.y1).abs() <= tolerance
}

fn allow_all(recorder: Arc<InMemoryEgressRecorder>) -> EgressGate {
    EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        recorder,
    )
}

struct Metered {
    budget: Arc<BudgetTracker>,
    ledger: Arc<InMemorySpendLedger>,
    cx: VisionCx,
}

fn metered(budget: RunBudget) -> Metered {
    let budget = Arc::new(BudgetTracker::new(budget));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let scope =
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "acme");
    Metered { budget, ledger, cx: VisionCx::new().with_scope(scope) }
}

#[tokio::test]
async fn routing_is_local_first_whatever_the_registration_order() {
    let runtime = VisionRuntime::builder()
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .detector(arc(ColourDetector::local("local")))
        .egress_gate(allow_all(arc(InMemoryEgressRecorder::new())))
        .build()
        .unwrap();
    let out = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap();
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].backend.as_str(), "local");
    assert_eq!(out.calls[0].locality, Locality::InProcess);
}

#[tokio::test]
async fn regions_come_back_in_source_pixels_through_a_downscale() {
    let mut local = ColourDetector::local("half");
    local.descriptor.limits = adk_vision::InputLimits::long_edge(320);
    let runtime = VisionRuntime::builder().detector(arc(local)).build().unwrap();
    let out = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap();
    assert_eq!(out.value.len(), 2);
    for region in &out.value {
        let truth = if region.label.as_deref() == Some("red") { RED } else { BLUE };
        assert!(near(region.shape.bbox().unwrap(), truth, 2.0), "{:?} vs {truth:?}", region.shape);
        assert_eq!(region.origin.chain.steps, vec![Transform::Resize { width: 320, height: 240 }]);
        assert_eq!(region.image, page().digest());
    }
}

#[tokio::test]
async fn a_remote_backend_without_an_egress_gate_is_refused() {
    let remote = arc(ColourDetector::remote("cloud"));
    let runtime = VisionRuntime::builder().remote_detector(remote.clone()).build().unwrap();
    let err = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::NotReleased { .. }), "{err}");
    assert_eq!(AdkError::from(err).category, ErrorCategory::Forbidden);
    assert_eq!(remote.calls(), 0);
}

#[tokio::test]
async fn a_remote_descriptor_cannot_register_as_on_device() {
    // `detector` takes on-device backends; a remote descriptor there is refused at build.
    let err = VisionRuntime::builder()
        .detector(arc(ColourDetector::remote("cloud")))
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("remote_"), "{err}");
}

#[tokio::test]
async fn escalation_sends_the_crop_not_the_page() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let local = arc(ColourDetector::local("rf-detr-m").scoring("blue", 0.3));
    let cloud = arc(ColourDetector::remote("sam-3.1"));
    let runtime = VisionRuntime::builder()
        .detector(local.clone())
        .remote_detector(cloud.clone())
        .egress_gate(allow_all(recorder.clone()))
        .route(
            VisionTask::Detect,
            Route::to("rf-detr-m").escalate(
                Escalation::to("sam-3.1")
                    .on(Trigger::LowConfidence { threshold: 0.5 })
                    .with_margin(16.0)
                    .with_data_class("document_crop"),
            ),
        )
        .build()
        .unwrap();
    let m = metered(RunBudget::new());
    let out = runtime.detect(&m.cx, &page(), &query()).await.unwrap();

    // Exactly one release: the blue field and its margin, never the page.
    let released = recorder.released();
    assert_eq!(released.len(), 1);
    let receipt = &released[0];
    assert_eq!(receipt.data_class.as_str(), "document_crop");
    assert_eq!(
        receipt.chain.steps[0],
        Transform::Crop { x: 364, y: 244, width: 212, height: 192 },
        "the crop is the field plus a 16 px margin"
    );
    assert_eq!(receipt.chain.output_size(), (212, 192));
    assert_eq!(receipt.source, page().digest());

    // The cloud answer replaced the local one, with provenance, in source pixels.
    let blue = out.value.iter().find(|r| r.label.as_deref() == Some("blue")).unwrap();
    assert!(near(blue.shape.bbox().unwrap(), BLUE, 2.0), "{:?}", blue.shape);
    let from = blue.origin.escalated_from.as_ref().unwrap();
    assert_eq!(from.backend.as_str(), "rf-detr-m");
    assert_eq!(from.score, Some(0.3));
    assert_eq!(from.reason, EscalationReason::LowConfidence { threshold: 0.5 });
    assert_eq!(blue.origin.backend.as_str(), "sam-3.1");
    let red = out.value.iter().find(|r| r.label.as_deref() == Some("red")).unwrap();
    assert!(red.origin.escalated_from.is_none());
    assert_eq!(out.value.len(), 2);

    // Two calls; the second is the escalation and names the released digest.
    assert_eq!(out.calls.len(), 2);
    assert_eq!(out.calls[1].egress, vec![receipt.egress]);
    assert!(out.calls[1].escalation.is_some());
    // Per-image price on the remote call, zero on the local one, both counted as images.
    assert_eq!(out.calls[0].cost_micro_usd, Some(0));
    assert_eq!(out.calls[1].cost_micro_usd, Some(2_500));
    assert_eq!(m.budget.units().get(UsageUnit::Images), 2);
    assert_eq!(m.budget.usage().cost_micro_usd, 2_500);
    assert_eq!(m.ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 2_500);
}

#[tokio::test]
async fn pages_that_clear_the_threshold_send_nothing() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let cloud = arc(ColourDetector::remote("cloud"));
    let runtime = VisionRuntime::builder()
        .detector(arc(ColourDetector::local("local")))
        .remote_detector(cloud.clone())
        .egress_gate(allow_all(recorder.clone()))
        .route(
            VisionTask::Detect,
            Route::to("local")
                .escalate(Escalation::to("cloud").on(Trigger::LowConfidence { threshold: 0.5 })),
        )
        .build()
        .unwrap();
    runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap();
    assert!(recorder.is_empty(), "a confident page sent bytes off the machine");
    assert_eq!(cloud.calls(), 0);
}

#[tokio::test]
async fn only_no_result_escalates_a_whole_image() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .detector(arc(ColourDetector::local("blind").seeing(&[])))
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .egress_gate(allow_all(recorder.clone()))
        .route(
            VisionTask::Detect,
            Route::to("blind").escalate(Escalation::to("cloud").on(Trigger::NoResult)),
        )
        .build()
        .unwrap();
    let out = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap();
    assert_eq!(out.value.len(), 2);
    let released = recorder.released();
    assert_eq!(released.len(), 1);
    assert!(released[0].chain.steps.iter().all(|s| !matches!(s, Transform::Crop { .. })));
    assert!(out.value.iter().all(|r| r.origin.escalated_from.as_ref().unwrap().reason == EscalationReason::NoResult));
}

#[tokio::test]
async fn disagreement_escalates_only_the_region_in_dispute() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .detector(arc(ColourDetector::local("primary")))
        .detector(arc(ColourDetector::local("second").seeing(&["red"])))
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .egress_gate(allow_all(recorder.clone()))
        .route(
            VisionTask::Detect,
            Route::to("primary").escalate(
                Escalation::to("cloud")
                    .on(Trigger::Disagreement { with: "second".into(), min_iou: 0.5 }),
            ),
        )
        .build()
        .unwrap();
    let out = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap();
    assert_eq!(recorder.released().len(), 1);
    let blue = out.value.iter().find(|r| r.label.as_deref() == Some("blue")).unwrap();
    assert!(matches!(
        blue.origin.escalated_from.as_ref().unwrap().reason,
        EscalationReason::Disagreement { .. }
    ));
}

#[tokio::test]
async fn a_disagreement_needs_an_on_device_second_opinion() {
    let err = VisionRuntime::builder()
        .detector(arc(ColourDetector::local("primary")))
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .route(
            VisionTask::Detect,
            Route::to("primary").escalate(
                Escalation::to("cloud")
                    .on(Trigger::Disagreement { with: "cloud".into(), min_iou: 0.5 }),
            ),
        )
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("on-device"), "{err}");
}

#[tokio::test]
async fn an_ask_rule_holds_until_that_exact_image_is_approved() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let gate =
        EgressGate::new(EgressPolicy::public_only(), EgressConfig::default(), recorder.clone());
    let cloud = arc(ColourDetector::remote("cloud"));
    let runtime =
        VisionRuntime::builder().remote_detector(cloud.clone()).egress_gate(gate).build().unwrap();
    let cx = VisionCx::new();

    let err = runtime.detect(&cx, &page(), &query()).await.unwrap_err();
    let VisionError::EgressHeld { egress, .. } = err else { panic!("expected a hold, got {err}") };
    assert_eq!(cloud.calls(), 0, "a held image reached the backend");

    // Approving that digest releases that image.
    let approved = CallOptions::new().approving(egress);
    runtime.detect_with(&cx, &page(), &query(), &approved).await.unwrap();
    assert_eq!(recorder.released()[0].egress, egress);

    // A different image (here, one more region redacted) is a different digest: held again.
    let swapped = approved.clone().with_marked(vec![BBox::new(0.0, 0.0, 10.0, 10.0)]);
    let err = runtime.detect_with(&cx, &page(), &query(), &swapped).await.unwrap_err();
    assert!(matches!(err, VisionError::EgressHeld { egress: other, .. } if other != egress));
}

#[tokio::test]
async fn deny_rules_and_unlisted_destinations_are_refused_and_recorded() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let config = EgressConfig::default().with_rule("vision.example", &[], EgressAction::Deny);
    let runtime = VisionRuntime::builder()
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .egress_gate(EgressGate::new(EgressPolicy::public_only(), config, recorder.clone()))
        .build()
        .unwrap();
    let err = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::EgressRefused { .. }), "{err}");
    assert!(matches!(&recorder.records()[0], EgressRecord::Refused(r) if r.step == "decide"));

    let recorder = arc(InMemoryEgressRecorder::new());
    let policy = EgressPolicy::allowlist(["api.other.example"]).unwrap();
    let config = EgressConfig::default().with_default(EgressAction::Allow);
    let runtime = VisionRuntime::builder()
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .egress_gate(EgressGate::new(policy, config, recorder.clone()))
        .build()
        .unwrap();
    let err = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::EgressRefused { .. }), "{err}");
    assert!(matches!(&recorder.records()[0], EgressRecord::Refused(r) if r.step == "destination"));
}

#[tokio::test]
async fn required_redaction_happens_before_release_or_nothing_leaves() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let config = EgressConfig::default()
        .with_default(EgressAction::Allow)
        .redacting(RedactionClass::faces());
    let without = VisionRuntime::builder()
        .remote_detector(arc(ColourDetector::remote("cloud")))
        .egress_gate(EgressGate::new(EgressPolicy::public_only(), config.clone(), recorder.clone()))
        .build()
        .unwrap();
    let err = without.detect(&VisionCx::new(), &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::RedactionUnavailable { .. }), "{err}");
    assert!(recorder.released().is_empty());

    // With a redactor (here it "finds" the red box), the red box leaves as solid black.
    let recorder = arc(InMemoryEgressRecorder::new());
    let gate = EgressGate::new(EgressPolicy::public_only(), config, recorder.clone())
        .with_redactor(arc(ColourRedactor { class: RedactionClass::faces(), colour: [255, 0, 0] }));
    let cloud = arc(ColourDetector::remote("cloud"));
    let with =
        VisionRuntime::builder().remote_detector(cloud.clone()).egress_gate(gate).build().unwrap();
    let out = with.detect(&VisionCx::new(), &page(), &query()).await.unwrap();
    let receipt = &recorder.released()[0];
    assert_eq!(receipt.redactions.get(&RedactionClass::faces()), Some(&1));
    assert_eq!(receipt.chain.steps.last(), Some(&Transform::Redact));
    // The cloud backend never saw red: only the blue box comes back.
    assert_eq!(
        out.value.iter().filter_map(|r| r.label.as_deref()).collect::<Vec<_>>(),
        vec!["blue"]
    );
}

#[tokio::test]
async fn a_frozen_kill_switch_refuses_calls_and_releases() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let local = arc(ColourDetector::local("local"));
    let cloud = arc(ColourDetector::remote("cloud"));
    let runtime = VisionRuntime::builder()
        .detector(local.clone())
        .remote_detector(cloud.clone())
        .egress_gate(allow_all(recorder.clone()))
        .build()
        .unwrap();
    let control = GovernanceControl::new();
    control.freeze("incident 7");
    let cx = VisionCx::new().with_governance(control);
    for route in ["local", "cloud"] {
        let options = CallOptions::new().with_route(Route::to(route));
        let err = runtime.detect_with(&cx, &page(), &query(), &options).await.unwrap_err();
        assert_eq!(AdkError::from(err).code, adk_core::GOVERNANCE_FROZEN_CODE);
    }
    assert_eq!(local.calls() + cloud.calls(), 0);
    assert!(recorder.released().is_empty());
}

#[tokio::test]
async fn cancellation_ends_a_call_at_once() {
    let slow = arc(ColourDetector::local("slow").slow(Duration::from_secs(30)));
    let runtime = VisionRuntime::builder().detector(slow).build().unwrap();
    let cx = VisionCx::new();
    let token = cx.cancellation().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    let started = Instant::now();
    let err = runtime.detect(&cx, &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::Cancelled), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_spent_budget_refuses_the_next_priced_call_before_its_request() {
    let cloud = arc(ColourDetector::remote("sam-3.1"));
    let runtime = VisionRuntime::builder()
        .remote_detector(cloud.clone())
        .egress_gate(allow_all(arc(InMemoryEgressRecorder::new())))
        .build()
        .unwrap();
    let m = metered(RunBudget::new().max_cost_micro_usd(5_000));
    let mut refused = None;
    for _ in 0..4 {
        if let Err(err) = runtime.detect(&m.cx, &page(), &query()).await {
            refused = Some(err);
            break;
        }
    }
    let err = refused.expect("the budget refused a call");
    assert_eq!(AdkError::from(err).category, ErrorCategory::ResourceExhausted);
    assert_eq!(cloud.calls(), 2, "the refused call reached the backend");
    assert_eq!(m.budget.usage().cost_micro_usd, 5_000);
}

#[tokio::test]
async fn segment_prompts_reach_the_backend_in_its_frame_and_masks_come_back_in_source_pixels() {
    let segmenter = arc(ColourSegmenter::local());
    let runtime = VisionRuntime::builder().segmenter(segmenter.clone()).build().unwrap();
    let prompts = vec![
        SegmentPrompt::Concept { text: "blue".into() },
        SegmentPrompt::Box { bbox: BLUE },
        SegmentPrompt::Point { at: [470.0, 340.0], positive: true },
    ];
    let out = runtime.segment(&VisionCx::new(), &page(), &prompts).await.unwrap();
    let Shape::Mask(mask) = &out.value[0].shape else { panic!("not a mask") };
    assert_eq!((mask.width, mask.height), (640, 480));
    assert!(near(mask.bbox().unwrap(), BLUE, 2.0), "{:?}", mask.bbox());

    // 640x480 into 320x320, centred: scale 0.5, 40 px of padding on top.
    let seen = segmenter.seen.lock().unwrap().clone();
    let SegmentPrompt::Box { bbox } = &seen[1] else { panic!() };
    assert_eq!(*bbox, BBox::new(190.0, 170.0, 280.0, 250.0));
    let SegmentPrompt::Point { at, .. } = &seen[2] else { panic!() };
    assert_eq!(*at, [235.0, 210.0]);
}

#[tokio::test]
async fn low_confidence_blocks_are_reread_from_their_crop() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .reader(arc(ColourReader::local("pp-ocr", 0.2)))
        .remote_reader(arc(RemoteReader::new()))
        .egress_gate(allow_all(recorder.clone()))
        .route(
            VisionTask::Read,
            Route::to("pp-ocr").escalate(
                Escalation::to("cloud-reader").on(Trigger::LowConfidence { threshold: 0.5 }),
            ),
        )
        .build()
        .unwrap();
    let out = runtime.read(&VisionCx::new(), &page(), &ReadOptions::default()).await.unwrap();
    assert_eq!(recorder.released().len(), 1);
    let blue = out.value.blocks.iter().find(|b| b.text.starts_with("blue")).unwrap();
    assert_eq!(blue.text, "blue text from cloud-reader");
    assert_eq!(blue.confidence, Some(0.99));
    let from = blue.region.origin.escalated_from.as_ref().unwrap();
    assert_eq!(from.text.as_deref(), Some("blue text from pp-ocr"));
    assert_eq!(from.backend.as_str(), "pp-ocr");
    assert!(near(blue.region.shape.bbox().unwrap(), BLUE, 2.0));
    assert!(near(blue.words[0].region.shape.bbox().unwrap(), BLUE, 2.0));
    let red = out.value.blocks.iter().find(|b| b.text.starts_with("red")).unwrap();
    assert_eq!(red.text, "red text from pp-ocr");
}

/// The on-device profile (spec 3.7 S19): each escalation is refused at the gate with its
/// reason, and the on-device answers stand.
#[tokio::test]
async fn a_refused_escalation_leaves_the_on_device_answer() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let config = EgressConfig::default().with_default(EgressAction::Allow).on_device();
    let read_route = Route::to("pp-ocr")
        .escalate(Escalation::to("cloud-reader").on(Trigger::LowConfidence { threshold: 0.5 }));
    let describe_route = Route::to("qwen3-vl")
        .escalate(Escalation::to("gemini").on(Trigger::LowConfidence { threshold: 0.6 }));
    let runtime = VisionRuntime::builder()
        .reader(arc(ColourReader::local("pp-ocr", 0.2)))
        .remote_reader(arc(RemoteReader::new()))
        .describer(arc(ColourDescriber::local("qwen3-vl", 0.4)))
        .remote_describer(arc(ColourDescriber::remote("gemini", 0.95)))
        .egress_gate(EgressGate::new(EgressPolicy::public_only(), config, recorder.clone()))
        .route(VisionTask::Read, read_route)
        .route(VisionTask::Describe, describe_route)
        .build()
        .unwrap();
    let cx = VisionCx::new();

    let read = runtime.read(&cx, &page(), &ReadOptions::default()).await.unwrap();
    let image = page();
    let spec = DescribeSpec::new("Which colours?").with_regions();
    let described = runtime.describe(&cx, &[&image], &spec).await.unwrap();

    let blue = read.value.blocks.iter().find(|b| b.text.starts_with("blue")).unwrap();
    assert_eq!(blue.text, "blue text from pp-ocr");
    assert!(blue.region.origin.escalated_from.is_none());
    assert!(described.value.escalated_from.is_none());
    assert!(read.calls.iter().chain(&described.calls).all(|c| c.locality == Locality::InProcess));
    assert!(recorder.released().is_empty());
    let refusals: Vec<_> = recorder
        .records()
        .into_iter()
        .map(|r| match r {
            EgressRecord::Refused(r) => (r.step, r.reason.contains("on-device profile")),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(refusals, vec![("destination".to_string(), true); 2]);
}

#[tokio::test]
async fn describe_escalates_a_low_confidence_answer_with_provenance() {
    let recorder = arc(InMemoryEgressRecorder::new());
    let runtime = VisionRuntime::builder()
        .describer(arc(ColourDescriber::local("qwen3-vl", 0.4)))
        .remote_describer(arc(ColourDescriber::remote("gemini", 0.95)))
        .egress_gate(allow_all(recorder.clone()))
        .route(
            VisionTask::Describe,
            Route::to("qwen3-vl")
                .escalate(Escalation::to("gemini").on(Trigger::LowConfidence { threshold: 0.6 })),
        )
        .build()
        .unwrap();
    let image = page();
    let spec = DescribeSpec::new("Which colours?").with_regions();
    let out = runtime.describe(&VisionCx::new(), &[&image], &spec).await.unwrap();
    assert!(out.value.text.as_deref().unwrap().starts_with("gemini says"));
    let from = out.value.escalated_from.as_ref().unwrap();
    assert_eq!(from.backend.as_str(), "qwen3-vl");
    assert_eq!(from.score, Some(0.4));
    assert_eq!(out.value.regions.len(), 2);
    for region in &out.value.regions {
        let truth = if region.label.as_deref() == Some("red") { RED } else { BLUE };
        assert!(near(region.shape.bbox().unwrap(), truth, 2.0));
        assert!(region.origin.escalated_from.is_some());
    }
    // Token-billed: the runtime records the image at no cost of its own.
    assert_eq!(out.calls[1].cost_micro_usd, None);
}

#[tokio::test]
async fn embeddings_must_fill_their_space() {
    let mut wrong = MeanEmbedder::local();
    wrong.dims = 2;
    let runtime = VisionRuntime::builder().embedder(arc(wrong)).build().unwrap();
    let image = page();
    let err =
        runtime.embed_images(&VisionCx::new(), &[&image], &CallOptions::new()).await.unwrap_err();
    assert!(matches!(err, VisionError::Parse { .. }), "{err}");

    let runtime = VisionRuntime::builder().embedder(arc(MeanEmbedder::local())).build().unwrap();
    let out = runtime
        .embed_images(&VisionCx::new(), &[&image, &image], &CallOptions::new())
        .await
        .unwrap();
    assert_eq!(out.value.len(), 2);
    assert_eq!(out.calls[0].units.get(UsageUnit::Images), 2);
}

#[tokio::test]
async fn edits_are_stored_as_derived_images() {
    let store = MediaStore::new(arc(InMemoryArtifactService::new()));
    let runtime = VisionRuntime::builder()
        .editor(arc(DeterministicEditor::new()))
        .media_store(store.clone())
        .build()
        .unwrap();
    let cx = VisionCx::new().with_media_scope(MediaScope::new("desk", "u1", "s1"));
    let image = page();
    let op = EditOp::Redact { regions: vec![BLUE], color: Rgba::BLACK };
    let first = runtime.edit(&cx, &image, &op, &CallOptions::new()).await.unwrap();
    let second = runtime.edit(&cx, &image, &op, &CallOptions::new()).await.unwrap();
    assert_eq!(
        first.value.image.artifact, second.value.image.artifact,
        "the same edit is one artifact"
    );
    let Provenance::Derived { from, .. } = first.value.image.provenance else { panic!() };
    assert_eq!(from, image.digest());
    let loaded = store.load(&first.value.image).await.unwrap();
    assert_eq!(loaded.pixels().pixel(470, 340), [0, 0, 0, 255]);
    assert_eq!(loaded.pixels().pixel(150, 100), [255, 0, 0, 255]);

    let no_scope =
        runtime.edit(&VisionCx::new(), &image, &op, &CallOptions::new()).await.unwrap_err();
    assert!(no_scope.to_string().contains("media scope"));
}

#[tokio::test]
async fn routes_must_name_registered_backends() {
    let err = VisionRuntime::builder()
        .detector(arc(ColourDetector::local("local")))
        .route(VisionTask::Detect, Route::to("missing"))
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("missing"));
    let err = VisionRuntime::builder()
        .detector(arc(ColourDetector::local("same")))
        .detector(arc(ColourDetector::local("same")))
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("already registered"));
    let runtime = VisionRuntime::builder().build().unwrap();
    let err = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::BackendNotFound(_)));
}

#[tokio::test]
async fn oversized_input_is_refused_with_the_limit_when_the_backend_says_so() {
    let mut strict = ColourDetector::local("strict");
    strict.descriptor.limits = adk_vision::InputLimits::long_edge(320).refusing();
    let runtime = VisionRuntime::builder().detector(arc(strict)).build().unwrap();
    let err = runtime.detect(&VisionCx::new(), &page(), &query()).await.unwrap_err();
    assert!(matches!(err, VisionError::InputTooLarge { .. }), "{err}");
    assert!(err.to_string().contains("320 px on the long edge"), "{err}");
}

/// A describer that records whether the run's metering scope is current while it runs, as
/// a token-billed model call made inside it would need.
struct ScopeProbe {
    descriptor: adk_vision::BackendDescriptor,
    ambient: ModelCallScope,
    saw_run_scope: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl adk_vision::Describer for ScopeProbe {
    fn descriptor(&self) -> &adk_vision::BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        _cx: &VisionCx,
        _request: &adk_vision::DescribeRequest,
    ) -> adk_vision::Result<adk_vision::Description<adk_vision::ImageRegion>> {
        let current = ModelCallScope::current().is_some_and(|scope| scope.same_as(&self.ambient));
        self.saw_run_scope.store(current, std::sync::atomic::Ordering::SeqCst);
        Ok(adk_vision::Description { text: Some("ok".into()), ..Default::default() })
    }
}

#[tokio::test]
async fn an_unmetered_context_leaves_the_run_scope_current_for_backend_model_calls() {
    let ambient = ModelCallScope::new().with_budget(Arc::new(BudgetTracker::new(RunBudget::new())));
    let probe = arc(ScopeProbe {
        descriptor: adk_vision::BackendDescriptor::new(
            "local-vlm",
            Locality::InProcess,
            adk_vision::CoordFrame::SentPixels,
        ),
        ambient: ambient.clone(),
        saw_run_scope: std::sync::atomic::AtomicBool::new(false),
    });
    let runtime = VisionRuntime::builder().describer(probe.clone()).build().unwrap();
    let image = page();
    ambient
        .within(runtime.describe(&VisionCx::new(), &[&image], &DescribeSpec::new("what is this?")))
        .await
        .unwrap();
    assert!(probe.saw_run_scope.load(std::sync::atomic::Ordering::SeqCst));
}
