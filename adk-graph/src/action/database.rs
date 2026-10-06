//! Database action node executor (compiled with the `action-db` feature).
//!
//! **Not implemented.** No database driver is integrated under any feature,
//! so this module validates the configuration and then returns an error saying
//! so. [`ActionNodeExecutor`](super::ActionNodeExecutor) rejects a database
//! node when the graph is built, before any node runs.

use adk_action::{DatabaseNodeConfig, DatabaseType};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Database action node.
///
/// Validates the configuration and returns a descriptive error indicating
/// which database driver is needed.
pub async fn execute_database(
    config: &DatabaseNodeConfig,
    _ctx: &NodeContext,
) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let db_type = &config.connection.database_type;

    // Validate connection config
    validate_connection(config, node_id)?;

    match db_type {
        DatabaseType::Postgresql | DatabaseType::Mysql | DatabaseType::Sqlite => {
            execute_sql_placeholder(config, node_id)
        }
        DatabaseType::Mongodb => execute_mongo_placeholder(config, node_id),
        DatabaseType::Redis => execute_redis_placeholder(config, node_id),
    }
}

/// Validate the database connection configuration.
fn validate_connection(config: &DatabaseNodeConfig, node_id: &str) -> Result<()> {
    let conn = &config.connection;

    // Must have either a connection string or a credential reference
    if conn.connection_string.is_none() && conn.credential_ref.is_none() {
        return Err(GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: "database node requires either a 'connection_string' or 'credential_ref' \
                      in the connection configuration"
                .to_string(),
        });
    }

    // Validate that the appropriate query config is present for the database type
    match conn.database_type {
        DatabaseType::Postgresql | DatabaseType::Mysql | DatabaseType::Sqlite => {
            if config.sql.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: format!(
                        "database node with type '{:?}' requires an 'sql' configuration block",
                        conn.database_type
                    ),
                });
            }
        }
        DatabaseType::Mongodb => {
            if config.mongo.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: "database node with type 'mongodb' requires a 'mongo' \
                              configuration block"
                        .to_string(),
                });
            }
        }
        DatabaseType::Redis => {
            if config.redis.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: "database node with type 'redis' requires a 'redis' \
                              configuration block"
                        .to_string(),
                });
            }
        }
    }

    Ok(())
}

/// SQL database placeholder (PostgreSQL, MySQL, SQLite).
///
/// The `action-db` feature flag is defined but `sqlx` is not yet wired as a
/// dependency. This placeholder validates the config and returns an error
/// explaining what is needed.
fn execute_sql_placeholder(config: &DatabaseNodeConfig, node_id: &str) -> Result<NodeOutput> {
    let db_type = &config.connection.database_type;
    let sql = config.sql.as_ref().expect("validated above");

    tracing::debug!(
        node = %node_id,
        db_type = ?db_type,
        operation = %sql.operation,
        query_len = sql.query.len(),
        "SQL database node validated (placeholder)"
    );

    Err(GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: format!(
            "database action nodes are not implemented: no {db_type:?} driver is integrated, \
             in any feature configuration; run the query from a function node instead"
        ),
    })
}

/// MongoDB placeholder.
///
/// The `action-db-mongo` feature flag is defined but the `mongodb` driver
/// is not yet wired. This placeholder validates the config and returns an
/// error explaining what is needed.
fn execute_mongo_placeholder(config: &DatabaseNodeConfig, node_id: &str) -> Result<NodeOutput> {
    let mongo = config.mongo.as_ref().expect("validated above");

    tracing::debug!(
        node = %node_id,
        collection = %mongo.collection,
        operation = %mongo.operation,
        "MongoDB node validated (placeholder)"
    );

    Err(GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "database action nodes are not implemented: no MongoDB driver is integrated, \
                  in any feature configuration; run the operation from a function node instead"
            .to_string(),
    })
}

/// Redis placeholder.
///
/// The `action-db-redis` feature flag is defined but the `redis` client
/// is not yet wired. This placeholder validates the config and returns an
/// error explaining what is needed.
fn execute_redis_placeholder(config: &DatabaseNodeConfig, node_id: &str) -> Result<NodeOutput> {
    let redis = config.redis.as_ref().expect("validated above");

    tracing::debug!(
        node = %node_id,
        command = %redis.command,
        key = %redis.key,
        "Redis node validated (placeholder)"
    );

    Err(GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "database action nodes are not implemented: no Redis client is integrated, \
                  in any feature configuration; run the command from a function node instead"
            .to_string(),
    })
}
