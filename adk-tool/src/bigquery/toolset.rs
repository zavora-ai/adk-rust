//! BigQuery toolset structure and `Toolset` trait implementation.

use crate::bigquery::tools::{
    BigQueryExecuteSql, BigQueryGetTableSchema, BigQueryListDatasets, BigQueryListTables,
};
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use std::sync::Arc;

/// How BigQuery credentials are resolved at runtime.
#[derive(Debug, Clone)]
pub enum CredentialSource {
    /// Use Google Cloud Application Default Credentials.
    ApplicationDefault,
    /// A secret name resolved via `ToolContext::get_secret()` at execution time.
    SecretRef(String),
}

/// Native BigQuery toolset providing SQL execution, schema inspection, and
/// dataset/table listing tools.
///
/// Authenticates using Google Cloud Application Default Credentials or a
/// service account key provided via the configured
/// [`SecretProvider`](adk_core::ToolContext::get_secret).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::bigquery::BigQueryToolset;
///
/// // Application Default Credentials
/// let toolset = BigQueryToolset::new();
///
/// // With explicit project ID
/// let toolset = BigQueryToolset::with_project("my-gcp-project");
///
/// // Via secret provider (resolved at tool execution time)
/// let toolset = BigQueryToolset::from_secret("bigquery-credentials");
/// ```
pub struct BigQueryToolset {
    pub(crate) project_id: Option<String>,
    pub(crate) credentials: CredentialSource,
}

impl BigQueryToolset {
    /// Create a new `BigQueryToolset` using Application Default Credentials.
    ///
    /// The project ID will be inferred from the environment or must be
    /// specified in each query.
    pub fn new() -> Self {
        Self { project_id: None, credentials: CredentialSource::ApplicationDefault }
    }

    /// Create a new `BigQueryToolset` with an explicit Google Cloud project ID.
    ///
    /// Uses Application Default Credentials for authentication.
    pub fn with_project(project_id: impl Into<String>) -> Self {
        Self {
            project_id: Some(project_id.into()),
            credentials: CredentialSource::ApplicationDefault,
        }
    }

    /// Create a new `BigQueryToolset` that resolves credentials from the
    /// secret provider at execution time via `ctx.get_secret(secret_name)`.
    pub fn from_secret(secret_name: impl Into<String>) -> Self {
        Self { project_id: None, credentials: CredentialSource::SecretRef(secret_name.into()) }
    }
}

impl Default for BigQueryToolset {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Toolset for BigQueryToolset {
    fn name(&self) -> &str {
        "bigquery"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let project_id = self.project_id.clone();
        let credentials = self.credentials.clone();

        Ok(vec![
            Arc::new(BigQueryExecuteSql::new(project_id.clone(), credentials.clone())),
            Arc::new(BigQueryGetTableSchema::new(project_id.clone(), credentials.clone())),
            Arc::new(BigQueryListDatasets::new(project_id.clone(), credentials.clone())),
            Arc::new(BigQueryListTables::new(project_id, credentials)),
        ])
    }
}
