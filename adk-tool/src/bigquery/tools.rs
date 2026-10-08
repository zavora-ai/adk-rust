//! Individual BigQuery tool implementations.
//!
//! Each tool creates a BigQuery client on demand and maps BigQuery API errors
//! to [`AdkError`].

use crate::bigquery::toolset::CredentialSource;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use gcp_bigquery_client::model::job::Job;
use gcp_bigquery_client::model::job_configuration::JobConfiguration;
use gcp_bigquery_client::model::job_configuration_query::JobConfigurationQuery;
use gcp_bigquery_client::model::query_request::QueryRequest;
use gcp_bigquery_client::{Client, yup_oauth2};
use serde_json::{Value, json};
use std::sync::Arc;

/// Default maximum number of result rows returned by `bigquery_execute_sql`.
const DEFAULT_MAX_RESULTS: i64 = 1000;

/// Create a BigQuery client from the configured credential source.
///
/// For [`CredentialSource::ApplicationDefault`], uses the
/// `GOOGLE_APPLICATION_CREDENTIALS` environment variable.
/// For [`CredentialSource::SecretRef`], resolves the service account key
/// JSON from the secret provider and parses it in memory; the key is never
/// written to disk.
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

            // The parser's message can quote the offending JSON value, which may be key
            // material, so it is not passed on.
            let key = yup_oauth2::parse_service_account_key(&secret_json).map_err(|_| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unauthorized,
                    "tool.bigquery.auth_error",
                    format!(
                        "BigQuery credentials secret '{secret_name}' is not a valid service \
                         account key JSON. Store the full key file contents in the secret."
                    ),
                )
            })?;
            Client::from_service_account_key(key, false)
                .await
                .map_err(|e| map_bigquery_error("client initialization", e))
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

/// The statement type a BigQuery dry run reports for a query that only reads.
const SELECT_STATEMENT_TYPE: &str = "SELECT";

/// The error returned when read-only mode refuses a statement.
fn read_only_violation(reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Forbidden,
        "tool.bigquery.read_only_violation",
        format!(
            "bigquery_execute_sql is read-only and refused the query: {reason}. Send a single \
             SELECT or WITH query, or build the toolset with \
             BigQueryToolset::with_read_only(false) to allow writes."
        ),
    )
}

/// Rejects SQL that is not a single read-only query, before any request is sent.
///
/// The check is lexical and conservative: comments are skipped, the contents of
/// string literals and quoted identifiers are ignored, the first keyword must be
/// `SELECT` or `WITH` (optionally after opening parentheses), and only comments may
/// follow a `;`. The dry run in [`BigQueryExecuteSql`] is the authoritative check;
/// this one keeps a statement that is plainly not a query from reaching BigQuery.
fn check_read_only_sql(sql: &str) -> Result<()> {
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut started = false;
    let mut terminated = false;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' || (c == '-' && next == Some('-')) {
            while chars.get(i).is_some_and(|&c| c != '\n') {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let close = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .ok_or_else(|| read_only_violation("the query has an unterminated comment"))?;
            i = close + 2;
        } else if terminated {
            return Err(read_only_violation("multiple statements are not allowed"));
        } else if c == ';' {
            terminated = true;
            i += 1;
        } else if !started && c == '(' {
            i += 1;
        } else if !started {
            let end = (i..chars.len())
                .find(|&j| !(chars[j].is_alphanumeric() || chars[j] == '_'))
                .unwrap_or(chars.len());
            let keyword: String = chars[i..end].iter().collect();
            if !(keyword.eq_ignore_ascii_case("SELECT") || keyword.eq_ignore_ascii_case("WITH")) {
                let found = if keyword.is_empty() { c.to_string() } else { keyword.to_uppercase() };
                return Err(read_only_violation(&format!(
                    "it starts with {found}, not SELECT or WITH"
                )));
            }
            started = true;
            i = end;
        } else if matches!(c, '\'' | '"' | '`') {
            // A string literal or quoted identifier; its contents are not SQL.
            let triple = c != '`' && next == Some(c) && chars.get(i + 2) == Some(&c);
            let width = if triple { 3 } else { 1 };
            let mut j = i + width;
            loop {
                match chars.get(j) {
                    None => {
                        return Err(read_only_violation(
                            "the query has an unterminated string or quoted identifier",
                        ));
                    }
                    Some('\\') => j += 2,
                    Some(&q) if q == c && (!triple || chars.get(j + 1..j + 3) == Some(&[c, c])) => {
                        break;
                    }
                    Some(_) => j += 1,
                }
            }
            i = j + width;
        } else {
            i += 1;
        }
    }
    if started { Ok(()) } else { Err(read_only_violation("the query is empty")) }
}

/// Execute a SQL query against BigQuery and return results as a JSON array.
///
/// Calls the BigQuery Jobs API
/// [`query`](https://cloud.google.com/bigquery/docs/reference/rest/v2/jobs/query)
/// endpoint. In read-only mode, the default, the query must pass
/// [`check_read_only_sql`], and a dry run must report a `SELECT` statement before
/// it runs.
pub(crate) struct BigQueryExecuteSql {
    project_id: Option<String>,
    credentials: CredentialSource,
    read_only: bool,
}

