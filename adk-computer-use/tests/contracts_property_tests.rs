//! Property-based tests for the computer-use wire contracts.
//!
//! These validate two families of invariants:
//! 1. **Round-trip fidelity** — camelCase serde types survive
//!    `to_value` → `from_value` unchanged.
//! 2. **Validation invariants** — `TargetSensitivityEvidence` rejects payloads
//!    that violate its disclosure-safe rules.

use adk_computer_use::{
    ActionClass, ControlLease, ExecutionMode, ExecutionReceipt, LeaseBoundaries, ReceiptStatus,
    TargetSensitivityAssessment, TargetSensitivityEvidence, TargetSensitivitySignal,
    TargetSensitivitySource,
};
use proptest::prelude::*;
use serde_json::json;

fn arb_execution_mode() -> impl Strategy<Value = ExecutionMode> {
    prop_oneof![
        Just(ExecutionMode::Shadow),
        Just(ExecutionMode::Background),
        Just(ExecutionMode::Foreground),
    ]
}

fn arb_action_class() -> impl Strategy<Value = ActionClass> {
    prop_oneof![
        Just(ActionClass::Observe),
        Just(ActionClass::Navigate),
        Just(ActionClass::EditReversible),
        Just(ActionClass::CommunicateExternal),
        Just(ActionClass::Authentication),
        Just(ActionClass::Financial),
        Just(ActionClass::Destructive),
        Just(ActionClass::PrivilegeChange),
        Just(ActionClass::SecretAccess),
    ]
}

fn arb_receipt_status() -> impl Strategy<Value = ReceiptStatus> {
    prop_oneof![
        Just(ReceiptStatus::Committed),
        Just(ReceiptStatus::Rejected),
        Just(ReceiptStatus::Interrupted),
        Just(ReceiptStatus::Indeterminate),
    ]
}

fn arb_sensitivity_assessment() -> impl Strategy<Value = TargetSensitivityAssessment> {
    prop_oneof![
        Just(TargetSensitivityAssessment::Sensitive),
        Just(TargetSensitivityAssessment::NonSensitive),
        Just(TargetSensitivityAssessment::Unknown),
    ]
}

fn arb_conclusive_assessment() -> impl Strategy<Value = TargetSensitivityAssessment> {
    prop_oneof![
        Just(TargetSensitivityAssessment::Sensitive),
        Just(TargetSensitivityAssessment::NonSensitive),
    ]
}

fn arb_sensitivity_signal() -> impl Strategy<Value = TargetSensitivitySignal> {
    prop_oneof![
        Just(TargetSensitivitySignal::SecureRole),
        Just(TargetSensitivitySignal::SecureSubrole),
        Just(TargetSensitivitySignal::ProtectedContent),
        Just(TargetSensitivitySignal::UiaIsPassword),
        Just(TargetSensitivitySignal::SensitiveLabel),
        Just(TargetSensitivitySignal::AmbiguousMatch),
        Just(TargetSensitivitySignal::ElementNotFound),
        Just(TargetSensitivitySignal::InspectionError),
        Just(TargetSensitivitySignal::InvalidField),
        Just(TargetSensitivitySignal::NativeSignalUnavailable),
    ]
}

fn arb_execution_receipt() -> impl Strategy<Value = ExecutionReceipt> {
    (
        "[a-z]{1,12}",
        "[a-z]{1,12}",
        "[a-z]{1,12}",
        "[0-9a-f]{8}",
        0u32..5,
        arb_receipt_status(),
        prop::option::of(any::<bool>()),
    )
        .prop_map(|(receipt_id, session_id, action_id, action_digest, attempt, status, ok)| {
            ExecutionReceipt {
                receipt_id,
                session_id,
                action_id,
                action_digest,
                attempt,
                status,
                created_at: None,
                updated_at: None,
                result: ok.map(|ok| json!({ "ok": ok })),
                error: None,
            }
        })
}

