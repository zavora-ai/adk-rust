//! Spanner toolset structure and `Toolset` trait implementation.

use crate::spanner::tools::{SpannerExecuteSql, SpannerGetTableSchema, SpannerListTables};
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use std::sync::Arc;

/// How Spanner credentials are resolved at runtime.
#[derive(Debug, Clone)]
pub enum CredentialSource {
    /// Use Google Cloud Application Default Credentials.
    ApplicationDefault,
    /// A secret name resolved via `ToolContext::get_secret()` at execution time.
    SecretRef(String),
}

/// Native Spanner toolset providing SQL execution, schema inspection, and
/// table listing tools.
///
/// Authenticates using Google Cloud Application Default Credentials or a
/// service account key provided via the configured
/// [`SecretProvider`](adk_core::ToolContext::get_secret).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::spanner::SpannerToolset;
///
/// // Application Default Credentials
/// let toolset = SpannerToolset::new("my-project", "my-instance", "my-database");
///
/// // Via secret provider (resolved at tool execution time)
/// let toolset = SpannerToolset::from_secret(
///     "my-project",
///     "my-instance",
///     "my-database",
///     "spanner-credentials",
/// );
/// ```
pub struct SpannerToolset {
    pub(crate) project_id: String,
    pub(crate) instance_id: String,
    pub(crate) database_id: String,
    pub(crate) credentials: CredentialSource,
}

impl SpannerToolset {
    /// Create a new `SpannerToolset` using Application Default Credentials.
    ///
    /// Requires a project ID, instance ID, and database ID to identify the
    /// target Spanner database.
    pub fn new(
        project_id: impl Into<String>,
        instance_id: impl Into<String>,
        database_id: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            instance_id: instance_id.into(),
            database_id: database_id.into(),
            credentials: CredentialSource::ApplicationDefault,
        }
    }

    /// Create a new `SpannerToolset` that resolves credentials from the
    /// secret provider at execution time via `ctx.get_secret(secret_name)`.
    pub fn from_secret(
        project_id: impl Into<String>,
        instance_id: impl Into<String>,
        database_id: impl Into<String>,
        secret_name: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            instance_id: instance_id.into(),
            database_id: database_id.into(),
            credentials: CredentialSource::SecretRef(secret_name.into()),
        }
    }
}

#[async_trait]
impl Toolset for SpannerToolset {
    fn name(&self) -> &str {
        "spanner"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let project_id = self.project_id.clone();
        let instance_id = self.instance_id.clone();
        let database_id = self.database_id.clone();
        let credentials = self.credentials.clone();

        Ok(vec![
            Arc::new(SpannerExecuteSql::new(
                project_id.clone(),
                instance_id.clone(),
                database_id.clone(),
                credentials.clone(),
            )),
            Arc::new(SpannerGetTableSchema::new(
                project_id.clone(),
                instance_id.clone(),
                database_id.clone(),
                credentials.clone(),
            )),
            Arc::new(SpannerListTables::new(project_id, instance_id, database_id, credentials)),
        ])
    }
}
