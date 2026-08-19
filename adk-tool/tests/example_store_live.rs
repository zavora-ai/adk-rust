//! Live integration test against a real Vertex AI Example Store.
//!
//! Requires ADC (`gcloud auth application-default login`), a pre-provisioned
//! Example Store in `us-central1`, and these environment variables:
//!
//! - `GOOGLE_CLOUD_PROJECT` — the Google Cloud project ID
//! - `GOOGLE_CLOUD_LOCATION` — the region (Example Store: `us-central1` only)
//! - `EXAMPLE_STORE_ID` — the Example Store ID (last resource-name segment)
//!
//! Run with:
//!
//! ```bash
//! cargo nextest run -p adk-tool --features example-store \
//!     --run-ignored all -E 'test(example_store_live)'
//! ```

#![cfg(feature = "example-store")]

use adk_core::Content;
use adk_tool::example_store::{
    ContentsExample, Example, ExampleStoreClient, ExampleStoreConfig, FetchExamplesRequest,
    SearchExamplesRequest, StoredContentsExample, UpsertExamplesRequest,
};

#[tokio::test]
#[ignore = "requires ADC credentials and a provisioned Example Store (GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, EXAMPLE_STORE_ID)"]
async fn example_store_live_upsert_search_fetch_roundtrip() {
    let config = ExampleStoreConfig::from_env().expect("example store env vars must be set");
    let client = ExampleStoreClient::new_with_adc(config).expect("build ADC client");

    let example_id = format!("adk-live-test-{}", uuid::Uuid::new_v4().simple());
    let upsert = client
        .upsert_examples(
            UpsertExamplesRequest::new(vec![
                Example::new(
                    StoredContentsExample::new(ContentsExample::new(
                        vec![Content::new("user").with_text("What is the capital of France?")],
                        vec![Content::new("model").with_text("The capital of France is Paris.")],
                    ))
                    .with_search_key("What is the capital of France?"),
                )
                .with_example_id(&example_id),
            ])
            .with_overwrite(true),
        )
        .await
        .expect("upsert should succeed");
    assert_eq!(upsert.results.len(), 1);
    let stored = upsert.results[0].example.as_ref().unwrap_or_else(|| {
        panic!("upsert result must carry the stored example: {:?}", upsert.results[0].status)
    });
    assert_eq!(stored.example_id.as_deref(), Some(example_id.as_str()));

    let search = client
        .search_examples(SearchExamplesRequest::by_search_key("capital cities of Europe", 5))
        .await
        .expect("search should succeed");
    assert!(!search.results.is_empty(), "search should return the upserted example");

    let fetch = client
        .fetch_examples(FetchExamplesRequest::new().with_example_ids(vec![example_id.clone()]))
        .await
        .expect("fetch should succeed");
    assert_eq!(fetch.examples.len(), 1);
    assert_eq!(fetch.examples[0].example_id.as_deref(), Some(example_id.as_str()));
}
