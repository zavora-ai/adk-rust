//! Configuration types for Amazon Bedrock provider.

use serde::{Deserialize, Serialize};

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
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            region: "us-east-1".to_string(),
            model_id: "us.anthropic.claude-sonnet-4-6".to_string(),
            endpoint_url: None,
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
}
