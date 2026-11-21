//! The default on-device redactors (spec 3.7 §7.8, Q1, Q3): `PersonRedactor::rf_detr` and
//! `PersonalTextRedactor::pp_ocr` load their weights on first use, and a load that fails
//! refuses the release at the redact step.
//!
//! The real-weights proof of S3 with these redactors is adk-harness's vision pack test
//! `s3_the_default_redactors_leave_no_person_and_no_personal_string`.

use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_vision::{
    BackendDescriptor, CoordFrame, DataClass, DecodedImage, EgressAction, EgressConfig, EgressGate,
    EgressRecord, FetchConfig, InMemoryAcceptanceStore, InMemoryEgressRecorder, Locality,
    ModelFetcher, PersonRedactor, PersonalTextRedactor, PpOcrV6Size, RedactionClass, Redactor,
    ReleaseRequest, SourceImage, VisionCx, VisionError,
};

#[tokio::test]
async fn a_default_redactor_without_its_weights_refuses_the_release() {
    let empty = tempfile::tempdir().unwrap();
    let offline = ModelFetcher::new(
        FetchConfig::from_lookup(|_| None).with_cache_dir(empty.path()).with_offline(true),
        Arc::new(InMemoryAcceptanceStore::new()),
    );
    let defaults: [(Arc<dyn Redactor>, &str); 2] = [
        (Arc::new(PersonRedactor::rf_detr(offline.clone())), "rf-detr-n"),
        (Arc::new(PersonalTextRedactor::pp_ocr(offline, PpOcrV6Size::Small)), "pp-ocrv6-small-det"),
    ];
    let page = SourceImage::from_pixels(DecodedImage::filled(320, 200, [255; 4])).unwrap();
    let backend = BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
        CoordFrame::SentPixels,
    );
    for (redactor, model) in defaults {
        let class: RedactionClass = redactor.class();
        let recorder = Arc::new(InMemoryEgressRecorder::new());
        let gate = EgressGate::new(
            EgressPolicy::public_only(),
            EgressConfig::default().with_default(EgressAction::Allow).redacting(class.clone()),
            recorder.clone(),
        )
        .with_redactor(redactor);
        let request = ReleaseRequest::new(&page, &backend, DataClass::new("document"));

        let err = gate.release(&VisionCx::new(), request).await.unwrap_err();

        assert!(
            matches!(&err, VisionError::Offline { model: m, .. } if m == model),
            "{class}: {err}"
        );
        assert!(recorder.released().is_empty());
        let refusals: Vec<_> = recorder
            .records()
            .into_iter()
            .filter_map(|record| match record {
                EgressRecord::Refused(refusal) => Some(refusal),
                _ => None,
            })
            .collect();
        assert_eq!(refusals.len(), 1, "{class}");
        assert_eq!(refusals[0].step, "redact");
        assert!(refusals[0].reason.contains("HF_HUB_OFFLINE"), "{}", refusals[0].reason);
    }
}
