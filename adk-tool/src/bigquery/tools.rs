//! Individual BigQuery tool implementations.
//!
//! Each tool creates a BigQuery client on demand and maps BigQuery API errors
//! to [`AdkError`].

use crate::bigquery::toolset::CredentialSource;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use gcp_bigquery_client::Client;
use gcp_bigquery_client::model::query_request::QueryRequest;
use serde_json::{Value, json};
use std::sync::Arc;

/// Default maximum number of result rows returned by `bigquery_execute_sql`.
const DEFAULT_MAX_RESULTS: i64 = 1000;

/// Create a BigQuery client from the configured credential source.
///
/// For [`CredentialSource::ApplicationDefault`], uses the
/// `GOOGLE_APPLICATION_CREDENTIALS` environment variable.
/// For [`CredentialSource::SecretRef`], resolves the service account key
/// JSON from the secret provider and writes it to a temporary file.
async fn create_client(
    credentials: &CredentialSource,
    ctx: &Arc<dyn ToolContext>,
) -> Result<Client> {
    match credentials {
        CredentialSource::ApplicationDefault => {
            let sa_key_path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS").map_err(|_| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unauthorized,
                    "tool.bigquery.missing_credentials",
                    "GOOGLE_APPLICATION_CREDENTIALS environment variable not set. \
                     Set it to the path of your service account key file, or use \
                     BigQueryToolset::from_secret() with a SecretProvider.",
                )
            })?;
            Client::from_service_account_key_file(&sa_key_path)
                .await
                .map_err(|e| map_bigquery_error("client initialization", e))
        }
        CredentialSource::SecretRef(secret_name) => {
            let secret_json = ctx.get_secret(secret_name).await?.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unauthorized,
                    "tool.bigquery.missing_secret",
                    format!(
                        "BigQuery credentials secret '{secret_name}' not found. \
                         Configure a SecretProvider or use BigQueryToolset::new() \
                         with Application Default Credentials."
                    ),
                )
            })?;

            // Write the secret to a temporary file for the client
            let tmp_dir = std::env::temp_dir();
            let tmp_path = tmp_dir.join(format!("adk-bq-{}.json", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp_path, &secret_json).await.map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.bigquery.temp_file_error",
                    format!("Failed to write temporary credentials file: {e}"),
                )
            })?;

            let client = Client::from_service_account_key_file(tmp_path.to_str().unwrap_or(""))
                .await
                .map_err(|e| map_bigquery_error("client initialization", e));

            // Clean up the temporary file
            let _ = tokio::fs::remove_file(&tmp_path).await;

            client
        }
    }
}

/// Resolve the project ID from the tool arguments or the toolset default.
fn resolve_project_id(args: &Value, default_project: &Option<String>) -> Result<String> {
    if let Some(project) = args["project_id"].as_str() {
        return Ok(project.to_string());
    }
    if let Some(project) = default_project {
        return Ok(project.clone());
    }
    Err(AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "tool.bigquery.missing_project_id",
        "Missing required parameter 'project_id'. Either provide it in the \
         tool arguments or configure BigQueryToolset::with_project().",
    ))
}

/// Map a BigQuery client error to an [`AdkError`] with the appropriate category.
fn map_bigquery_error(operation: &str, err: gcp_bigquery_client::error::BQError) -> AdkError {
    let msg = format!("{err}");
    let (category, code) = categorize_error(&msg);
    AdkError::new(
        ErrorComponent::Tool,
        category,
        code,
        format!("BigQuery {operation} failed: {msg}"),
    )
}

/// Categorize a BigQuery error message into an [`ErrorCategory`].
fn categorize_error(msg: &str) -> (ErrorCategory, &'static str) {
    let lower = msg.to_lowercase();

    if lower.contains("unauthorized")
        || lower.contains("unauthenticated")
        || lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("forbidden")
        || lower.contains("invalid credentials")
        || lower.contains("401")
        || lower.contains("403")
    {
        return (ErrorCategory::Unauthorized, "tool.bigquery.auth_error");
    }

    if lower.contains("quota")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
        || lower.contains("429")
        || lower.contains("exceeded")
    {
        return (ErrorCategory::RateLimited, "tool.bigquery.quota_exceeded");
    }

    if lower.contains("not found")
        || lower.contains("notfound")
        || lower.contains("404")
        || lower.contains("does not exist")
    {
        return (ErrorCategory::NotFound, "tool.bigquery.not_found");
    }

    if lower.contains("invalid")
        || lower.contains("syntax error")
        || lower.contains("parse error")
        || lower.contains("bad request")
        || lower.contains("400")
        || lower.contains("unrecognized")
    {
        return (ErrorCategory::InvalidInput, "tool.bigquery.invalid_request");
    }

    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("network")
        || lower.contains("unavailable")
        || lower.contains("503")
    {
        return (ErrorCategory::Unavailable, "tool.bigquery.unavailable");
    }

    (ErrorCategory::Internal, "tool.bigquery.api_error")
}

