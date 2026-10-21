//! `DockerClient::with_bind_mount` (spec 3.1 §10 S4) against a real Docker daemon.
//!
//! A harness bind-mounts the host workspace at `/workspace` so commands in the container and
//! file tools on the host see the same files. These tests need a running Docker daemon and
//! the `alpine:3.20` image, so they are ignored by default:
//!
//! ```bash
//! cargo nextest run -p adk-sandbox --features workspace-docker --test docker_bind_mount_tests --run-ignored only
//! ```

#![cfg(all(unix, feature = "workspace-docker"))]

use std::path::PathBuf;

use adk_sandbox::workspace::{DockerClient, Manifest, SandboxClient};

const IMAGE: &str = "alpine:3.20";

/// A canonical temporary directory under the user's temp space, which Docker Desktop shares
/// with its VM by default.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let canonical = std::fs::canonicalize(directory.path()).expect("canonicalize");
    (directory, canonical)
}

#[tokio::test]
#[ignore = "needs a Docker daemon and the alpine:3.20 image"]
async fn the_container_and_the_host_share_the_mounted_workspace() {
    use std::os::unix::fs::MetadataExt;

    let (_workspace_guard, workspace) = canonical_tempdir();
    let (_reference_guard, reference) = canonical_tempdir();
    std::fs::write(workspace.join("from-host.txt"), "written on the host\n").unwrap();
    std::fs::write(reference.join("pinned.txt"), "read-only reference\n").unwrap();
    // Root without capabilities cannot write a directory only its host owner may write, so
    // the container runs as that owner.
    let owner = std::fs::metadata(&workspace).unwrap();

    let client = DockerClient::with_image(IMAGE)
        .await
        .expect("a Docker daemon is running")
        .with_bind_mount(&workspace, "/workspace", false)
        .with_bind_mount(&reference, "/reference", true)
        .with_user(format!("{}:{}", owner.uid(), owner.gid()));
    let handle = client.provision(&Manifest { entries: vec![] }).await.expect("provision");
    let session = client.start(&handle).await.expect("start");

    let read = session.exec_command("cat from-host.txt /reference/pinned.txt", None).await;
    let write =
        session.exec_command("echo written in the container > from-container.txt", None).await;
    let read_only = session.exec_command("echo tampered > /reference/pinned.txt", None).await;
    client.stop(&handle).await.expect("stop");

    let read = read.expect("exec");
    assert_eq!(read.exit_code, 0, "{read:?}");
    assert_eq!(read.stdout, "written on the host\nread-only reference\n");

    let write = write.expect("exec");
    assert_eq!(write.exit_code, 0, "{write:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("from-container.txt")).unwrap(),
        "written in the container\n",
        "the host must see the container's write"
    );

    let read_only = read_only.expect("exec");
    assert_ne!(read_only.exit_code, 0, "{read_only:?}");
    assert!(read_only.stderr.contains("Read-only file system"), "{read_only:?}");
    assert_eq!(
        std::fs::read_to_string(reference.join("pinned.txt")).unwrap(),
        "read-only reference\n"
    );
}

/// The container runs as the user the client names, so what it writes into the mounted
/// workspace belongs to that user on the host.
#[tokio::test]
#[ignore = "needs a Docker daemon and the alpine:3.20 image"]
async fn the_container_runs_as_the_configured_user() {
    use std::os::unix::fs::MetadataExt;

    let (_guard, workspace) = canonical_tempdir();
    let uid = std::fs::metadata(&workspace).unwrap().uid();
    let gid = std::fs::metadata(&workspace).unwrap().gid();

    let client = DockerClient::with_image(IMAGE)
        .await
        .expect("a Docker daemon is running")
        .with_bind_mount(&workspace, "/workspace", false)
        .with_user(format!("{uid}:{gid}"));
    let handle = client.provision(&Manifest { entries: vec![] }).await.expect("provision");
    let session = client.start(&handle).await.expect("start");

    let id = session.exec_command("id -u; id -g", None).await;
    let write = session.exec_command("echo owned > owned.txt", None).await;
    client.stop(&handle).await.expect("stop");

    let id = id.expect("exec");
    assert_eq!(id.exit_code, 0, "{id:?}");
    assert_eq!(id.stdout, format!("{uid}\n{gid}\n"));
    let write = write.expect("exec");
    assert_eq!(write.exit_code, 0, "{write:?}");
    let owned = std::fs::metadata(workspace.join("owned.txt")).unwrap();
    assert_eq!((owned.uid(), owned.gid()), (uid, gid), "the host user owns the file");
}

#[tokio::test]
#[ignore = "needs a Docker daemon and the alpine:3.20 image"]
async fn a_missing_host_path_fails_provisioning() {
    let (_guard, root) = canonical_tempdir();
    let client = DockerClient::with_image(IMAGE)
        .await
        .expect("a Docker daemon is running")
        .with_bind_mount(root.join("does-not-exist"), "/workspace", false);

    let result = client.provision(&Manifest { entries: vec![] }).await;

    assert!(matches!(result, Err(adk_sandbox::SandboxError::ProvisionFailed { .. })), "{result:?}");
}
