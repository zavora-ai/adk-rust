//! Visual memory (spec 3.7 §7.15) over adk-rag's embedded LanceDB backend: the shared suite
//! against a local directory, each reopen a new connection, so the space record and the
//! chunks are read back from disk.

mod memory_suite;

use std::sync::Arc;

use adk_rag::VectorStore;
use adk_rag::lancedb::LanceDBVectorStore;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread")]
async fn the_suite_passes_on_lancedb_across_connections() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    memory_suite::run(|| {
        let path = path.clone();
        async move {
            Arc::new(LanceDBVectorStore::new(&path).await.unwrap()) as Arc<dyn VectorStore>
        }
    })
    .await;
}
