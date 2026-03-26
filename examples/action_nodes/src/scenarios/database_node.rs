//! Database Node scenarios: config validation (placeholder executors).

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

pub async fn run() -> Result<()> {
    println!("── 13. Database Node (config validation) ───────");

    // 13a. SQL config — missing connection string → fallback
    let graph = GraphAgent::builder("db-validate")
        .description("DB config validation")
        .channels(&["dbResult"])
        .action_node(ActionNodeConfig::Database(DatabaseNodeConfig {
            standard: StandardProperties {
                id: "bad_db".into(),
                name: "Bad DB Config".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Fallback,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: Some(json!({"error": "validation failed"})),
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout: 5000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "dbResult".into(),
                },
            },
            connection: DatabaseConnection {
                database_type: DatabaseType::Postgresql,
                connection_string: None,
                credential_ref: None,
            },
            sql: Some(SqlConfig {
                query: "SELECT 1".into(),
                operation: "query".into(),
                params: vec![],
            }),
            mongo: None,
            redis: None,
        }))
        .edge(START, "bad_db")
        .edge("bad_db", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  Missing conn: fallback={}", result["dbResult"]);
    assert_eq!(result["dbResult"]["error"], json!("validation failed"));

    // 13b. Valid SQL config (placeholder executor, caught by fallback)
    let graph = GraphAgent::builder("db-sql-valid")
        .description("Valid SQL config")
        .channels(&["dbResult"])
        .action_node(ActionNodeConfig::Database(DatabaseNodeConfig {
            standard: StandardProperties {
                id: "sql_node".into(),
                name: "SQL Query".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Fallback,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: Some(json!({"status": "placeholder"})),
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout: 5000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "dbResult".into(),
                },
            },
            connection: DatabaseConnection {
                database_type: DatabaseType::Postgresql,
                connection_string: Some("postgres://user:pass@localhost/db".into()),
                credential_ref: None,
            },
            sql: Some(SqlConfig {
                query: "SELECT * FROM users WHERE id = $1".into(),
                operation: "query".into(),
                params: vec![json!(1)],
            }),
            mongo: None,
            redis: None,
        }))
        .edge(START, "sql_node")
        .edge("sql_node", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  Valid SQL:   fallback={} (placeholder)", result["dbResult"]["status"]);

    // 13c. Redis config validation
    let graph = GraphAgent::builder("db-redis")
        .description("Valid Redis config")
        .channels(&["dbResult"])
        .action_node(ActionNodeConfig::Database(DatabaseNodeConfig {
            standard: StandardProperties {
                id: "redis_node".into(),
                name: "Redis GET".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Fallback,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: Some(json!({"status": "placeholder"})),
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout: 5000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "dbResult".into(),
                },
            },
            connection: DatabaseConnection {
                database_type: DatabaseType::Redis,
                connection_string: Some("redis://localhost:6379".into()),
                credential_ref: None,
            },
            sql: None,
            mongo: None,
            redis: Some(RedisConfig {
                command: "get".into(),
                key: "session:abc123".into(),
                value: None,
                ttl: None,
                field: None,
            }),
        }))
        .edge(START, "redis_node")
        .edge("redis_node", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  Valid Redis: fallback={} (placeholder)", result["dbResult"]["status"]);

    println!("  ✓ All Database node scenarios passed\n");
    Ok(())
}
