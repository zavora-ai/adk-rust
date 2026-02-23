//! Configuration types for Amazon Bedrock provider.

use serde::{Deserialize, Serialize};

/// TTL options for Bedrock prompt caching.
///
/// Bedrock supports explicit prompt caching via `CachePoint` blocks in the
/// Converse API. The TTL controls how long cached content is retained.
///
/// # Example
///
/// ```rust
/// use adk_model::bedrock::BedrockCacheTtl;
///
/// let ttl = BedrockCacheTtl::FiveMinutes; // default
/// let ttl = BedrockCacheTtl::OneHour;     // Claude Opus 4.5, Haiku 4.5, Sonnet 4.5
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BedrockCacheTtl {
    /// 5-minute TTL (default, supported by all cacheable models).
    #[default]
    FiveMinutes,
    /// 1-hour TTL (supported by Claude Opus 4.5, Haiku 4.5, Sonnet 4.5).
    OneHour,
}

/// Configuration for Bedrock prompt caching.
///
/// When enabled on [`BedrockConfig`], the Bedrock request builder injects
/// `CachePoint` blocks after system prompts and tool definitions.
///
/// # Example
///
/// ```rust
/// use adk_model::bedrock::{BedrockCacheConfig, BedrockCacheTtl};
///
/// // Default 5-minute TTL
/// let config = BedrockCacheConfig::default();
///
/// // 1-hour TTL for supported models
/// let config = BedrockCacheConfig { ttl: BedrockCacheTtl::OneHour };
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BedrockCacheConfig {
    /// Cache time-to-live. Defaults to 5 minutes.
    pub ttl: BedrockCacheTtl,
}

/// Configuration for Amazon Bedrock.
///
/// Bedrock uses AWS IAM/STS authentication rather than API keys.
/// Credentials are loaded from the environment via the AWS SDK
/// (environment variables, shared config, IMDS, etc.).
///
/// # Inference Profiles
///
/// Newer Bedrock models require cross-region inference profile IDs
/// (prefixed with `us.` or `global.`) instead of raw model IDs.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::bedrock::BedrockConfig;
///
/// // Default: us-east-1, Claude Sonnet 4.6
/// let config = BedrockConfig::default();
///
/// // Custom region and model
/// let config = BedrockConfig::new("eu-west-1", "us.anthropic.claude-haiku-4-5-20251001-v1:0");
///
/// // With a custom endpoint (e.g., VPC endpoint)
/// let config = BedrockConfig::new("us-west-2", "us.anthropic.claude-sonnet-4-6")
///     .with_endpoint_url("https://vpce-xxx.bedrock-runtime.us-west-2.vpce.amazonaws.com");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BedrockConfig {
    /// AWS region for the Bedrock endpoint (e.g., `"us-east-1"`).
    pub region: String,
    /// Bedrock model identifier (e.g., `"us.anthropic.claude-sonnet-4-6"`).
    pub model_id: String,
    /// Optional custom endpoint URL (e.g., a VPC endpoint).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    /// Optional prompt caching configuration.
    ///
    /// When set, the Bedrock request builder injects `CachePoint` blocks
    /// after system prompts and tool definitions in the Converse API request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_caching: Option<BedrockCacheConfig>,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            region: "us-east-1".to_string(),
            model_id: "us.anthropic.claude-sonnet-4-6".to_string(),
            endpoint_url: None,
            prompt_caching: None,
        }
    }
}

impl BedrockConfig {
    /// Create a new Bedrock config with the given region and model ID.
    pub fn new(region: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self { region: region.into(), model_id: model_id.into(), ..Default::default() }
    }

    /// Set a custom endpoint URL (e.g., a VPC endpoint).
    pub fn with_endpoint_url(mut self, url: impl Into<String>) -> Self {
        self.endpoint_url = Some(url.into());
        self
    }

    /// Enable prompt caching with the given configuration.
    ///
    /// When enabled, the Bedrock request builder injects `CachePoint` blocks
    /// after system prompts and tool definitions in the Converse API request.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_model::bedrock::{BedrockConfig, BedrockCacheConfig, BedrockCacheTtl};
    ///
    /// let config = BedrockConfig::default()
    ///     .with_prompt_caching(BedrockCacheConfig { ttl: BedrockCacheTtl::FiveMinutes });
    /// ```
    pub fn with_prompt_caching(mut self, config: BedrockCacheConfig) -> Self {
        self.prompt_caching = Some(config);
        self
    }
}
