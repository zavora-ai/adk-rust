//! Approval evidence (spec 3.7 X4, S4, V-I5): a held call carries the exact image it would
//! send, its decision binds to the image's digest through the call's fingerprint, and a call
//! whose image changed before it ran is held again.

use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::{ActingContext, AgentPrincipal, SubjectRef};
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRoute, ApprovalScope, ApprovalService, ApprovalStore,
    ApproverSelector, ArtifactLocator, Evidence, GovernedCall, ImageEvidence,
    InMemoryApprovalStore, InboxQuery, Result, RunConfig, StoreApprovalService, Tool, ToolApproval,
    ToolAuthorization, ToolConfirmationRequest, ToolContext, ToolGate, ToolOrigin,
    authorize_tool_call, tool_call_fingerprint,
};
use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::{Value, json};

const CROP: &str = "sha256:9f2c0a7e5b1d4c8a6f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a";
const SWAPPED: &str = "sha256:0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8";

fn image(digest: &str) -> Evidence {
    ImageEvidence::new(
        digest,
        ArtifactLocator::new("desk", "ana", "s1", "egress/invoice-7-total.png", 1),
        "image/png",
        640,
        180,
    )
    .into()
}

/// Sends the current egress copy of a crop to a cloud model; the copy can change between the
/// hold and the run while the arguments stay the same.
struct SendCrop {
    egress_digest: Arc<Mutex<String>>,
}

#[async_trait]
impl Tool for SendCrop {
    fn name(&self) -> &str {
        "vision_describe"
    }

    fn description(&self) -> &str {
        "Describes a redacted crop with a cloud model"
    }

    fn approval_evidence(&self, _args: &Value) -> Vec<Evidence> {
        vec![image(&self.egress_digest.lock().unwrap())]
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        Ok(args)
    }
}

fn args() -> Value {
    json!({ "artifact": "invoice-7.png", "question": "What is the total?" })
}

#[tokio::test]
async fn an_image_swapped_before_execution_is_held_again() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let recorder = InMemoryAuditRecorder::new();
    let config = RunConfig::builder()
        .approval_store(store.clone())
        .audit(AuditTrail::new(Arc::new(recorder.clone())))
        .build();
    let scope = ApprovalScope::new("desk", "ana", "s1");
    let gate = ToolGate::new(&config, "document_desk", "inv-1", scope.clone());
    let digest = Arc::new(Mutex::new(CROP.to_string()));
    let tool = SendCrop { egress_digest: digest.clone() };
    let call = |id: &str| GovernedCall::for_tool(&tool, id, args()).requiring_confirmation(true);

    // Held with the egress image as evidence.
    let ToolAuthorization::Pending { request: held, .. } =
        authorize_tool_call(&gate, call("call-1")).await.unwrap()
    else {
        panic!("the call is held");
    };
    assert_eq!(held.evidence, vec![image(CROP)]);
    assert_eq!(store.pending(&scope).await.unwrap()[0].request, held);

    // The approver approves that image.
    store.decide(&scope, &held.fingerprint(), ToolApproval::approve()).await.unwrap();

    // The image is swapped before the call runs: a different call, held again.
    *digest.lock().unwrap() = SWAPPED.to_string();
    let resumed = call("call-2").resuming(held.clone());
    let ToolAuthorization::Pending { request: again, .. } =
        authorize_tool_call(&gate, resumed).await.unwrap()
    else {
        panic!("a swapped image is held again");
    };
    assert_eq!(again.evidence, vec![image(SWAPPED)]);
    assert_ne!(again.fingerprint(), held.fingerprint());
    assert!(store.has_decision(&scope, &held.fingerprint()).await.unwrap(), "not spent");

    // With the approved image back, the decision applies and the call runs.
    *digest.lock().unwrap() = CROP.to_string();
    assert!(matches!(
        authorize_tool_call(&gate, call("call-3").resuming(held.clone())).await.unwrap(),
        ToolAuthorization::Execute { .. }
    ));

    // Every record of the call names the image it was decided on, by digest.
    let digests: Vec<Value> = recorder
        .records()
        .iter()
        .map(|record| record.ext.get("evidenceDigests").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        digests,
        vec![json!([CROP]), json!([SWAPPED]), json!([CROP]), json!([CROP])],
        "held, held again, approval, allowed"
    );
    let approval = &recorder.records()[2];
    assert_eq!(approval.ext["source"], "approval_store");
}

#[tokio::test]
async fn an_inbox_request_carries_the_calls_evidence_then_the_routes() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let recorder = InMemoryAuditRecorder::new();
    let overlay = image(SWAPPED);
    let route_overlay = overlay.clone();
    let service = Arc::new(
        StoreApprovalService::new(store.clone())
            .with_router(move |_call: &adk_core::ApprovalCall| {
                Some(
                    ApprovalRoute::new(ApprovalPolicy::new([ApproverSelector::group("privacy")]))
                        .with_evidence([route_overlay.clone()]),
                )
            })
            .with_audit(AuditTrail::new(Arc::new(recorder.clone()))),
    );
    let config = RunConfig::builder().approval_service(service.clone()).build();
    let gate =
        ToolGate::new(&config, "document_desk", "inv-1", ApprovalScope::new("desk", "ana", "s1"));
    let tool = SendCrop { egress_digest: Arc::new(Mutex::new(CROP.to_string())) };
    let call = GovernedCall::for_tool(&tool, "call-1", args()).requiring_confirmation(true);
    let ToolAuthorization::Pending { request: held, .. } =
        authorize_tool_call(&gate, call).await.unwrap()
    else {
        panic!("the call is held");
    };

    let inbox = service.inbox(InboxQuery::for_group("privacy")).await.unwrap().items;
    assert_eq!(inbox.len(), 1);
    let request = &inbox[0];
    assert_eq!(request.evidence, vec![image(CROP), overlay]);
    assert_eq!(request.fingerprint, held.fingerprint(), "bound to the call's own image");
    let wire = serde_json::to_value(request).unwrap();
    assert_eq!(wire["evidence"][0]["kind"], "image");
    assert_eq!(wire["evidence"][0]["digest"], CROP);

    let decided = service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human("pia")).with_groups(["privacy"]),
        )
        .await
        .unwrap();
    assert_eq!(decided.evidence, request.evidence);
    let record = recorder.records().pop().unwrap();
    assert_eq!(record.ext["approvalId"], json!(request.id));
    assert_eq!(record.ext["evidenceDigests"], json!([CROP, SWAPPED]));
}

