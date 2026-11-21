use adk_core::{Content, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::time::Duration;

/// One stored memory: what was said or learned, who said it, and when.
///
/// | Field | Set by | Meaning |
/// |-------|--------|---------|
/// | `id` | [`with_id`](Self::with_id), or the backend when it stores the entry | The handle [`MemoryService::delete_memory`] takes; every entry a backend returns carries one |
/// | `expires_at` | [`with_ttl`](Self::with_ttl), [`with_expires_at`](Self::with_expires_at), or the service's default TTL | When the entry stops being recalled; `None` keeps it until it is deleted |
///
/// [`crate::expiry`] states the expiry rule every backend applies.
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_memory::MemoryEntry;
/// use std::time::Duration;
///
/// let entry = MemoryEntry::new(Content::new("user").with_text("Parking: level 3, bay 12."), "user")
///     .with_id("parking-today")
///     .with_ttl(Duration::from_secs(8 * 60 * 60));
/// assert_eq!(entry.id.as_deref(), Some("parking-today"));
/// assert!(!entry.is_expired_at(chrono::Utc::now()));
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MemoryEntry {
    /// The entry's id within its user's memory; `None` until a backend stores it, unless set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What the memory holds.
    pub content: Content,
    /// Who the memory came from.
    pub author: String,
    /// When the memory was recorded.
    pub timestamp: DateTime<Utc>,
    /// When the memory expires; `None` takes the service's default TTL, or never expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl MemoryEntry {
    /// An entry of `content` by `author`, recorded now, without an id or an expiry.
    pub fn new(content: Content, author: impl Into<String>) -> Self {
        Self { id: None, content, author: author.into(), timestamp: Utc::now(), expires_at: None }
    }

    /// Sets when the memory was recorded.
    pub fn with_timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Sets the entry's id, so a caller can delete it later without searching for it.
    ///
    /// The id should be unique within the user's memory:
    /// [`delete_memory`](MemoryService::delete_memory) removes every entry carrying it.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Expires the entry at `expires_at`, overriding the service's default TTL.
    pub fn with_expires_at(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(crate::expiry::to_millis(expires_at));
        self
    }

    /// Expires the entry `ttl` from now, overriding the service's default TTL.
    pub fn with_ttl(self, ttl: Duration) -> Self {
        let expires_at = crate::expiry::expiry_after(Utc::now(), ttl);
        self.with_expires_at(expires_at)
    }

    /// Adds a reference to the artifact at `uri` — an image, a frame, a document — as a
    /// `Part::FileData` after the entry's text. The text stays what a search matches; the
    /// reference comes back with the entry.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Content;
    /// use adk_memory::MemoryEntry;
    /// use adk_memory::text::{ArtifactReference, extract_text};
    ///
    /// let entry = MemoryEntry::new(Content::new("model").with_text("torn seal, carton 117"), "inspector")
    ///     .with_artifact("artifact://inspector/u1/s1/vision-ab12.png#0", "image/png");
    /// assert_eq!(extract_text(&entry.content), "torn seal, carton 117");
    /// assert_eq!(
    ///     entry.artifact_references(),
    ///     vec![ArtifactReference::new("artifact://inspector/u1/s1/vision-ab12.png#0", "image/png")]
    /// );
    /// ```
    pub fn with_artifact(mut self, uri: impl Into<String>, mime_type: impl Into<String>) -> Self {
        self.content.parts.push(crate::text::ArtifactReference::new(uri, mime_type).to_part());
        self
    }

    /// The artifacts the entry refers to, by [`crate::text::artifact_references`].
    pub fn artifact_references(&self) -> Vec<crate::text::ArtifactReference> {
        crate::text::artifact_references(&self.content)
    }

    /// Whether the entry has expired at `now`, by [`crate::expiry::is_expired`].
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        crate::expiry::is_expired(self.expires_at, now)
    }
}

/// One stored memory of a user, as [`MemoryService::export_user`] returns it.
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_memory::{MemoryEntry, UserMemory};
///
/// let memory = UserMemory {
///     entry: MemoryEntry::new(Content::new("user").with_text("Hometown: Nairobi."), "user")
///         .with_id("m1"),
///     session_id: Some("s1".to_string()),
///     project_id: None,
/// };
/// let json = serde_json::to_value(&memory).unwrap();
/// assert_eq!(json["sessionId"], "s1");
/// assert_eq!(json["author"], "user");
/// assert_eq!(json["id"], "m1");
/// ```
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMemory {
    /// The entry.
    #[serde(flatten)]
    pub entry: MemoryEntry,
    /// The session the entry was added from; `None` for an entry added directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The project the entry is scoped to; `None` for a global entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub user_id: String,
    pub app_name: String,
    /// Maximum number of results to return. `None` defaults to 10.
    pub limit: Option<usize>,
    /// Minimum similarity score threshold (0.0–1.0). Results below this
    /// score are excluded. `None` means no threshold.
    pub min_score: Option<f32>,
    /// Optional project scope. `None` returns only global entries.
    /// `Some(id)` returns global entries + entries for that project.
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub memories: Vec<MemoryEntry>,
}

/// Validate a project identifier.
///
/// Returns `Ok(())` if the project_id is non-empty and at most 256 characters.
/// Returns a descriptive error otherwise.
pub fn validate_project_id(project_id: &str) -> Result<()> {
    if project_id.is_empty() {
        return Err(adk_core::AdkError::memory("project_id must not be empty"));
    }
    if project_id.len() > 256 {
        return Err(adk_core::AdkError::memory(format!(
            "project_id exceeds maximum length of 256 characters (got {})",
            project_id.len()
        )));
    }
    Ok(())
}

#[async_trait]
pub trait MemoryService: Send + Sync {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()>;
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;

