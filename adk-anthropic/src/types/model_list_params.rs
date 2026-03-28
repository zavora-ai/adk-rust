use serde::{Deserialize, Serialize};

/// Parameters for listing models.
///
/// This struct contains the parameters that can be passed when listing models
/// from the Anthropic API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelListParams {
    /// ID of the object to use as a cursor for pagination.
    ///
    /// When provided, returns the page of results immediately after this object.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "after_id")]
    pub after_id: Option<String>,

    /// ID of the object to use as a cursor for pagination.
    ///
    /// When provided, returns the page of results immediately before this object.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "before_id")]
    pub before_id: Option<String>,

    /// Number of items to return per page.
    ///
    /// Defaults to `20`. Ranges from `1` to `1000`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Optional header to specify the beta version(s) you want to use.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "anthropic-beta")]
    pub betas: Option<Vec<String>>,
}

impl ModelListParams {
    /// Create a new, empty instance of ModelListParams.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the after_id parameter for pagination.
    ///
    /// When provided, returns the page of results immediately after this object.
    pub fn with_after_id(mut self, after_id: impl Into<String>) -> Self {
        self.after_id = Some(after_id.into());
        self
    }

    /// Set the before_id parameter for pagination.
    ///
    /// When provided, returns the page of results immediately before this object.
    pub fn with_before_id(mut self, before_id: impl Into<String>) -> Self {
        self.before_id = Some(before_id.into());
        self
    }

    /// Set the limit for the number of items to return per page.
    ///
    /// Defaults to `20`. Ranges from `1` to `1000`.
    pub fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set the beta versions to use for this request.
    pub fn with_betas(mut self, betas: Vec<String>) -> Self {
        self.betas = Some(betas);
        self
    }

    /// Add a single beta version to use for this request.
    pub fn with_beta(mut self, beta: String) -> Self {
        match &mut self.betas {
            Some(betas) => betas.push(beta),
            None => self.betas = Some(vec![beta]),
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_list_params() {
        let params = ModelListParams::default();
        assert_eq!(params.after_id, None);
        assert_eq!(params.before_id, None);
        assert_eq!(params.limit, None);
        assert_eq!(params.betas, None);
    }

    #[test]
    fn model_list_params_builder() {
        let params = ModelListParams::new()
            .with_after_id("model_123")
            .with_limit(50)
            .with_beta("token-counting-2024-11-01".to_string());

        assert_eq!(params.after_id, Some("model_123".to_string()));
        assert_eq!(params.before_id, None);
        assert_eq!(params.limit, Some(50));
        assert_eq!(params.betas, Some(vec!["token-counting-2024-11-01".to_string()]));
    }

    #[test]
    fn model_list_params_serialization() {
        let params = ModelListParams::new()
            .with_limit(50)
            .with_beta("token-counting-2024-11-01".to_string());

        let json = serde_json::to_value(&params).unwrap();
        let expected = serde_json::json!({
            "limit": 50,
            "anthropic-beta": ["token-counting-2024-11-01"]
        });
        assert_eq!(json, expected);
    }

    #[test]
    fn model_list_params_deserialization() {
        let json = serde_json::json!({
            "after_id": "model_123",
            "limit": 50,
            "anthropic-beta": ["token-counting-2024-11-01"]
        });
        let params: ModelListParams = serde_json::from_value(json).unwrap();

        assert_eq!(params.after_id, Some("model_123".to_string()));
        assert_eq!(params.limit, Some(50));
        assert_eq!(params.betas, Some(vec!["token-counting-2024-11-01".to_string()]));
    }
}