// ---------------------------------------------------------------------------
// bigquery_execute_sql
// ---------------------------------------------------------------------------

/// Execute a SQL query against BigQuery and return results as a JSON array.
///
/// Calls the BigQuery Jobs API
/// [`query`](https://cloud.google.com/bigquery/docs/reference/rest/v2/jobs/query)
/// endpoint.
pub(crate) struct BigQueryExecuteSql {
    project_id: Option<String>,
    credentials: CredentialSource,
}

impl BigQueryExecuteSql {
    pub fn new(project_id: Option<String>, credentials: CredentialSource) -> Self {
        Self { project_id, credentials }
    }
}

#[async_trait]
impl Tool for BigQueryExecuteSql {
    fn name(&self) -> &str {
        "bigquery_execute_sql"
    }

    fn description(&self) -> &str {
        "Execute a SQL query against Google BigQuery and return results as a JSON array of row objects."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The SQL query to execute."
                },
                "project_id": {
                    "type": "string",
                    "description": "The Google Cloud project ID. Uses the toolset default if not provided."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of rows to return (default 1000)."
                }
            },
            "required": ["query"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = create_client(&self.credentials, &ctx).await?;
        let project_id = resolve_project_id(&args, &self.project_id)?;

        let sql = args["query"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.bigquery.missing_query",
                "Missing required parameter 'query'",
            )
        })?;

        let max_results = args["max_results"].as_i64().unwrap_or(DEFAULT_MAX_RESULTS);

        let mut query_request = QueryRequest::new(sql);
        query_request.max_results = Some(max_results as i32);

        let response = client
            .job()
            .query(&project_id, query_request)
            .await
            .map_err(|e| map_bigquery_error("query execution", e))?;

        // Convert the query response to a JSON array of row objects
        let mut rs = gcp_bigquery_client::model::query_response::ResultSet::new_from_query_response(
            response,
        );

        let column_names = rs.column_names();
        let mut rows: Vec<Value> = Vec::new();
        while rs.next_row() {
            let mut row_obj = serde_json::Map::new();
            for field_name in &column_names {
                let value =
                    rs.get_json_value_by_name(field_name).ok().flatten().unwrap_or(Value::Null);
                row_obj.insert(field_name.clone(), value);
            }
            rows.push(Value::Object(row_obj));
        }

        Ok(json!({
            "rows": rows,
            "total_rows": rows.len(),
        }))
    }
}

// ---------------------------------------------------------------------------
// bigquery_get_table_schema
// ---------------------------------------------------------------------------

/// Retrieve column definitions for a BigQuery table.
pub(crate) struct BigQueryGetTableSchema {
    project_id: Option<String>,
    credentials: CredentialSource,
}

impl BigQueryGetTableSchema {
    pub fn new(project_id: Option<String>, credentials: CredentialSource) -> Self {
        Self { project_id, credentials }
    }
}

#[async_trait]
impl Tool for BigQueryGetTableSchema {
    fn name(&self) -> &str {
        "bigquery_get_table_schema"
    }

