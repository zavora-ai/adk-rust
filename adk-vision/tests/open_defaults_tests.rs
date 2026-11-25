//! Permissive by default, every restriction an opt-in: with no `.adk/vision.toml` an image
//! with a person in it leaves unredacted and unasked, and each restriction the rules can
//! choose — `ask`, `deny`, a `redact` class, a `provider` rule, public networks only — holds,
//! refuses or covers as chosen.

mod governed;

use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_vision::{
    BackendDescriptor, CoordFrame, DataClass, EgressAction, EgressConfig, EgressDecision,
    EgressGate, EgressRecord, InMemoryEgressRecorder, Locality, PersonRedactor, RedactionClass,
    ReleaseRequest, VisionConfig, VisionCx, VisionError,
};
use governed::*;

fn claude() -> BackendDescriptor {
    BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
        CoordFrame::SentPixels,
    )
}

fn gate(config: EgressConfig) -> (EgressGate, Arc<InMemoryEgressRecorder>) {
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    (EgressGate::new(EgressPolicy::allow_private_networks(), config, recorder.clone()), recorder)
}

fn served_at(host: &str) -> BackendDescriptor {
    BackendDescriptor::new(
        "self-hosted",
        Locality::Remote { hosts: vec![host.into()] },
        CoordFrame::SentPixels,
    )
}

fn photo() -> DataClass {
    DataClass::new("photo")
}

#[test]
fn an_empty_file_is_the_permissive_default() {
    let empty = VisionConfig::from_toml_str("").unwrap();
    assert_eq!(empty, VisionConfig::default());
    assert_eq!(empty.egress.default, EgressAction::Allow);
    assert!(empty.egress.redact.is_empty());
    assert!(empty.egress.rules.is_empty());
    assert!(!empty.egress.on_device);
    assert!(empty.egress.private_networks);
}

#[tokio::test]
async fn with_no_rules_an_image_with_a_person_leaves_unredacted_and_unasked() {
    let card = identity_card();
    let (gate, recorder) = gate(VisionConfig::default().egress);
    let backend = claude();

    let released = gate
        .release(&VisionCx::new(), ReleaseRequest::new(&card.source, &backend, photo()))
        .await
        .unwrap();

    assert_eq!(released.receipt().decision, EgressDecision::Allowed { rule: None });
    assert!(released.receipt().redactions.is_empty(), "nothing was redacted");
    assert!(shows(released.pixels(), &card.portrait, RED), "the portrait leaves as it is");
    for word in card.personal() {
        assert!(shows(released.pixels(), &word.bbox, BLUE), "{} was covered", word.text);
    }
    assert_eq!(recorder.released().len(), 1, "the release is still recorded");
}

#[tokio::test]
async fn an_ask_rule_holds_the_exact_image_until_a_person_approves_it() {
    let card = identity_card();
    let (gate, recorder) = gate(EgressConfig::default().with_default(EgressAction::Ask));
    let backend = claude();
    let cx = VisionCx::new();

    let held = gate.release(&cx, ReleaseRequest::new(&card.source, &backend, photo())).await;
    let Err(VisionError::EgressHeld { egress, .. }) = held else { panic!("held: {held:?}") };
    assert!(recorder.released().is_empty());

    let approved = ReleaseRequest::new(&card.source, &backend, photo()).approved(egress);
    let released = gate.release(&cx, approved).await.unwrap();
    assert_eq!(released.receipt().egress, egress, "the approved image is the one that leaves");
    assert_eq!(released.receipt().decision, EgressDecision::Approved { rule: None });
}

#[tokio::test]
async fn a_deny_rule_refuses_and_records_the_refusal() {
    let card = identity_card();
    let config =
        EgressConfig::default().with_rule("api.anthropic.com", &["photo"], EgressAction::Deny);
    let (gate, recorder) = gate(config);
    let backend = claude();

    let refused =
        gate.release(&VisionCx::new(), ReleaseRequest::new(&card.source, &backend, photo())).await;

    assert!(matches!(refused, Err(VisionError::EgressRefused { .. })), "{refused:?}");
    let records = recorder.records();
    assert!(
        matches!(&records[..], [EgressRecord::Refused(refusal)] if refusal.step == "decide"),
        "{records:?}"
    );
}

#[tokio::test]
async fn a_listed_redaction_class_covers_or_fails_closed() {
    let card = identity_card();
    let backend = claude();
    let faces = EgressConfig::default().redacting(RedactionClass::faces());

    let (covered, _) = gate(faces.clone());
    let covered =
        covered.with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))));
    let released = covered
        .release(&VisionCx::new(), ReleaseRequest::new(&card.source, &backend, photo()))
        .await
        .unwrap();
    assert!(all_black(released.pixels(), &card.portrait), "the portrait is covered");
    assert_eq!(released.receipt().redactions[&RedactionClass::faces()], 1);

    let (uncovered, recorder) = gate(faces);
    let refused = uncovered
        .release(&VisionCx::new(), ReleaseRequest::new(&card.source, &backend, photo()))
        .await;
    assert!(matches!(refused, Err(VisionError::RedactionUnavailable { .. })), "{refused:?}");
    assert!(recorder.released().is_empty(), "nothing left unredacted");
}

