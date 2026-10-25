//! The legacy archive (spec 3.5 A-I10): the export is stored encrypted through `adk-artifact`,
//! read back only with its key, never deleted by any step, and recorded as `site.archive`.
#![cfg(feature = "migrate")]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_artifact::{
    ArtifactService, DeleteRequest, InMemoryArtifactService, ListRequest, ListResponse,
    LoadRequest, LoadResponse, SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
use adk_core::Part;
use adk_core::audit_log::{AuditRecorder, InMemoryAuditRecorder};
use adk_site::migrate::archive::{ARCHIVE_MEDIA_TYPE, ArchiveKey, LegacyArchive};
use async_trait::async_trait;
use chrono::Utc;

/// Wraps an artifact service and counts deletes, which the archive must never send.
struct NoDeletes {
    inner: InMemoryArtifactService,
    deletes: AtomicUsize,
}

#[async_trait]
impl ArtifactService for NoDeletes {
    async fn save(&self, req: SaveRequest) -> adk_core::Result<SaveResponse> {
        self.inner.save(req).await
    }
    async fn load(&self, req: LoadRequest) -> adk_core::Result<LoadResponse> {
        self.inner.load(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(req).await
    }
    async fn list(&self, req: ListRequest) -> adk_core::Result<ListResponse> {
        self.inner.list(req).await
    }
    async fn versions(&self, req: VersionsRequest) -> adk_core::Result<VersionsResponse> {
        self.inner.versions(req).await
    }
}

#[tokio::test]
async fn the_export_is_archived_encrypted_and_never_deleted() {
    let service =
        Arc::new(NoDeletes { inner: InMemoryArtifactService::new(), deletes: AtomicUsize::new(0) });
    let key = ArchiveKey::generate().unwrap();
    let archive =
        LegacyArchive::new(service.clone(), "solar.test", "owner", "migration", key.clone());
    let export = std::fs::read(support::fixtures().join("export.xml")).unwrap();
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let audit: Arc<dyn AuditRecorder> = recorder.clone();
    let first = archive
        .store("export.xml", &export, Utc::now(), Some((&audit, &support::acting())))
        .await
        .unwrap();
    let second = archive.store("export.xml", &export, Utc::now(), None).await.unwrap();
    assert_eq!((first.version, second.version), (1, 2), "storing appends a version");
    assert_eq!(first.file_name, "legacy-export.xml.awparc");
    assert_eq!(first.key_id, key.id());
    assert_eq!(archive.load(&first.file_name, Some(1)).await.unwrap().as_ref(), export.as_slice());

    let stored = service
        .load(LoadRequest {
            app_name: "solar.test".into(),
            user_id: "owner".into(),
            session_id: "migration".into(),
            file_name: first.file_name.clone(),
            version: Some(1),
        })
        .await
        .unwrap();
    let Part::InlineData { mime_type, data, .. } = stored.part else { panic!("inline data") };
    assert_eq!(mime_type, ARCHIVE_MEDIA_TYPE);
    let needle = b"Example Solar";
    assert!(!data.windows(needle.len()).any(|w| w == needle), "the stored bytes are ciphertext");

    let stranger = LegacyArchive::new(
        service.clone(),
        "solar.test",
        "owner",
        "migration",
        ArchiveKey::generate().unwrap(),
    );
    assert!(stranger.load(&first.file_name, Some(1)).await.is_err(), "another key opens nothing");
    let mut tampered = data.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    service
        .save(SaveRequest {
            app_name: "solar.test".into(),
            user_id: "owner".into(),
            session_id: "migration".into(),
            file_name: first.file_name.clone(),
            part: Part::inline_data(ARCHIVE_MEDIA_TYPE, tampered),
            version: None,
        })
        .await
        .unwrap();
    assert!(
        archive.load(&first.file_name, Some(3)).await.is_err(),
        "an altered archive does not open"
    );
    assert_eq!(service.deletes.load(Ordering::SeqCst), 0, "no step deletes (A-I10)");
    let records = recorder.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action, "site.archive");
    assert_eq!(
        format!("{key:?}"),
        format!("ArchiveKey({:?})", key.id()),
        "only the key id is printed"
    );
}
