//! Integration tests for the Managed Agents Vaults API.
//!
//! These tests require a real `ANTHROPIC_API_KEY` environment variable and are
//! marked `#[ignore]` so they don't run in CI by default.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features managed-agents --test managed_agents_vaults_integration -- --ignored
//! ```

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{
    CreateCredentialParams, CreateVaultParams, ManagedAgentsClient,
};

// ─── Test Infrastructure ─────────────────────────────────────────────────────

fn test_client() -> Option<ManagedAgentsClient> {
    match ManagedAgentsClient::from_env() {
        Ok(client) => Some(client),
        Err(_) => {
            eprintln!("ANTHROPIC_API_KEY not set, skipping integration test");
            None
        }
    }
}

// ─── Vault Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_create_and_archive_vault() {
    let Some(client) = test_client() else { return };

    // Create a vault
    let vault = client
        .create_vault(CreateVaultParams {
            display_name: "ADK Test Vault".to_string(),
            metadata: Some(serde_json::Map::from_iter([(
                "test_id".to_string(),
                serde_json::json!("integration_test"),
            )])),
        })
        .await
        .expect("failed to create vault");

    assert!(!vault.id.is_empty());
    assert_eq!(vault.display_name.as_deref(), Some("ADK Test Vault"));
    eprintln!("Created vault: {}", vault.id);

    // Get the vault
    let retrieved = client.get_vault(&vault.id).await.expect("failed to get vault");
    assert_eq!(retrieved.id, vault.id);

    // List vaults (should include ours)
    let vaults = client.list_vaults().await.expect("failed to list vaults");
    assert!(vaults.iter().any(|v| v.id == vault.id), "created vault should appear in list");

    // Archive the vault
    client.archive_vault(&vault.id).await.expect("failed to archive vault");
    eprintln!("Archived vault: {}", vault.id);
}

#[tokio::test]
#[ignore]
async fn test_create_and_delete_vault() {
    let Some(client) = test_client() else { return };

    let vault = client
        .create_vault(CreateVaultParams {
            display_name: "ADK Delete Test Vault".to_string(),
            metadata: None,
        })
        .await
        .expect("failed to create vault");

    assert!(!vault.id.is_empty());

    // Delete the vault (hard delete)
    client.delete_vault(&vault.id).await.expect("failed to delete vault");

    // Verify it's gone
    let result = client.get_vault(&vault.id).await;
    assert!(result.is_err(), "vault should not exist after deletion");
}

#[tokio::test]
#[ignore]
async fn test_create_static_bearer_credential() {
    let Some(client) = test_client() else { return };

    // Create a vault
    let vault = client
        .create_vault(CreateVaultParams {
            display_name: "Credential Test Vault".to_string(),
            metadata: None,
        })
        .await
        .expect("failed to create vault");

    // Add a static bearer credential
    let credential = client
        .create_credential(
            &vault.id,
            CreateCredentialParams::static_bearer(
                "Test MCP Token",
                "https://mcp.example.com/test",
                "test-bearer-token-12345",
            ),
        )
        .await
        .expect("failed to create credential");

    assert!(!credential.id.is_empty());
    eprintln!("Created credential: {}", credential.id);

    // List credentials
    let credentials = client.list_credentials(&vault.id).await.expect("failed to list credentials");
    assert!(
        credentials.iter().any(|c| c.id == credential.id),
        "created credential should appear in list"
    );

    // Get credential
    let retrieved =
        client.get_credential(&vault.id, &credential.id).await.expect("failed to get credential");
    assert_eq!(retrieved.id, credential.id);

    // Archive credential
    client
        .archive_credential(&vault.id, &credential.id)
        .await
        .expect("failed to archive credential");

    // Clean up vault
    client.delete_vault(&vault.id).await.expect("failed to delete vault");
}

#[tokio::test]
#[ignore]
async fn test_create_mcp_oauth_credential() {
    let Some(client) = test_client() else { return };

    let vault = client
        .create_vault(CreateVaultParams {
            display_name: "OAuth Test Vault".to_string(),
            metadata: None,
        })
        .await
        .expect("failed to create vault");

    // Add an MCP OAuth credential (without refresh for simplicity)
    let credential = client
        .create_credential(
            &vault.id,
            CreateCredentialParams::mcp_oauth(
                "Test OAuth Token",
                "https://mcp.example.com/oauth-test",
                "fake-access-token-xyz",
                "2099-12-31T23:59:59Z",
                None, // no refresh config
            ),
        )
        .await
        .expect("failed to create OAuth credential");

    assert!(!credential.id.is_empty());
    eprintln!("Created OAuth credential: {}", credential.id);

    // Clean up
    client
        .archive_credential(&vault.id, &credential.id)
        .await
        .expect("failed to archive credential");
    client.delete_vault(&vault.id).await.expect("failed to delete vault");
}

#[tokio::test]
#[ignore]
async fn test_delete_credential() {
    let Some(client) = test_client() else { return };

    let vault = client
        .create_vault(CreateVaultParams {
            display_name: "Delete Cred Test Vault".to_string(),
            metadata: None,
        })
        .await
        .expect("failed to create vault");

    let credential = client
        .create_credential(
            &vault.id,
            CreateCredentialParams::static_bearer(
                "Deletable Token",
                "https://mcp.example.com/delete-test",
                "token-to-delete",
            ),
        )
        .await
        .expect("failed to create credential");

    // Hard delete
    client.delete_credential(&vault.id, &credential.id).await.expect("failed to delete credential");

    // Verify it's gone
    let result = client.get_credential(&vault.id, &credential.id).await;
    assert!(result.is_err(), "credential should not exist after deletion");

    // Clean up vault
    client.delete_vault(&vault.id).await.expect("failed to delete vault");
}

/// Cleanup utility: archive all test vaults.
#[tokio::test]
#[ignore]
async fn test_cleanup_test_vaults() {
    let Some(client) = test_client() else { return };

    let vaults = client.list_vaults().await.unwrap_or_default();
    let mut cleaned = 0;
    for vault in &vaults {
        let name = vault.display_name.as_deref().unwrap_or("");
        if name.contains("Test") || name.contains("ADK") {
            if client.archive_vault(&vault.id).await.is_ok() {
                cleaned += 1;
                eprintln!("Archived vault: {} ({})", vault.id, name);
            }
        }
    }
    eprintln!("Cleaned up {cleaned} test vaults");
}