fn treasurer(call: ToolConfirmationRequest) -> ToolConfirmationRequest {
    call.with_acting_context(ActingContext::new(AgentPrincipal::derived("desk", "reader")))
        .with_origin(ToolOrigin::new("vision", &json!({ "name": "vision_describe" }), None))
}

#[test]
fn a_call_without_evidence_keeps_its_fingerprint() {
    let call = treasurer(ToolConfirmationRequest::new("vision_describe", args()));
    let origin = call.origin.clone().unwrap();
    let expected = format!(
        "urn:adk:agent:desk/reader\u{1f}{}\u{1f}{}\u{1f}{}",
        origin.source,
        origin.declaration_digest,
        tool_call_fingerprint("vision_describe", &args())
    );
    assert_eq!(call.fingerprint(), expected);
    assert_eq!(call.clone().with_evidence([]).fingerprint(), expected);
}

#[test]
fn evidence_keeps_the_fingerprints_five_parts() {
    let call = treasurer(ToolConfirmationRequest::new("vision_describe", args()))
        .with_evidence([image(CROP)]);
    let fingerprint = call.fingerprint();
    assert_eq!(
        ToolConfirmationRequest::fingerprint_agent(&fingerprint),
        Some("urn:adk:agent:desk/reader")
    );
    let parts: Vec<&str> = fingerprint.splitn(5, '\u{1f}').collect();
    assert_eq!(parts.len(), 5);
    assert_eq!(parts[3], "vision_describe");
    assert!(parts[2].ends_with(&format!("\u{1e}evidence:[\"{CROP}\"]")), "{:?}", parts[2]);
    let request = adk_core::ApprovalRequest::new(
        "apr-1",
        ApprovalScope::new("desk", "ana", "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::human("ana")]),
    );
    assert!(request.shows_call_args(), "the shown arguments are still the call's");
}

#[test]
fn evidence_round_trips_and_older_records_read_without_it() {
    let call =
        ToolConfirmationRequest::new("vision_describe", args()).with_evidence([Evidence::from(
            ImageEvidence::new(
                CROP,
                ArtifactLocator::new("desk", "ana", "s1", "egress/crop.png", 4),
                "image/webp",
                320,
                200,
            )
            .with_regions(json!({ "regions": [] }))
            .with_caption("crop"),
        )]);
    let wire = serde_json::to_value(&call).unwrap();
    assert_eq!(serde_json::from_value::<ToolConfirmationRequest>(wire).unwrap(), call);
    let older = json!({ "toolName": "vision_describe", "args": args() });
    let read: ToolConfirmationRequest = serde_json::from_value(older).unwrap();
    assert!(read.evidence.is_empty());
    assert!(serde_json::to_value(&read).unwrap().get("evidence").is_none());
}

fn digest() -> impl Strategy<Value = String> {
    prop_oneof![
        "sha256:[0-9a-f]{64}",
        // Hostile digests: separators, quotes, commas and brackets must not collide.
        "[a-z:\u{1e}\u{1f}\",\\[\\] ]{0,12}",
    ]
}

fn evidence_of(digests: &[String], file: &str, caption: Option<&str>) -> Vec<Evidence> {
    digests
        .iter()
        .map(|digest| {
            let mut image = ImageEvidence::new(
                digest.as_str(),
                ArtifactLocator::new("desk", "ana", "s1", file, 1),
                "image/png",
                64,
                64,
            );
            if let Some(caption) = caption {
                image = image.with_caption(caption);
            }
            image.into()
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Two calls share a fingerprint exactly when their evidence digests are the same list;
    /// where the image is stored, how it is captioned and what regions it carries do not
    /// change it.
    #[test]
    fn a_decision_never_applies_to_a_different_digest(
        first in proptest::collection::vec(digest(), 0..4),
        second in proptest::collection::vec(digest(), 0..4),
        caption in proptest::option::of("[a-z ]{0,10}"),
    ) {
        let base = treasurer(ToolConfirmationRequest::new("vision_describe", args()));
        let held = base.clone().with_evidence(evidence_of(&first, "egress/a.png", None));
        let other = base.clone().with_evidence(evidence_of(&second, "egress/b.png", caption.as_deref()));
        prop_assert_eq!(held.fingerprint() == other.fingerprint(), first == second);
        let fingerprint = held.fingerprint();
        prop_assert_eq!(
            ToolConfirmationRequest::fingerprint_agent(&fingerprint),
            Some("urn:adk:agent:desk/reader")
        );
        if first.is_empty() {
            prop_assert_eq!(held.fingerprint(), base.fingerprint());
        }
    }
}
