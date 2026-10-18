//! The versioning, naming, and scoping contract every local artifact backend keeps.
//!
//! `gcs_tests.rs` runs the same checks against the GCS backend and a scripted bucket.

use adk_artifact::{
    ArtifactService, DeleteRequest, FileArtifactService, InMemoryArtifactService, ListRequest,
    LoadRequest, SaveRequest, VersionsRequest,
};
use adk_core::{ErrorCategory, Part};
use std::collections::BTreeSet;
use std::sync::Arc;

fn save(session: &str, name: &str, text: &str, version: Option<i64>) -> SaveRequest {
    SaveRequest {
        app_name: "app".to_string(),
        user_id: "u1".to_string(),
        session_id: session.to_string(),
        file_name: name.to_string(),
        part: Part::Text { text: text.to_string() },
        version,
    }
}

fn load(session: &str, name: &str, version: Option<i64>) -> LoadRequest {
    LoadRequest {
        app_name: "app".to_string(),
        user_id: "u1".to_string(),
        session_id: session.to_string(),
        file_name: name.to_string(),
        version,
    }
}

fn list(session: &str) -> ListRequest {
    ListRequest {
        app_name: "app".to_string(),
        user_id: "u1".to_string(),
        session_id: session.to_string(),
    }
}

fn versions(session: &str, name: &str) -> VersionsRequest {
    VersionsRequest {
        app_name: "app".to_string(),
        user_id: "u1".to_string(),
        session_id: session.to_string(),
        file_name: name.to_string(),
    }
}

async fn text(service: &dyn ArtifactService, session: &str, name: &str) -> String {
    match service.load(load(session, name, None)).await.unwrap().part {
        Part::Text { text } => text,
        other => panic!("expected text, got {other:?}"),
    }
}

/// The local backends, each with fresh storage. The temp dir must outlive the service.
fn backends() -> Vec<(&'static str, Arc<dyn ArtifactService>, Option<tempfile::TempDir>)> {
    let dir = tempfile::tempdir().unwrap();
    vec![
        ("memory", Arc::new(InMemoryArtifactService::new()), None),
        ("file", Arc::new(FileArtifactService::new(dir.path()).unwrap()), Some(dir)),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_saves_each_store_a_distinct_version() {
    for (label, service, _dir) in backends() {
        for round in 0..20 {
            let name = format!("race-{round}.txt");
            let writers = 16;
            let barrier = Arc::new(tokio::sync::Barrier::new(writers));
            let tasks: Vec<_> = (0..writers)
                .map(|writer| {
                    let (service, barrier, name) = (service.clone(), barrier.clone(), name.clone());
                    tokio::spawn(async move {
                        barrier.wait().await;
                        service.save(save("s1", &name, &format!("w{writer}"), None)).await
                    })
                })
                .collect();
            let mut returned = BTreeSet::new();
            for task in tasks {
                returned.insert(task.await.unwrap().unwrap().version);
            }
            let stored: BTreeSet<i64> = service
                .versions(versions("s1", &name))
                .await
                .unwrap()
                .versions
                .into_iter()
                .collect();
            assert_eq!(returned.len(), writers, "{label}: duplicate versions {returned:?}");
            assert_eq!(stored, returned, "{label}: stored versions differ from returned ones");
        }
    }
}

#[tokio::test]
async fn an_explicit_existing_version_is_never_replaced() {
    for (label, service, _dir) in backends() {
        let first = service.save(save("s1", "doc.txt", "original", None)).await.unwrap();
        let error = service
            .save(save("s1", "doc.txt", "replacement", Some(first.version)))
            .await
            .unwrap_err();
        assert_eq!(error.category, ErrorCategory::Conflict, "{label}: {error}");
        assert_eq!(text(service.as_ref(), "s1", "doc.txt").await, "original", "{label}");

        // A free explicit version is written as asked.
        let explicit = service.save(save("s1", "doc.txt", "seven", Some(7))).await.unwrap();
        assert_eq!(explicit.version, 7, "{label}");
        let next = service.save(save("s1", "doc.txt", "eight", None)).await.unwrap();
        assert_eq!(next.version, 8, "{label}");
    }
}

#[tokio::test]
async fn the_user_scope_session_id_is_rejected() {
    for (label, service, _dir) in backends() {
        let rejected = [
            service.save(save("user", "draft.txt", "x", None)).await.map(|_| ()),
            service.load(load("user", "draft.txt", None)).await.map(|_| ()),
            service.list(list("user")).await.map(|_| ()),
            service.versions(versions("user", "draft.txt")).await.map(|_| ()),
            service
                .delete(DeleteRequest {
                    app_name: "app".to_string(),
                    user_id: "u1".to_string(),
                    session_id: "user".to_string(),
                    file_name: "draft.txt".to_string(),
                    version: None,
                })
                .await,
        ];
        for result in rejected {
            let error = result.unwrap_err();
            assert_eq!(error.category, ErrorCategory::InvalidInput, "{label}: {error}");
            assert_eq!(error.code, "artifact.reserved_session_id", "{label}");
        }
        assert!(service.list(list("s2")).await.unwrap().file_names.is_empty(), "{label}");
    }
}

#[tokio::test]
async fn names_that_differ_only_in_colons_and_underscores_are_distinct() {
    for (label, service, _dir) in backends() {
        let names = ["a:b", "a__b", "a_b", "a___b", "user:x", "user__x", "50%:off"];
        for name in names {
            service.save(save("s1", name, name, None)).await.unwrap();
        }
        for name in names {
            assert_eq!(text(service.as_ref(), "s1", name).await, name, "{label}");
            let stored = service.versions(versions("s1", name)).await.unwrap().versions;
            assert_eq!(stored, vec![1], "{label}: {name}");
        }
        let listed: BTreeSet<String> =
            service.list(list("s1")).await.unwrap().file_names.into_iter().collect();
        assert_eq!(listed, names.iter().map(ToString::to_string).collect(), "{label}");
    }
}
