#![cfg(feature = "personas")]
//! Property-based tests for PersonaProfile serialization.
//!
//! **Feature: competitive-parity-v070, Property 7: Persona Profile JSON Round-Trip**
//!
//! *For any* valid `PersonaProfile` (with arbitrary name, description, traits,
//! goals, and constraints), serializing to JSON and then deserializing back
//! SHALL produce an equivalent `PersonaProfile`.
//!
//! **Validates: Requirements 5.1, 5.5**

use adk_eval::personas::{ExpertiseLevel, PersonaProfile, PersonaTraits, Verbosity};
use proptest::prelude::*;

/// Strategy for generating arbitrary `Verbosity` values.
fn arb_verbosity() -> impl Strategy<Value = Verbosity> {
    prop_oneof![Just(Verbosity::Terse), Just(Verbosity::Normal), Just(Verbosity::Verbose),]
}

/// Strategy for generating arbitrary `ExpertiseLevel` values.
fn arb_expertise_level() -> impl Strategy<Value = ExpertiseLevel> {
    prop_oneof![
        Just(ExpertiseLevel::Novice),
        Just(ExpertiseLevel::Intermediate),
        Just(ExpertiseLevel::Expert),
    ]
}

/// Strategy for generating arbitrary `PersonaTraits`.
fn arb_persona_traits() -> impl Strategy<Value = PersonaTraits> {
    ("[a-zA-Z ]{1,50}", arb_verbosity(), arb_expertise_level()).prop_map(
        |(communication_style, verbosity, expertise_level)| PersonaTraits {
            communication_style,
            verbosity,
            expertise_level,
        },
    )
}

/// Strategy for generating arbitrary `PersonaProfile`.
fn arb_persona_profile() -> impl Strategy<Value = PersonaProfile> {
    (
        "[a-zA-Z0-9_-]{1,30}",
        "[a-zA-Z0-9 .,!?]{1,100}",
        arb_persona_traits(),
        prop::collection::vec("[a-zA-Z0-9 ]{1,50}", 0..5),
        prop::collection::vec("[a-zA-Z0-9 ]{1,50}", 0..5),
    )
        .prop_map(|(name, description, traits, goals, constraints)| PersonaProfile {
            name,
            description,
            traits,
            goals,
            constraints,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 7: Persona Profile JSON Round-Trip**
    ///
    /// *For any* valid PersonaProfile, serializing to JSON and deserializing back
    /// produces an equivalent PersonaProfile.
    ///
    /// **Validates: Requirements 5.1, 5.5**
    #[test]
    fn prop_persona_profile_json_round_trip(profile in arb_persona_profile()) {
        let json = serde_json::to_string(&profile).expect("serialization should succeed");
        let deserialized: PersonaProfile =
            serde_json::from_str(&json).expect("deserialization should succeed");
        prop_assert_eq!(&profile, &deserialized);
    }

    /// Verify that PersonaProfile round-trips through serde_json::Value as well.
    #[test]
    fn prop_persona_profile_value_round_trip(profile in arb_persona_profile()) {
        let value = serde_json::to_value(&profile).expect("to_value should succeed");
        let deserialized: PersonaProfile =
            serde_json::from_value(value).expect("from_value should succeed");
        prop_assert_eq!(&profile, &deserialized);
    }
}
