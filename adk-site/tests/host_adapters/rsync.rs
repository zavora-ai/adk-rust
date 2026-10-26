//! `RsyncHost` with the real `rsync` and `sh`, against a local directory (the same
//! commands the SSH target runs on a server), and over SSH when a server is configured.

use std::path::Path;
use std::sync::Arc;

use adk_site::host::conformance::{self, Fixture};
use adk_site::host::rsync::{RsyncHost, RsyncTarget};
use adk_site::host::{HostTarget, SiteHost};
use adk_site::{Result, SiteError};

use crate::support::credential;

fn local(root: &Path) -> RsyncHost {
    RsyncHost::new(RsyncTarget::Local { root: root.to_path_buf() })
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().ino()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rsync_release_directories_pass_the_conformance_suite() {
    let root = tempfile::tempdir().unwrap();
    let next = std::sync::atomic::AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Arc::new(local(&root.path().join(n.to_string()))) as Arc<dyn SiteHost>
    })
    .await;
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Unchanged files are copied on the server, never shared between releases, and every served
/// file is read-only; the log's manifests are carried forward (spec 3.5 §7.8 "Hosts").
#[cfg(unix)]
#[tokio::test]
async fn unchanged_files_are_copies_and_current_is_a_symlink() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let host = local(root.path());
    let mut fixture = Fixture::new().await;
    let files = |v: &str| {
        vec![
            ("/index.html", format!("<p>{v}</p>").into_bytes()),
            ("/about/index.html", b"<p>about</p>".to_vec()),
        ]
    };
    let a = fixture.release(&files("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&files("b"), Vec::new(), false).await;
    let report = host.publish(&b, HostTarget::Production).await?;
    let content: Vec<&String> =
        report.uploaded.iter().filter(|p| !adk_site::release::is_log_path(p)).collect();
    assert_eq!(content, vec!["/index.html"]);
    assert_eq!(std::fs::read_link(root.path().join("current")).unwrap(), Path::new("releases/2"));
    let (first, second) = (
        root.path().join("releases/1/about/index.html"),
        root.path().join("releases/2/about/index.html"),
    );
    assert_eq!(std::fs::read(&first).unwrap(), std::fs::read(&second).unwrap());
    assert_ne!(inode(&first), inode(&second), "each release holds its own copy");
    assert_eq!((mode(&first), mode(&second)), (0o444, 0o444), "served files are read-only");
    assert_eq!(
        std::fs::read(root.path().join("releases/1/.well-known/awp/releases/1.json")).unwrap(),
        std::fs::read(root.path().join("releases/2/.well-known/awp/releases/1.json")).unwrap(),
        "the earlier manifest is carried forward"
    );
    assert_eq!(
        mode(&root.path().join("releases/2/.well-known/awp/releases/1.json")),
        0o444,
        "the carried log is read-only"
    );
    assert!(
        root.path().join("releases/2.index.json").is_file(),
        "the index sits outside the served directory"
    );
    assert!(!root.path().join("releases/2/releases").exists());
    let preview = fixture.release(&files("next"), Vec::new(), false).await;
    host.publish(&preview, HostTarget::Preview).await?;
    let served = std::fs::read(root.path().join("preview/index.html")).unwrap();
    assert_eq!(served, b"<p>next</p>");
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>b</p>".as_slice()));
    host.publish(&preview, HostTarget::Preview).await?;
    Ok(())
}

/// A write through a file of the serving release — made writable and changed in place —
/// changes that release only: the previous release still rolls back exactly, and a rollback
/// holds copies of its own (spec 3.5 §7.8 "Hosts", A-I11).
#[cfg(unix)]
#[tokio::test]
async fn a_write_through_one_release_changes_no_other() -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let host = local(root.path());
    let mut fixture = Fixture::new().await;
    let files = |v: &str| {
        vec![
            ("/index.html", format!("<p>{v}</p>").into_bytes()),
            ("/about/index.html", b"<p>about</p>".to_vec()),
        ]
    };
    let a = fixture.release(&files("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&files("b"), Vec::new(), false).await;
    host.publish(&b, HostTarget::Production).await?;

    let served = root.path().join("current/about/index.html");
    let refused = std::fs::OpenOptions::new().write(true).open(&served).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied, "a served file is read-only");
    std::fs::set_permissions(&served, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut handle = std::fs::OpenOptions::new().write(true).truncate(true).open(&served).unwrap();
    handle.write_all(b"<p>defaced</p>").unwrap();
    drop(handle);

    let report = host.rollback(&a.manifest).await?;
    assert_eq!(report.seq, 1);
    assert_eq!(host.fetch("/about/").await?.as_deref(), Some(b"<p>about</p>".as_slice()));
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>a</p>".as_slice()));
    let rolled = std::fs::read_link(root.path().join("current")).unwrap();
    assert_ne!(
        inode(&root.path().join(&rolled).join("about/index.html")),
        inode(&root.path().join("releases/1/about/index.html")),
        "a rollback holds copies of its own"
    );
    Ok(())
}

#[tokio::test]
async fn a_tampered_release_is_not_rolled_back_to_and_paths_stay_inside() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let host = local(root.path());
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&[("/index.html", b"<p>a</p>".to_vec())], Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&[("/index.html", b"<p>b</p>".to_vec())], Vec::new(), false).await;
    host.publish(&b, HostTarget::Production).await?;
    assert!(matches!(host.publish(&b, HostTarget::Production).await, Err(SiteError::Sequence(_))));
    let page = root.path().join("releases/1/index.html");
    std::fs::remove_file(&page).unwrap();
    std::fs::write(&page, "<p>defaced</p>").unwrap();
    let error = host.rollback(&a.manifest).await.unwrap_err();
    assert!(
        matches!(error, SiteError::DigestMismatch { ref path, .. } if path == "/index.html"),
        "{error}"
    );
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>b</p>".as_slice()));
    let unknown = awp_types::ReleaseManifest { seq: 9, ..a.manifest.clone() };
    assert!(matches!(
        host.rollback(&unknown).await,
        Err(SiteError::ReleaseNotFound { seq: 9, .. })
    ));
    assert!(host.fetch("/../current").await.is_err());
    assert!(host.fetch("/.well-known/").await?.is_none());
    Ok(())
}

