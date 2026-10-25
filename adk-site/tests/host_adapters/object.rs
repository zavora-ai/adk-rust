//! `ObjectStoreHost` over an in-memory store, both layouts (spec 3.5 §7.8.3
//! `PrefixPointer`, Q16).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_site::host::conformance::{self, Fixture};
use adk_site::host::object::{
    CachePurge, InMemoryObjectStore, ObjectMeta, ObjectStore, ObjectStoreHost,
    POINTER_CACHE_CONTROL, PRODUCTION_POINTER,
};
use adk_site::host::{HostTarget, SiteHost};
use adk_site::{Result, SiteError};
use async_trait::async_trait;
use awp_types::Atomicity;
use bytes::Bytes;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prefix_pointer_passes_the_conformance_suite() {
    conformance::run_all(|| {
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(InMemoryObjectStore::new())))
            as Arc<dyn SiteHost>
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_place_passes_the_conformance_suite() {
    conformance::run_all(|| {
        Arc::new(ObjectStoreHost::in_place(
            Arc::new(InMemoryObjectStore::new()),
            Arc::new(InMemoryObjectStore::new()),
        )) as Arc<dyn SiteHost>
    })
    .await;
}

fn page(version: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("/index.html", format!("<p>{version}</p>").into_bytes()),
        ("/about/index.html", b"<p>about</p>".to_vec()),
        ("/style.css", b"body{}".to_vec()),
    ]
}

/// Fails every write after the first `allowed`.
#[derive(Debug)]
struct Flaky {
    inner: InMemoryObjectStore,
    writes: AtomicUsize,
    allowed: AtomicUsize,
}

#[async_trait]
impl ObjectStore for Flaky {
    fn name(&self) -> &str {
        "flaky"
    }

    async fn put(&self, key: &str, body: Bytes, meta: &ObjectMeta) -> Result<()> {
        if self.writes.fetch_add(1, Ordering::SeqCst) >= self.allowed.load(Ordering::SeqCst) {
            return Err(SiteError::Host {
                host: "flaky".into(),
                message: format!("refused {key}"),
            });
        }
        self.inner.put(key, body, meta).await
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        self.inner.get(key).await
    }

    async fn copy(&self, from: &str, to: &str) -> Result<()> {
        self.inner.copy(from, to).await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.inner.delete(key).await
    }
}

#[tokio::test]
async fn a_failed_publish_leaves_the_previous_release_serving() -> Result<()> {
    for in_place in [false, true] {
        let store = Arc::new(Flaky {
            inner: InMemoryObjectStore::new(),
            writes: AtomicUsize::new(0),
            allowed: AtomicUsize::new(usize::MAX),
        });
        let host = if in_place {
            ObjectStoreHost::in_place(Arc::new(InMemoryObjectStore::new()), store.clone())
        } else {
            ObjectStoreHost::prefix_pointer(store.clone())
        };
        let mut fixture = Fixture::new().await;
        let a = fixture.release(&page("a"), Vec::new(), false).await;
        host.publish(&a, HostTarget::Production).await?;
        let b = fixture.release(&page("b"), Vec::new(), false).await;
        store.allowed.store(store.writes.load(Ordering::SeqCst) + 2, Ordering::SeqCst);
        assert!(host.publish(&b, HostTarget::Production).await.is_err(), "the third write fails");
        assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>a</p>".as_slice()));
        store.allowed.store(usize::MAX, Ordering::SeqCst);
        host.publish(&b, HostTarget::Production).await?;
        assert_eq!(
            host.fetch("/").await?.as_deref(),
            Some(b"<p>b</p>".as_slice()),
            "a retry publishes"
        );
    }
    Ok(())
}

