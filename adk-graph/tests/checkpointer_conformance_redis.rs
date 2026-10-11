//! The Redis checkpointer passes the shared conformance suite.
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`). Every case
//! writes under fresh identifiers, so a shared server is safe:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-graph --features redis --run-ignored only \
//!   --test checkpointer_conformance_redis
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_graph::RedisCheckpointer;
use adk_graph::checkpoint::Checkpointer;
use adk_graph::conformance::CheckpointerHarness;

/// Each `open` connects a new client, as a restarted process would.
struct Redis;

#[async_trait::async_trait]
impl CheckpointerHarness for Redis {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        let url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
        let checkpointer = RedisCheckpointer::new(&url).await.expect("connect to redis");
        checkpointer.migrate().await.expect("load scripts");
        Arc::new(checkpointer)
    }
}

adk_graph::checkpointer_conformance_tests!(
    #[ignore = "requires a Redis server at REDIS_URL"]
    harness = Redis,
);
