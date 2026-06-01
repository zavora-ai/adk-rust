//! Vault and credential management for MCP server authentication.
//!
//! Vaults store per-user credentials for third-party MCP servers. You create a vault
//! once per end user, add credentials for each MCP server they need access to, then
//! reference the vault ID at session creation.
//!
//! See: <https://platform.claude.com/docs/en/managed-agents/authenticate-with-vaults>

use serde::{Deserialize, Serialize};

// ─── Vault Types ─────────────────────────────────────────────────────────────

/// A vault containing credentials for an end user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Vault {
    /// Unique identifier (e.g., `"vlt_01ABC..."`).
    pub id: String,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Arbitrary metadata for mapping to your own user records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// ISO 8601 timestamp of archival (null if active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parameters for creating a new vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateVaultParams {
    /// Human-readable display name for the vault.
    pub display_name: String,
    /// Optional metadata for mapping to your own user records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

// ─── Credential Types ────────────────────────────────────────────────────────

/// A credential stored in a vault.
///
/// Secret fields (token, access_token, refresh_token, client_secret) are
/// write-only and never returned in API responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credential {
    /// Unique identifier (e.g., `"vcrd_01ABC..."`).
    pub id: String,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The vault this credential belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// ISO 8601 timestamp of archival (null if active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parameters for creating a new credential in a vault.
///
/// The `auth` field contains the credential payload (mcp_oauth or static_bearer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateCredentialParams {
    /// Human-readable display name.
    pub display_name: String,
    /// The authentication payload.
    pub auth: serde_json::Value,
}

impl CreateCredentialParams {
    /// Create a static bearer token credential.
    ///
    /// # Arguments
    /// * `display_name` - Human-readable name
    /// * `mcp_server_url` - The MCP server URL this credential authenticates to
    /// * `token` - The bearer token
    pub fn static_bearer(
        display_name: impl Into<String>,
        mcp_server_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            display_name: display_name.into(),
            auth: serde_json::json!({
                "type": "static_bearer",
                "mcp_server_url": mcp_server_url.into(),
                "token": token.into(),
            }),
        }
    }

    /// Create an MCP OAuth credential.
    ///
    /// # Arguments
    /// * `display_name` - Human-readable name
    /// * `mcp_server_url` - The MCP server URL
    /// * `access_token` - The OAuth access token
    /// * `expires_at` - Token expiration (ISO 8601)
    /// * `refresh` - Optional refresh configuration (pass `None` for non-refreshable tokens)
    pub fn mcp_oauth(
        display_name: impl Into<String>,
        mcp_server_url: impl Into<String>,
        access_token: impl Into<String>,
        expires_at: impl Into<String>,
        refresh: Option<serde_json::Value>,
    ) -> Self {
        let mut auth = serde_json::json!({
            "type": "mcp_oauth",
            "mcp_server_url": mcp_server_url.into(),
            "access_token": access_token.into(),
            "expires_at": expires_at.into(),
        });
        if let Some(refresh_config) = refresh {
            auth.as_object_mut().unwrap().insert("refresh".to_string(), refresh_config);
        }
        Self { display_name: display_name.into(), auth }
    }
}

/// Parameters for updating/rotating a credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateCredentialParams {
    /// The updated authentication payload.
    pub auth: serde_json::Value,
}

/// Response from the OAuth validation endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialValidation {
    /// Credential ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    /// Vault ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    /// When the validation was performed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validated_at: Option<String>,
    /// Whether the credential has a refresh token.
    #[serde(default)]
    pub has_refresh_token: bool,
    /// Validation status: `"valid"`, `"invalid"`, or `"unknown"`.
    pub status: String,
    /// MCP probe details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_probe: Option<serde_json::Value>,
    /// Refresh attempt details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<serde_json::Value>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// List response wrapper for vaults/credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct VaultListResponse<T> {
    pub data: Vec<T>,
}
