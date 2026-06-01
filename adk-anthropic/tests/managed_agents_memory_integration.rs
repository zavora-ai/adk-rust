//! Integration tests for Managed Agents memory stores.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features managed-agents --test managed_agents_memory_integration -- --ignored --test-threads=1
//! ```

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{
    CreateMemoryParams, CreateMemoryStoreParams, ManagedAgentsClient, MemoryPrecondition,
    UpdateMemoryParams,
};

fn test_client() -> Option<ManagedAgentsClient> {
    ManagedAgentsClient::from_env().ok().or_else(|| {
        eprintln!("ANTHROPIC_API_KEY not set, skipping");
        None
    })
}

#[tokio::test]
#[ignore]
async fn test_create_and_delete_memory_store() {
    let Some(client) = test_client() else { return };

    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "ADK Test Store".to_string(),
            description: Some("Integration test memory store".to_string()),
        })
        .await
        .expect("failed to create memory store");

    assert!(!store.id.is_empty());
    assert_eq!(store.name.as_deref(), Some("ADK Test Store"));
    eprintln!("Created store: {}", store.id);

    // Get
    let retrieved = client.get_memory_store(&store.id).await.expect("failed to get store");
    assert_eq!(retrieved.id, store.id);

    // List
    let stores = client.list_memory_stores().await.expect("failed to list stores");
    assert!(stores.iter().any(|s| s.id == store.id));

    // Delete
    client.delete_memory_store(&store.id).await.expect("failed to delete store");
}

#[tokio::test]
#[ignore]
async fn test_memory_crud() {
    let Some(client) = test_client() else { return };

    // Create store
    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "Memory CRUD Test".to_string(),
            description: None,
        })
        .await
        .expect("failed to create store");

    // Create a memory
    let mem = client
        .create_memory(
            &store.id,
            CreateMemoryParams {
                path: "/preferences/style.md".to_string(),
                content: "Always use tabs, not spaces.".to_string(),
            },
        )
        .await
        .expect("failed to create memory");

    assert!(!mem.id.is_empty());
    assert_eq!(mem.path.as_deref(), Some("/preferences/style.md"));
    eprintln!("Created memory: {}", mem.id);

    // Get memory (content is only returned on get, not create)
    let retrieved = client.get_memory(&store.id, &mem.id).await.expect("failed to get memory");
    assert_eq!(retrieved.content.as_deref(), Some("Always use tabs, not spaces."));

    // List memories
    let memories = client.list_memories(&store.id).await.expect("failed to list memories");
    assert!(memories.iter().any(|m| m.id == mem.id));

    // Update memory
    let _updated = client
        .update_memory(
            &store.id,
            &mem.id,
            UpdateMemoryParams {
                content: Some("Always use 2-space indentation.".to_string()),
                path: None,
                precondition: None,
            },
        )
        .await
        .expect("failed to update memory");

    // Verify update via get
    let after_update =
        client.get_memory(&store.id, &mem.id).await.expect("failed to get updated memory");
    assert_eq!(after_update.content.as_deref(), Some("Always use 2-space indentation."));

    // Delete memory
    client.delete_memory(&store.id, &mem.id).await.expect("failed to delete memory");

    // Cleanup store
    client.delete_memory_store(&store.id).await.expect("failed to delete store");
}

#[tokio::test]
#[ignore]
async fn test_memory_optimistic_concurrency() {
    let Some(client) = test_client() else { return };

    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "Concurrency Test".to_string(),
            description: None,
        })
        .await
        .expect("failed to create store");

    // Create memory and get its hash
    let mem = client
        .create_memory(
            &store.id,
            CreateMemoryParams {
                path: "/config.txt".to_string(),
                content: "version=1".to_string(),
            },
        )
        .await
        .expect("failed to create memory");

    let sha = mem.content_sha256.expect("should have content_sha256");

    // Update with correct precondition
    let _updated = client
        .update_memory(
            &store.id,
            &mem.id,
            UpdateMemoryParams {
                content: Some("version=2".to_string()),
                path: None,
                precondition: Some(MemoryPrecondition::sha256(&sha)),
            },
        )
        .await
        .expect("failed to update with precondition");

    // Verify via get
    let after = client.get_memory(&store.id, &mem.id).await.expect("failed to get after update");
    assert_eq!(after.content.as_deref(), Some("version=2"));

    // Update with stale precondition should fail
    let result = client
        .update_memory(
            &store.id,
            &mem.id,
            UpdateMemoryParams {
                content: Some("version=3".to_string()),
                path: None,
                precondition: Some(MemoryPrecondition::sha256(&sha)), // stale!
            },
        )
        .await;
    assert!(result.is_err(), "update with stale precondition should fail");

    // Cleanup
    client.delete_memory_store(&store.id).await.expect("failed to delete store");
}

#[tokio::test]
#[ignore]
async fn test_memory_versions() {
    let Some(client) = test_client() else { return };

    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "Versions Test".to_string(),
            description: None,
        })
        .await
        .expect("failed to create store");

    // Create and update to generate versions
    let mem = client
        .create_memory(
            &store.id,
            CreateMemoryParams {
                path: "/notes.md".to_string(),
                content: "First version".to_string(),
            },
        )
        .await
        .expect("failed to create memory");

    let _ = client
        .update_memory(
            &store.id,
            &mem.id,
            UpdateMemoryParams {
                content: Some("Second version".to_string()),
                path: None,
                precondition: None,
            },
        )
        .await
        .expect("failed to update memory");

    // List versions
    let versions = client.list_memory_versions(&store.id).await.expect("failed to list versions");
    assert!(versions.len() >= 2, "should have at least 2 versions, got {}", versions.len());
    eprintln!("Found {} versions", versions.len());

    // Get a specific version
    let version =
        client.get_memory_version(&store.id, &versions[0].id).await.expect("failed to get version");
    assert!(!version.id.is_empty());

    // Cleanup
    client.delete_memory_store(&store.id).await.expect("failed to delete store");
}

#[tokio::test]
#[ignore]
async fn test_archive_memory_store() {
    let Some(client) = test_client() else { return };

    let store = client
        .create_memory_store(CreateMemoryStoreParams {
            name: "Archive Test".to_string(),
            description: None,
        })
        .await
        .expect("failed to create store");

    client.archive_memory_store(&store.id).await.expect("failed to archive store");

    let archived = client.get_memory_store(&store.id).await.expect("failed to get archived store");
    assert!(archived.archived_at.is_some(), "archived_at should be set");
}

/// Cleanup utility.
#[tokio::test]
#[ignore]
async fn test_cleanup_test_memory_stores() {
    let Some(client) = test_client() else { return };

    let stores = client.list_memory_stores().await.unwrap_or_default();
    let mut cleaned = 0;
    for store in &stores {
        let name = store.name.as_deref().unwrap_or("");
        if name.contains("Test") || name.contains("ADK") {
            if client.delete_memory_store(&store.id).await.is_ok() {
                cleaned += 1;
                eprintln!("Deleted store: {} ({})", store.id, name);
            }
        }
    }
    eprintln!("Cleaned up {cleaned} test stores");
}
