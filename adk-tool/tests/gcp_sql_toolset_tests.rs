//! What the BigQuery and Spanner SQL toolsets allow, and how they handle key secrets.
//!
//! `bigquery_execute_sql` reported itself read-only while running any SQL, so parallel
//! dispatch treated a `DROP TABLE` as safe to run alongside other calls. Both toolsets
//! wrote a service-account key from the secret provider to the shared temp directory
//! with default permissions and echoed the JSON parser's message, which can quote key
//! material, into the tool error.
//!
//! None of these tests reach Google Cloud: each call is refused before a request is sent.

#![cfg(any(feature = "bigquery", feature = "spanner"))]

use adk_core::{
    AdkError, CallbackContext, Content, ErrorCategory, EventActions, MemoryEntry, ReadonlyContext,
    Result, Tool, ToolContext, Toolset,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Key material that must never appear in an error.
const KEY_MATERIAL: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7";

/// A secret holding a key in the wrong shape: a bare JSON string instead of the key
/// file object. A JSON parser reports the unexpected string by quoting it.
fn malformed_key_secret() -> String {
    json!(format!("-----BEGIN PRIVATE KEY-----\n{KEY_MATERIAL}\n-----END PRIVATE KEY-----\n"))
        .to_string()
}

/// A tool context whose secret provider returns `secret` for every name.
struct SecretContext {
    secret: Option<String>,
    content: Content,
}

impl SecretContext {
    fn new(secret: Option<String>) -> Arc<Self> {
        Arc::new(Self { secret, content: Content::new("user") })
    }
}

#[async_trait]
impl ReadonlyContext for SecretContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for SecretContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for SecretContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
    async fn get_secret(&self, _name: &str) -> Result<Option<String>> {
        Ok(self.secret.clone())
    }
}

/// The named tool from `toolset`.
async fn tool(toolset: &dyn Toolset, name: &str) -> Arc<dyn Tool> {
    let ctx: Arc<dyn ReadonlyContext> = SecretContext::new(None);
    toolset
        .tools(ctx)
        .await
        .unwrap()
        .into_iter()
        .find(|tool| tool.name() == name)
        .expect("the toolset must expose the tool")
}

/// Runs `tool` with `args` and returns the error it must produce.
async fn expect_error(tool: &Arc<dyn Tool>, secret: Option<String>, args: Value) -> AdkError {
    let ctx: Arc<dyn ToolContext> = SecretContext::new(secret);
    tool.execute(ctx, args).await.expect_err("the call must be refused")
}

#[cfg(feature = "bigquery")]
mod bigquery {
    use super::*;
    use adk_tool::bigquery::BigQueryToolset;

    const EXECUTE_SQL: &str = "bigquery_execute_sql";

    async fn execute_sql(toolset: BigQueryToolset) -> Arc<dyn Tool> {
        tool(&toolset, EXECUTE_SQL).await
    }

    // ── Read-only mode is the default and is enforced ─────────────────

    #[tokio::test]
    async fn execute_sql_is_read_only_by_default() {
        let tool = execute_sql(BigQueryToolset::with_project("p")).await;
        assert!(tool.is_read_only());
    }

    #[tokio::test]
    async fn execute_sql_reports_itself_mutating_when_writes_are_allowed() {
        // Parallel dispatch trusts this flag, so it must follow the configuration.
        let tool = execute_sql(BigQueryToolset::with_project("p").with_read_only(false)).await;
        assert!(!tool.is_read_only());
    }

    #[tokio::test]
    async fn the_inspection_tools_stay_read_only_when_writes_are_allowed() {
        let toolset = BigQueryToolset::with_project("p").with_read_only(false);
        for name in ["bigquery_get_table_schema", "bigquery_list_datasets", "bigquery_list_tables"]
        {
            assert!(tool(&toolset, name).await.is_read_only(), "{name} must stay read-only");
        }
    }

    #[tokio::test]
    async fn read_only_mode_refuses_statements_that_write() {
        let tool = execute_sql(BigQueryToolset::with_project("p")).await;

        for query in [
            "DROP TABLE x",
            "SELECT 1; DROP TABLE x",
            "delete from ds.t where true",
            "/* SELECT */ INSERT INTO ds.t VALUES (1)",
            "MERGE ds.t USING ds.s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "EXECUTE IMMEDIATE 'DROP TABLE x'",
        ] {
            let err = expect_error(&tool, None, json!({ "query": query })).await;
            assert_eq!(
                (err.code, err.category),
                ("tool.bigquery.read_only_violation", ErrorCategory::Forbidden),
                "{query} was not refused as a write: {err}"
            );
        }
    }

    #[tokio::test]
    async fn read_only_mode_lets_a_query_through_to_the_service() {
        // The query passes the gate and fails only for want of credentials.
        let tool = execute_sql(BigQueryToolset::from_secret("bq")).await;

        for query in ["SELECT 1", "WITH t AS (SELECT 1 AS n) SELECT n FROM t"] {
            let err = expect_error(&tool, None, json!({ "query": query, "project_id": "p" })).await;
            assert_eq!(err.code, "tool.bigquery.missing_secret", "{query}: {err}");
        }
    }

    #[tokio::test]
    async fn allowing_writes_turns_the_gate_off() {
        let tool = execute_sql(BigQueryToolset::from_secret("bq").with_read_only(false)).await;

        let err =
            expect_error(&tool, None, json!({ "query": "DROP TABLE x", "project_id": "p" })).await;
        assert_eq!(err.code, "tool.bigquery.missing_secret", "{err}");
    }

    // ── Key secrets ───────────────────────────────────────────────────

    #[tokio::test]
    async fn a_malformed_key_secret_is_not_echoed_into_the_error() {
        let tool = execute_sql(BigQueryToolset::from_secret("bq")).await;

        let err = expect_error(
            &tool,
            Some(malformed_key_secret()),
            json!({ "query": "SELECT 1", "project_id": "p" }),
        )
        .await;

        assert_eq!(err.code, "tool.bigquery.auth_error", "{err}");
        assert!(!err.to_string().contains(KEY_MATERIAL), "the error quoted key material: {err}");
    }
}

#[cfg(feature = "spanner")]
mod spanner {
    use super::*;
    use adk_tool::spanner::SpannerToolset;

    #[tokio::test]
    async fn a_malformed_key_secret_is_not_echoed_into_the_error() {
        let toolset = SpannerToolset::from_secret("p", "i", "d", "spanner");
        let tool = tool(&toolset, "spanner_execute_sql").await;

        let err =
            expect_error(&tool, Some(malformed_key_secret()), json!({ "query": "SELECT 1" })).await;

        assert_eq!(err.code, "tool.spanner.auth_error", "{err}");
        assert!(!err.to_string().contains(KEY_MATERIAL), "the error quoted key material: {err}");
    }
}
