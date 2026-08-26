//! Live round-trip against a real Agent Retrieval project.
//!
//! Data-plane only, mirroring how the pgvector/Qdrant live tests manage
//! tables: the test creates a uniquely-named collection, exercises the
//! store, and deletes the collection. Requires ADC plus
//! `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION` (an Agent Retrieval
//! region, e.g. `us-central1`).

#![cfg(feature = "agent-retrieval")]

use adk_rag::VectorStore;
use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
use adk_rag::{Chunk, RagError};
use std::collections::HashMap;

fn chunk(id: &str, text: &str, embedding: Vec<f32>) -> Chunk {
    Chunk {
        id: id.to_string(),
        text: text.to_string(),
        embedding,
        metadata: HashMap::from([("suite".to_string(), "live".to_string())]),
        document_id: "live-doc".to_string(),
    }
}

#[tokio::test]
#[ignore = "requires ADC, GOOGLE_CLOUD_PROJECT, and GOOGLE_CLOUD_LOCATION"]
async fn agent_retrieval_live_round_trip() -> Result<(), RagError> {
    let config = AgentRetrievalConfig::from_env()?;
    let store = AgentRetrievalStore::new_with_adc(config)?;
    let collection = format!("adk-live-{}", std::process::id());

    store.create_collection(&collection, 4).await?;
    let result = async {
        let stored = vec![
            chunk("live-a", "alpha", vec![1.0, 0.0, 0.0, 0.0]),
            chunk("live-b", "beta", vec![0.0, 1.0, 0.0, 0.0]),
        ];
        store.upsert(&collection, &stored).await?;

        let results = store.search(&collection, &[0.0, 1.0, 0.0, 0.0], 2).await?;
        assert!(!results.is_empty(), "live search returned no results");
        assert_eq!(results[0].chunk.id, "live-b");

        store.delete(&collection, &["live-a", "live-b"]).await?;
        Ok::<_, RagError>(())
    }
    .await;

    // Always delete the collection, then surface the inner result.
    store.delete_collection(&collection).await?;
    result
}
