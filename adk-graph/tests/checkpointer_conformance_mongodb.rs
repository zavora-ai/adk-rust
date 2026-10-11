//! The MongoDB checkpointer passes the shared conformance suite against a live server.
//!
//! Run with a server at `MONGODB_URL` (default `mongodb://localhost:27017`); a standalone
//! server is enough. The suite uses the `adk_store_conformance` database, and every case
//! writes under fresh identifiers:
//!
//! ```bash
//! MONGODB_URL=mongodb://localhost:27017 \
//!   cargo nextest run -p adk-graph --features mongodb --run-ignored only \
//!   --test checkpointer_conformance_mongodb
//! ```

#![cfg(feature = "mongodb")]

use std::sync::Arc;

use adk_graph::MongoCheckpointer;
use adk_graph::checkpoint::Checkpointer;
use adk_graph::conformance::CheckpointerHarness;

/// Each `open` connects a new client, as a restarted process would.
struct Mongo;

#[async_trait::async_trait]
impl CheckpointerHarness for Mongo {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        let url = std::env::var("MONGODB_URL")
            .unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
        let checkpointer = MongoCheckpointer::new(&url, "adk_store_conformance")
            .await
            .expect("connect to mongodb");
        checkpointer.migrate().await.expect("create checkpoint indexes");
        Arc::new(checkpointer)
    }
}

adk_graph::checkpointer_conformance_tests!(
    #[ignore = "requires a MongoDB server at MONGODB_URL"]
    harness = Mongo,
);
