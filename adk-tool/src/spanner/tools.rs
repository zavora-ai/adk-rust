//! Individual Spanner tool implementations.
//!
//! Each tool creates a Spanner client on demand and maps Spanner API errors
//! to [`AdkError`]. Table listing and schema inspection use Spanner's
//! `INFORMATION_SCHEMA` SQL queries.

use crate::spanner::toolset::CredentialSource;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use google_cloud_spanner::client::{Client, ClientConfig, Error as SpannerError};
use google_cloud_spanner::statement::Statement;
use serde_json::{Value, json};
use std::sync::Arc;

/// Convenience wrapper: convert a gRPC `Status` (from query iterators)
/// into a `SpannerError` for uniform error handling.
///
/// `SpannerError` implements `From<Status>`, so this just delegates.
fn wrap_status<S: Into<SpannerError>>(status: S) -> SpannerError {
    status.into()
}

/// Build the Spanner database path from project, instance, and database IDs.
fn database_path(project_id: &str, instance_id: &str, database_id: &str) -> String {
    format!("projects/{project_id}/instances/{instance_id}/databases/{database_id}")
}

/// Create a Spanner client from the configured credential source.
///
/// For [`CredentialSource::ApplicationDefault`], uses the
/// `GOOGLE_APPLICATION_CREDENTIALS` environment variable or ADC.
/// For [`CredentialSource::SecretRef`], resolves the service account key
/// JSON from the secret provider and writes it to a temporary file.
async fn create_client(
    project_id: &str,
    instance_id: &str,
    database_id: &str,
    credentials: &CredentialSource,
    ctx: &Arc<dyn ToolContext>,
) -> Result<Client> {
    let db_path = database_path(project_id, instance_id, database_id);

    match credentials {
        CredentialSource::ApplicationDefault => {
            let config = ClientConfig::default().with_auth().await.map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unauthorized,
                    "tool.spanner.auth_error",
                    format!(
                        "Failed to initialize Spanner credentials via Application Default \
                         Credentials: {e}. Set GOOGLE_APPLICATION_CREDENTIALS or use \
                         SpannerToolset::from_secret() with a SecretProvider."
                    ),
                )
            })?;
            Client::new(&db_path, config)
                .await
                .map_err(|e| map_spanner_error("client initialization", e))
        }
        CredentialSource::SecretRef(secret_name) => {
            let secret_json = ctx.get_secret(secret_name).await?.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unauthorized,
                    "tool.spanner.missing_secret",
                    format!(
                        "Spanner credentials secret '{secret_name}' not found. \
                         Configure a SecretProvider or use SpannerToolset::new() \
                         with Application Default Credentials."
                    ),
                )
            })?;

            // Write the secret to a temporary file for the client
            let tmp_dir = std::env::temp_dir();
            let tmp_path = tmp_dir.join(format!("adk-spanner-{}.json", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp_path, &secret_json).await.map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.spanner.temp_file_error",
                    format!("Failed to write temporary credentials file: {e}"),
                )
            })?;

            // Parse the credentials file and create config
            let cred_file =
                google_cloud_spanner::client::google_cloud_auth::credentials::CredentialsFile::new_from_file(
                    tmp_path.to_str().unwrap_or("").to_string(),
                )
                .await
                .map_err(|e| {
                    let _ = std::fs::remove_file(&tmp_path);
                    AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Unauthorized,
                        "tool.spanner.auth_error",
                        format!("Failed to parse Spanner credentials from secret: {e}"),
                    )
                })?;

            let config =
                ClientConfig::default().with_credentials(cred_file).await.map_err(|e| {
                    let _ = std::fs::remove_file(&tmp_path);
                    AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Unauthorized,
                        "tool.spanner.auth_error",
                        format!("Failed to initialize Spanner credentials from secret: {e}"),
                    )
                })?;

            // Clean up the temporary file
            let _ = tokio::fs::remove_file(&tmp_path).await;

            Client::new(&db_path, config)
                .await
                .map_err(|e| map_spanner_error("client initialization", e))
        }
    }
}

