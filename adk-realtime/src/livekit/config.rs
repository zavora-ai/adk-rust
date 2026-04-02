use secrecy::{ExposeSecret, SecretString};

/// Configuration for connecting to a LiveKit server.
#[derive(Clone)]
pub struct LiveKitConfig {
    pub url: String,
    pub api_key: SecretString,
    pub api_secret: SecretString,
}

impl std::fmt::Debug for LiveKitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitConfig")
            .field("url", &self.url)
            .field("api_key", &"<redacted>")
            .field("api_secret", &"<redacted>")
            .finish()
    }
}

impl LiveKitConfig {
    /// Create a new LiveKit configuration.
    pub fn new(
        url: impl Into<String>,
        api_key: impl Into<String>,
        api_secret: impl Into<String>,
    ) -> Result<Self, crate::livekit::LiveKitError> {
        let url_str = url.into();
        let parsed_url = url::Url::parse(&url_str)
            .map_err(|e| crate::livekit::LiveKitError::ConfigError(format!("Invalid URL: {e}")))?;

        let api_key_str: String = api_key.into();
        if api_key_str.is_empty() {
            return Err(crate::livekit::LiveKitError::ConfigError(
                "LiveKit API key cannot be empty".to_string(),
            ));
        }

        let api_secret_str: String = api_secret.into();
        if api_secret_str.is_empty() {
            return Err(crate::livekit::LiveKitError::ConfigError(
                "LiveKit API secret cannot be empty".to_string(),
            ));
        }

        Ok(Self {
            url: parsed_url.to_string(),
            api_key: SecretString::new(api_key_str.into()),
            api_secret: SecretString::new(api_secret_str.into()),
        })
    }

    /// Generate an access token for the given identity and optional room grants.
    pub fn generate_token(
        &self,
        identity: &str,
        grants: Option<livekit_api::access_token::VideoGrants>,
    ) -> Result<String, livekit_api::access_token::AccessTokenError> {
        self.generate_token_with_name(identity, None, grants)
    }

    /// Generate an access token for the given identity, name, and optional room grants.
    pub fn generate_token_with_name(
        &self,
        identity: &str,
        name: Option<&str>,
        grants: Option<livekit_api::access_token::VideoGrants>,
    ) -> Result<String, livekit_api::access_token::AccessTokenError> {
        let mut token = livekit_api::access_token::AccessToken::with_api_key(
            self.api_key.expose_secret(),
            self.api_secret.expose_secret(),
        );
        token = token.with_identity(identity);

        if let Some(n) = name {
            token = token.with_name(n);
        }

        if let Some(grants) = grants {
            token = token.with_grants(grants);
        }

        token.to_jwt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_livekit_config_new_success() {
        let config = LiveKitConfig::new("wss://test.livekit.cloud", "key123", "secret456").unwrap();
        assert_eq!(config.url, "wss://test.livekit.cloud/");
    }

    #[test]
    fn test_livekit_config_invalid_url() {
        let err = LiveKitConfig::new("not_a_url", "key123", "secret456").unwrap_err();
        assert!(matches!(err, crate::livekit::LiveKitError::ConfigError(_)));
    }

    #[test]
    fn test_livekit_config_empty_credentials() {
        assert!(LiveKitConfig::new("wss://test.livekit.cloud", "", "secret456").is_err());
        assert!(LiveKitConfig::new("wss://test.livekit.cloud", "key123", "").is_err());
    }

    #[test]
    fn test_generate_token() {
        let config = LiveKitConfig::new("wss://test.livekit.cloud", "key", "secret").unwrap();
        let token = config.generate_token_with_name("agent1", Some("Agent Name"), None);
        assert!(token.is_ok());
    }
}
