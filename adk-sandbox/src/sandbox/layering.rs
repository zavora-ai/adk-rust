//! The order in which an enforcer applies a policy's allowed paths, so that the most specific
//! entry decides access on Seatbelt and bubblewrap alike (spec 3.1 §10 S3).
//!
//! Seatbelt applies the last rule that matches a path, and bubblewrap shows the last mount
//! made over it. Both therefore give "most specific entry wins" when entries are applied
//! shallowest first. Two things beyond the order make a read-only entry inside a read-write
//! entry hold on both:
//!
//! - **Shielding.** Seatbelt allows add up, so a read-only grant alone leaves the writable
//!   parent's grant in force; a shielded entry also emits an explicit write denial. A
//!   bubblewrap read-only mount needs nothing more.
//! - **Pinning.** Seatbelt matches path names, bubblewrap matches mounts. Renaming a writable
//!   directory above a read-only entry would move the entry out from under a Seatbelt rule,
//!   and on bubblewrap would free its old path to be recreated writable. A pinned directory
//!   cannot be renamed or removed: Seatbelt denies unlinking it, and bubblewrap makes it a
//!   mount point, which the kernel refuses to rename. A read-write entry nested in read-only
//!   space is pinned for the same reason, since moving it would change the read-only
//!   directory that holds it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{AccessMode, AllowedPath};

/// One allowed path, in application order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layer<'a> {
    pub path: &'a Path,
    pub mode: AccessMode,
    /// A read-only entry inside writable space, which Seatbelt must deny writes to explicitly.
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        allow(dead_code, reason = "only the Seatbelt profile emits write denials")
    )]
    pub shielded: bool,
}

/// A policy's allowed paths arranged for an enforcer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layering<'a> {
    /// Every entry, shallowest path first, a read-only entry before a read-write entry for the
    /// same path, and policy order otherwise.
    pub layers: Vec<Layer<'a>>,
    /// Directories that must not be renamed or removed, sorted.
    pub pinned: Vec<PathBuf>,
}

impl<'a> Layering<'a> {
    pub fn new(paths: &'a [AllowedPath]) -> Self {
        let mut ordered: Vec<&AllowedPath> = paths.iter().collect();
        ordered.sort_by_key(|entry| sort_key(&entry.path, entry.mode));

        let mut pinned = BTreeSet::new();
        let layers = ordered
            .into_iter()
            .map(|entry| {
                let path = entry.path.as_path();
                let enclosing = parent_mode(paths, path);
                let shielded = entry.mode == AccessMode::ReadOnly
                    && enclosing == Some(AccessMode::ReadWrite)
                    && effective_mode(paths, path) == Some(AccessMode::ReadOnly);
                if shielded {
                    let writable_ancestors = path.ancestors().skip(1).take_while(|dir| {
                        effective_mode(paths, dir) == Some(AccessMode::ReadWrite)
                    });
                    pinned.extend(writable_ancestors.map(Path::to_path_buf));
                } else if entry.mode == AccessMode::ReadWrite
                    && enclosing.is_some()
                    && enclosing != Some(AccessMode::ReadWrite)
                {
                    pinned.insert(path.to_path_buf());
                }
                Layer { path, mode: entry.mode, shielded }
            })
            .collect();

        Self { layers, pinned: pinned.into_iter().collect() }
    }

    /// The mounts bubblewrap makes, in order: every entry, plus a read-write bind of each
    /// pinned directory that is not already an entry, placed by depth like any other.
    #[cfg(any(test, all(feature = "sandbox-linux", target_os = "linux")))]
    pub fn mounts(&self) -> Vec<(&Path, AccessMode)> {
        let mut mounts: Vec<(&Path, AccessMode)> =
            self.layers.iter().map(|layer| (layer.path, layer.mode)).collect();
        mounts.extend(
            self.pinned
                .iter()
                .filter(|dir| !self.layers.iter().any(|layer| layer.path == dir.as_path()))
                .map(|dir| (dir.as_path(), AccessMode::ReadWrite)),
        );
        mounts.sort_by_key(|(path, mode)| sort_key(path, *mode));
        mounts
    }
}

/// Shallower paths first; for one path, read-only before read-write so read-write wins.
fn sort_key(path: &Path, mode: AccessMode) -> (usize, bool) {
    (path.components().count(), mode == AccessMode::ReadWrite)
}

