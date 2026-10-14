//! `PostgresAuditSink` migration and queries against a live PostgreSQL server.
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-auth --features postgres-audit --run-ignored only --test audit_postgres_tests
//! ```
#![cfg(feature = "postgres-audit")]

use std::str::FromStr;

use adk_auth::{
    AuditEvent, AuditEventType, AuditFilter, AuditOutcome, AuditSink, PostgresAuditSink,
};
use chrono::{Duration, TimeZone, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection};

fn database_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must point at a PostgreSQL server")
}

/// Resources of the events `filter` returns within `tenant`, newest first.
async fn resources(sink: &PostgresAuditSink, tenant: &str, filter: AuditFilter) -> Vec<String> {
    let filter = AuditFilter { tenant_id: Some(tenant.to_string()), ..filter };
    sink.query(&filter).await.expect("query").into_iter().map(|event| event.resource).collect()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn query_applies_every_filter_with_bound_values() {
    let sink = PostgresAuditSink::new(&database_url()).await.expect("connect");
    sink.migrate().await.expect("migrate");
    let tenant = format!("tenant-{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
    let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();

    let events = [
        AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)
            .with_workspace("ws-1")
            .with_resource_id("r-1")
            .with_agent_id("urn:adk:agent:app/researcher"),
        AuditEvent::tool_access("bob", "search_web", AuditOutcome::Denied).with_workspace("ws-2"),
        // A value that would break the statement if it were interpolated.
        AuditEvent::agent_access("o'brien", "planner", AuditOutcome::Allowed)
            .with_resource_id("r-2'; DROP TABLE audit_events; --"),
    ];
    for (offset, event) in events.into_iter().enumerate() {
        let mut event = event.with_tenant(&tenant);
        event.timestamp = start + Duration::minutes(offset as i64);
        sink.log(event).await.expect("log");
    }

    let all = resources(&sink, &tenant, AuditFilter::default()).await;
    assert_eq!(all, ["planner", "search_web", "search"]);

    let cases = [
        (AuditFilter { user: Some("o'brien".into()), ..Default::default() }, vec!["planner"]),
        (
            AuditFilter { workspace_id: Some("ws-2".into()), ..Default::default() },
            vec!["search_web"],
        ),
        (
            AuditFilter { event_type: Some(AuditEventType::AgentAccess), ..Default::default() },
            vec!["planner"],
        ),
        (
            AuditFilter { outcome: Some(AuditOutcome::Denied), ..Default::default() },
            vec!["search_web"],
        ),
        (
            AuditFilter { resource: Some("search".into()), ..Default::default() },
            vec!["search_web", "search"],
        ),
        (
            AuditFilter {
                resource_id: Some("r-2'; DROP TABLE audit_events; --".into()),
                ..Default::default()
            },
            vec!["planner"],
        ),
        (
            AuditFilter {
                agent_id: Some("urn:adk:agent:app/researcher".into()),
                ..Default::default()
            },
            vec!["search"],
        ),
        (
            AuditFilter {
                after: Some(start),
                before: Some(start + Duration::minutes(2)),
                ..Default::default()
            },
            vec!["search_web"],
        ),
        (AuditFilter { limit: Some(1), offset: Some(1), ..Default::default() }, vec!["search_web"]),
    ];
    for (filter, expected) in cases {
        let description = format!("{filter:?}");
        assert_eq!(resources(&sink, &tenant, filter).await, expected, "{description}");
    }

    // Same test, not a second one: concurrent `migrate` calls race on a fresh database.
    resource_filter_matches_a_literal_substring(&sink).await;
}

/// `%` and `_` in a resource filter are literal characters, as in
/// `AuditFilter::matches`, not LIKE wildcards.
async fn resource_filter_matches_a_literal_substring(sink: &PostgresAuditSink) {
    let tenant = format!("literal-{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
    let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();

    for (offset, resource) in
        ["delete_file", "deleteXfile", "quota_50%", "quota_50X"].iter().enumerate()
    {
        let mut event =
            AuditEvent::tool_access("alice", resource, AuditOutcome::Allowed).with_tenant(&tenant);
        event.timestamp = start + Duration::minutes(offset as i64);
        sink.log(event).await.expect("log");
    }

    for (needle, expected) in [
        ("delete_file", vec!["delete_file"]),
        ("e_f", vec!["delete_file"]),
        ("50%", vec!["quota_50%"]),
        ("%", vec!["quota_50%"]),
        ("_", vec!["quota_50X", "quota_50%", "delete_file"]),
    ] {
        let filter = AuditFilter { resource: Some(needle.into()), ..Default::default() };
        assert_eq!(resources(sink, &tenant, filter).await, expected, "resource filter {needle:?}");
    }
}

/// `%` and `_` in a resource filter are literal characters, as in
/// `AuditFilter::matches`, not LIKE wildcards.
#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn resource_filter_matches_a_literal_substring() {
    let sink = PostgresAuditSink::new(&database_url()).await.expect("connect");
    sink.migrate().await.expect("migrate");
    let sink = &sink;
    let tenant = format!("literal-{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
    let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();

    for (offset, resource) in
        ["delete_file", "deleteXfile", "quota_50%", "quota_50X"].iter().enumerate()
    {
        let mut event =
            AuditEvent::tool_access("alice", resource, AuditOutcome::Allowed).with_tenant(&tenant);
        event.timestamp = start + Duration::minutes(offset as i64);
        sink.log(event).await.expect("log");
    }

    for (needle, expected) in [
        ("delete_file", vec!["delete_file"]),
        ("e_f", vec!["delete_file"]),
        ("50%", vec!["quota_50%"]),
        ("%", vec!["quota_50%"]),
        ("_", vec!["quota_50X", "quota_50%", "delete_file"]),
    ] {
        let filter = AuditFilter { resource: Some(needle.into()), ..Default::default() };
        assert_eq!(resources(sink, &tenant, filter).await, expected, "resource filter {needle:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DATABASE_URL"]
async fn concurrent_migrations_on_a_fresh_schema_all_succeed() {
    let schema = format!("audit_migrate_{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
    let mut admin = PgConnection::connect(&database_url()).await.expect("connect admin");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await
        .expect("create schema");

    let options = PgConnectOptions::from_str(&database_url())
        .expect("parse DATABASE_URL")
        .options([("search_path", schema.as_str())]);
    let pool =
        PgPoolOptions::new().max_connections(8).connect_with(options).await.expect("connect");
    let mut migrations = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let sink = PostgresAuditSink::from_pool(pool.clone());
        migrations.spawn(async move { sink.migrate().await });
    }
    let results: Vec<String> = migrations
        .join_all()
        .await
        .into_iter()
        .filter_map(|result| result.err().map(|error| error.to_string()))
        .collect();

    let sink = PostgresAuditSink::from_pool(pool.clone());
    sink.log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed))
        .await
        .expect("log after concurrent migrations");
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut admin)
        .await
        .expect("drop schema");

    assert_eq!(results, Vec::<String>::new());
}
