//! Property tests for Seatbelt profile generation correctness.
//!
//! **Feature: os-sandbox-profiles, Property 1: Seatbelt Profile Generation Correctness**
//!
//! *For any* valid `SandboxPolicy`, the Seatbelt profile string returned by
//! `MacOsEnforcer::generate_profile` SHALL satisfy structural invariants.
//!
//! **Validates: Requirements 3.2, 3.4, 3.5, 3.6, 3.7, 9.2, 9.3, 9.4, 14.1, 14.2, 14.3, 14.4, 14.5**

#![cfg(all(feature = "sandbox-macos", target_os = "macos"))]

use std::path::PathBuf;

use proptest::prelude::*;

use adk_sandbox::sandbox::macos::MacOsEnforcer;
use adk_sandbox::sandbox::{AccessMode, AllowedPath, SandboxPolicy};

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

fn arb_sandbox_policy() -> impl Strategy<Value = SandboxPolicy> {
    (
        proptest::collection::vec(arb_allowed_path(), 0..10),
        any::<bool>(),
        any::<bool>(),
        proptest::collection::vec(("[A-Z_]{1,8}", "[a-zA-Z0-9]{0,16}"), 0..5),
    )
        .prop_map(|(paths, network, spawn, env_pairs)| SandboxPolicy {
            allowed_paths: paths,
            allow_network: network,
            allow_process_spawn: spawn,
            network_rules: Vec::new(),
            env: env_pairs.into_iter().collect(),
        })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: os-sandbox-profiles, Property 1: Seatbelt Profile Generation Correctness**
    ///
    /// *For any* valid `SandboxPolicy`, the generated Seatbelt profile SHALL:
    /// - Contain `(version 1)` and `(deny default)` and `(allow default)`
    /// - Contain `(deny network*)` iff `allow_network` is false
    /// - Contain `(deny process-fork)` iff `allow_process_spawn` is false
    /// - Contain `(deny file-write*)`
    /// - Have one `(allow file-read* (subpath "..."))` per read-only path
    /// - Have one `(allow file-read* file-write* (subpath "..."))` per read-write path
    /// - Have balanced parentheses
    ///
    /// **Validates: Requirements 3.2, 3.4, 3.5, 3.6, 3.7, 9.2, 9.3, 9.4, 14.1, 14.2, 14.3, 14.4, 14.5**
    #[test]
    fn prop_seatbelt_profile_generation(policy in arb_sandbox_policy()) {
        let profile = MacOsEnforcer::generate_profile(&policy);

        // Must contain base directives
        prop_assert!(
            profile.contains("(version 1)"),
            "profile missing (version 1):\n{profile}"
        );
        prop_assert!(
            profile.contains("(deny default)"),
            "profile missing (deny default):\n{profile}"
        );
        prop_assert!(
            profile.contains("(allow default)"),
            "profile missing (allow default):\n{profile}"
        );

        // Network: deny iff allow_network is false
        if policy.allow_network {
            prop_assert!(
                !profile.contains("(deny network*)"),
                "profile should NOT contain (deny network*) when network is allowed:\n{profile}"
            );
        } else {
            prop_assert!(
                profile.contains("(deny network*)"),
                "profile should contain (deny network*) when network is denied:\n{profile}"
            );
        }

        // Process spawn: deny iff allow_process_spawn is false
        if policy.allow_process_spawn {
            prop_assert!(
                !profile.contains("(deny process-fork)"),
                "profile should NOT contain (deny process-fork) when spawn is allowed:\n{profile}"
            );
        } else {
            prop_assert!(
                profile.contains("(deny process-fork)"),
                "profile should contain (deny process-fork) when spawn is denied:\n{profile}"
            );
        }

        // File writes are always denied by default
        prop_assert!(
            profile.contains("(deny file-write*)"),
            "profile missing (deny file-write*):\n{profile}"
        );

        // Count read-only path directives
        let read_only_count = policy
            .allowed_paths
            .iter()
            .filter(|p| p.mode == AccessMode::ReadOnly)
            .count();
        let read_only_directive_count = profile
            .lines()
            .filter(|line| {
                line.contains("(allow file-read* (subpath")
                    && !line.contains("file-write*")
            })
            .count();
        prop_assert_eq!(
            read_only_directive_count,
            read_only_count,
        );

        // Count read-write path directives
        let read_write_count = policy
            .allowed_paths
            .iter()
            .filter(|p| p.mode == AccessMode::ReadWrite)
            .count();
        let read_write_directive_count = profile
            .lines()
            .filter(|line| line.contains("(allow file-read* file-write* (subpath"))
            .count();
        prop_assert_eq!(
            read_write_directive_count,
            read_write_count,
        );

        // Verify each path appears in the profile
        for entry in &policy.allowed_paths {
            let path_str = entry.path.to_string_lossy();
            match entry.mode {
                AccessMode::ReadOnly => {
                    let directive = format!("(allow file-read* (subpath \"{path_str}\"))");
                    prop_assert!(
                        profile.contains(&directive),
                        "missing read-only directive for {path_str}:\n{profile}"
                    );
                }
                AccessMode::ReadWrite => {
                    let directive =
                        format!("(allow file-read* file-write* (subpath \"{path_str}\"))");
                    prop_assert!(
                        profile.contains(&directive),
                        "missing read-write directive for {path_str}:\n{profile}"
                    );
                }
            }
        }

        // Balanced parentheses
        let open = profile.chars().filter(|c| *c == '(').count();
        let close = profile.chars().filter(|c| *c == ')').count();
        prop_assert_eq!(open, close);
    }
}
