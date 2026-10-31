//! Property tests for Seatbelt profile generation correctness.
//!
//! **Feature: os-sandbox-profiles, Property 1: Seatbelt Profile Generation Correctness**
//!
//! *For any* valid `SandboxPolicy`, the Seatbelt profile string returned by
//! `MacOsEnforcer::generate_profile` SHALL satisfy structural invariants, and no allowed path
//! SHALL be able to alter the profile's structure.
//!
//! **Validates: Requirements 3.2, 3.4, 3.5, 3.6, 3.7, 9.2, 9.3, 9.4, 14.1, 14.2, 14.3, 14.4, 14.5**

#![cfg(all(feature = "sandbox-macos", target_os = "macos"))]

use std::path::{Path, PathBuf};

use proptest::prelude::*;

use adk_sandbox::sandbox::macos::MacOsEnforcer;
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

/// Paths built from the characters that matter to SBPL: quotes, backslashes, parentheses,
/// whitespace, and control characters.
fn arb_hostile_path() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('"'),
            Just('\\'),
            Just('('),
            Just(')'),
            Just(' '),
            Just('\n'),
            Just('\t'),
            Just('\u{1}'),
            Just('*'),
            Just(';'),
            Just('/'),
            proptest::char::range('a', 'z'),
        ],
        1..40,
    )
    .prop_map(|chars| format!("/{}", chars.into_iter().collect::<String>()))
}

/// Removes every SBPL string literal, leaving the profile's structure.
fn strip_string_literals(profile: &str) -> String {
    let mut structure = String::with_capacity(profile.len());
    let mut chars = profile.chars();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            structure.push(ch);
            continue;
        }
        structure.push_str("\"\"");
        while let Some(inner) = chars.next() {
            match inner {
                '\\' => {
                    chars.next();
                }
                '"' => break,
                _ => {}
            }
        }
    }
    structure
}

