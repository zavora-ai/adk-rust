//! Business context loader with hot-reload support via [`ArcSwap`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::{ArcSwap, Guard};
use awp_types::BusinessContext;
use tokio::time::{Duration, interval};

use crate::config::AwpConfigError;

/// Loads and manages a [`BusinessContext`] from a TOML file.
///
/// The context is stored behind an [`ArcSwap`] for lock-free reads and
/// atomic updates during hot-reload.
#[derive(Debug)]
pub struct BusinessContextLoader {
    context: Arc<ArcSwap<BusinessContext>>,
}

impl BusinessContextLoader {
    /// Parse and validate a `business.toml` file.
    ///
    /// # Errors
    ///
    /// Returns [`AwpConfigError`] if the file cannot be read, contains invalid
    /// TOML, or fails validation.
    pub fn from_file(path: &Path) -> Result<Self, AwpConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| AwpConfigError::FileRead {
            path: path.display().to_string(),
            source: e,
        })?;
        let ctx: BusinessContext = toml::from_str(&content).map_err(|e| {
            AwpConfigError::TomlParse { path: path.display().to_string(), source: e }
        })?;
        validate_business_context(&ctx)?;
        Ok(Self { context: Arc::new(ArcSwap::from_pointee(ctx)) })
    }

    /// Get a snapshot of the current business context.
    pub fn load(&self) -> Guard<Arc<BusinessContext>> {
        self.context.load()
    }

    /// Get a cloneable reference to the underlying [`ArcSwap`].
    pub fn context_ref(&self) -> Arc<ArcSwap<BusinessContext>> {
        self.context.clone()
    }

    /// Start watching the file for changes, re-parsing and swapping on
    /// modification.
    ///
    /// Spawns a tokio task that polls the file every 5 seconds. Parse or
    /// validation errors are logged but do not crash the watcher.
    pub async fn watch(&self, path: PathBuf) -> Result<(), AwpConfigError> {
        let context = self.context.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(5));
            let mut last_content = String::new();
            loop {
                tick.tick().await;
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => {
                        if content == last_content {
                            continue;
                        }
                        match toml::from_str::<BusinessContext>(&content) {
                            Ok(ctx) => {
                                if let Err(e) = validate_business_context(&ctx) {
                                    tracing::warn!(
                                        "business.toml validation failed on reload: {e}"
                                    );
                                    continue;
                                }
                                context.store(Arc::new(ctx));
                                last_content = content;
                                tracing::info!("business.toml reloaded successfully");
                            }
                            Err(e) => {
                                tracing::warn!("business.toml parse error on reload: {e}");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to read business.toml for reload: {e}");
                    }
                }
            }
        });
        Ok(())
    }
}

/// Validate that every capability has a non-empty name and endpoint.
fn validate_business_context(ctx: &BusinessContext) -> Result<(), AwpConfigError> {
    for (i, cap) in ctx.capabilities.iter().enumerate() {
        if cap.name.is_empty() {
            return Err(AwpConfigError::ValidationError { index: i, field: "name".into() });
        }
        if cap.endpoint.is_empty() {
            return Err(AwpConfigError::ValidationError { index: i, field: "endpoint".into() });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::{BusinessCapability, TrustLevel};

    fn sample_toml() -> String {
        r#"
site_name = "Test Site"
site_description = "A test site"
domain = "example.com"

[[capabilities]]
name = "read_data"
description = "Read data"
endpoint = "/api/data"
method = "GET"
access_level = "anonymous"

[[policies]]
name = "privacy"
description = "Privacy policy"
policy_type = "privacy"
"#
        .to_string()
    }

    #[test]
    fn test_from_file_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("business.toml");
        std::fs::write(&path, sample_toml()).unwrap();

        let loader = BusinessContextLoader::from_file(&path).unwrap();
        let ctx = loader.load();
        assert_eq!(ctx.site_name, "Test Site");
        assert_eq!(ctx.capabilities.len(), 1);
    }

    #[test]
    fn test_from_file_missing() {
        let result = BusinessContextLoader::from_file(Path::new("/nonexistent/business.toml"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AwpConfigError::FileRead { .. }));
    }

    #[test]
    fn test_from_file_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("business.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();

        let result = BusinessContextLoader::from_file(&path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AwpConfigError::TomlParse { .. }));
    }

    #[test]
    fn test_from_file_empty_capability_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("business.toml");
        let toml = r#"
site_name = "Test"
site_description = "Test"
domain = "example.com"
policies = []

[[capabilities]]
name = ""
description = "Bad"
endpoint = "/api/bad"
method = "GET"
access_level = "anonymous"
"#;
        std::fs::write(&path, toml).unwrap();

        let result = BusinessContextLoader::from_file(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            AwpConfigError::ValidationError { index, field } => {
                assert_eq!(index, 0);
                assert_eq!(field, "name");
            }
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    #[test]
    fn test_from_file_empty_capability_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("business.toml");
        let toml = r#"
site_name = "Test"
site_description = "Test"
domain = "example.com"
policies = []

[[capabilities]]
name = "valid_name"
description = "Bad"
endpoint = ""
method = "GET"
access_level = "anonymous"
"#;
        std::fs::write(&path, toml).unwrap();

        let result = BusinessContextLoader::from_file(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            AwpConfigError::ValidationError { index, field } => {
                assert_eq!(index, 0);
                assert_eq!(field, "endpoint");
            }
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    #[test]
    fn test_validate_business_context_valid() {
        let mut ctx = BusinessContext::core("Test", "Test", "example.com");
        ctx.capabilities = vec![BusinessCapability {
            name: "read".to_string(),
            description: "Read".to_string(),
            endpoint: "/api/read".to_string(),
            method: "GET".to_string(),
            access_level: TrustLevel::Anonymous,
        }];
        assert!(validate_business_context(&ctx).is_ok());
    }

    #[test]
    fn test_validate_empty_capabilities() {
        let ctx = BusinessContext::core("Test", "Test", "example.com");
        assert!(validate_business_context(&ctx).is_ok());
    }
}
