//! Environment wire types for the Gemini Interactions API.
//!
//! An environment is a server-side sandbox workspace attached to an interaction.
//! It represents the second state dimension (sandbox state), distinct from
//! conversation state managed via `previous_interaction_id`.
//!
//! Three forms are supported:
//! - Request a fresh remote sandbox (`"remote"`)
//! - Resume an existing environment by ID (e.g. `"env_abc123"`)
//! - Provide an inline configuration with sources and network rules
//!
//! All types in this module are gated behind the `interactions` feature flag.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ══════════════════════════════════════════════════════════════════════
// Environment (polymorphic request field)
// ══════════════════════════════════════════════════════════════════════

/// The `environment` request field: attach a sandbox to an interaction.
///
/// Three forms: request a fresh remote sandbox, resume an existing one by ID,
/// or provide an inline configuration with sources and network rules.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::{Environment, EnvironmentConfig};
///
/// // Request a fresh remote sandbox
/// let env = Environment::remote();
///
/// // Resume an existing environment
/// let env = Environment::resume("env_abc123");
///
/// // Provide inline configuration
/// let config = EnvironmentConfig::new();
/// let env = Environment::config(config);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Environment {
    /// A bare string: either the literal `"remote"` (fresh sandbox) or an
    /// existing environment ID (resume).
    Id(String),
    /// An inline environment configuration.
    Config(EnvironmentConfig),
}

impl Environment {
    /// Request a fresh remote sandbox.
    ///
    /// Serializes as the string `"remote"`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::Environment;
    ///
    /// let env = Environment::remote();
    /// let json = serde_json::to_string(&env).unwrap();
    /// assert_eq!(json, r#""remote""#);
    /// ```
    pub fn remote() -> Self {
        Self::Id("remote".to_string())
    }

    /// Resume an existing environment by ID.
    ///
    /// Serializes as the environment identifier string.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::Environment;
    ///
    /// let env = Environment::resume("env_abc123");
    /// let json = serde_json::to_string(&env).unwrap();
    /// assert_eq!(json, r#""env_abc123""#);
    /// ```
    pub fn resume(id: impl Into<String>) -> Self {
        Self::Id(id.into())
    }

    /// Provide an inline environment configuration.
    ///
    /// Serializes as the full configuration object with type, sources, and
    /// network rules.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::{Environment, EnvironmentConfig};
    ///
    /// let config = EnvironmentConfig::new();
    /// let env = Environment::config(config);
    /// ```
    pub fn config(config: EnvironmentConfig) -> Self {
        Self::Config(config)
    }
}

// ══════════════════════════════════════════════════════════════════════
// EnvironmentConfig
// ══════════════════════════════════════════════════════════════════════

/// Inline environment configuration: type, sources, and network rules.
///
/// Describes a sandbox environment to create, including filesystem sources to
/// seed and network egress rules.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::{EnvironmentConfig, EnvironmentSource, NetworkConfig};
///
/// let config = EnvironmentConfig::new()
///     .with_sources(vec![
///         EnvironmentSource::Repository {
///             source: "https://github.com/user/repo".to_string(),
///             target: "/workspace".to_string(),
///         },
///     ])
///     .with_network(NetworkConfig::disabled());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentConfig {
    /// Always `"remote"` for the current API version.
    #[serde(rename = "type")]
    pub env_type: String,

    /// Files to seed into the sandbox filesystem.
    ///
    /// When empty, the `sources` key is omitted from the serialized payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EnvironmentSource>,

    /// Network egress configuration.
    ///
    /// When `None`, the `network` key is omitted from the serialized payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkConfig>,
}

impl EnvironmentConfig {
    /// Create a new environment configuration with type `"remote"`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::EnvironmentConfig;
    ///
    /// let config = EnvironmentConfig::new();
    /// assert_eq!(config.env_type, "remote");
    /// ```
    pub fn new() -> Self {
        Self { env_type: "remote".to_string(), sources: Vec::new(), network: None }
    }

