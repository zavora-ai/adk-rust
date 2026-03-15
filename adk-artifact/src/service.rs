use adk_core::{Part, Result};
use async_trait::async_trait;

/// Request to save a binary artifact.
#[derive(Debug, Clone)]
pub struct SaveRequest {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session the artifact belongs to.
    pub session_id: String,
    /// Artifact filename (validated against path traversal).
    pub file_name: String,
    /// The binary content to store.
    pub part: Part,
    /// Explicit version to write, or `None` for auto-increment.
    pub version: Option<i64>,
}

/// Response from a successful save operation.
#[derive(Debug, Clone)]
pub struct SaveResponse {
    /// The version number that was written.
    pub version: i64,
}

/// Request to load an artifact.
#[derive(Debug, Clone)]
pub struct LoadRequest {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session the artifact belongs to.
    pub session_id: String,
    /// Artifact filename.
    pub file_name: String,
    /// Specific version to load, or `None` for the latest.
    pub version: Option<i64>,
}

/// Response from a successful load operation.
#[derive(Debug, Clone)]
pub struct LoadResponse {
    /// The binary content that was stored.
    pub part: Part,
}

/// Request to delete an artifact.
#[derive(Debug, Clone)]
pub struct DeleteRequest {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session the artifact belongs to.
    pub session_id: String,
    /// Artifact filename.
    pub file_name: String,
    /// Specific version to delete, or `None` to delete all versions.
    pub version: Option<i64>,
}

/// Request to list artifacts in a session.
#[derive(Debug, Clone)]
pub struct ListRequest {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session to list artifacts from.
    pub session_id: String,
}

/// Response containing artifact filenames.
#[derive(Debug, Clone)]
pub struct ListResponse {
    /// Filenames of all artifacts in the session.
    pub file_names: Vec<String>,
}

/// Request to list versions of a specific artifact.
#[derive(Debug, Clone)]
pub struct VersionsRequest {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session the artifact belongs to.
    pub session_id: String,
    /// Artifact filename.
    pub file_name: String,
}

/// Response containing available version numbers.
#[derive(Debug, Clone)]
pub struct VersionsResponse {
    /// Version numbers in ascending order.
    pub versions: Vec<i64>,
}

/// Trait for artifact storage backends.
///
/// Implementations must be thread-safe (`Send + Sync`) and support
/// versioned binary storage scoped by app, user, and session.
#[async_trait]
pub trait ArtifactService: Send + Sync {
    /// Store a binary artifact, returning the version that was written.
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse>;
    /// Retrieve a stored artifact by filename and optional version.
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse>;
    /// Delete an artifact (specific version or all versions).
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    /// List all artifact filenames in a session.
    async fn list(&self, req: ListRequest) -> Result<ListResponse>;
    /// List all available versions of a specific artifact.
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse>;

    /// Verify backend connectivity.
    ///
    /// The default implementation succeeds, which is appropriate for
    /// in-memory backends.
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