/// Map a Spanner client error to an [`AdkError`] with the appropriate category.
fn map_spanner_error(operation: &str, err: SpannerError) -> AdkError {
    let (category, code) = match &err {
        SpannerError::GRPC(status) => {
            let code_int: i32 = status.code().into();
            categorize_grpc_code(code_int)
        }
        SpannerError::InvalidSession(_) => {
            (ErrorCategory::Unavailable, "tool.spanner.invalid_session")
        }
        SpannerError::Connection(_) => (ErrorCategory::Unavailable, "tool.spanner.connection"),
        SpannerError::InvalidConfig(_) => {
            (ErrorCategory::InvalidInput, "tool.spanner.invalid_config")
        }
        SpannerError::ParseError(_) => (ErrorCategory::Internal, "tool.spanner.parse_error"),
    };
    AdkError::new(
        ErrorComponent::Tool,
        category,
        code,
        format!("Spanner {operation} failed: {err}"),
    )
}

/// Categorize a gRPC status code into an [`ErrorCategory`].
fn categorize_grpc_code(code: i32) -> (ErrorCategory, &'static str) {
    // gRPC status codes: https://grpc.io/docs/guides/status-codes/
    match code {
        7 | 16 => {
            // PERMISSION_DENIED (7), UNAUTHENTICATED (16)
            (ErrorCategory::Unauthorized, "tool.spanner.auth_error")
        }
        5 => {
            // NOT_FOUND (5)
            (ErrorCategory::NotFound, "tool.spanner.not_found")
        }
        3 => {
            // INVALID_ARGUMENT (3)
            (ErrorCategory::InvalidInput, "tool.spanner.invalid_request")
        }
        8 => {
            // RESOURCE_EXHAUSTED (8)
            (ErrorCategory::RateLimited, "tool.spanner.quota_exceeded")
        }
        4 | 10 | 14 => {
            // DEADLINE_EXCEEDED (4), ABORTED (10), UNAVAILABLE (14)
            (ErrorCategory::Unavailable, "tool.spanner.unavailable")
        }
        _ => (ErrorCategory::Internal, "tool.spanner.api_error"),
    }
}

// ---------------------------------------------------------------------------
// spanner_execute_sql
// ---------------------------------------------------------------------------

/// Execute a SQL query against Cloud Spanner and return results as a JSON array.
pub(crate) struct SpannerExecuteSql {
    project_id: String,
    instance_id: String,
    database_id: String,
    credentials: CredentialSource,
}

impl SpannerExecuteSql {
    pub fn new(
        project_id: String,
        instance_id: String,
        database_id: String,
        credentials: CredentialSource,
    ) -> Self {
        Self { project_id, instance_id, database_id, credentials }
    }
}

#[async_trait]
impl Tool for SpannerExecuteSql {
    fn name(&self) -> &str {
        "spanner_execute_sql"
    }