#[tokio::test]
async fn pointers_are_written_uncached_and_files_carry_their_types() -> Result<()> {
    let store = Arc::new(InMemoryObjectStore::new());
    let host =
        ObjectStoreHost::prefix_pointer(store.clone()).with_cache_control("public, max-age=60");
    assert_eq!(host.limits().atomic, Atomicity::PrefixPointer);
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&page("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let pointer = store.meta(PRODUCTION_POINTER).await.expect("pointer");
    assert_eq!(pointer.cache_control.as_deref(), Some(POINTER_CACHE_CONTROL));
    assert_eq!(store.get(PRODUCTION_POINTER).await?.as_deref(), Some(b"releases/1".as_slice()));
    let css = store.meta("releases/1/style.css").await.expect("css");
    assert_eq!(
        css,
        ObjectMeta {
            content_type: "text/css; charset=utf-8".into(),
            cache_control: Some("public, max-age=60".into())
        }
    );
    let checkpoint =
        store.meta("releases/1/.well-known/awp/log/checkpoint").await.expect("checkpoint");
    assert_eq!(checkpoint.cache_control, None, "log files are not given the site's cache policy");
    Ok(())
}

#[tokio::test]
async fn previews_have_their_own_pointer_and_never_touch_production() -> Result<()> {
    let store = Arc::new(InMemoryObjectStore::new());
    let host = ObjectStoreHost::prefix_pointer(store.clone());
    let mut fixture = Fixture::new().await;
    let live = fixture.release(&page("live"), Vec::new(), false).await;
    host.publish(&live, HostTarget::Production).await?;
    assert!(matches!(
        host.publish(&live, HostTarget::Production).await,
        Err(SiteError::Sequence(_))
    ));
    let next = fixture.release(&page("next"), Vec::new(), false).await;
    host.publish(&next, HostTarget::Preview).await?;
    let preview = host.serving_dir(HostTarget::Preview).await?.expect("preview");
    assert!(preview.starts_with("previews/2-"), "{preview}");
    assert_eq!(
        store.get(&format!("{preview}/index.html")).await?.as_deref(),
        Some(b"<p>next</p>".as_slice())
    );
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>live</p>".as_slice()));
    host.publish(&next, HostTarget::Preview).await?;
    assert!(host.fetch("/../current.pointer").await.is_err(), "paths stay inside the site");
    Ok(())
}

#[tokio::test]
async fn a_tampered_stored_release_is_not_rolled_back_to() -> Result<()> {
    let store = Arc::new(InMemoryObjectStore::new());
    let host = ObjectStoreHost::prefix_pointer(store.clone());
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&page("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&page("b"), Vec::new(), false).await;
    host.publish(&b, HostTarget::Production).await?;
    let meta = ObjectMeta { content_type: "text/html".into(), cache_control: None };
    store.put("releases/1/index.html", Bytes::from_static(b"<p>defaced</p>"), &meta).await?;
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
    Ok(())
}

#[derive(Debug, Default)]
struct Purges(tokio::sync::Mutex<Vec<Vec<String>>>);

#[async_trait]
impl CachePurge for Purges {
    async fn purge(&self, paths: &[String]) -> Result<()> {
        self.0.lock().await.push(paths.to_vec());
        Ok(())
    }
}

#[tokio::test]
async fn the_cache_is_purged_of_changed_and_removed_paths_after_the_switch() -> Result<()> {
    for in_place in [false, true] {
        let purges = Arc::new(Purges::default());
        let host = if in_place {
            ObjectStoreHost::in_place(
                Arc::new(InMemoryObjectStore::new()),
                Arc::new(InMemoryObjectStore::new()),
            )
        } else {
            ObjectStoreHost::prefix_pointer(Arc::new(InMemoryObjectStore::new()))
        }
        .with_cache_purge(purges.clone());
        let mut fixture = Fixture::new().await;
        let a = fixture.release(&page("a"), Vec::new(), false).await;
        host.publish(&a, HostTarget::Production).await?;
        let mut next = page("b");
        next.retain(|(p, _)| *p != "/style.css");
        let b = fixture.release(&next, Vec::new(), false).await;
        host.publish(&b, HostTarget::Production).await?;
        let recorded = purges.0.lock().await;
        assert_eq!(recorded.len(), 2, "one purge per publish");
        let second = &recorded[1];
        assert!(second.contains(&"/index.html".to_string()), "{second:?}");
        assert!(second.contains(&"/style.css".to_string()), "removed paths are purged: {second:?}");
        assert!(
            !second.contains(&"/about/index.html".to_string()),
            "unchanged paths are not: {second:?}"
        );
        assert!(second.contains(&"/.well-known/awp/log/checkpoint".to_string()), "{second:?}");
    }
    Ok(())
}

#[tokio::test]
async fn in_place_removes_files_and_keeps_releases_in_the_archive_only() -> Result<()> {
    let live = Arc::new(InMemoryObjectStore::new());
    let archive = Arc::new(InMemoryObjectStore::new());
    let host = ObjectStoreHost::in_place(live.clone(), archive.clone());
    assert_eq!(host.limits().atomic, Atomicity::None);
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&page("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let mut next = page("b");
    next.retain(|(p, _)| *p != "/style.css");
    let b = fixture.release(&next, Vec::new(), false).await;
    let report = host.publish(&b, HostTarget::Production).await?;
    assert_eq!(report.removed, vec!["/style.css".to_string()]);
    let keys = live.keys().await;
    assert!(!keys.contains(&"style.css".to_string()));
    assert!(
        keys.iter().all(|k| !k.starts_with("releases/") && !k.ends_with(".pointer")),
        "{keys:?}"
    );
    assert!(archive.keys().await.contains(&"releases/1/style.css".to_string()));
    host.rollback(&a.manifest).await?;
    assert_eq!(live.get("style.css").await?.as_deref(), Some(b"body{}".as_slice()));
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>a</p>".as_slice()));
    Ok(())
}

mod properties {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use adk_site::host::conformance::Fixture;
    use adk_site::host::object::{InMemoryObjectStore, ObjectStoreHost};
    use adk_site::host::{HostTarget, SiteHost};
    use proptest::prelude::*;

    const PATHS: [&str; 6] =
        ["/index.html", "/a.html", "/b/index.html", "/b/c.css", "/d.txt", "/e/f/g.html"];

    /// A release: a version of each path it holds, and whether it is published as a delta.
    type Release = (BTreeMap<usize, u8>, bool);

    /// Up to five releases, each a non-empty subset of `PATHS` with one of three contents,
    /// published as deltas or snapshots; then a rollback target.
    fn releases() -> impl Strategy<Value = (Vec<Release>, usize)> {
        let release =
            (prop::collection::btree_map(0..PATHS.len(), 0u8..3, 1..=PATHS.len()), any::<bool>());
        prop::collection::vec(release, 1..=5).prop_flat_map(|releases| {
            let len = releases.len();
            (Just(releases), 0..len)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, ..ProptestConfig::default() })]

        /// A host always serves exactly the latest release, and a rollback exactly the
        /// earlier one: no file missing, none left over (spec 3.5 §7.8.3, A-I4).
        #[test]
        fn hosts_serve_exactly_the_latest_release((sequence, back) in releases(), in_place in any::<bool>()) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let host = if in_place {
                    ObjectStoreHost::in_place(Arc::new(InMemoryObjectStore::new()), Arc::new(InMemoryObjectStore::new()))
                } else {
                    ObjectStoreHost::prefix_pointer(Arc::new(InMemoryObjectStore::new()))
                };
                let mut fixture = Fixture::new().await;
                let mut staged = Vec::new();
                for (n, (files, delta)) in sequence.iter().enumerate() {
                    let files: Vec<(&str, Vec<u8>)> = files
                        .iter()
                        .map(|(path, version)| (PATHS[*path], format!("{path}-{version}").into_bytes()))
                        .collect();
                    let release = fixture.release(&files, Vec::new(), *delta && n > 0).await;
                    host.publish(&release, HostTarget::Production).await.unwrap();
                    for (i, path) in PATHS.iter().enumerate() {
                        let expected = sequence[n].0.get(&i).map(|v| format!("{i}-{v}").into_bytes());
                        assert_eq!(host.fetch(path).await.unwrap().map(|b| b.to_vec()), expected, "{path} after release {}", n + 1);
                    }
                    staged.push(release);
                }
                host.rollback(&staged[back].manifest).await.unwrap();
                for (i, path) in PATHS.iter().enumerate() {
                    let expected = sequence[back].0.get(&i).map(|v| format!("{i}-{v}").into_bytes());
                    assert_eq!(host.fetch(path).await.unwrap().map(|b| b.to_vec()), expected, "{path} after rolling back to {}", back + 1);
                }
            });
        }
    }
}
