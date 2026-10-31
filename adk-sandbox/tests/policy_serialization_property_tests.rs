//! Property tests for `SandboxPolicy` JSON serialization round-trip.
//!
//! **Feature: os-sandbox-profiles, Property 4: SandboxPolicy JSON Serialization Round-Trip**
//!
//! *For any* valid `SandboxPolicy`, serializing to JSON and deserializing back
//! SHALL produce a value equal to the original. The serialized JSON SHALL use
//! `camelCase` field names.
//!
//! **Validates: Requirements 16.1, 16.2, 16.3**

use std::path::PathBuf;

use proptest::prelude::*;

use adk_sandbox::sandbox::{AccessMode, AllowedPath, NetworkAccess, ProxyEndpoint, SandboxPolicy};

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn arb_access_mode() -> impl Strategy<Value = AccessMode> {
    prop_oneof![Just(AccessMode::ReadOnly), Just(AccessMode::ReadWrite)]
}

fn arb_allowed_path() -> impl Strategy<Value = AllowedPath> {
    ("/[a-z]{1,5}(/[a-z]{1,5}){0,3}", arb_access_mode())
        .prop_map(|(path, mode)| AllowedPath { path: PathBuf::from(path), mode })
}

fn arb_network() -> impl Strategy<Value = NetworkAccess> {
    prop_oneof![
        Just(NetworkAccess::Denied),
        Just(NetworkAccess::Open),
        (1u16..).prop_map(|port| NetworkAccess::Proxied { endpoint: ProxyEndpoint { port } }),
    ]
}

fn arb_sandbox_policy() -> impl Strategy<Value = SandboxPolicy> {
    (
        proptest::collection::vec(arb_allowed_path(), 0..10),
        arb_network(),
        any::<bool>(),
        proptest::collection::vec(("[A-Z_]{1,8}", "[a-zA-Z0-9]{0,16}"), 0..5),
    )
        .prop_map(|(paths, network, spawn, env_pairs)| SandboxPolicy {
            allowed_paths: paths,
            network,
            allow_process_spawn: spawn,
            env: env_pairs.into_iter().collect(),
        })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: os-sandbox-profiles, Property 4: SandboxPolicy JSON Serialization Round-Trip**
    ///
    /// *For any* valid `SandboxPolicy`:
    /// - `serde_json::from_str::<SandboxPolicy>(&serde_json::to_string(&policy).unwrap()).unwrap() == policy`
    /// - Serialized JSON contains camelCase field names (allowedPaths, network, allowProcessSpawn)
    ///
    /// **Validates: Requirements 16.1, 16.2, 16.3**
    #[test]
    fn prop_json_round_trip(policy in arb_sandbox_policy()) {
        let json = serde_json::to_string(&policy).expect("serialization should succeed");
        let deserialized: SandboxPolicy =
            serde_json::from_str(&json).expect("deserialization should succeed");

        prop_assert_eq!(&deserialized, &policy);

        // Verify camelCase field names in the serialized JSON
        prop_assert!(
            json.contains("allowedPaths") || policy.allowed_paths.is_empty(),
            "expected camelCase 'allowedPaths' in JSON: {json}"
        );
        prop_assert!(json.contains("\"network\":{\"mode\":"), "expected 'network' in JSON: {json}");
        prop_assert!(
            json.contains("allowProcessSpawn"),
            "expected camelCase 'allowProcessSpawn' in JSON: {json}"
        );

        // Verify snake_case is NOT used
        prop_assert!(
            !json.contains("allowed_paths"),
            "unexpected snake_case 'allowed_paths' in JSON: {json}"
        );
        prop_assert!(
            !json.contains("allowNetwork"),
            "unexpected pre-3.0 'allowNetwork' in JSON: {json}"
        );
        prop_assert!(
            !json.contains("allow_process_spawn"),
            "unexpected snake_case 'allow_process_spawn' in JSON: {json}"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// A policy serialized before 3.0 loads: `allowNetwork` maps to `Open` or `Denied`, and
    /// `networkRules`, which no enforcer applied, is dropped.
    #[test]
    fn prop_pre_3_0_policies_load(open in any::<bool>(), rules in any::<bool>(), spawn in any::<bool>()) {
        let rules = if rules { r#","networkRules":[{"domain":"api.example.com","ports":[443]}]"# } else { "" };
        let json = format!(
            r#"{{"allowedPaths":[{{"path":"/usr","mode":"readOnly"}}],"allowNetwork":{open}{rules},"allowProcessSpawn":{spawn},"env":{{"A":"1"}}}}"#
        );
        let policy: SandboxPolicy = serde_json::from_str(&json).expect("a pre-3.0 policy loads");
        let expected = SandboxPolicy {
            allowed_paths: vec![AllowedPath { path: PathBuf::from("/usr"), mode: AccessMode::ReadOnly }],
            network: if open { NetworkAccess::Open } else { NetworkAccess::Denied },
            allow_process_spawn: spawn,
            env: [("A".to_string(), "1".to_string())].into_iter().collect(),
        };
        prop_assert_eq!(policy, expected);
    }
}
