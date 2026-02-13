use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::types::Content;
use crate::{Model, Tool, ToolConfig};

/// Cached content resource returned by the API (with all server-provided fields).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CachedContent {
    /// The resource name of the cached content.
    /// Format: cachedContents/{id}
    pub name: String,

    /// The name of the model used for cached content.
    pub model: Model,

    /// Output only. Creation time of the cached content.
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,

    /// Output only. Last update time of the cached content.
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,

    /// Output only. Usage metadata for the cached content.
    pub usage_metadata: CacheUsageMetadata,

    /// Expiration information for the cached content.
    #[serde(flatten)]
    pub expiration: CacheExpirationResponse,

    /// The user-generated display name (if provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// The cached contents (may be omitted in some API responses for size).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<Vec<Content>>,

    /// The cached tools (may be omitted in some API responses for size).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,

    /// The cached system instruction (may be omitted in some API responses for size).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,

    /// The cached tool config (may be omitted in some API responses for size).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
}

/// Usage metadata specifically for cached content (more predictable than general UsageMetadata).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheUsageMetadata {
    /// Total tokens in the cached content.
    pub total_token_count: i32,
}

/// Expiration configuration for cached content in requests (union type).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum CacheExpirationRequest {
    /// Timestamp in UTC of when this resource is considered expired.
    /// Uses RFC 3339 format.
    ExpireTime {
        #[serde(with = "time::serde::rfc3339")]
        expire_time: OffsetDateTime,
    },
    /// New TTL for this resource, input only.
    /// A duration in seconds with up to nine fractional digits, ending with 's'.
    /// Example: "3.5s" or "86400s".
    Ttl { ttl: String },
}

impl CacheExpirationRequest {
    /// Create expiration with TTL from a Duration.
    pub fn from_ttl(duration: std::time::Duration) -> Self {
        Self::Ttl { ttl: format!("{}s", duration.as_secs()) }
    }

    /// Create expiration with specific expire time.
    pub fn from_expire_time(expire_time: OffsetDateTime) -> Self {
        Self::ExpireTime { expire_time }
    }
}

/// Expiration information in cached content responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheExpirationResponse {
    /// Timestamp in UTC of when this resource is considered expired.
    /// This is always provided on output, regardless of what was sent on input.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expire_time: Option<OffsetDateTime>,
    /// TTL that was set for this resource (input only, may not be present in response).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

/// Request for creating cached content.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateCachedContentRequest {
    /// The user-generated meaningful display name of the cached content.
    /// Maximum 128 Unicode characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Required. The name of the model to use for cached content.
    pub model: Model,

    /// Optional. Input only. Immutable. The content to cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<Vec<Content>>,

    /// Optional. Input only. Immutable. A list of tools the model may use to generate the next response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,

    /// Optional. Input only. Immutable. Developer set system instruction. Currently text only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,

    /// Optional. Input only. Immutable. Tool config. This config is shared for all tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,

    /// Expiration configuration for the cached content.
    #[serde(flatten)]
    pub expiration: CacheExpirationRequest,
}

/// Summary of cached content (used in list operations, may omit large content fields).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CachedContentSummary {
    /// The resource name of the cached content.
    pub name: String,

    /// The name of the model used for cached content.
    pub model: Model,

    /// Creation time of the cached content.
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,

    /// Last update time of the cached content.
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,

    /// Usage metadata for the cached content.
    pub usage_metadata: CacheUsageMetadata,

    /// Expiration information.
    #[serde(flatten)]
    pub expiration: CacheExpirationResponse,

    /// Display name if provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Response from listing cached contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListCachedContentsResponse {
    /// The list of cached content summaries.
    #[serde(default)]
    pub cached_contents: Vec<CachedContentSummary>,
    /// A token to retrieve the next page of results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}
