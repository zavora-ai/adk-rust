//! Integration tests for Managed Agents Dreams API.
//!
//! Dreams is a Research Preview feature requiring access. These tests
//! skip gracefully if the API returns a permission error.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features managed-agents --test managed_agents_dreams_integration -- --ignored --test-threads=1
//! ```

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{
    CreateDreamParams, CreateMemoryParams, CreateMemoryStoreParams, DreamStatus,
    ManagedAgentsClient,
};

fn test_client() -> Option<ManagedAgentsClient> {
    ManagedAgentsClient::from_env().ok().or_else(|| {
        eprintln!("ANTHROPIC_API_KEY not set, skipping");
        None
    })
}

#[tokio::test]
#[ignore]
async fn test_create_and_poll_dream() {
    let Some(client) = test_client() else { return };

    // Create a memory store with some content
    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "Dream Test Store".to_string(),
            description: Some("Test store for dreaming".to_string()),
        })
        .await
        .expect("failed to create store");

    // Seed with a memory
    let _ = client
        .create_memory(
            &store.id,
            CreateMemoryParams {
                path: "/notes.md".to_string(),
                content: "User prefers dark mode. User likes concise responses.".to_string(),
            },
        )
        .await
        .expect("failed to create memory");

    // Create a dream (may fail if Research Preview access not granted)
    let dream_result = client
        .create_dream(
            CreateDreamParams::new(&store.id, vec![], "claude-sonnet-4-6")
                .with_instructions("Consolidate user preferences."),
        )
        .await;

    match dream_result {
        Ok(dream) => {
            assert!(!dream.id.is_empty());
            assert_eq!(dream.status, DreamStatus::Pending);
            eprintln!("Created dream: {} (status: {:?})", dream.id, dream.status);

            // Poll once
            let polled = client.get_dream(&dream.id).await.expect("failed to get dream");
            eprintln!("Polled dream: {:?}", polled.status);

            // Cancel it (don't wait for completion — saves cost)
            if !polled.is_terminal() {
                let _ = client.cancel_dream(&dream.id).await;
                eprintln!("Canceled dream");
            }

            // Archive it
            // Wait a moment for cancel to propagate
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let final_dream =
                client.get_dream(&dream.id).await.expect("failed to get dream after cancel");
            if final_dream.is_terminal() {
                let _ = client.archive_dream(&dream.id).await;
                eprintln!("Archived dream");
            }
        }
        Err(e) => {
            let err_str = format!("{e}");
            if err_str.contains("permission")
                || err_str.contains("403")
                || err_str.contains("not enabled")
                || err_str.contains("beta")
                || err_str.contains("not found")
                || err_str.contains("Not found")
            {
                eprintln!("Dreams API not available (Research Preview): {e}");
                eprintln!("Skipping test — request access to enable dreaming.");
            } else {
                panic!("unexpected error creating dream: {e}");
            }
        }
    }

    // Cleanup
    client.delete_memory_store(&store.id).await.expect("failed to delete store");
}

#[tokio::test]
#[ignore]
async fn test_list_dreams() {
    let Some(client) = test_client() else { return };

    let result = client.list_dreams().await;
    match result {
        Ok(dreams) => {
            eprintln!("Listed {} dreams", dreams.len());
        }
        Err(e) => {
            let err_str = format!("{e}");
            if err_str.contains("permission")
                || err_str.contains("403")
                || err_str.contains("beta")
                || err_str.contains("not found")
                || err_str.contains("Not found")
            {
                eprintln!("Dreams API not available: {e}");
            } else {
                panic!("unexpected error listing dreams: {e}");
            }
        }
    }
}
