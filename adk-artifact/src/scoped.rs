use crate::service::{ArtifactService, ListRequest, LoadRequest, SaveRequest};
use adk_core::{Artifacts, Part, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Scoped wrapper around ArtifactService that binds session context.
/// 
/// This wrapper implements the simple `adk_core::Artifacts` trait by automatically
/// injecting app_name, user_id, and session_id into service requests. This mirrors
/// the adk-go architecture where agents use a simple API but service calls include
/// full session scoping.
///
/// # Example
///
/// ```no_run
/// use adk_artifact::{ScopedArtifacts, InMemoryArtifactService};
/// use adk_core::{Artifacts, Part};
/// use std::sync::Arc;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let service = Arc::new(InMemoryArtifactService::new());
/// let artifacts = ScopedArtifacts::new(
///     service,
///     "my_app".to_string(),
///     "user_123".to_string(),
///     "session_456".to_string(),
/// );
///
/// // Simple API - scoping is automatic
/// let version = artifacts.save("report.pdf", &Part::text("data")).await?;
/// let loaded = artifacts.load("report.pdf").await?;
/// let files = artifacts.list().await?;
/// # Ok(())
/// # }
/// ```
pub struct ScopedArtifacts {
    service: Arc<dyn ArtifactService>,
    app_name: String,
    user_id: String,
    session_id: String,
}

impl ScopedArtifacts {
    /// Creates a new scoped artifacts instance.
    ///
    /// # Arguments
    ///
    /// * `service` - The underlying artifact service
    /// * `app_name` - Application name for scoping
    /// * `user_id` - User ID for scoping
    /// * `session_id` - Session ID for scoping
    pub fn new(
        service: Arc<dyn ArtifactService>,
        app_name: String,
        user_id: String,
        session_id: String,
    ) -> Self {
        Self {
            service,
            app_name,
            user_id,
            session_id,
        }
    }
}

#[async_trait]
impl Artifacts for ScopedArtifacts {
    async fn save(&self, name: &str, data: &Part) -> Result<i64> {
        let resp = self
            .service
            .save(SaveRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
                file_name: name.to_string(),
                part: data.clone(),
                version: None,
            })
            .await?;
        Ok(resp.version)
    }

    async fn load(&self, name: &str) -> Result<Part> {
        let resp = self
            .service
            .load(LoadRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
                file_name: name.to_string(),
                version: None,
            })
            .await?;
        Ok(resp.part)
    }

    async fn list(&self) -> Result<Vec<String>> {
        let resp = self
            .service
            .list(ListRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
            })
            .await?;
        Ok(resp.file_names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryArtifactService;

    #[tokio::test]
    async fn test_scoped_artifacts_session_isolation() {
        let service = Arc::new(InMemoryArtifactService::new());

        let sess1 = ScopedArtifacts::new(
            service.clone(),
            "app".to_string(),
            "user".to_string(),
            "sess1".to_string(),
        );
        let sess2 = ScopedArtifacts::new(
            service.clone(),
            "app".to_string(),
            "user".to_string(),
            "sess2".to_string(),
        );

        // Save different data to same filename in different sessions
        sess1
            .save("file.txt", &Part::Text {
                text: "session 1 data".to_string(),
            })
            .await
            .unwrap();
        sess2
            .save("file.txt", &Part::Text {
                text: "session 2 data".to_string(),
            })
            .await
            .unwrap();

        // Load from each session - should get isolated data
        let loaded1 = sess1.load("file.txt").await.unwrap();
        let loaded2 = sess2.load("file.txt").await.unwrap();

        match (loaded1, loaded2) {
            (Part::Text { text: text1 }, Part::Text { text: text2 }) => {
                assert_eq!(text1, "session 1 data");
                assert_eq!(text2, "session 2 data");
            }
            _ => panic!("Expected Text parts"),
        }
    }

    #[tokio::test]
    async fn test_scoped_artifacts_list_isolation() {
        let service = Arc::new(InMemoryArtifactService::new());

        let sess1 = ScopedArtifacts::new(
            service.clone(),
            "app".to_string(),
            "user".to_string(),
            "sess1".to_string(),
        );
        let sess2 = ScopedArtifacts::new(
            service.clone(),
            "app".to_string(),
            "user".to_string(),
            "sess2".to_string(),
        );

        // Save files in different sessions
        sess1.save("file1.txt", &Part::Text { text: "data1".to_string() }).await.unwrap();
        sess2.save("file2.txt", &Part::Text { text: "data2".to_string() }).await.unwrap();

        // List should only show session-specific files
        let files1 = sess1.list().await.unwrap();
        let files2 = sess2.list().await.unwrap();

        assert_eq!(files1, vec!["file1.txt"]);
        assert_eq!(files2, vec!["file2.txt"]);
    }

    #[tokio::test]
    async fn test_scoped_artifacts_user_prefix() {
        let service = Arc::new(InMemoryArtifactService::new());

        let sess1 = ScopedArtifacts::new(
            service.clone(),
            "app".to_string(),
            "user1".to_string(),
            "sess1".to_string(),
        );
        let sess2 = ScopedArtifacts::new(
            service.clone(),
            "app".to_string(),
            "user1".to_string(),
            "sess2".to_string(),
        );

        // Save user-scoped artifact (with "user:" prefix)
        sess1.save("user:shared.txt", &Part::Text { text: "shared data".to_string() }).await.unwrap();

        // Should be accessible from both sessions (user-scoped)
        let loaded1 = sess1.load("user:shared.txt").await.unwrap();
        let loaded2 = sess2.load("user:shared.txt").await.unwrap();

        match (loaded1, loaded2) {
            (Part::Text { text: text1 }, Part::Text { text: text2 }) => {
                assert_eq!(text1, "shared data");
                assert_eq!(text2, "shared data");
            }
            _ => panic!("Expected Text parts"),
        }
    }
}
