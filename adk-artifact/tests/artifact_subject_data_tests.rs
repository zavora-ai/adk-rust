//! `ArtifactSubjectData` passes `adk_core::subject_data_store_conformance_tests!` on the
//! in-memory and file backends: after an erasure no read path — `list_user`, a session
//! listing, `user:` artifacts — returns the subject's artifacts, and other users' stay.
//! The GCS backend runs the same suite against a mock in `gcs_tests.rs`.

use std::sync::Arc;

use adk_artifact::{
    ArtifactService, ArtifactSubjectData, FileArtifactService, InMemoryArtifactService,
    ListRequest, SaveRequest, UserArtifact,
};
use adk_core::conformance::SubjectDataStoreHarness;
use adk_core::subject::SubjectDataStore;
use adk_core::{Part, async_trait};

struct Harness {
    service: Arc<dyn ArtifactService>,
    _dir: Option<tempfile::TempDir>,
}

impl Harness {
    fn in_memory() -> Self {
        Self { service: Arc::new(InMemoryArtifactService::new()), _dir: None }
    }

    fn file() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let service = FileArtifactService::new(dir.path()).expect("file service");
        Self { service: Arc::new(service), _dir: Some(dir) }
    }

    async fn save(&self, app_name: &str, user_id: &str, file_name: &str, part: Part) {
        self.service
            .save(SaveRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: "session-1".to_string(),
                file_name: file_name.to_string(),
                part,
                version: None,
            })
            .await
            .expect("save");
    }
}

#[async_trait]
impl SubjectDataStoreHarness for Harness {
    async fn open(&self) -> Arc<dyn SubjectDataStore> {
        Arc::new(ArtifactSubjectData::new(Arc::clone(&self.service)))
    }

    async fn seed(&self, app_name: &str, user_id: &str) {
        self.save(app_name, user_id, "receipt.txt", Part::Text { text: "paid".into() }).await;
        self.save(app_name, user_id, "receipt.txt", Part::Text { text: "refunded".into() }).await;
        let avatar = Part::InlineData {
            mime_type: "image/png".into(),
            data: vec![0x89, 0x50, 0x4e, 0x47],
            uri: None,
            annotations: None,
        };
        self.save(app_name, user_id, "user:avatar.png", avatar).await;
    }

    async fn visible(&self, app_name: &str, user_id: &str) -> usize {
        let listed = self
            .service
            .list(ListRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: "session-1".to_string(),
            })
            .await
            .expect("list")
            .file_names
            .len();
        listed + self.service.list_user(app_name, user_id).await.expect("list user").len()
    }
}

mod in_memory {
    adk_core::subject_data_store_conformance_tests!(harness = super::Harness::in_memory());
}

mod file {
    adk_core::subject_data_store_conformance_tests!(harness = super::Harness::file());
}

/// `list_user` names every version with its session, type, and size; `user:` artifacts
/// belong to no session.
#[tokio::test]
async fn list_user_reports_sessions_types_and_sizes() {
    for harness in [Harness::in_memory(), Harness::file()] {
        harness.seed("shop", "alice").await;
        harness.save("shop", "bob", "other.txt", Part::Text { text: "bob".into() }).await;
        let listed = harness.service.list_user("shop", "alice").await.expect("list user");
        let text = |version, size| UserArtifact {
            session_id: Some("session-1".into()),
            file_name: "receipt.txt".into(),
            version,
            mime_type: Some("text/plain".into()),
            size,
        };
        let mut expected = vec![
            UserArtifact {
                session_id: None,
                file_name: "user:avatar.png".into(),
                version: 1,
                mime_type: Some("image/png".into()),
                size: 4,
            },
            text(1, 4),
            text(2, 8),
        ];
        let mut listed_sorted = listed.clone();
        listed_sorted.sort_by(|a, b| (&a.file_name, a.version).cmp(&(&b.file_name, b.version)));
        expected.sort_by(|a, b| (&a.file_name, a.version).cmp(&(&b.file_name, b.version)));
        assert_eq!(listed_sorted, expected);
        assert_eq!(harness.service.delete_user("shop", "alice").await.expect("delete"), 3);
        assert_eq!(harness.service.list_user("shop", "bob").await.expect("list user").len(), 1);
    }
}