    /// Set the sources for this environment configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::{EnvironmentConfig, EnvironmentSource};
    ///
    /// let config = EnvironmentConfig::new().with_sources(vec![
    ///     EnvironmentSource::Inline {
    ///         content: "fn main() {}".to_string(),
    ///         target: "/workspace/main.rs".to_string(),
    ///     },
    /// ]);
    /// ```
    pub fn with_sources(mut self, sources: Vec<EnvironmentSource>) -> Self {
        self.sources = sources;
        self
    }

    /// Set the network configuration for this environment.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::{EnvironmentConfig, NetworkConfig};
    ///
    /// let config = EnvironmentConfig::new()
    ///     .with_network(NetworkConfig::disabled());
    /// ```
    pub fn with_network(mut self, network: NetworkConfig) -> Self {
        self.network = Some(network);
        self
    }
}

impl Default for EnvironmentConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ══════════════════════════════════════════════════════════════════════
// EnvironmentSource (polymorphic)
// ══════════════════════════════════════════════════════════════════════

/// A source entry that seeds the sandbox filesystem.
///
/// Each source has a `type` discriminator and provides content to be placed at a
/// `target` path within the sandbox.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::EnvironmentSource;
///
/// // Inline content (max 1 MB per file, 2 MB total)
/// let inline = EnvironmentSource::Inline {
///     content: "fn main() {}".to_string(),
///     target: "/workspace/main.rs".to_string(),
/// };
///
/// // Git repository (max 500 MB)
/// let repo = EnvironmentSource::Repository {
///     source: "https://github.com/user/repo".to_string(),
///     target: "/workspace".to_string(),
/// };
///
/// // Google Cloud Storage object (max 2 GB)
/// let gcs = EnvironmentSource::Gcs {
///     source: "gs://bucket/archive.tar.gz".to_string(),
///     target: "/workspace".to_string(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EnvironmentSource {
    /// Inline content (max 1 MB per file, 2 MB total).
    Inline {
        /// The file content to write.
        content: String,
        /// The target path within the sandbox.
        target: String,
    },
    /// A git repository (max 500 MB).
    Repository {
        /// The repository URL.
        source: String,
        /// The target path within the sandbox.
        target: String,
    },
    /// A Google Cloud Storage object (max 2 GB).
    Gcs {
        /// The GCS URI (e.g. `gs://bucket/object`).
        source: String,
        /// The target path within the sandbox.
        target: String,
    },
    /// A source type not modelled by this crate version.
    ///
    /// This variant ensures forward compatibility: unknown source types from
    /// future API revisions deserialize here rather than failing.
    #[serde(untagged)]
    Other(serde_json::Value),
}

// ══════════════════════════════════════════════════════════════════════
// NetworkConfig (polymorphic)
// ══════════════════════════════════════════════════════════════════════

/// Network egress configuration for an environment.
///
/// Either all outbound traffic is disabled, or an allowlist of domains is
/// specified with optional credential injection via transform maps.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::{NetworkConfig, NetworkRule, TransformMap};
/// use std::collections::HashMap;
///
/// // Block all outbound traffic
/// let disabled = NetworkConfig::disabled();
///
/// // Allow specific domains
/// let allowlist = NetworkConfig::allowlist(vec![
///     NetworkRule::new("crates.io"),
///     NetworkRule::new("*.github.com")
///         .with_transform(TransformMap(HashMap::from([
///             ("Authorization".to_string(), "Bearer ghp_xxx".to_string()),
///         ]))),
/// ]);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NetworkConfig {
    /// All outbound traffic is blocked. Serializes as `"disabled"`.
    Disabled(String),
    /// An allowlist of permitted outbound domains.
    Allowlist {
        /// The list of network rules permitting outbound access.
        allowlist: Vec<NetworkRule>,
    },
}

