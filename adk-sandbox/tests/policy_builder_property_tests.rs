//! Property tests for `SandboxPolicyBuilder` correctness.
//!
//! **Feature: os-sandbox-profiles, Property 3: SandboxPolicyBuilder Correctness**
//!
//! *For any* sequence of builder operations, the `SandboxPolicy` returned by
//! `build()` SHALL faithfully reflect the operations applied.
//!
//! **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 13.5, 13.6, 13.7**

use std::path::PathBuf;

use proptest::prelude::*;

use adk_sandbox::sandbox::{AccessMode, AllowedPath, SandboxPolicyBuilder};

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generates an arbitrary access mode.
fn arb_access_mode() -> impl Strategy<Value = AccessMode> {
    prop_oneof![Just(AccessMode::ReadOnly), Just(AccessMode::ReadWrite)]
}

/// Generates an arbitrary allowed path entry.
fn arb_allowed_path() -> impl Strategy<Value = AllowedPath> {
    ("/[a-z]{1,5}(/[a-z]{1,5}){0,3}", arb_access_mode())
        .prop_map(|(path, mode)| AllowedPath { path: PathBuf::from(path), mode })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: os-sandbox-profiles, Property 3: SandboxPolicyBuilder Correctness**
    ///
    /// *For any* sequence of builder operations (arbitrary combinations of
    /// `allow_read`, `allow_read_write`, `allow_network`, `allow_process_spawn`,
    /// and `env` calls), the `SandboxPolicy` returned by `build()` SHALL:
    /// - Contain exactly the `AllowedPath` entries in order (allow_read → ReadOnly,
    ///   allow_read_write → ReadWrite)
    /// - Have `allow_network` true iff `allow_network()` was called
    /// - Have `allow_process_spawn` true iff `allow_process_spawn()` was called
    /// - Contain exactly the environment variable entries added via `env()` calls
    ///
    /// **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 13.5, 13.6, 13.7**
    #[test]
    fn prop_builder_correctness(
        paths in proptest::collection::vec(arb_allowed_path(), 0..10),
        enable_network in any::<bool>(),
        enable_spawn in any::<bool>(),
        env_pairs in proptest::collection::vec(
            ("[A-Z_]{1,8}", "[a-zA-Z0-9]{0,16}"),
            0..5,
        ),
    ) {
        let mut builder = SandboxPolicyBuilder::new();

        // Apply path operations
        for entry in &paths {
            match entry.mode {
                AccessMode::ReadOnly => {
                    builder = builder.allow_read(&entry.path);
                }
                AccessMode::ReadWrite => {
                    builder = builder.allow_read_write(&entry.path);
                }
            }
        }

        // Apply network
        if enable_network {
            builder = builder.allow_network();
        }

        // Apply process spawn
        if enable_spawn {
            builder = builder.allow_process_spawn();
        }

        // Apply env vars
        for (key, value) in &env_pairs {
            builder = builder.env(key, value);
        }

        let policy = builder.build();

        // Verify paths match in order
        prop_assert_eq!(policy.allowed_paths.len(), paths.len());
        for (actual, expected) in policy.allowed_paths.iter().zip(paths.iter()) {
            prop_assert_eq!(&actual.path, &expected.path);
            prop_assert_eq!(actual.mode, expected.mode);
        }

        // Verify network flag
        prop_assert_eq!(policy.allow_network, enable_network);

        // Verify process spawn flag
        prop_assert_eq!(policy.allow_process_spawn, enable_spawn);

        // Verify env entries (HashMap may reorder, but all keys/values must be present)
        // Note: duplicate keys in env_pairs will be overwritten, so we build the expected map
        let expected_env: std::collections::HashMap<String, String> =
            env_pairs.into_iter().collect();
        prop_assert_eq!(&policy.env, &expected_env);
    }

    /// Default builder produces a deny-all policy.
    ///
    /// **Validates: Requirements 13.6, 13.7**
    #[test]
    fn prop_default_builder_deny_all(_seed in 0u32..1) {
        let policy = SandboxPolicyBuilder::new().build();

        prop_assert!(policy.allowed_paths.is_empty());
        prop_assert!(!policy.allow_network);
        prop_assert!(!policy.allow_process_spawn);
        prop_assert!(policy.env.is_empty());
    }
}