    fn description(&self) -> &str {
        "Execute a SQL query against Google Cloud Spanner and return results as a JSON array of row objects."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The SQL query to execute."
                }
            },
            "required": ["query"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = create_client(
            &self.project_id,
            &self.instance_id,
            &self.database_id,
            &self.credentials,
            &ctx,
        )
        .await?;

        let sql = args["query"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.spanner.missing_query",
                "Missing required parameter 'query'",
            )
        })?;

        let stmt = Statement::new(sql);
        let mut tx =
            client.single().await.map_err(|e| map_spanner_error("begin transaction", e))?;
        let mut iter = tx
            .query(stmt)
            .await
            .map_err(|e| map_spanner_error("query execution", wrap_status(e)))?;

        // Extract column metadata for building row objects
        let fields = iter.columns_metadata().clone();
        let column_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();

        let mut rows: Vec<Value> = Vec::new();
        while let Some(row) = iter
            .next()
            .await
            .map_err(|status| map_spanner_error("reading row", SpannerError::GRPC(status)))?
        {
            let mut row_obj = serde_json::Map::new();
            for (idx, col_name) in column_names.iter().enumerate() {
                // Try to extract as String; fall back to null for unsupported types
                let value: Value = if let Ok(v) = row.column::<String>(idx) {
                    Value::String(v)
                } else if let Ok(v) = row.column::<Option<String>>(idx) {
                    match v {
                        Some(s) => Value::String(s),
                        None => Value::Null,
                    }
                } else if let Ok(v) = row.column::<i64>(idx) {
                    json!(v)
                } else if let Ok(v) = row.column::<Option<i64>>(idx) {
                    match v {
                        Some(n) => json!(n),
                        None => Value::Null,
                    }
                } else if let Ok(v) = row.column::<f64>(idx) {
                    json!(v)
                } else if let Ok(v) = row.column::<Option<f64>>(idx) {
                    match v {
                        Some(n) => json!(n),
                        None => Value::Null,
                    }
                } else if let Ok(v) = row.column::<bool>(idx) {
                    json!(v)
                } else if let Ok(v) = row.column::<Option<bool>>(idx) {
                    match v {
                        Some(b) => json!(b),
                        None => Value::Null,
                    }
                } else {
                    // Fallback: try as nullable string for any other type
                    Value::Null
                };
                row_obj.insert(col_name.clone(), value);
            }
            rows.push(Value::Object(row_obj));
        }

        client.close().await;

        Ok(json!({
            "rows": rows,
            "total_rows": rows.len(),
        }))
    }
}

// ---------------------------------------------------------------------------
// spanner_get_table_schema
// ---------------------------------------------------------------------------

/// Retrieve column definitions and key information for a Spanner table.
pub(crate) struct SpannerGetTableSchema {
    project_id: String,
    instance_id: String,
    database_id: String,
    credentials: CredentialSource,
}

impl SpannerGetTableSchema {
    pub fn new(
        project_id: String,
        instance_id: String,
        database_id: String,
        credentials: CredentialSource,
    ) -> Self {
        Self { project_id, instance_id, database_id, credentials }
    }
}

#[async_trait]
impl Tool for SpannerGetTableSchema {
    fn name(&self) -> &str {
        "spanner_get_table_schema"
    }

    fn description(&self) -> &str {
        "Retrieve the schema (column definitions and key information) for a Cloud Spanner table."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "table_name": {
                    "type": "string",
                    "description": "The name of the Spanner table."
                }
            },
            "required": ["table_name"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = create_client(
            &self.project_id,
            &self.instance_id,
            &self.database_id,
            &self.credentials,
            &ctx,
        )
        .await?;

        let table_name = args["table_name"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.spanner.missing_table_name",
                "Missing required parameter 'table_name'",
            )
        })?;

        // Query column definitions from INFORMATION_SCHEMA
        let mut col_stmt = Statement::new(
            "SELECT COLUMN_NAME, SPANNER_TYPE, IS_NULLABLE, ORDINAL_POSITION \
             FROM INFORMATION_SCHEMA.COLUMNS \
             WHERE TABLE_NAME = @table_name \
             ORDER BY ORDINAL_POSITION",
        );
        col_stmt.add_param("table_name", &table_name);

        let mut tx =
            client.single().await.map_err(|e| map_spanner_error("begin transaction", e))?;
        let mut col_iter = tx
            .query(col_stmt)
            .await
            .map_err(|e| map_spanner_error("query columns", wrap_status(e)))?;

        let mut columns: Vec<Value> = Vec::new();
        while let Some(row) = col_iter.next().await.map_err(|status| {
            map_spanner_error("reading column metadata", SpannerError::GRPC(status))
        })? {
            let col_name = row.column_by_name::<String>("COLUMN_NAME").unwrap_or_default();
            let spanner_type = row.column_by_name::<String>("SPANNER_TYPE").unwrap_or_default();
            let is_nullable = row.column_by_name::<String>("IS_NULLABLE").unwrap_or_default();
            let ordinal = row.column_by_name::<i64>("ORDINAL_POSITION").unwrap_or(0);

            columns.push(json!({
                "name": col_name,
                "type": spanner_type,
                "nullable": is_nullable == "YES",
                "ordinal_position": ordinal,
            }));
        }

        // Query primary key columns from INFORMATION_SCHEMA
        let mut pk_stmt = Statement::new(
            "SELECT COLUMN_NAME, COLUMN_ORDERING, ORDINAL_POSITION \
             FROM INFORMATION_SCHEMA.INDEX_COLUMNS \
             WHERE TABLE_NAME = @table_name AND INDEX_NAME = 'PRIMARY_KEY' \
             ORDER BY ORDINAL_POSITION",
        );
        pk_stmt.add_param("table_name", &table_name);

        let mut pk_iter = tx
            .query(pk_stmt)
            .await
            .map_err(|e| map_spanner_error("query primary keys", wrap_status(e)))?;

        let mut primary_keys: Vec<Value> = Vec::new();
        while let Some(row) = pk_iter.next().await.map_err(|status| {
            map_spanner_error("reading primary key metadata", SpannerError::GRPC(status))
        })? {
            let col_name = row.column_by_name::<String>("COLUMN_NAME").unwrap_or_default();
            let ordering = row.column_by_name::<Option<String>>("COLUMN_ORDERING").unwrap_or(None);

            primary_keys.push(json!({
                "column": col_name,
                "ordering": ordering.unwrap_or_else(|| "ASC".to_string()),
            }));
        }

        client.close().await;

        Ok(json!({
            "table": table_name,
            "database": format!(
                "projects/{}/instances/{}/databases/{}",
                self.project_id, self.instance_id, self.database_id
            ),
            "columns": columns,
            "primary_key": primary_keys,
        }))
    }
}