#[tokio::test]
async fn a_provider_rule_matches_every_host_of_that_provider() {
    let card = identity_card();
    let config = EgressConfig::default().with_provider_rule("anthropic", &[], EgressAction::Ask);
    let (gate, _) = gate(config);
    let gateway = BackendDescriptor::new(
        "claude",
        Locality::Remote { hosts: vec!["llm-gateway.example.com".into()] },
        CoordFrame::SentPixels,
    );
    let cx = VisionCx::new();

    let anthropic = gateway.clone().with_provider("anthropic");
    let held = gate.release(&cx, ReleaseRequest::new(&card.source, &anthropic, photo())).await;
    assert!(matches!(held, Err(VisionError::EgressHeld { .. })), "{held:?}");

    let other = gateway.with_provider("gemini");
    gate.release(&cx, ReleaseRequest::new(&card.source, &other, photo())).await.unwrap();
}

#[tokio::test]
async fn a_model_on_a_private_network_receives_images_unless_the_rules_keep_them_public() {
    let card = identity_card();
    let cx = VisionCx::new();
    let (open, _) = gate(VisionConfig::default().egress);
    for host in ["127.0.0.1:11434", "192.168.1.20:8000", "10.0.4.7"] {
        let backend = served_at(host);
        open.release(&cx, ReleaseRequest::new(&card.source, &backend, photo()))
            .await
            .unwrap_or_else(|err| panic!("{host}: {err}"));
    }

    let public = VisionConfig::from_toml_str("[egress]\nprivate_networks = false").unwrap();
    let (restricted, recorder) = gate(public.egress);
    let backend = served_at("192.168.1.20:8000");
    let refused =
        restricted.release(&cx, ReleaseRequest::new(&card.source, &backend, photo())).await;
    assert!(matches!(refused, Err(VisionError::EgressRefused { .. })), "{refused:?}");
    let records = recorder.records();
    assert!(
        matches!(&records[..], [EgressRecord::Refused(refusal)] if refusal.step == "destination"),
        "{records:?}"
    );
    restricted.release(&cx, ReleaseRequest::new(&card.source, &claude(), photo())).await.unwrap();
}

#[tokio::test]
async fn the_operator_policy_stays_the_ceiling_and_metadata_endpoints_stay_refused() {
    let card = identity_card();
    let cx = VisionCx::new();
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let public =
        EgressGate::new(EgressPolicy::public_only(), VisionConfig::default().egress, recorder);
    let lan = served_at("192.168.1.20:8000");
    let refused = public.release(&cx, ReleaseRequest::new(&card.source, &lan, photo())).await;
    assert!(matches!(refused, Err(VisionError::EgressRefused { .. })), "{refused:?}");

    let (open, _) = gate(VisionConfig::default().egress);
    let metadata = served_at("169.254.169.254");
    let refused = open.release(&cx, ReleaseRequest::new(&card.source, &metadata, photo())).await;
    assert!(matches!(refused, Err(VisionError::EgressRefused { .. })), "{refused:?}");
}

#[cfg(feature = "cloud")]
mod describer {
    use super::*;
    use adk_core::DataDestination;
    use adk_model::{ScriptedLlm, ScriptedResponse};
    use adk_vision::{Describer, LlmDescriber};

    fn hosted(destinations: &[DataDestination]) -> Arc<ScriptedLlm> {
        let mut model = ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new());
        for destination in destinations {
            model = model.with_data_destination(destination.clone());
        }
        Arc::new(model)
    }

    #[test]
    fn the_describer_takes_its_hosts_and_providers_from_the_model() {
        let model =
            hosted(&[DataDestination::new("bedrock", ["bedrock-runtime.eu-west-1.amazonaws.com"])]);
        let describer = LlmDescriber::remote(model.clone()).unwrap();
        let descriptor = describer.descriptor();
        assert_eq!(
            descriptor.locality,
            Locality::Remote { hosts: vec!["bedrock-runtime.eu-west-1.amazonaws.com".into()] }
        );
        assert_eq!(descriptor.providers, ["bedrock"]);

        // An explicit host list overrides the model's; the provider stays the model's.
        let gateway = LlmDescriber::remote_with_hosts(model, ["llm-gateway.example.com"]).unwrap();
        assert_eq!(
            gateway.descriptor().locality,
            Locality::Remote { hosts: vec!["llm-gateway.example.com".into()] }
        );
        assert_eq!(gateway.descriptor().providers, ["bedrock"]);
    }

    #[test]
    fn a_describer_with_no_host_from_either_is_refused() {
        let silent = hosted(&[]);
        let err = LlmDescriber::remote(silent.clone()).unwrap_err();
        assert!(err.to_string().contains("remote_with_hosts"), "{err}");
        assert!(LlmDescriber::remote_with_hosts(silent.clone(), Vec::<String>::new()).is_err());
        assert!(LlmDescriber::remote_with_hosts(silent, ["api.anthropic.com"]).is_ok());
    }

    #[test]
    fn a_provider_rule_applies_to_a_describer_of_that_provider() {
        let config =
            EgressConfig::default().with_provider_rule("anthropic", &[], EgressAction::Deny);
        let (gate, _) = gate(config);
        let anthropic = LlmDescriber::remote(hosted(&[DataDestination::new(
            "anthropic",
            ["api.anthropic.com"],
        )]))
        .unwrap();
        assert_eq!(gate.decide(anthropic.descriptor(), &photo()), (EgressAction::Deny, Some(0)));

        let gemini = LlmDescriber::remote(hosted(&[DataDestination::new(
            "gemini",
            ["generativelanguage.googleapis.com"],
        )]))
        .unwrap();
        assert_eq!(gate.decide(gemini.descriptor(), &photo()), (EgressAction::Allow, None));
    }
}