/// The access the most specific entry at or above `path` grants, read-write winning between
/// entries for the same path; `None` when no entry covers it.
fn effective_mode(paths: &[AllowedPath], path: &Path) -> Option<AccessMode> {
    paths
        .iter()
        .filter(|entry| path.starts_with(&entry.path))
        .max_by_key(|entry| sort_key(&entry.path, entry.mode))
        .map(|entry| entry.mode)
}

/// The access the most specific entry strictly above `path` grants.
fn parent_mode(paths: &[AllowedPath], path: &Path) -> Option<AccessMode> {
    paths
        .iter()
        .filter(|entry| entry.path != path && path.starts_with(&entry.path))
        .max_by_key(|entry| sort_key(&entry.path, entry.mode))
        .map(|entry| entry.mode)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn entry(path: &str, mode: AccessMode) -> AllowedPath {
        AllowedPath { path: PathBuf::from(path), mode }
    }

    const RO: AccessMode = AccessMode::ReadOnly;
    const RW: AccessMode = AccessMode::ReadWrite;

    fn layer(path: &str, mode: AccessMode, shielded: bool) -> Layer<'_> {
        Layer { path: Path::new(path), mode, shielded }
    }

    #[test]
    fn unrelated_paths_need_no_shield_or_pin() {
        let paths = [entry("/usr/lib", RO), entry("/tmp/work", RW), entry("/opt", RO)];
        let layering = Layering::new(&paths);

        assert_eq!(
            layering,
            Layering {
                layers: vec![
                    layer("/opt", RO, false),
                    layer("/usr/lib", RO, false),
                    layer("/tmp/work", RW, false),
                ],
                pinned: vec![],
            }
        );
    }

    #[test]
    fn a_read_only_child_of_a_writable_root_is_shielded_and_the_root_pinned() {
        // Listed child first: the order in the policy does not matter.
        let paths = [entry("/ws/.git", RO), entry("/ws/AGENTS.md", RO), entry("/ws", RW)];
        let layering = Layering::new(&paths);

        assert_eq!(
            layering,
            Layering {
                layers: vec![
                    layer("/ws", RW, false),
                    layer("/ws/.git", RO, true),
                    layer("/ws/AGENTS.md", RO, true),
                ],
                pinned: vec![PathBuf::from("/ws")],
            }
        );
        assert_eq!(
            layering.mounts(),
            [(Path::new("/ws"), RW), (Path::new("/ws/.git"), RO), (Path::new("/ws/AGENTS.md"), RO),]
        );
    }

    #[test]
    fn every_writable_directory_above_a_deep_read_only_entry_is_pinned() {
        let paths = [entry("/home", RW), entry("/home/ws", RW), entry("/home/ws/a/b/.git", RO)];
        let layering = Layering::new(&paths);

        assert_eq!(
            layering.pinned,
            ["/home", "/home/ws", "/home/ws/a", "/home/ws/a/b"].map(PathBuf::from)
        );
        // `/home` and `/home/ws` are entries and already mount points.
        assert_eq!(
            layering.mounts(),
            [
                (Path::new("/home"), RW),
                (Path::new("/home/ws"), RW),
                (Path::new("/home/ws/a"), RW),
                (Path::new("/home/ws/a/b"), RW),
                (Path::new("/home/ws/a/b/.git"), RO),
            ]
        );
    }

    #[test]
    fn pinning_stops_at_read_only_space() {
        let paths = [entry("/r", RO), entry("/r/w", RW), entry("/r/w/x/.git", RO)];
        let layering = Layering::new(&paths);

        // `/r/w` is pinned twice over: as a writable ancestor of `.git`, and as a writable
        // entry inside read-only `/r`. `/r` itself cannot be written.
        assert_eq!(layering.pinned, ["/r/w", "/r/w/x"].map(PathBuf::from));
    }

    #[test]
    fn a_writable_entry_inside_a_read_only_entry_wins_and_is_pinned() {
        let paths = [entry("/ws", RW), entry("/ws/.git", RO), entry("/ws/.git/tmp", RW)];
        let layering = Layering::new(&paths);

        assert_eq!(
            layering.layers,
            [
                layer("/ws", RW, false),
                layer("/ws/.git", RO, true),
                layer("/ws/.git/tmp", RW, false),
            ]
        );
        assert_eq!(layering.pinned, ["/ws", "/ws/.git/tmp"].map(PathBuf::from));
        assert_eq!(effective_mode(&paths, Path::new("/ws/.git/tmp/x")), Some(RW));
        assert_eq!(effective_mode(&paths, Path::new("/ws/.git/config")), Some(RO));
    }

    #[test]
    fn read_write_wins_for_the_same_path() {
        let paths = [entry("/ws", RW), entry("/ws/out", RW), entry("/ws/out", RO)];
        let layering = Layering::new(&paths);

        assert_eq!(
            layering.layers,
            [layer("/ws", RW, false), layer("/ws/out", RO, false), layer("/ws/out", RW, false),]
        );
        assert!(layering.pinned.is_empty(), "{layering:?}");
    }

    #[test]
    fn a_writable_entry_inside_a_writable_entry_is_left_as_it_was() {
        let paths = [entry("/ws/target", RW), entry("/ws", RW)];
        let layering = Layering::new(&paths);

        assert_eq!(layering.layers, [layer("/ws", RW, false), layer("/ws/target", RW, false)]);
        assert!(layering.pinned.is_empty());
    }

    #[test]
    fn path_prefixes_are_compared_by_component() {
        let paths = [entry("/ws", RW), entry("/wsx", RO)];
        let layering = Layering::new(&paths);

        assert!(layering.layers.iter().all(|layer| !layer.shielded), "{layering:?}");
    }

    /// The decision an enforcer reaches for `probe`: the last layer that covers it.
    fn last_covering(mounts: &[(&Path, AccessMode)], probe: &Path) -> Option<AccessMode> {
        mounts.iter().rev().find(|(path, _)| probe.starts_with(path)).map(|(_, mode)| *mode)
    }

    /// Every path over the alphabet of [`arb_paths`], one level deeper than it reaches.
    fn every_probe() -> Vec<PathBuf> {
        let mut level = vec![PathBuf::from("/")];
        let mut probes = Vec::new();
        for _ in 0..5 {
            level = level.iter().flat_map(|dir| [dir.join("a"), dir.join("b")]).collect();
            probes.extend(level.iter().cloned());
        }
        probes
    }

    fn arb_paths() -> impl Strategy<Value = Vec<AllowedPath>> {
        // A two-letter alphabet makes nesting and duplicates common.
        proptest::collection::vec(
            ("/[ab](/[ab]){0,3}", prop_oneof![Just(RO), Just(RW)])
                .prop_map(|(path, mode)| AllowedPath { path: PathBuf::from(path), mode }),
            0..8,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Applying the mounts in order gives every path the most specific entry's access,
        /// whatever order the policy listed them in.
        #[test]
        fn the_last_mount_over_a_path_is_its_most_specific_entry(paths in arb_paths()) {
            let layering = Layering::new(&paths);
            let mounts = layering.mounts();
            for probe in every_probe() {
                prop_assert_eq!(
                    last_covering(&mounts, &probe),
                    effective_mode(&paths, &probe),
                    "probe {:?}: {:?}",
                    probe,
                    mounts
                );
            }
        }

        /// Every shielded entry has every writable directory above it pinned, and a pinned
        /// directory is always writable space.
        #[test]
        fn shielded_entries_have_their_writable_ancestors_pinned(paths in arb_paths()) {
            let layering = Layering::new(&paths);
            for layer in layering.layers.iter().filter(|layer| layer.shielded) {
                prop_assert_eq!(effective_mode(&paths, layer.path), Some(RO));
                for dir in layer.path.ancestors().skip(1) {
                    if effective_mode(&paths, dir) != Some(RW) {
                        break;
                    }
                    prop_assert!(layering.pinned.iter().any(|pin| pin == dir), "{:?}", layering);
                }
            }
            for pin in &layering.pinned {
                prop_assert_eq!(effective_mode(&paths, pin), Some(RW));
            }
        }

        /// A read-only entry is shielded exactly when the space around it is writable and no
        /// read-write entry for the same path overrides it.
        #[test]
        fn shielding_matches_the_enclosing_access(paths in arb_paths()) {
            let layering = Layering::new(&paths);
            for layer in &layering.layers {
                let expected = layer.mode == RO
                    && parent_mode(&paths, layer.path) == Some(RW)
                    && effective_mode(&paths, layer.path) == Some(RO);
                prop_assert_eq!(layer.shielded, expected, "{:?}", layer);
            }
        }
    }
}
