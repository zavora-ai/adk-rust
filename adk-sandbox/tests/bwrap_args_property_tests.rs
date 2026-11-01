//! Property tests for Bubblewrap argument generation correctness.
//!
//! **Feature: os-sandbox-profiles, Property 2: Bubblewrap Argument Generation Correctness**
//!
//! *For any* valid `SandboxPolicy`, the argument list returned by
//! `LinuxEnforcer::generate_args` SHALL satisfy structural invariants.
//!
//! **Validates: Requirements 4.2, 4.3, 4.4, 4.5, 4.6, 4.8, 10.2, 10.3, 10.4, 15.1, 15.2, 15.3, 15.4**

#![cfg(all(feature = "sandbox-linux", target_os = "linux"))]

use std::path::{Path, PathBuf};

use proptest::prelude::*;

use adk_sandbox::sandbox::linux::{LinuxEnforcer, SECCOMP_FD};
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

/// The bind mounts in `args`, in order, as `(path, read_only)`.
fn mounts(args: &[String]) -> Vec<(PathBuf, bool)> {
    args.windows(3)
        .filter(|window| matches!(window[0].as_str(), "--bind" | "--ro-bind"))
        .map(|window| {
            assert_eq!(window[1], window[2], "every mount maps a path onto itself");
            (PathBuf::from(&window[1]), window[0] == "--ro-bind")
        })
        .collect()
}

/// Policies over a two-letter alphabet, so nested and repeated paths are common.
fn arb_nested_policy() -> impl Strategy<Value = SandboxPolicy> {
    proptest::collection::vec(
        ("/[ab](/[ab]){0,3}", arb_access_mode())
            .prop_map(|(path, mode)| AllowedPath { path: PathBuf::from(path), mode }),
        0..8,
    )
    .prop_map(|allowed_paths| SandboxPolicy {
        allowed_paths,
        network: NetworkAccess::Denied,
        allow_process_spawn: true,
        env: Default::default(),
    })
}

/// Every path over the alphabet of [`arb_nested_policy`], one level deeper than it reaches.
fn every_probe() -> Vec<PathBuf> {
    let mut level = vec![PathBuf::from("/")];
    let mut probes = Vec::new();
    for _ in 0..5 {
        level = level.iter().flat_map(|dir| [dir.join("a"), dir.join("b")]).collect();
        probes.extend(level.iter().cloned());
    }
    probes
}

/// The access the most specific entry at or above `path` grants, read-write winning
/// between entries for the same path (spec 3.1 §10 S3).
fn effective_mode(paths: &[AllowedPath], path: &Path, strictly_above: bool) -> Option<AccessMode> {
    paths
        .iter()
        .filter(|entry| path.starts_with(&entry.path) && !(strictly_above && entry.path == path))
        .max_by_key(|entry| (entry.path.components().count(), entry.mode == AccessMode::ReadWrite))
        .map(|entry| entry.mode)
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
    /// - Contain `--unshare-net` iff the network is not `Open`
    /// - Contain `--new-session` exactly once, whatever the policy
    /// - Contain `--seccomp <SECCOMP_FD>` iff `allow_process_spawn` is false
    /// - Have one `--ro-bind path path` per read-only path
    /// - Have one `--bind path path` per read-write path, plus one per writable directory
    ///   above a read-only path nested in writable space
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

        // Network: --unshare-net unless the network is open
        let has_unshare_net = args.contains(&"--unshare-net".to_string());
        if !policy.network.restricts() {
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

        // Every policy starts a new session (spec 3.1 §10 S2).
        prop_assert_eq!(
            count_flag(&args, "--new-session"),
            1,
            "--new-session should appear exactly once: {:?}",
            args
        );

        // Process spawn: the seccomp program is referenced iff allow_process_spawn is false
        let seccomp_fd = args
            .windows(2)
            .find(|pair| pair[0] == "--seccomp")
            .map(|pair| pair[1].clone());
        if policy.allow_process_spawn {
            prop_assert_eq!(seccomp_fd, None);
        } else {
            prop_assert_eq!(seccomp_fd, Some(SECCOMP_FD.to_string()));
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
        prop_assert!(actual_rw >= expected_rw, "{} < {}: {:?}", actual_rw, expected_rw, args);
        for (path, read_only) in mounts(&args) {
            let is_rw_entry = policy
                .allowed_paths
                .iter()
                .any(|entry| entry.mode == AccessMode::ReadWrite && entry.path == path);
            if read_only || is_rw_entry {
                continue;
            }
            let pins_a_read_only_entry = policy.allowed_paths.iter().any(|entry| {
                entry.mode == AccessMode::ReadOnly && entry.path != path && entry.path.starts_with(&path)
            });
            prop_assert!(pins_a_read_only_entry, "unexpected mount {:?}: {:?}", path, args);
        }

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
            // `prop_assert_eq!` expands through `concat!`, which cannot capture an inline
            // `{args:?}`; the argument has to be positional. This file is both Linux-only
            // and behind `sandbox-linux`, so nothing ever compiled it.
            "--die-with-parent should appear exactly once: {:?}",
            args
        );

        // --unshare-pid should appear exactly once
        prop_assert_eq!(
            count_flag(&args, "--unshare-pid"),
            1,
            // `prop_assert_eq!` expands through `concat!`, which cannot capture an inline
            // `{args:?}`; the argument has to be positional. This file is both Linux-only
            // and behind `sandbox-linux`, so nothing ever compiled it.
            "--unshare-pid should appear exactly once: {:?}",
            args
        );
    }

    /// Spec 3.1 §10 S3: whatever order the policy lists them in, the last mount over a path
    /// is its most specific entry's, so a read-only entry inside a read-write entry is not
    /// writable and a read-write entry inside that is. Every writable directory above such a
    /// read-only entry is a mount point, which the kernel refuses to rename or remove.
    #[test]
    fn prop_the_most_specific_entry_decides_access(policy in arb_nested_policy()) {
        let args = LinuxEnforcer::generate_args(&policy);
        let mounts = mounts(&args);
        let paths = &policy.allowed_paths;

        for probe in every_probe() {
            let visible = mounts
                .iter()
                .rev()
                .find(|(path, _)| probe.starts_with(path))
                .map(|(_, read_only)| if *read_only { AccessMode::ReadOnly } else { AccessMode::ReadWrite });
            prop_assert_eq!(visible, effective_mode(paths, &probe, false), "probe {:?}: {:?}", probe, args);
        }

        for entry in paths.iter().filter(|entry| {
            entry.mode == AccessMode::ReadOnly
                && effective_mode(paths, &entry.path, true) == Some(AccessMode::ReadWrite)
                && effective_mode(paths, &entry.path, false) == Some(AccessMode::ReadOnly)
        }) {
            let entry_mount = mounts
                .iter()
                .position(|(path, read_only)| *read_only && *path == entry.path)
                .expect("the read-only entry is mounted");
            for dir in entry.path.ancestors().skip(1) {
                if effective_mode(paths, dir, false) != Some(AccessMode::ReadWrite) {
                    break;
                }
                let pinned = mounts[..entry_mount].iter().any(|(path, _)| path == dir);
                prop_assert!(pinned, "{} is not a mount point: {:?}", dir.display(), args);
            }
        }
    }
}
