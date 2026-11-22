//! The verifier loop (spec 3.7 §7.14, "Verification"): a fresh observation after each action
//! is compared with the one before; a failed check re-grounds once, then holds for a person.

use std::sync::Arc;
use std::sync::Mutex;

use adk_artifact::InMemoryArtifactService;
use adk_core::{ApprovalScope, Evidence, GovernedCall, RunConfig, ToolAuthorization, ToolGate};
use adk_vision::{
    BBox, BackendId, CoordFrame, DecodedImage, Digest, ExpectedChange, LoopOutcome, MediaScope,
    MediaStore, Observation, Observer, Provenance, RawRegion, RegionExpectation, RegionNormalizer,
    RegionSet, TransformChain, VERIFICATION_TOOL, VerificationReport, VerifiedAction, VerifierLoop,
    VisionCx, check_change,
};

fn set(frame: &[u8], boxes: &[(BBox, &str)]) -> RegionSet {
    let digest = Digest::of(frame);
    let normalizer = RegionNormalizer::new(
        digest,
        TransformChain::new(200, 100),
        CoordFrame::SentPixels,
        BackendId::new("marks"),
    );
    let raw = boxes.iter().map(|(b, label)| RawRegion::boxed(*b, *label, 0.9)).collect();
    RegionSet::new(digest, 200, 100, normalizer.regions(raw).unwrap())
}

const SAVE_LEFT: BBox = BBox { x0: 10.0, y0: 70.0, x1: 50.0, y1: 90.0 };
const TOAST: BBox = BBox { x0: 60.0, y0: 10.0, x1: 140.0, y1: 30.0 };
const DIALOG: BBox = BBox { x0: 40.0, y0: 20.0, x1: 160.0, y1: 80.0 };

#[test]
fn a_check_passes_only_with_the_expected_change_and_nothing_else() {
    let before = set(b"before", &[(SAVE_LEFT, "button")]);
    let saved = set(b"saved", &[(SAVE_LEFT, "button"), (TOAST, "saved")]);
    let expected = ExpectedChange::none().appears(RegionExpectation::label("saved").within(TOAST));
    assert!(check_change(&before, &saved, &expected).passed);

    // The wrong button: a dialog appeared and no toast did.
    let wrong = set(b"wrong", &[(SAVE_LEFT, "button"), (DIALOG, "dialog")]);
    let report = check_change(&before, &wrong, &expected);
    assert!(!report.passed);
    assert_eq!(report.missing, vec!["`saved` did not appear in [60, 10, 140, 30]".to_string()]);
    assert_eq!(report.unexpected.len(), 1);
    assert_eq!(report.unexpected[0].label.as_deref(), Some("dialog"));

    // Something that vanished unexpectedly fails too.
    let gone = set(b"gone", &[(TOAST, "saved")]);
    let report = check_change(&before, &gone, &expected);
    assert!(!report.passed);
    assert_eq!(report.unexpected[0].label.as_deref(), Some("button"));
}

/// Observations served in order.
struct Script {
    observations: Mutex<Vec<Observation>>,
}

#[async_trait::async_trait]
impl Observer for Script {
    async fn observe(&self, _cx: &VisionCx) -> adk_vision::Result<Observation> {
        Ok(self.observations.lock().unwrap().remove(0))
    }
}

/// Clicks; on a failed check, grounds again and clicks again.
#[derive(Default)]
struct Click {
    attempts: Mutex<Vec<Option<VerificationReport>>>,
}

#[async_trait::async_trait]
impl VerifiedAction for Click {
    async fn act(
        &self,
        _cx: &VisionCx,
        failed: Option<&VerificationReport>,
    ) -> adk_vision::Result<ExpectedChange> {
        self.attempts.lock().unwrap().push(failed.cloned());
        Ok(ExpectedChange::none().appears(RegionExpectation::label("saved")))
    }
}

fn script(observations: Vec<RegionSet>) -> Arc<Script> {
    Arc::new(Script {
        observations: Mutex::new(observations.into_iter().map(Observation::new).collect()),
    })
}

#[tokio::test]
async fn a_mis_grounding_is_caught_and_grounded_again_once() {
    let observer = script(vec![
        set(b"before", &[(SAVE_LEFT, "button")]),
        set(b"wrong", &[(SAVE_LEFT, "button"), (DIALOG, "dialog")]),
        set(b"right", &[(SAVE_LEFT, "button"), (DIALOG, "dialog"), (TOAST, "saved")]),
    ]);
    let click = Click::default();
    let outcome = VerifierLoop::new(observer).run(&VisionCx::new(), &click).await.unwrap();
    let LoopOutcome::Verified { attempts, .. } = outcome else { panic!("{outcome:?}") };
    assert_eq!(attempts, 2);
    let seen = click.attempts.lock().unwrap().clone();
    assert!(seen[0].is_none(), "the first attempt has no failed check");
    assert!(seen[1].as_ref().is_some_and(|report| !report.passed), "the second gets it");
}

#[tokio::test]
async fn a_second_failure_holds_for_a_person_with_both_frames() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts);
    let scope = MediaScope::new("desk", "ana", "s1");
    let mut frames = Vec::new();
    for colour in [[200, 200, 200, 255], [190, 190, 190, 255], [180, 180, 180, 255]] {
        let (image, _) = store
            .put_pixels(&scope, DecodedImage::filled(200, 100, colour), Provenance::Upload)
            .await
            .unwrap();
        frames.push(image);
    }
    let observer = Arc::new(Script {
        observations: Mutex::new(vec![
            Observation::new(set(b"before", &[(SAVE_LEFT, "button")]))
                .with_frame(frames[0].clone()),
            Observation::new(set(b"wrong", &[(SAVE_LEFT, "button"), (DIALOG, "dialog")]))
                .with_frame(frames[1].clone()),
            Observation::new(set(b"still", &[(SAVE_LEFT, "button")])).with_frame(frames[2].clone()),
        ]),
    });
    let outcome =
        VerifierLoop::new(observer).run(&VisionCx::new(), &Click::default()).await.unwrap();
    let LoopOutcome::Held(hold) = outcome else { panic!("{outcome:?}") };
    assert_eq!(hold.reports.len(), 2);
    assert_eq!(hold.frames, vec![frames[1].clone(), frames[2].clone()]);

    // The hold is one governed call, with the frames before and after as evidence.
    let call: GovernedCall = hold.governed_call("verify-1");
    assert_eq!(call.tool_name, VERIFICATION_TOOL);
    let digests: Vec<String> = call.evidence.iter().map(|e| e.digest().to_string()).collect();
    assert_eq!(digests, vec![frames[1].digest.to_string(), frames[2].digest.to_string()]);
    let config = RunConfig::default();
    let gate = ToolGate::new(&config, "copilot", "inv-1", ApprovalScope::new("desk", "ana", "s1"));
    let ToolAuthorization::Pending { request, .. } =
        adk_core::authorize_tool_call(&gate, call).await.unwrap()
    else {
        panic!("held for a person")
    };
    assert!(matches!(request.evidence[1], Evidence::Image(_)));
}
