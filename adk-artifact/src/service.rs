use adk_core::{AdkError, ErrorCategory, ErrorComponent, Part, Result};
use async_trait::async_trait;

/// The session segment that user-scoped (`user:`) artifacts are stored under.
///
/// Every backend rejects it as a session id, so a session's artifacts never mix with the
/// artifacts every session of the user shares.
pub const USER_SCOPE_SESSION_ID: &str = "user";

/// Rejects the session id reserved for the user scope.
pub(crate) fn check_session_id(session_id: &str) -> Result<()> {
    if session_id == USER_SCOPE_SESSION_ID {
        return Err(AdkError::new(
            ErrorComponent::Artifact,
            ErrorCategory::InvalidInput,
            "artifact.reserved_session_id",
            format!(
                "invalid artifact session id '{USER_SCOPE_SESSION_ID}': it names the scope that \
                 `user:` artifacts share across sessions; use another session id"
            ),
        ));
    }
    Ok(())
}

/// The error for a load or version listing of an artifact, or a version of one, that is
/// not stored. Every backend returns it, so `AdkError::is_not_found` holds for each.
pub(crate) fn artifact_not_found(file_name: &str, version: Option<i64>) -> AdkError {
    let message = match version {
        Some(version) => format!(
            "artifact '{file_name}' has no version {version}; list its versions to see which exist"
        ),
        None => format!("artifact '{file_name}' was not found; save it before loading it"),
    };
    AdkError::not_found(ErrorComponent::Artifact, "artifact.not_found", message)
}

/// The error for an artifact file name the backend cannot store, such as one with a path
/// separator or a `..` traversal.
pub(crate) fn invalid_file_name(file_name: &str, rule: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Artifact,
        ErrorCategory::InvalidInput,
        "artifact.invalid_name",
        format!("invalid artifact file name '{file_name}': {rule}"),
    )
}

/// The error for an app name, user ID, or session ID that cannot address artifact storage.
pub(crate) fn invalid_identifier(field: &str, value: &str, rule: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Artifact,
        ErrorCategory::InvalidInput,
        "artifact.invalid_identifier",
        format!("invalid artifact {field} '{value}': {rule}"),
    )
}

/// The error for a save naming a version that already exists.
pub(crate) fn version_exists(file_name: &str, version: i64) -> AdkError {
    AdkError::new(
        ErrorComponent::Artifact,
        ErrorCategory::Conflict,
        "artifact.version_exists",
        format!(
            "artifact '{file_name}' already has version {version}; versions are never \
             replaced, so save without a version to append a new one"
        ),
    )
}

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
    /// Explicit version to write, or `None` to append the next version.
    ///
    /// A version that already exists is never replaced: the save fails with
    /// [`ErrorCategory::Conflict`].
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

/// One stored version of one of a user's artifacts, as
/// [`ArtifactService::list_user`] returns it.
///
/// # Example
///
/// ```rust
/// use adk_artifact::UserArtifact;
///
/// let shared = UserArtifact {
///     session_id: None,
///     file_name: "user:avatar.png".to_string(),
///     version: 1,
///     mime_type: Some("image/png".to_string()),
///     size: 2048,
/// };
/// assert!(shared.session_id.is_none(), "user: artifacts belong to no session");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserArtifact {
    /// The session the artifact belongs to; `None` for a `user:` artifact every session
    /// of the user shares.
    pub session_id: Option<String>,
    /// The artifact's file name.
    pub file_name: String,
    /// The version.
    pub version: i64,
    /// The stored part's MIME type: `text/plain` for text, the declared type for inline
    /// data and file references, `None` for any other part.
    pub mime_type: Option<String>,
    /// The stored content's size in bytes: the text or inline data, 0 for a file
    /// reference, and the JSON form for any other part.
    pub size: u64,
}

/// The MIME type and byte size [`UserArtifact`] reports for a stored part.
pub(crate) fn part_summary(part: &Part) -> (Option<String>, u64) {
    match part {
        Part::Text { text } => (Some("text/plain".to_string()), text.len() as u64),
        Part::InlineData { mime_type, data, .. } => (Some(mime_type.clone()), data.len() as u64),
        Part::FileData { mime_type, .. } => (Some(mime_type.clone()), 0),
        Part::Thinking { .. }
        | Part::FunctionCall { .. }
        | Part::FunctionResponse { .. }
        | Part::ServerToolCall { .. }
        | Part::ServerToolResponse { .. }
        | Part::EmbeddedResource { .. } => {
            (None, serde_json::to_vec(part).map_or(0, |json| json.len() as u64))
        }
    }
}

/// Trait for artifact storage backends.
///
/// Implementations must be thread-safe (`Send + Sync`) and support
/// versioned binary storage scoped by app, user, and session. Every backend in
/// this crate keeps the same contract:
///
/// - **Versions are immutable.** A save never replaces a stored version.
/// - **Concurrent saves each get their own version.** Saves of one artifact without
///   an explicit version that run at the same time return distinct versions, and
///   every one of them is stored.
/// - **Distinct names are distinct artifacts.** No two file names share storage.
/// - **The session id [`USER_SCOPE_SESSION_ID`] is reserved.** Every operation
///   rejects it with [`ErrorCategory::InvalidInput`].
#[async_trait]
pub trait ArtifactService: Send + Sync {
    /// Store a binary artifact, returning the version that was written.
    ///
    /// # Errors
    ///
    /// Returns a [`Conflict`](ErrorCategory::Conflict) error when
    /// `req.version` names a version that already exists, and an
    /// [`InvalidInput`](ErrorCategory::InvalidInput) error for an invalid
    /// name or the reserved session id.
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse>;
    /// Retrieve a stored artifact by filename and optional version.
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse>;
    /// Delete an artifact (specific version or all versions).
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    /// List all artifact filenames in a session.
    async fn list(&self, req: ListRequest) -> Result<ListResponse>;
    /// List all available versions of a specific artifact.
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse>;

    /// Lists every stored version of every artifact of `user_id` in `app_name` — in
    /// every session and in the `user:` scope — ordered by session, name, and version.
    ///
    /// [`ArtifactSubjectData`](crate::ArtifactSubjectData) exports a data subject's
    /// artifacts through it.
    ///
    /// # Errors
    ///
    /// The default returns `subject.unsupported`
    /// ([`SUBJECT_UNSUPPORTED_CODE`](adk_core::subject::SUBJECT_UNSUPPORTED_CODE)).
    async fn list_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserArtifact>> {
        let _ = (app_name, user_id);
        Err(adk_core::subject::unsupported_error("artifacts", "list"))
    }

    /// Deletes every version of every artifact of `user_id` in `app_name`, in every
    /// session and in the `user:` scope, and returns how many versions it deleted.
    ///
    /// # Errors
    ///
    /// The default returns `subject.unsupported`
    /// ([`SUBJECT_UNSUPPORTED_CODE`](adk_core::subject::SUBJECT_UNSUPPORTED_CODE)).
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<u64> {
        let _ = (app_name, user_id);
        Err(adk_core::subject::unsupported_error("artifacts", "delete"))
    }

    /// Verify backend connectivity.
    ///
    /// The default implementation succeeds, which is appropriate for
    /// in-memory backends.
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