    fn description(&self) -> &str {
        "Retrieve the schema (column definitions) for a BigQuery table."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "The Google Cloud project ID. Uses the toolset default if not provided."
                },
                "dataset_id": {
                    "type": "string",
                    "description": "The BigQuery dataset ID containing the table."
                },
                "table_id": {
                    "type": "string",
                    "description": "The BigQuery table ID."
                }
            },
            "required": ["dataset_id", "table_id"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = create_client(&self.credentials, &ctx).await?;
        let project_id = resolve_project_id(&args, &self.project_id)?;

        let dataset_id = args["dataset_id"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.bigquery.missing_dataset_id",
                "Missing required parameter 'dataset_id'",
            )
        })?;

        let table_id = args["table_id"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.bigquery.missing_table_id",
                "Missing required parameter 'table_id'",
            )
        })?;

        let table = client
            .table()
            .get(&project_id, dataset_id, table_id, None)
            .await
            .map_err(|e| map_bigquery_error("get table schema", e))?;

        let columns: Vec<Value> = table
            .schema
            .fields
            .as_ref()
            .map(|fields| {
                fields
                    .iter()
                    .map(|f| {
                        json!({
                            "name": f.name,
                            "type": f.r#type,
                            "mode": f.mode,
                            "description": f.description,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(json!({
            "table": format!("{project_id}.{dataset_id}.{table_id}"),
            "columns": columns,
        }))
    }
}

// ---------------------------------------------------------------------------
// bigquery_list_datasets
// ---------------------------------------------------------------------------

/// List available datasets in a BigQuery project.
pub(crate) struct BigQueryListDatasets {
    project_id: Option<String>,
    credentials: CredentialSource,
}

impl BigQueryListDatasets {
    pub fn new(project_id: Option<String>, credentials: CredentialSource) -> Self {
        Self { project_id, credentials }
    }
}

#[async_trait]
impl Tool for BigQueryListDatasets {
    fn name(&self) -> &str {
        "bigquery_list_datasets"
    }

    fn description(&self) -> &str {
        "List available datasets in a Google BigQuery project."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "The Google Cloud project ID. Uses the toolset default if not provided."
                }
            }
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = create_client(&self.credentials, &ctx).await?;
        let project_id = resolve_project_id(&args, &self.project_id)?;

        let datasets = client
            .dataset()
            .list(&project_id, gcp_bigquery_client::dataset::ListOptions::default())
            .await
            .map_err(|e| map_bigquery_error("list datasets", e))?;

        let dataset_list: Vec<Value> = datasets
            .datasets
            .iter()
            .map(|ds| {
                let id = &ds.dataset_reference.dataset_id;
                let friendly_name = ds.friendly_name.as_deref().unwrap_or("");
                json!({
                    "dataset_id": id,
                    "friendly_name": friendly_name,
                })
            })
            .collect();

        Ok(json!({
            "project_id": project_id,
            "datasets": dataset_list,
            "total": dataset_list.len(),
        }))
    }
}

// ---------------------------------------------------------------------------
// bigquery_list_tables
// ---------------------------------------------------------------------------

/// List tables in a BigQuery dataset.
pub(crate) struct BigQueryListTables {
    project_id: Option<String>,
    credentials: CredentialSource,
}

impl BigQueryListTables {
    pub fn new(project_id: Option<String>, credentials: CredentialSource) -> Self {
        Self { project_id, credentials }
    }
}

#[async_trait]
impl Tool for BigQueryListTables {
    fn name(&self) -> &str {
        "bigquery_list_tables"
    }

    fn description(&self) -> &str {
        "List tables in a Google BigQuery dataset."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "The Google Cloud project ID. Uses the toolset default if not provided."
                },
                "dataset_id": {
                    "type": "string",
                    "description": "The BigQuery dataset ID to list tables from."
                }
            },
            "required": ["dataset_id"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = create_client(&self.credentials, &ctx).await?;
        let project_id = resolve_project_id(&args, &self.project_id)?;

        let dataset_id = args["dataset_id"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.bigquery.missing_dataset_id",
                "Missing required parameter 'dataset_id'",
            )
        })?;

        let tables = client
            .table()
            .list(&project_id, dataset_id, gcp_bigquery_client::table::ListOptions::default())
            .await
            .map_err(|e| map_bigquery_error("list tables", e))?;

        let table_list: Vec<Value> = tables
            .tables
            .unwrap_or_default()
            .iter()
            .map(|t| {
                let table_id = &t.table_reference.table_id;
                let table_type = t.r#type.as_deref().unwrap_or("TABLE");
                let friendly_name = t.friendly_name.as_deref().unwrap_or("");
                json!({
                    "table_id": table_id,
                    "type": table_type,
                    "friendly_name": friendly_name,
                })
            })
            .collect();

        Ok(json!({
            "project_id": project_id,
            "dataset_id": dataset_id,
            "tables": table_list,
            "total": table_list.len(),
        }))
    }
}