/// Lines granting exactly one policy path, as opposed to the multi-path system grants.
fn single_path_directives<'a>(profile: &'a str, prefix: &'a str) -> impl Iterator<Item = &'a str> {
    profile
        .lines()
        .filter(move |line| line.starts_with(prefix) && line.matches("(subpath").count() == 1)
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

/// The access the most specific entry at or above `path` grants, read-write winning
/// between entries for the same path (spec 3.1 §10 S3).
fn effective_mode(paths: &[AllowedPath], path: &Path, strictly_above: bool) -> Option<AccessMode> {
    paths
        .iter()
        .filter(|entry| path.starts_with(&entry.path) && !(strictly_above && entry.path == path))
        .max_by_key(|entry| (entry.path.components().count(), entry.mode == AccessMode::ReadWrite))
        .map(|entry| entry.mode)
}

/// A read-only entry inside writable space, which the profile must deny writes to.
fn is_shielded(paths: &[AllowedPath], entry: &AllowedPath) -> bool {
    entry.mode == AccessMode::ReadOnly
        && effective_mode(paths, &entry.path, true) == Some(AccessMode::ReadWrite)
        && effective_mode(paths, &entry.path, false) == Some(AccessMode::ReadOnly)
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

/// The quoted path in a single-path directive such as `(deny file-write* (subpath "/a"))`.
fn directive_path<'a>(line: &'a str, prefix: &str) -> Option<&'a Path> {
    line.strip_prefix(prefix)?.strip_suffix("\"))")?.strip_prefix('"').map(Path::new)
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: os-sandbox-profiles, Property 1: Seatbelt Profile Generation Correctness**
    ///
    /// *For any* valid `SandboxPolicy`, the generated Seatbelt profile SHALL:
    /// - Contain `(version 1)` and `(deny default)`, and never `(allow default)`
    /// - Contain `(allow network*)` iff the network is `Open`, and a `network-outbound` grant
    ///   to `localhost:<port>` iff it is `Proxied`
    /// - Contain `(allow process-fork)` iff `allow_process_spawn` is true
    /// - Have one `(allow file-read* (subpath "..."))` per read-only path
    /// - Have one `(allow file-read* file-write* (subpath "..."))` per read-write path
    /// - Mention writes only in those read-write directives and in the write denials for
    ///   read-only entries inside writable space
    /// - Have balanced parentheses
    ///
    /// **Validates: Requirements 3.2, 3.4, 3.5, 3.6, 3.7, 9.2, 9.3, 9.4, 14.1, 14.2, 14.3, 14.4, 14.5**
    #[test]
    fn prop_seatbelt_profile_generation(policy in arb_sandbox_policy()) {
        let profile = MacOsEnforcer::generate_profile(&policy);

        prop_assert!(profile.contains("(version 1)"), "profile missing (version 1):\n{profile}");
        prop_assert!(profile.contains("(deny default)"), "profile missing (deny default):\n{profile}");
        prop_assert!(
            !profile.contains("(allow default)"),
            "(allow default) overrides the deny-default base:\n{profile}"
        );

        prop_assert_eq!(profile.contains("(allow network*)"), policy.network == NetworkAccess::Open);
        let outbound = profile.contains("(allow network-outbound");
        match policy.network {
            NetworkAccess::Proxied { endpoint } => {
                let grant = format!("(allow network-outbound (remote ip \"localhost:{}\"))", endpoint.port);
                prop_assert!(profile.contains(&grant), "missing {grant}:\n{profile}");
                prop_assert_eq!(profile.matches("network").count(), 1, "one network rule:\n{}", profile);
            }
            NetworkAccess::Denied | NetworkAccess::Open => prop_assert!(!outbound),
        }
        prop_assert_eq!(profile.contains("(allow process-fork)"), policy.allow_process_spawn);

        let read_only_count = policy
            .allowed_paths
            .iter()
            .filter(|p| p.mode == AccessMode::ReadOnly)
            .count();
        prop_assert_eq!(
            single_path_directives(&profile, "(allow file-read* (subpath ").count(),
            read_only_count
        );

        let read_write_count = policy
            .allowed_paths
            .iter()
            .filter(|p| p.mode == AccessMode::ReadWrite)
            .count();
        prop_assert_eq!(
            single_path_directives(&profile, "(allow file-read* file-write* (subpath ").count(),
            read_write_count
        );
        let shielded_count =
            policy.allowed_paths.iter().filter(|entry| is_shielded(&policy.allowed_paths, entry)).count();
        prop_assert_eq!(profile.matches("file-write*").count(), read_write_count + shielded_count);

        for entry in &policy.allowed_paths {
            let path_str = entry.path.to_string_lossy();
            let directive = match entry.mode {
                AccessMode::ReadOnly => format!("(allow file-read* (subpath \"{path_str}\"))"),
                AccessMode::ReadWrite => {
                    format!("(allow file-read* file-write* (subpath \"{path_str}\"))")
                }
            };
            prop_assert!(profile.contains(&directive), "missing {directive}:\n{profile}");
        }

        let open = profile.chars().filter(|c| *c == '(').count();
        let close = profile.chars().filter(|c| *c == ')').count();
        prop_assert_eq!(open, close);
    }

    /// No path, whatever it contains, changes the profile's structure: every character stays
    /// inside its string literal, so no directive can be injected.
    #[test]
    fn prop_hostile_paths_cannot_inject_directives(
        hostile in arb_hostile_path(),
        read_write in any::<bool>(),
    ) {
        let builder = adk_sandbox::SandboxPolicyBuilder::new();
        let builder = if read_write {
            builder.allow_read_write(hostile.as_str())
        } else {
            builder.allow_read(hostile.as_str())
        };
        let hostile_profile = MacOsEnforcer::generate_profile(&builder.build());

        let benign = adk_sandbox::SandboxPolicyBuilder::new();
        let benign = if read_write { benign.allow_read_write("/x") } else { benign.allow_read("/x") };
        let benign_profile = MacOsEnforcer::generate_profile(&benign.build());

        prop_assert_eq!(
            strip_string_literals(&hostile_profile),
            strip_string_literals(&benign_profile),
            "the path {:?} changed the profile structure",
            hostile
        );
    }

    /// Spec 3.1 §10 S3: whatever order the policy lists them in, the last write rule over a
    /// path is its most specific entry's, so a read-only entry inside a read-write entry is
    /// not writable and a read-write entry inside that is. Every writable directory above
    /// a shielded entry is pinned against renaming, and nothing else is.
    #[test]
    fn prop_the_most_specific_entry_decides_writes(policy in arb_nested_policy()) {
        let profile = MacOsEnforcer::generate_profile(&policy);
        let paths = &policy.allowed_paths;

        let write_rules: Vec<(bool, &Path)> = profile
            .lines()
            .filter_map(|line| {
                directive_path(line, "(allow file-read* file-write* (subpath ")
                    .map(|path| (true, path))
                    .or_else(|| directive_path(line, "(deny file-write* (subpath ").map(|path| (false, path)))
            })
            .collect();
        for probe in every_probe() {
            let writable = write_rules
                .iter()
                .rev()
                .find(|(_, path)| probe.starts_with(path))
                .is_some_and(|(allow, _)| *allow);
            prop_assert_eq!(
                writable,
                effective_mode(paths, &probe, false) == Some(AccessMode::ReadWrite),
                "probe {:?}:\n{}",
                probe,
                profile
            );
        }

        let pinned: Vec<&Path> = profile
            .lines()
            .filter_map(|line| directive_path(line, "(deny file-write-unlink (literal "))
            .collect();
        for pin in &pinned {
            prop_assert_eq!(effective_mode(paths, pin, false), Some(AccessMode::ReadWrite));
        }
        for entry in paths.iter().filter(|entry| is_shielded(paths, entry)) {
            for dir in entry.path.ancestors().skip(1) {
                if effective_mode(paths, dir, false) != Some(AccessMode::ReadWrite) {
                    break;
                }
                prop_assert!(pinned.contains(&dir), "{} is not pinned:\n{}", dir.display(), profile);
            }
        }
    }
}