impl NetworkConfig {
    /// Block all outbound traffic.
    ///
    /// Serializes as the string `"disabled"`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::NetworkConfig;
    ///
    /// let net = NetworkConfig::disabled();
    /// let json = serde_json::to_string(&net).unwrap();
    /// assert_eq!(json, r#""disabled""#);
    /// ```
    pub fn disabled() -> Self {
        Self::Disabled("disabled".to_string())
    }

    /// Allow outbound traffic to the specified domains.
    ///
    /// Serializes as an object with an `"allowlist"` array.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::{NetworkConfig, NetworkRule};
    ///
    /// let net = NetworkConfig::allowlist(vec![
    ///     NetworkRule::new("crates.io"),
    /// ]);
    /// ```
    pub fn allowlist(rules: Vec<NetworkRule>) -> Self {
        Self::Allowlist { allowlist: rules }
    }
}

// ══════════════════════════════════════════════════════════════════════
// NetworkRule and TransformMap
// ══════════════════════════════════════════════════════════════════════

/// A single network allowlist entry.
///
/// Specifies a domain pattern and optional HTTP header injection for credential
/// forwarding through the egress proxy.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::{NetworkRule, TransformMap};
/// use std::collections::HashMap;
///
/// // Simple domain allowlist entry
/// let rule = NetworkRule::new("crates.io");
///
/// // Domain with credential injection
/// let rule = NetworkRule::new("*.github.com")
///     .with_transform(TransformMap(HashMap::from([
///         ("Authorization".to_string(), "Bearer ghp_xxx".to_string()),
///     ])));
/// ```
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkRule {
    /// The domain pattern: exact hostname, wildcard (`*.example.com`), or `*`.
    pub domain: String,

    /// HTTP headers injected by the egress proxy into matching requests.
    ///
    /// Values are sensitive credentials — see [`TransformMap`] for redaction
    /// behavior in debug output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TransformMap>,
}

impl NetworkRule {
    /// Create a new network rule for the given domain pattern.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::NetworkRule;
    ///
    /// let rule = NetworkRule::new("crates.io");
    /// assert_eq!(rule.domain, "crates.io");
    /// assert!(rule.transform.is_none());
    /// ```
    pub fn new(domain: impl Into<String>) -> Self {
        Self { domain: domain.into(), transform: None }
    }

    /// Attach a transform map for credential injection.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::{NetworkRule, TransformMap};
    /// use std::collections::HashMap;
    ///
    /// let rule = NetworkRule::new("*.github.com")
    ///     .with_transform(TransformMap(HashMap::from([
    ///         ("Authorization".to_string(), "Bearer ghp_xxx".to_string()),
    ///     ])));
    /// assert!(rule.transform.is_some());
    /// ```
    pub fn with_transform(mut self, transform: TransformMap) -> Self {
        self.transform = Some(transform);
        self
    }
}

impl fmt::Debug for NetworkRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkRule")
            .field("domain", &self.domain)
            .field("transform", &self.transform)
            .finish()
    }
}

/// A flat map of HTTP header names to credential values.
///
/// Implements a custom [`Debug`] that redacts all values to prevent credential
/// leakage in logs. Serialization preserves the actual values for the wire.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::TransformMap;
/// use std::collections::HashMap;
///
/// let map = TransformMap(HashMap::from([
///     ("Authorization".to_string(), "Bearer secret_token".to_string()),
/// ]));
///
/// // Debug output redacts values
/// let debug = format!("{:?}", map);
/// assert!(debug.contains("Authorization"));
/// assert!(debug.contains("[REDACTED]"));
/// assert!(!debug.contains("secret_token"));
///
/// // Serialization preserves actual values
/// let json = serde_json::to_string(&map).unwrap();
/// assert!(json.contains("secret_token"));
/// ```
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformMap(pub HashMap<String, String>);

impl fmt::Debug for TransformMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted: HashMap<&str, &str> =
            self.0.keys().map(|k| (k.as_str(), "[REDACTED]")).collect();
        f.debug_tuple("TransformMap").field(&redacted).finish()
    }
}
