//! A workspace must contain file tools, including when symlinks are involved.
//!
//! `Workspace::resolve` normalized a path lexically and checked `starts_with(root)`.
//! A symlink sitting lexically under the root satisfies that check while pointing
//! anywhere on the host, and ordinary file I/O follows it. A symlinked parent
//! directory redirected creation and writes the same way. The existing containment
//! test covered `..` traversal only.

#![cfg(unix)]

use adk_devtools::Workspace;
use std::fs;
use std::os::unix::fs::symlink;

/// A workspace root plus an outside directory, both inside one temp dir.
struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "host secret").unwrap();
    Fixture { _temp: temp, root, outside }
}

#[test]
fn a_symlinked_file_pointing_outside_is_refused() {
    let f = fixture();
    symlink(f.outside.join("secret.txt"), f.root.join("link.txt")).unwrap();

    let workspace = Workspace::new(&f.root);
    let result = workspace.resolve("link.txt");

    assert!(
        result.is_err(),
        "a symlink to a host file was accepted, resolving to {:?}",
        result.ok()
    );
}

#[test]
fn a_symlinked_directory_pointing_outside_is_refused() {
    let f = fixture();
    symlink(&f.outside, f.root.join("escape")).unwrap();

    let workspace = Workspace::new(&f.root);

    assert!(
        workspace.resolve("escape/secret.txt").is_err(),
        "a read through a symlinked directory was accepted"
    );
    // Creation through a symlinked parent must be refused too, even though the
    // final component does not exist yet.
    assert!(
        workspace.resolve("escape/planted.txt").is_err(),
        "a write through a symlinked directory was accepted"
    );
}

#[test]
fn a_nested_symlinked_parent_is_refused() {
    let f = fixture();
    fs::create_dir_all(f.root.join("a/b")).unwrap();
    symlink(&f.outside, f.root.join("a/b/out")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(
        workspace.resolve("a/b/out/secret.txt").is_err(),
        "a symlink deeper in the tree was accepted"
    );
}

#[test]
fn parent_traversal_is_still_refused() {
    // The original containment property must keep holding.
    let f = fixture();
    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("../outside/secret.txt").is_err());
    assert!(workspace.resolve("/etc/passwd").is_err());
}

#[test]
fn ordinary_paths_inside_the_workspace_still_resolve() {
    // Guards against the containment check rejecting legitimate work.
    let f = fixture();
    fs::create_dir_all(f.root.join("src")).unwrap();
    fs::write(f.root.join("src/main.rs"), "fn main() {}").unwrap();

    let workspace = Workspace::new(&f.root);

    let existing = workspace.resolve("src/main.rs").expect("an existing file must resolve");
    assert!(existing.ends_with("src/main.rs"));

    let new_file = workspace.resolve("src/new_module.rs").expect("a new file must resolve");
    assert!(new_file.ends_with("src/new_module.rs"));

    let new_dir = workspace.resolve("docs/guide/index.md").expect("a new nested path must resolve");
    assert!(new_dir.ends_with("docs/guide/index.md"));
}

#[test]
fn a_symlink_that_stays_inside_the_workspace_is_allowed() {
    // Containment is about where a link *points*, not that a link exists.
    // Repositories legitimately contain internal symlinks, and refusing them would
    // break ordinary work without improving containment.
    let f = fixture();
    fs::write(f.root.join("real.txt"), "inside").unwrap();
    symlink(f.root.join("real.txt"), f.root.join("alias.txt")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("alias.txt").is_ok(), "an inside-pointing link must resolve");
    assert!(workspace.resolve("real.txt").is_ok());
}
