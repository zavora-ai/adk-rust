//! Live integration test for the Vertex AI Memory Bank backend.
//!
//! Requires a provisioned Agent Engine with Memory Bank and ADC:
//!
//! ```bash
//! export GOOGLE_CLOUD_PROJECT=my-project
//! export GOOGLE_CLOUD_LOCATION=us-central1
//! export GOOGLE_CLOUD_AGENT_ENGINE_ID=1234567890
//! cargo nextest run -p adk-memory --features vertex-memory \
//!     --run-ignored all -E 'test(live_memory_bank_round_trip)'
//! ```

#![cfg(feature = "vertex-memory")]

use adk_core::Content;
use adk_memory::{
    MemoryEntry, MemoryService, SearchRequest, VertexAiMemoryBankService, VertexAiMemoryConfig,
};
use chrono::Utc;

#[tokio::test]
#[ignore = "requires ADC and a provisioned Agent Engine with Memory Bank"]
async fn live_memory_bank_round_trip() {
    let config = VertexAiMemoryConfig::from_env().expect("platform env vars set");
    let service = VertexAiMemoryBankService::new_with_adc(config).expect("ADC available");

    let user_id = format!("adk-rust-live-{}", Utc::now().timestamp());
    let app_name = "adk-rust-live-test";

    service
        .add_session(
            app_name,
            &user_id,
            "live-session",
            vec![MemoryEntry {
                content: Content::new("user")
                    .with_text("My favourite programming language is Rust."),
                author: "user".to_string(),
                timestamp: Utc::now(),
            }],
        )
        .await
        .expect("memories:generate succeeds");

    let response = service
        .search(SearchRequest {
            query: "what programming language does the user like".to_string(),
            user_id: user_id.clone(),
            app_name: app_name.to_string(),
            limit: Some(5),
            min_score: None,
            project_id: None,
        })
        .await
        .expect("memories:retrieve succeeds");
    assert!(
        !response.memories.is_empty(),
        "memory bank returned no memories for the generated fact"
    );

    // Leave no residue in the shared engine.
    service.delete_user(app_name, &user_id).await.expect("scope cleanup succeeds");
}