#[tokio::test]
async fn an_ssh_target_needs_an_absolute_root() {
    let host = RsyncHost::new(RsyncTarget::Ssh {
        destination: "deploy@example.com".into(),
        port: None,
        root: "srv".into(),
    });
    let error = host.fetch("/").await.unwrap_err();
    assert!(error.to_string().contains("absolute path"), "{error}");
}

/// Runs the conformance suite on a server over SSH.
///
/// With the `linuxserver/openssh-server` image (rsync installed by its mod):
///
/// ```sh
/// ssh-keygen -t ed25519 -N '' -f /tmp/adk-site-ssh
/// docker run -d --name adk-site-ssh -p 127.0.0.1:2222:2222 -e USER_NAME=deploy \
///   -e PUBLIC_KEY="$(cat /tmp/adk-site-ssh.pub)" -e DOCKER_MODS=linuxserver/mods:openssh-server-rsync \
///   lscr.io/linuxserver/openssh-server
/// ADK_SITE_RSYNC_DESTINATION=deploy@127.0.0.1 ADK_SITE_RSYNC_PORT=2222 ADK_SITE_RSYNC_ROOT=/config/site \
/// ADK_SITE_RSYNC_KEY="$(cat /tmp/adk-site-ssh)" \
/// ADK_SITE_RSYNC_KNOWN_HOSTS="$(ssh-keyscan -p 2222 127.0.0.1 2>/dev/null)" \
///   cargo nextest run -p adk-site --features host-rsync --run-ignored only rsync_over_ssh
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs an SSH server with rsync: set ADK_SITE_RSYNC_DESTINATION, ADK_SITE_RSYNC_ROOT, ADK_SITE_RSYNC_KEY and ADK_SITE_RSYNC_KNOWN_HOSTS"]
async fn rsync_over_ssh_passes_the_conformance_suite() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let destination = var("ADK_SITE_RSYNC_DESTINATION");
    let root = var("ADK_SITE_RSYNC_ROOT");
    let port: Option<u16> =
        std::env::var("ADK_SITE_RSYNC_PORT").ok().map(|p| p.parse().expect("port"));
    let key = var("ADK_SITE_RSYNC_KEY");
    let known = var("ADK_SITE_RSYNC_KNOWN_HOSTS");
    let host = destination.rsplit('@').next().unwrap().to_string();
    let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let next = std::sync::atomic::AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = format!("{root}/adk-site-conformance-{run}/{n}");
        let resource = format!("ssh://{host}:{}{dir}", port.unwrap_or(22));
        Arc::new(
            RsyncHost::new(RsyncTarget::Ssh { destination: destination.clone(), port, root: dir })
                .with_identity(credential(&resource, &key))
                .with_known_hosts(known.clone()),
        ) as Arc<dyn SiteHost>
    })
    .await;
}
