use adk_artifact::*;
use adk_core::Part;

#[tokio::test]
async fn test_save_and_load() {
    let service = InMemoryArtifactService::new();

    let save_req = SaveRequest {
        app_name: "app1".to_string(),
        user_id: "user1".to_string(),
        session_id: "session1".to_string(),
        file_name: "test.txt".to_string(),
        part: Part::Text { text: "Hello World".to_string() },
        version: None,
    };

    let save_resp = service.save(save_req).await.unwrap();
    assert_eq!(save_resp.version, 1);

    let load_req = LoadRequest {
        app_name: "app1".to_string(),
        user_id: "user1".to_string(),
        session_id: "session1".to_string(),
        file_name: "test.txt".to_string(),
        version: None,
    };

    let load_resp = service.load(load_req).await.unwrap();
    assert_eq!(load_resp.part, Part::Text { text: "Hello World".to_string() });
}

#[tokio::test]
async fn test_versioning() {
    let service = InMemoryArtifactService::new();

    // Save version 1
    service
        .save(SaveRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            part: Part::Text { text: "v1".to_string() },
            version: None,
        })
        .await
        .unwrap();

    // Save version 2
    service
        .save(SaveRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            part: Part::Text { text: "v2".to_string() },
            version: None,
        })
        .await
        .unwrap();

    // Load latest (v2)
    let load_resp = service
        .load(LoadRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            version: None,
        })
        .await
        .unwrap();
    assert_eq!(load_resp.part, Part::Text { text: "v2".to_string() });

    // Load v1
    let load_resp = service
        .load(LoadRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            version: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(load_resp.part, Part::Text { text: "v1".to_string() });

    // List versions
    let versions_resp = service
        .versions(VersionsRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(versions_resp.versions, vec![2, 1]);
}

#[tokio::test]
async fn test_list() {
    let service = InMemoryArtifactService::new();

    service
        .save(SaveRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "file1.txt".to_string(),
            part: Part::Text { text: "content1".to_string() },
            version: None,
        })
        .await
        .unwrap();

    service
        .save(SaveRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "file2.txt".to_string(),
            part: Part::Text { text: "content2".to_string() },
            version: None,
        })
        .await
        .unwrap();

    let list_resp = service
        .list(ListRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(list_resp.file_names, vec!["file1.txt", "file2.txt"]);
}

#[tokio::test]
async fn test_delete() {
    let service = InMemoryArtifactService::new();

    service
        .save(SaveRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            part: Part::Text { text: "content".to_string() },
            version: None,
        })
        .await
        .unwrap();

    service
        .delete(DeleteRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            version: None,
        })
        .await
        .unwrap();

    let load_result = service
        .load(LoadRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "test.txt".to_string(),
            version: None,
        })
        .await;

    assert!(load_result.is_err());
}

#[tokio::test]
async fn test_user_scoped_artifacts() {
    let service = InMemoryArtifactService::new();

    // Save user-scoped artifact
    service
        .save(SaveRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            file_name: "user:profile.txt".to_string(),
            part: Part::Text { text: "user data".to_string() },
            version: None,
        })
        .await
        .unwrap();

    // Should be accessible from different session
    let load_resp = service
        .load(LoadRequest {
            app_name: "app1".to_string(),
            user_id: "user1".to_string(),
            session_id: "session2".to_string(),
            file_name: "user:profile.txt".to_string(),
            version: None,
        })
        .await
        .unwrap();

    assert_eq!(load_resp.part, Part::Text { text: "user data".to_string() });
}

#[tokio::test]
async fn test_reject_invalid_artifact_file_names() {
    let service = InMemoryArtifactService::new();
    let invalid_names = ["../secret", "a/b.txt", r"a\b.txt"];

    for file_name in invalid_names {
        let save_result = service
            .save(SaveRequest {
                app_name: "app1".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
                file_name: file_name.to_string(),
                part: Part::Text { text: "blocked".to_string() },
                version: None,
            })
            .await;
        assert!(save_result.is_err(), "save should reject '{}'", file_name);

        let load_result = service
            .load(LoadRequest {
                app_name: "app1".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
                file_name: file_name.to_string(),
                version: None,
            })
            .await;
        assert!(load_result.is_err(), "load should reject '{}'", file_name);
    }
}
