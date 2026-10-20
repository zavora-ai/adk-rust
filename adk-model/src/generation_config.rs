//! Rejects generation config fields a provider client cannot honour.
//!
//! A field the client cannot send, or whose result it cannot return, would be left out, so the
//! generation would run as if it were unset: a seed that does not make the output reproducible,
//! or a stop sequence that does not stop it. The request fails instead, naming the field.

use adk_core::{AdkError, ErrorCategory, ErrorComponent, GenerateContentConfig};

/// The [`GenerateContentConfig`] fields a provider client honours, among those not every
/// provider can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Supports {
    pub(crate) stop_sequences: bool,
    pub(crate) seed: bool,
    /// `frequency_penalty` and `presence_penalty`.
    pub(crate) penalties: bool,
    pub(crate) top_logprobs: bool,
}

/// Fails with `model.unsupported_config` when `config` sets a field `supports` rules out.
pub(crate) fn reject_unsupported(
    provider: &str,
    config: Option<&GenerateContentConfig>,
    supports: Supports,
) -> Result<(), AdkError> {
    let Some(config) = config else {
        return Ok(());
    };
    let set = [
        ("response_modalities", config.response_modalities.is_some()),
        ("stop_sequences", !supports.stop_sequences && !config.stop_sequences.is_empty()),
        ("seed", !supports.seed && config.seed.is_some()),
        ("frequency_penalty", !supports.penalties && config.frequency_penalty.is_some()),
        ("presence_penalty", !supports.penalties && config.presence_penalty.is_some()),
        ("top_logprobs", !supports.top_logprobs && config.top_logprobs.is_some()),
    ];
    let fields: Vec<String> =
        set.iter().filter(|(_, rejected)| *rejected).map(|(name, _)| format!("`{name}`")).collect();
    if fields.is_empty() {
        return Ok(());
    }
    let fields = fields.join(", ");
    Err(AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.unsupported_config",
        format!(
            "the {provider} client does not support {fields}; remove it from the \
             GenerateContentConfig, or use a provider that supports it"
        ),
    )
    .with_provider(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Supports =
        Supports { stop_sequences: false, seed: false, penalties: false, top_logprobs: false };

    #[test]
    fn unset_fields_pass() {
        let config = GenerateContentConfig { temperature: Some(0.2), ..Default::default() };
        assert!(reject_unsupported("p", Some(&config), NONE).is_ok());
        assert!(reject_unsupported("p", None, NONE).is_ok());
    }

    #[test]
    fn every_set_unsupported_field_is_named() {
        let config = GenerateContentConfig {
            response_modalities: Some(vec!["IMAGE".into()]),
            seed: Some(7),
            top_logprobs: Some(2),
            stop_sequences: vec!["END".to_string()],
            ..Default::default()
        };
        let error = reject_unsupported(
            "anthropic",
            Some(&config),
            Supports { stop_sequences: true, ..NONE },
        )
        .expect_err("seed and top_logprobs are unsupported");
        assert_eq!(error.code, "model.unsupported_config");
        assert_eq!(error.category, ErrorCategory::InvalidInput);
        assert!(error.message.contains(
            "the anthropic client does not support `response_modalities`, `seed`, `top_logprobs`"
        ));
        assert!(!error.message.contains("stop_sequences"));
    }
}