// ---------------------------------------------------------------------------
// spanner_list_tables
// ---------------------------------------------------------------------------

/// List all tables in a Cloud Spanner database.
pub(crate) struct SpannerListTables {
    project_id: String,
    instance_id: String,
    database_id: String,
    credentials: CredentialSource,
}

impl SpannerListTables {
    pub fn new(
        project_id: String,
        instance_id: String,
        database_id: String,
        credentials: CredentialSource,
    ) -> Self {
        Self { project_id, instance_id, database_id, credentials }
    }
}

#[async_trait]
impl Tool for SpannerListTables {
    fn name(&self) -> &str {
        "spanner_list_tables"
    }

    fn description(&self) -> &str {
        "List all tables in a Google Cloud Spanner database."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let client = create_client(
            &self.project_id,
            &self.instance_id,
            &self.database_id,
            &self.credentials,
            &ctx,
        )
        .await?;

        let stmt = Statement::new(
            "SELECT TABLE_NAME, PARENT_TABLE_NAME, TABLE_TYPE \
             FROM INFORMATION_SCHEMA.TABLES \
             WHERE TABLE_SCHEMA = '' \
             ORDER BY TABLE_NAME",
        );

        let mut tx =
            client.single().await.map_err(|e| map_spanner_error("begin transaction", e))?;
        let mut iter =
            tx.query(stmt).await.map_err(|e| map_spanner_error("list tables", wrap_status(e)))?;

        let mut tables: Vec<Value> = Vec::new();
        while let Some(row) = iter.next().await.map_err(|status| {
            map_spanner_error("reading table metadata", SpannerError::GRPC(status))
        })? {
            let table_name = row.column_by_name::<String>("TABLE_NAME").unwrap_or_default();
            let parent = row.column_by_name::<Option<String>>("PARENT_TABLE_NAME").unwrap_or(None);
            let table_type = row.column_by_name::<Option<String>>("TABLE_TYPE").unwrap_or(None);

            tables.push(json!({
                "table_name": table_name,
                "parent_table": parent,
                "type": table_type.unwrap_or_else(|| "BASE TABLE".to_string()),
            }));
        }

        client.close().await;

        Ok(json!({
            "database": format!(
                "projects/{}/instances/{}/databases/{}",
                self.project_id, self.instance_id, self.database_id
            ),
            "tables": tables,
            "total": tables.len(),
        }))
    }
}