fn arb_control_lease() -> impl Strategy<Value = ControlLease> {
    (
        "[a-z]{1,12}",
        0u64..1_000,
        "[a-z]{1,12}",
        "[a-z]{1,12}",
        prop::option::of("[a-z]{1,8}"),
        "[a-z]{1,10}",
        arb_execution_mode(),
        0u32..10,
        0u32..10,
    )
        .prop_map(
            |(
                lease_id,
                revision,
                session_id,
                principal_id,
                agent_id,
                kind,
                execution_mode,
                action_budget,
                actions_used,
            )| {
                ControlLease {
                    lease_id,
                    revision,
                    session_id,
                    principal_id,
                    agent_id,
                    kind,
                    execution_mode,
                    state: "active".into(),
                    acquired_at: None,
                    expires_at: "2026-07-13T10:01:00Z".into(),
                    action_budget,
                    actions_used,
                    boundaries: LeaseBoundaries::default(),
                }
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// **Feature: adk-computer-use, Property 1: Enum wire round-trip**
    /// *For any* contract enum value, JSON serialization then deserialization
    /// SHALL yield the original value (stable snake_case wire tags).
    /// **Validates: wire contract stability**
    #[test]
    fn prop_enum_variants_round_trip(
        mode in arb_execution_mode(),
        class in arb_action_class(),
        status in arb_receipt_status(),
        assessment in arb_sensitivity_assessment(),
    ) {
        for value in [
            serde_json::to_value(mode).unwrap(),
            serde_json::to_value(class).unwrap(),
            serde_json::to_value(status).unwrap(),
            serde_json::to_value(assessment).unwrap(),
        ] {
            prop_assert!(value.is_string());
        }
        prop_assert_eq!(mode, serde_json::from_value(serde_json::to_value(mode).unwrap()).unwrap());
        prop_assert_eq!(class, serde_json::from_value(serde_json::to_value(class).unwrap()).unwrap());
        prop_assert_eq!(
            status,
            serde_json::from_value(serde_json::to_value(status).unwrap()).unwrap()
        );
        prop_assert_eq!(
            assessment,
            serde_json::from_value(serde_json::to_value(assessment).unwrap()).unwrap()
        );
    }

    /// **Feature: adk-computer-use, Property 2: ExecutionReceipt round-trip**
    /// *For any* valid receipt, JSON round-trip SHALL preserve every field.
    /// **Validates: receipt wire contract**
    #[test]
    fn prop_execution_receipt_round_trips(receipt in arb_execution_receipt()) {
        let value = serde_json::to_value(&receipt).unwrap();
        let back: ExecutionReceipt = serde_json::from_value(value).unwrap();
        prop_assert_eq!(&receipt, &back);
    }

    /// **Feature: adk-computer-use, Property 3: ControlLease round-trip**
    /// *For any* valid lease, JSON round-trip SHALL preserve every field.
    /// **Validates: control-lease wire contract**
    #[test]
    fn prop_control_lease_round_trips(lease in arb_control_lease()) {
        let value = serde_json::to_value(&lease).unwrap();
        let back: ControlLease = serde_json::from_value(value).unwrap();
        prop_assert_eq!(&lease, &back);
    }

    /// **Feature: adk-computer-use, Property 4: Valid sensitivity evidence round-trips**
    /// *For any* conclusive sensitive evidence built from one signal, JSON
    /// round-trip SHALL preserve it (including the `try_from` re-validation).
    /// **Validates: value-free sensitivity wire contract**
    #[test]
    fn prop_valid_sensitivity_evidence_round_trips(
        signal in arb_sensitivity_signal(),
        fields_checked in 1u32..=100,
    ) {
        let evidence = TargetSensitivityEvidence::try_new(
            TargetSensitivityAssessment::Sensitive,
            TargetSensitivitySource::Accessibility,
            vec![signal],
            fields_checked,
            "2026-07-13T10:00:00Z",
        )
        .unwrap();
        let value = serde_json::to_value(&evidence).unwrap();
        let back: TargetSensitivityEvidence = serde_json::from_value(value).unwrap();
        prop_assert_eq!(&evidence, &back);
    }

    /// **Feature: adk-computer-use, Property 5: Conclusive sensitivity needs accessibility evidence**
    /// *For any* conclusive assessment, evidence without an accessibility source
    /// or without a checked field SHALL be rejected.
    /// **Validates: disclosure-safe sensitivity invariant**
    #[test]
    fn prop_conclusive_sensitivity_requires_accessibility(
        assessment in arb_conclusive_assessment(),
        signal in arb_sensitivity_signal(),
    ) {
        prop_assert!(
            TargetSensitivityEvidence::try_new(
                assessment,
                TargetSensitivitySource::Unavailable,
                vec![signal],
                1,
                "2026-07-13T10:00:00Z",
            )
            .is_err()
        );
        prop_assert!(
            TargetSensitivityEvidence::try_new(
                assessment,
                TargetSensitivitySource::Accessibility,
                vec![signal],
                0,
                "2026-07-13T10:00:00Z",
            )
            .is_err()
        );
    }

    /// **Feature: adk-computer-use, Property 6: Duplicate sensitivity signals rejected**
    /// *For any* signal, evidence carrying it twice SHALL be rejected.
    /// **Validates: unique-signal sensitivity invariant**
    #[test]
    fn prop_duplicate_sensitivity_signals_rejected(signal in arb_sensitivity_signal()) {
        prop_assert!(
            TargetSensitivityEvidence::try_new(
                TargetSensitivityAssessment::Sensitive,
                TargetSensitivitySource::Accessibility,
                vec![signal, signal],
                1,
                "2026-07-13T10:00:00Z",
            )
            .is_err()
        );
    }
}
