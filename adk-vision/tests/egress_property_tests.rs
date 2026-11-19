//! Properties of the egress gate (spec 3.7 §11): no release without redaction under random
//! policies, and a held decision never applies to a different digest.

mod governed;

use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, EgressAction, EgressConfig, EgressGate,
    InMemoryEgressRecorder, Locality, PersonRedactor, PersonalTextRedactor, RedactionClass,
    ReleaseRequest, SourceImage, VisionCx, VisionError,
};
use governed::*;
use proptest::prelude::*;

fn backend() -> BackendDescriptor {
    BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
        CoordFrame::SentPixels,
    )
}

fn action(index: u8) -> EgressAction {
    match index % 3 {
        0 => EgressAction::Allow,
        1 => EgressAction::Ask,
        _ => EgressAction::Deny,
    }
}

/// The card with its portrait moved `shift` pixels, as a fresh source image.
fn card(shift: f32) -> Card {
    identity_card_shifted(shift)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the policy, an image that leaves has had every required class filled: no
    /// face and no personal word is visible, and a required class with no redactor refuses.
    #[test]
    fn no_release_without_redaction(
        default in 0u8..3,
        rule in proptest::option::of(0u8..3),
        require_faces in any::<bool>(),
        require_text in any::<bool>(),
        install_faces in any::<bool>(),
        install_text in any::<bool>(),
        shift in 0.0f32..20.0,
        crop in proptest::option::of((0.0f32..200.0, 0.0f32..80.0)),
        approve in any::<bool>(),
    ) {
        let card = card(shift);
        let mut config = EgressConfig::default().with_default(action(default));
        if let Some(rule) = rule {
            config = config.with_rule("api.anthropic.com", &[], action(rule));
        }
        if require_faces {
            config = config.redacting(RedactionClass::faces());
        }
        if require_text {
            config = config.redacting(RedactionClass::personal_text());
        }
        let recorder = Arc::new(InMemoryEgressRecorder::new());
        let mut gate = EgressGate::new(EgressPolicy::public_only(), config, recorder.clone());
        if install_faces {
            gate = gate.with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))));
        }
        if install_text {
            gate = gate.with_redactor(Arc::new(
                PersonalTextRedactor::new(Arc::new(FixtureReader::for_card(&card)))
                    .with_labelled_fields(["Name"]),
            ));
        }
        let descriptor = backend();
        let cx = VisionCx::new();
        let crop = crop.map(|(x, y)| BBox::new(x, y, x + 420.0, y + 300.0));
        let request = || {
            let mut request = ReleaseRequest::new(&card.source, &descriptor, "document".into());
            if let Some(crop) = crop {
                request = request.with_crop(crop);
            }
            request
        };

        let outcome = runtime().block_on(async {
            let preview = gate.preview(&cx, &request()).await;
            let mut request = request();
            if approve && let Ok(preview) = &preview {
                request = request.approved(preview.digest());
            }
            gate.release(&cx, request).await
        });

        let missing = (require_faces && !install_faces) || (require_text && !install_text);
        match outcome {
            Ok(released) => {
                prop_assert!(!missing, "released although a required class has no redactor");
                let (pixels, _) = DecodedImage::decode(released.bytes(), &Default::default()).unwrap();
                let chain = &released.receipt().chain;
                let (w, h) = pixels.size();
                if require_faces {
                    let face = chain.bbox_to_target(&card.portrait).clamped(w, h);
                    prop_assert!(face.area() < 1.0 || all_black(&pixels, &face), "a face left");
                }
                if require_text {
                    for word in card.personal() {
                        let target = chain.bbox_to_target(&word.bbox).clamped(w, h);
                        prop_assert!(
                            target.area() < 1.0 || !shows(&pixels, &target, BLUE),
                            "{} left", word.text
                        );
                    }
                }
                prop_assert_eq!(recorder.released().len(), 1);
            }
            Err(VisionError::RedactionUnavailable { .. }) => prop_assert!(missing),
            Err(VisionError::EgressHeld { .. }) => prop_assert!(!approve),
            Err(VisionError::EgressRefused { .. }) => {}
            Err(other) => prop_assert!(false, "unexpected error {other}"),
        }
    }

    /// An approval names one egress digest; an image whose redacted copy differs is held.
    #[test]
    fn a_held_decision_never_applies_to_a_different_digest(
        approved_shift in 0.0f32..30.0,
        sent_shift in 0.0f32..30.0,
    ) {
        let approved_card = card(approved_shift);
        let sent_card = card(sent_shift);
        let config = EgressConfig::default()
            .with_default(EgressAction::Ask)
            .redacting(RedactionClass::faces());
        let recorder = Arc::new(InMemoryEgressRecorder::new());
        let gate = EgressGate::new(EgressPolicy::public_only(), config, recorder.clone())
            .with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))));
        let descriptor = backend();
        let cx = VisionCx::new();

        let (approved, outcome) = runtime().block_on(async {
            let approved = gate
                .preview(&cx, &ReleaseRequest::new(&approved_card.source, &descriptor, "document".into()))
                .await
                .unwrap()
                .digest();
            let request = ReleaseRequest::new(&sent_card.source, &descriptor, "document".into())
                .approved(approved);
            (approved, gate.release(&cx, request).await)
        });

        match outcome {
            Ok(released) => prop_assert_eq!(released.receipt().egress, approved),
            Err(VisionError::EgressHeld { egress, .. }) => prop_assert_ne!(egress, approved),
            Err(other) => prop_assert!(false, "unexpected error {other}"),
        }
    }
}

#[test]
fn a_redactor_never_sees_a_remote_backend() {
    let remote = BackendDescriptor::new(
        "cloud-people",
        Locality::Remote { hosts: vec!["people.example".into()] },
        CoordFrame::SentPixels,
    );
    struct Remote(BackendDescriptor);
    #[async_trait::async_trait]
    impl adk_vision::Detector for Remote {
        fn descriptor(&self) -> &BackendDescriptor {
            &self.0
        }
        async fn detect(
            &self,
            _: &VisionCx,
            _: &adk_vision::SentImage,
            _: &adk_vision::DetectQuery,
        ) -> adk_vision::Result<Vec<adk_vision::RawRegion>> {
            Ok(Vec::new())
        }
    }
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default()
            .with_default(EgressAction::Allow)
            .redacting(RedactionClass::faces()),
        Arc::new(InMemoryEgressRecorder::new()),
    )
    .with_redactor(Arc::new(PersonRedactor::new(Arc::new(Remote(remote)))));
    let source = SourceImage::from_pixels(DecodedImage::filled(64, 64, [255; 4])).unwrap();
    let descriptor = backend();
    let outcome = runtime().block_on(
        gate.release(&VisionCx::new(), ReleaseRequest::new(&source, &descriptor, "photo".into())),
    );
    let err = outcome.unwrap_err().to_string();
    assert!(err.contains("a redactor runs on the device"), "{err}");
}
