//! Concurrent `migrate` calls on the MongoDB and Neo4j memory services, as replicas that
//! start together make them, against live servers.
//!
//! Each test opens several services, each with its own client, and migrates them at once;
//! every call succeeds and the registry records each version once. MongoDB gets a new
//! database per run. Neo4j Community has one database, so its schema rules are new only on
//! a new server; the nightly `durable-stores` job runs these tests before any other suite
//! migrates the memory schema.
//!
//! ```bash
//! MONGODB_URL=mongodb://localhost:27017 NEO4J_PASSWORD=secret \
//!   cargo nextest run -p adk-memory --features mongodb-memory,neo4j-memory --run-ignored only \
//!   --test concurrent_migrate_tests
//! ```

/// Services migrating at once in each test.
#[cfg(any(feature = "mongodb-memory", feature = "neo4j-memory"))]
const INSTANCES: usize = 8;

#[cfg(feature = "mongodb-memory")]
mod mongodb {
    use std::sync::Arc;

    use adk_memory::MongoMemoryService;
    use mongodb::bson::{Document, doc};

    #[tokio::test]
    #[ignore = "requires a MongoDB server at MONGODB_URL"]
    async fn concurrent_migrate_on_a_new_database_records_each_version_once() {
        let url = std::env::var("MONGODB_URL")
            .unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
        let database = format!("adk_memory_migrate_{}", uuid::Uuid::new_v4().simple());
        let mut migrations = tokio::task::JoinSet::new();
        let mut services = Vec::new();
        for _ in 0..super::INSTANCES {
            let client = mongodb::Client::with_uri_str(&url).await.expect("connect to mongodb");
            services
                .push(Arc::new(MongoMemoryService::new(client, &database, None).expect("service")));
        }
        for service in &services {
            let service = Arc::clone(service);
            migrations.spawn(async move { service.migrate().await });
        }
        let results = migrations.join_all().await;

        let client = mongodb::Client::with_uri_str(&url).await.expect("connect to mongodb");
        let db = client.database(&database);
        let mut cursor = db
            .collection::<Document>("_adk_memory_migrations")
            .find(doc! {})
            .sort(doc! { "version": 1 })
            .await
            .expect("read registry");
        let mut versions = Vec::new();
        while cursor.advance().await.expect("read registry") {
            let row = cursor.deserialize_current().expect("registry row");
            versions.push(row.get_i64("version").expect("version"));
        }
        let schema_version = services[0].schema_version().await.expect("schema version");
        db.drop().await.expect("drop database");

        for result in results {
            result.expect("concurrent migrate");
        }
        assert_eq!(versions, vec![1, 2]);
        assert_eq!(schema_version, 2);
    }
}

#[cfg(feature = "neo4j-memory")]
mod neo4j {
    use std::sync::Arc;

    use adk_memory::Neo4jMemoryService;

    fn env_or(name: &str, default: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| default.to_string())
    }

    #[tokio::test]
    #[ignore = "requires a Neo4j server at NEO4J_URI"]
    async fn concurrent_migrate_records_each_version_once() {
        let mut migrations = tokio::task::JoinSet::new();
        let mut services = Vec::new();
        for _ in 0..super::INSTANCES {
            let graph = neo4rs::Graph::new(
                env_or("NEO4J_URI", "bolt://localhost:7687"),
                env_or("NEO4J_USER", "neo4j"),
                env_or("NEO4J_PASSWORD", "password"),
            )
            .await
            .expect("connect to neo4j");
            services.push(Arc::new(Neo4jMemoryService::new(graph, None).expect("service")));
        }
        for service in &services {
            let service = Arc::clone(service);
            migrations.spawn(async move { service.migrate().await });
        }
        for result in migrations.join_all().await {
            result.expect("concurrent migrate");
        }

        let mut rows = services[0]
            .graph()
            .execute(neo4rs::query(
                "MATCH (m:_AdkMemoryMigration) RETURN m.version AS version ORDER BY version",
            ))
            .await
            .expect("read registry");
        let mut versions = Vec::new();
        while let Some(row) = rows.next().await.expect("read registry") {
            versions.push(row.get::<i64>("version").expect("version"));
        }
        assert_eq!(versions, vec![1, 2]);
        assert_eq!(services[0].schema_version().await.expect("schema version"), 2);
    }
}
