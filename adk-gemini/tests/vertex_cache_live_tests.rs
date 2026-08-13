//! Live integration test for cached content on the Vertex AI backend.
//!
//! Exercises the full cache lifecycle (create with TTL, get, update TTL,
//! list, delete) against a real Vertex AI project. Requires real Google
//! Cloud credentials, so the test is marked `#[ignore]` and must be run
//! manually.
//!
//! # Required Environment Variables
//!
//! - `GOOGLE_CLOUD_PROJECT` — GCP project ID with Vertex AI API enabled
//! - `GOOGLE_CLOUD_LOCATION` — GCP location (defaults to `us-central1`)
//! - Application Default Credentials must be configured:
//!   `gcloud auth application-default login`
//!
//! # Running
//!
//! ```bash
//! cargo test -p adk-gemini --features vertex \
//!     --test vertex_cache_live_tests -- --ignored
//! ```

#![cfg(feature = "vertex")]

use adk_gemini::{CacheExpirationRequest, Gemini, Model};
use futures::TryStreamExt;
use std::time::Duration;

/// Cache lifecycle on live Vertex AI: create with a TTL, get, update the TTL,
/// verify the cache appears in list, then delete.
#[tokio::test]
#[ignore]
async fn vertex_cached_content_lifecycle() {
    let project_id =
        std::env::var("GOOGLE_CLOUD_PROJECT").expect("GOOGLE_CLOUD_PROJECT env var is required");
    let location =
        std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".to_string());

    let gemini = Gemini::with_google_cloud_adc_model(&project_id, &location, Model::Gemini25Flash)
        .expect("failed to build Vertex client with ADC");

    // Vertex enforces a minimum cached token count (1024 for 2.5 Flash), so
    // the cached context must be large enough.
    let large_context = "The quick brown fox jumps over the lazy dog. ".repeat(400);

    // Create with a TTL.
    let cache = gemini
        .create_cache()
        .with_display_name("adk-gemini vertex live test cache")
        .expect("display name within limit")
        .with_system_instruction("You are a test fixture. Answer briefly.")
        .with_user_message(large_context)
        .with_ttl(Duration::from_secs(600))
        .execute()
        .await
        .expect("cache creation should succeed");
    let cache_name = cache.name().to_string();
    assert!(cache_name.contains("cachedContents/"), "unexpected cache name: {cache_name}");

    // Get.
    let fetched = cache.get().await.expect("get should succeed");
    assert_eq!(fetched.name, cache_name);
    let first_expiry = fetched.expiration.expire_time.expect("expire_time should be set");

    // Update TTL (the runner's cache-refresh path).
    let updated = cache
        .update(CacheExpirationRequest::from_ttl(Duration::from_secs(3600)))
        .await
        .expect("ttl update should succeed");
    let refreshed_expiry = updated.expiration.expire_time.expect("expire_time should be set");
    assert!(
        refreshed_expiry > first_expiry,
        "TTL refresh should push expiry later: {first_expiry} -> {refreshed_expiry}"
    );

    // List contains it.
    let summaries: Vec<_> =
        gemini.list_cached_contents(50).try_collect().await.expect("list should succeed");
    assert!(
        summaries.iter().any(|summary| summary.name == cache_name),
        "created cache '{cache_name}' should appear in list"
    );

    // Delete.
    cache.delete().await.map_err(|(_, e)| e).expect("delete should succeed");
}