impl BigQueryExecuteSql {
    pub fn new(project_id: Option<String>, credentials: CredentialSource, read_only: bool) -> Self {
        Self { project_id, credentials, read_only }
    }
}

#[async_trait]
impl Tool for BigQueryExecuteSql {
    fn name(&self) -> &str {
        "bigquery_execute_sql"
    }

    fn description(&self) -> &str {
        if self.read_only {
            "Run a read-only SQL query (a single SELECT or WITH statement) against Google \
             BigQuery and return results as a JSON array of row objects."
        } else {
            "Execute a SQL statement against Google BigQuery and return results as a JSON \
             array of row objects."
        }
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
        self.read_only
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let sql = args["query"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.bigquery.missing_query",
                "Missing required parameter 'query'",
            )
        })?;
        if self.read_only {
            check_read_only_sql(sql)?;
        }
        let project_id = resolve_project_id(&args, &self.project_id)?;
        let client = create_client(&self.credentials, &ctx).await?;

        if self.read_only {
            // A dry run validates the query and reports its statement type without
            // running it, which settles what a lexical check cannot.
            let dry_run = Job {
                configuration: Some(JobConfiguration {
                    dry_run: Some(true),
                    query: Some(JobConfigurationQuery {
                        query: sql.to_string(),
                        use_legacy_sql: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let job = client
                .job()
                .insert(&project_id, dry_run)
                .await
                .map_err(|e| map_bigquery_error("query validation", e))?;
            let statement_type =
                job.statistics.and_then(|stats| stats.query).and_then(|query| query.statement_type);
            if statement_type.as_deref() != Some(SELECT_STATEMENT_TYPE) {
                let reported = statement_type.as_deref().unwrap_or("no statement type");
                return Err(read_only_violation(&format!("BigQuery classifies it as {reported}")));
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_queries_are_accepted() {
        for sql in [
            "SELECT 1",
            "select 1;",
            "WITH t AS (SELECT 1 AS n) SELECT n FROM t",
            "(SELECT 1) UNION ALL (SELECT 2)",
            "  -- leading comment\n  SELECT 1",
            "#standardSQL\nSELECT 1",
            "/* block */ SELECT 1; -- trailing comment",
            "SELECT ';DROP TABLE x' AS s",
            "SELECT \"a;b\", 'it\\'s; fine', `col;name` FROM `p.d.t`",
            "SELECT '''multi;\nline''' AS s, r\"raw;\" AS r",
            "SELECT * FROM t WHERE note = '--not a comment'; ",
        ] {
            assert!(check_read_only_sql(sql).is_ok(), "rejected a read-only query: {sql}");
        }
    }

    #[test]
    fn statements_that_are_not_queries_are_rejected() {
        for sql in [
            "DROP TABLE x",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1 WHERE true",
            "DELETE FROM t WHERE true",
            "MERGE t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "TRUNCATE TABLE t",
            "CREATE TABLE t (a INT64)",
            "ALTER TABLE t ADD COLUMN b INT64",
            "EXPORT DATA OPTIONS(uri='gs://b/*') AS SELECT 1",
            "CALL proc()",
            "EXECUTE IMMEDIATE 'DROP TABLE x'",
            "DECLARE x INT64",
            "BEGIN SELECT 1; END",
            "/* SELECT */ DELETE FROM t WHERE true",
            "-- SELECT\nDROP TABLE x",
            "'SELECT 1'",
        ] {
            assert!(check_read_only_sql(sql).is_err(), "accepted a non-query: {sql}");
        }
    }

    #[test]
    fn multiple_statements_are_rejected() {
        for sql in [
            "SELECT 1; DROP TABLE x",
            "SELECT 1;DROP TABLE x",
            "SELECT 1; SELECT 2",
            "SELECT 1; /* c */ DROP TABLE x",
            "SELECT 'a'; DROP TABLE x",
            "SELECT 1;;",
        ] {
            assert!(check_read_only_sql(sql).is_err(), "accepted multiple statements: {sql}");
        }
    }

    #[test]
    fn unterminated_or_empty_input_is_rejected() {
        for sql in ["", "   ", "-- only a comment", "SELECT 'open", "SELECT 1 /* open", "SELECT `x"]
        {
            assert!(check_read_only_sql(sql).is_err(), "accepted malformed input: {sql:?}");
        }
    }

    #[test]
    fn a_rejection_explains_how_to_allow_writes() {
        let err = check_read_only_sql("DROP TABLE x").unwrap_err();
        assert_eq!(
            (err.code, err.category),
            ("tool.bigquery.read_only_violation", ErrorCategory::Forbidden)
        );
        assert!(err.message.contains("with_read_only(false)"), "{}", err.message);
    }
}
