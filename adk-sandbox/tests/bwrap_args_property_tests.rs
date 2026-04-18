//! Property tests for Bubblewrap argument generation correctness.
//!
//! **Feature: os-sandbox-profiles, Property 2: Bubblewrap Argument Generation Correctness**
//!
//! *For any* valid `SandboxPolicy`, the argument list returned by
//! `LinuxEnforcer::generate_args` SHALL satisfy structural invariants.
//!
//! **Validates: Requirements 4.2, 4.3, 4.4, 4.5, 4.6, 4.8, 10.2, 10.3, 10.4, 15.1, 15.2, 15.3, 15.4**

#![cfg(all(feature = "sandbox-linux", target_os = "linux"))]

use std::path::PathBuf;

use proptest::prelude::*;

use adk_sandbox::sandbox::linux::LinuxEnforcer;
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
            env: env_pairs.into_iter().collect(),
        })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Counts occurrences of a flag in the args list.
fn count_flag(args: &[String], flag: &str) -> usize {
    args.iter().filter(|a| a.as_str() == flag).count()
}

/// Counts bind-mount pairs (flag followed by two path arguments).
fn count_bind_pairs(args: &[String], flag: &str) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            // A valid bind pair has the flag followed by two path strings
            if i + 2 < args.len() {
                count += 1;
            }
            i += 3; // skip flag + two path args
        } else {
            i += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: os-sandbox-profiles, Property 2: Bubblewrap Argument Generation Correctness**
    ///
    /// *For any* valid `SandboxPolicy`, the generated bwrap args SHALL:
    /// - Start with `--die-with-parent`
    /// - Contain `--unshare-pid`
    /// - Contain `--unshare-net` iff `allow_network` is false
    /// - Contain `--new-session` iff `allow_process_spawn` is false
    /// - Have one `--ro-bind path path` per read-only path
    /// - Have one `--bind path path` per read-write path
    /// - Contain no empty strings
    ///
    /// **Validates: Requirements 4.2, 4.3, 4.4, 4.5, 4.6, 4.8, 10.2, 10.3, 10.4, 15.1, 15.2, 15.3, 15.4**
    #[test]
    fn prop_bwrap_args_generation(policy in arb_sandbox_policy()) {
        let args = LinuxEnforcer::generate_args(&policy);

        // Must start with --die-with-parent
        prop_assert!(
            !args.is_empty() && args[0] == "--die-with-parent",
            "args must start with --die-with-parent, got: {args:?}"
        );

        // Must contain --unshare-pid
        prop_assert!(
            args.contains(&"--unshare-pid".to_string()),
            "args must contain --unshare-pid: {args:?}"
        );

        // Network: --unshare-net iff allow_network is false
        let has_unshare_net = args.contains(&"--unshare-net".to_string());
        if policy.allow_network {
            prop_assert!(
                !has_unshare_net,
                "args should NOT contain --unshare-net when network is allowed: {args:?}"
            );
        } else {
            prop_assert!(
                has_unshare_net,
                "args should contain --unshare-net when network is denied: {args:?}"
            );
        }

        // Process spawn: --new-session iff allow_process_spawn is false
        let has_new_session = args.contains(&"--new-session".to_string());
        if policy.allow_process_spawn {
            prop_assert!(
                !has_new_session,
                "args should NOT contain --new-session when spawn is allowed: {args:?}"
            );
        } else {
            prop_assert!(
                has_new_session,
                "args should contain --new-session when spawn is denied: {args:?}"
            );
        }

        // Count read-only bind mounts
        let expected_ro = policy
            .allowed_paths
            .iter()
            .filter(|p| p.mode == AccessMode::ReadOnly)
            .count();
        let actual_ro = count_bind_pairs(&args, "--ro-bind");
        prop_assert_eq!(actual_ro, expected_ro);

        // Count read-write bind mounts
        let expected_rw = policy
            .allowed_paths
            .iter()
            .filter(|p| p.mode == AccessMode::ReadWrite)
            .count();
        let actual_rw = count_bind_pairs(&args, "--bind");
        prop_assert_eq!(actual_rw, expected_rw);

        // Verify each path appears correctly
        for entry in &policy.allowed_paths {
            let path_str = entry.path.to_string_lossy().to_string();
            let flag = match entry.mode {
                AccessMode::ReadOnly => "--ro-bind",
                AccessMode::ReadWrite => "--bind",
            };
            // Find the flag followed by the path twice
            let found = args.windows(3).any(|w| {
                w[0] == flag && w[1] == path_str && w[2] == path_str
            });
            prop_assert!(
                found,
                "missing {flag} {path_str} {path_str} in: {args:?}"
            );
        }

        // No empty strings
        for arg in &args {
            prop_assert!(
                !arg.is_empty(),
                "found empty string in bwrap args: {args:?}"
            );
        }

        // --die-with-parent should appear exactly once
        prop_assert_eq!(
            count_flag(&args, "--die-with-parent"),
            1,
            "--die-with-parent should appear exactly once: {args:?}"
        );

        // --unshare-pid should appear exactly once
        prop_assert_eq!(
            count_flag(&args, "--unshare-pid"),
            1,
            "--unshare-pid should appear exactly once: {args:?}"
        );
    }
}
