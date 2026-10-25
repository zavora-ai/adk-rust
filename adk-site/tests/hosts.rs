//! `DirectoryHost` under the host conformance suite (spec 3.5 §7.8 "Hosts", §7.8.3), plus
//! what only a directory host can show: a tampered stored release cannot be rolled back to,
//! previews are separate, and a release cannot be published twice.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_site::host::conformance::{self, Fixture};
use adk_site::host::{DirectoryHost, HostTarget, SiteHost};
use adk_site::{Result, SiteError};

fn page(version: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("/index.html", format!("<p>{version}</p>").into_bytes()),
        ("/about/index.html", b"<p>about</p>".to_vec()),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_host_passes_the_conformance_suite() {
    let root = tempfile::tempdir().unwrap();
    let next = AtomicUsize::new(0);
    let path = root.path().to_path_buf();
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::Relaxed);
        Arc::new(DirectoryHost::new(path.join(n.to_string()))) as Arc<dyn SiteHost>
    })
    .await;
}

#[tokio::test]
async fn a_tampered_release_is_not_rolled_back_to() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let host = DirectoryHost::new(root.path());
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&page("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&page("b"), Vec::new(), false).await;
    host.publish(&b, HostTarget::Production).await?;

    std::fs::write(root.path().join("releases/1/index.html"), "<p>defaced</p>").unwrap();
    let error = host.rollback(&a.manifest).await.unwrap_err();
    assert!(
        matches!(error, SiteError::DigestMismatch { ref path, .. } if path == "/index.html"),
        "{error}"
    );
    assert_eq!(
        host.fetch("/").await?.as_deref(),
        Some(b"<p>b</p>".as_slice()),
        "the live release is untouched"
    );

    let unknown = awp_types::ReleaseManifest { seq: 9, ..a.manifest.clone() };
    assert!(matches!(
        host.rollback(&unknown).await,
        Err(SiteError::ReleaseNotFound { seq: 9, .. })
    ));
    Ok(())
}

#[tokio::test]
async fn previews_are_served_apart_and_releases_publish_once() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let host = DirectoryHost::new(root.path());
    let mut fixture = Fixture::new().await;
    let live = fixture.release(&page("live"), Vec::new(), false).await;
    host.publish(&live, HostTarget::Production).await?;
    assert!(matches!(
        host.publish(&live, HostTarget::Production).await,
        Err(SiteError::Sequence(_))
    ));

    let next = fixture.release(&page("next"), Vec::new(), false).await;
    host.publish(&next, HostTarget::Preview).await?;
    let preview = host.serving_dir(HostTarget::Preview)?.unwrap();
    assert_eq!(std::fs::read(preview.join("index.html")).unwrap(), b"<p>next</p>");
    assert_eq!(host.fetch("/index.html").await?.as_deref(), Some(b"<p>live</p>".as_slice()));
    assert!(host.fetch("/../etc/passwd").await.is_err(), "paths stay inside the site");
    Ok(())
}