    /// Delete all memory entries for a specific user.
    ///
    /// Removes every stored memory of the user in the app, embeddings and
    /// project-scoped entries included.
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let _ = (app_name, user_id);
        Err(adk_core::AdkError::memory("delete_user not implemented"))
    }

    /// Returns every memory held for `user_id` in `app_name` — added from a session or
    /// directly, global or project-scoped — oldest first. [`delete_user`](Self::delete_user)
    /// removes exactly these.
    ///
    /// [`MemorySubjectData`](crate::MemorySubjectData) exports a data subject's memory
    /// through it.
    ///
    /// # Errors
    ///
    /// The default returns `subject.unsupported`
    /// ([`SUBJECT_UNSUPPORTED_CODE`](adk_core::subject::SUBJECT_UNSUPPORTED_CODE)), so a
    /// backend that cannot list a user's entries is reported, not silently empty.
    async fn export_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserMemory>> {
        let _ = (app_name, user_id);
        Err(adk_core::subject::unsupported_error("memory", "export"))
    }

    /// Delete all memory entries for a specific session.
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        let _ = (app_name, user_id, session_id);
        Err(adk_core::AdkError::memory("delete_session not implemented"))
    }

    /// Add a single memory entry directly (not tied to a session).
    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()> {
        let _ = (app_name, user_id, entry);
        Err(adk_core::AdkError::memory("add_entry not implemented"))
    }

    /// Delete entries matching a query. Returns count of deleted entries.
    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64> {
        let _ = (app_name, user_id, query);
        Err(adk_core::AdkError::memory("delete_entries not implemented"))
    }

    /// List the most recent entries for a user (global + project-scoped),
    /// newest first. Unlike `search`, this is a pure recency listing — no
    /// query, no similarity ranking — suitable for "what does the agent
    /// remember?" UIs and latest-item lookups.
    async fn list_recent(
        &self,
        app_name: &str,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let _ = (app_name, user_id, limit);
        Err(adk_core::AdkError::memory("list_recent not implemented"))
    }

    /// Deletes the memory `memory_id` of `user_id` in `app_name`, global or in any project,
    /// and returns how many entries it removed: 0 when the user holds none with that id.
    ///
    /// `memory_id` is the [`MemoryEntry::id`] a search, listing or export returned, or the id
    /// the caller set with [`MemoryEntry::with_id`]. Another user's entry with the same id is
    /// never touched.
    ///
    /// # Errors
    ///
    /// The default returns an error, so a backend that cannot delete one entry is reported
    /// rather than silently keeping it.
    async fn delete_memory(&self, app_name: &str, user_id: &str, memory_id: &str) -> Result<u64> {
        let _ = (app_name, user_id, memory_id);
        Err(adk_core::AdkError::memory(
            "delete_memory is not implemented by this memory backend; every adk-memory backend \
             implements it, or remove the user's entries with delete_user",
        ))
    }

    /// Removes every stored entry whose expiry has passed, across all apps and users, and
    /// returns how many it removed.
    ///
    /// Reads never return an expired entry, so a purge reclaims storage without changing what
    /// a search recalls. A backend whose store expires data natively (Redis key expiry, the
    /// MongoDB TTL index, Memory Bank) also removes entries on the store's own schedule; the
    /// count covers only what this call removed. [`crate::expiry::spawn_expiry_sweep`] runs
    /// it on an interval.
    ///
    /// # Errors
    ///
    /// The default returns an error, so a backend that cannot purge is reported rather than
    /// silently keeping expired entries.
    async fn purge_expired(&self) -> Result<u64> {
        Err(adk_core::AdkError::memory(
            "purge_expired is not implemented by this memory backend; every adk-memory backend \
             implements it",
        ))
    }

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// Whether this backend keeps project-scoped entries isolated.
    ///
    /// Returns `false` by default. A backend that implements the project methods
    /// below overrides this to `true`, so a caller can tell isolation apart from a
    /// backend that has no project support rather than discovering it from data.
    fn supports_project_scoping(&self) -> bool {
        false
    }

    /// Adds session entries scoped to a project.
    ///
    /// # Errors
    ///
    /// The default implementation returns an error. Discarding `project_id` and
    /// writing globally would make entries intended for one project visible to
    /// everything else under the same app and user, with nothing in the return value
    /// to say so, so a backend without project support refuses the write instead.
    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        let _ = (app_name, user_id, session_id, project_id, entries);
        Err(project_scoping_unsupported("add_session_to_project"))
    }

    /// Adds a single entry scoped to a project.
    ///
    /// # Errors
    ///
    /// Returns an error by default, for the reason given on
    /// [`MemoryService::add_session_to_project`].
    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        let _ = (app_name, user_id, project_id, entry);
        Err(project_scoping_unsupported("add_entry_to_project"))
    }

    /// Deletes entries matching a query within a specific project.
    ///
    /// # Errors
    ///
    /// Returns an error by default. Falling back to a global delete would remove
    /// entries outside the named project, which is worse than refusing.
    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        let _ = (app_name, user_id, project_id, query);
        Err(project_scoping_unsupported("delete_entries_in_project"))
    }

    /// Delete all entries for a specific project.
    /// Default returns "not implemented" error.
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        let _ = (app_name, user_id, project_id);
        Err(adk_core::AdkError::memory("delete_project not implemented"))
    }
}

/// The error a backend without project support returns from a project method.
fn project_scoping_unsupported(method: &str) -> adk_core::AdkError {
    adk_core::AdkError::memory(format!(
        "this memory backend does not implement project scoping, so `{method}` cannot honour \
         the project boundary; use a backend whose `supports_project_scoping` returns true, or \
         call the global method explicitly if global scope is intended"
    ))
}
