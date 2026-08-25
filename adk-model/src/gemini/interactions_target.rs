//! Interactions API target allowlist.
//!
//! The Gemini Interactions API (Beta) only supports a fixed set of model and
//! agent targets. This module defines the [`InteractionTarget`] type together
//! with the documented allowlists
//! ([`MODEL_TARGETS`](crate::gemini::interactions_target::MODEL_TARGETS) and
//! [`AGENT_TARGETS`](crate::gemini::interactions_target::AGENT_TARGETS)).
//!
//! A [`InteractionTarget::Model`] sets the request `model` field, while a
//! [`InteractionTarget::Agent`] sets the request `agent` field (e.g. Deep
//! Research). Parsing and validation of arbitrary identifiers against these
//! allowlists is layered on top of this module.
//!
//! This module is only compiled when the `gemini-interactions` feature is
//! enabled.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// The fixed set of Interactions-supported **model** targets.
///
/// When the Interactions transport is configured with one of these
/// identifiers, the request's `model` field is set. Any identifier outside
/// this list (and [`AGENT_TARGETS`]) is rejected with an `InvalidInput` error.
pub const MODEL_TARGETS: &[&str] = &[
    "gemini-3.7-flash",
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-3.5-flash-lite",
    "gemini-3.1-flash-lite",
    "gemini-3.1-pro-preview",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "lyria-3-clip-preview",
    "lyria-3-pro-preview",
];

/// The fixed set of Interactions-supported **agent** targets.
///
/// When the Interactions transport is configured with one of these
/// identifiers, the request's `agent` field is set (e.g. Deep Research). Any
/// identifier outside this list (and [`MODEL_TARGETS`]) is rejected with an
/// `InvalidInput` error.
pub const AGENT_TARGETS: &[&str] = &[
    "deep-research-pro-preview-12-2025",
    "deep-research-preview-04-2026",
    "deep-research-max-preview-04-2026",
];

/// A validated Interactions destination.
///
/// An Interactions request targets either a supported **model** (which sets the
/// request `model` field) or a supported **agent** (which sets the request
/// `agent` field). The wrapped `String` is the target's identifier, which is
/// guaranteed to be a member of [`MODEL_TARGETS`] or [`AGENT_TARGETS`] when the
/// value is produced through validated construction.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::gemini::interactions_target::InteractionTarget;
///
/// let target = InteractionTarget::Model("gemini-2.5-flash".to_string());
/// assert_eq!(target.identifier(), "gemini-2.5-flash");
/// assert!(!target.is_agent());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionTarget {
    /// A supported model target. Sets the request `model` field.
    Model(String),
    /// A supported agent target (e.g. Deep Research). Sets the request `agent`
    /// field.
    Agent(String),
}

impl InteractionTarget {
    /// Parses and validates an identifier against the Interactions target
    /// allowlists.
    ///
    /// The identifier is matched against [`MODEL_TARGETS`] and
    /// [`AGENT_TARGETS`]. A match in [`MODEL_TARGETS`] yields an
    /// [`InteractionTarget::Model`]; a match in [`AGENT_TARGETS`] yields an
    /// [`InteractionTarget::Agent`].
    ///
    /// # Arguments
    ///
    /// * `identifier` - The model or agent id to validate.
    ///
    /// # Returns
    ///
    /// A validated [`InteractionTarget`] when the identifier is on an
    /// allowlist.
    ///
    /// # Errors
    ///
    /// Returns an [`AdkError`] with category [`ErrorCategory::InvalidInput`]
    /// and provider `"gemini"` when `identifier` is not present in either
    /// [`MODEL_TARGETS`] or [`AGENT_TARGETS`]. The error message names the
    /// supported model and agent targets so the caller can correct the
    /// configuration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_model::gemini::interactions_target::InteractionTarget;
    ///
    /// let model = InteractionTarget::parse("gemini-2.5-flash")?;
    /// assert!(model.is_model());
    ///
    /// let agent = InteractionTarget::parse("deep-research-preview-04-2026")?;
    /// assert!(agent.is_agent());
    ///
    /// assert!(InteractionTarget::parse("gpt-4").is_err());
    /// ```
    pub fn parse(identifier: &str) -> adk_core::Result<InteractionTarget> {
        if MODEL_TARGETS.contains(&identifier) {
            Ok(InteractionTarget::Model(identifier.to_string()))
        } else if AGENT_TARGETS.contains(&identifier) {
            Ok(InteractionTarget::Agent(identifier.to_string()))
        } else {
            Err(unsupported_target_error(identifier))
        }
    }

    /// Returns the target's identifier string.
    ///
    /// This is the model or agent id, regardless of which variant is held.
    pub fn identifier(&self) -> &str {
        match self {
            InteractionTarget::Model(id) | InteractionTarget::Agent(id) => id,
        }
    }

    /// Returns `true` if this target is an [`InteractionTarget::Agent`].
    pub fn is_agent(&self) -> bool {
        matches!(self, InteractionTarget::Agent(_))
    }

    /// Returns `true` if this target is an [`InteractionTarget::Model`].
    pub fn is_model(&self) -> bool {
        matches!(self, InteractionTarget::Model(_))
    }
}

/// Builds the `InvalidInput` error returned for an unsupported Interactions
/// target.
///
/// The message names every supported model and agent target so the caller can
/// correct the configuration without consulting external documentation.
fn unsupported_target_error(identifier: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.gemini.interactions.unsupported_target",
        format!(
            "unsupported Interactions target '{identifier}'. Supported model targets: [{}]. \
             Supported agent targets: [{}].",
            MODEL_TARGETS.join(", "),
            AGENT_TARGETS.join(", "),
        ),
    )
    .with_provider("gemini")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_targets_are_non_empty_and_unique() {
        assert!(!MODEL_TARGETS.is_empty());
        let mut seen = std::collections::HashSet::new();
        for id in MODEL_TARGETS {
            assert!(seen.insert(*id), "duplicate model target: {id}");
        }
    }

    #[test]
    fn agent_targets_are_non_empty_and_unique() {
        assert!(!AGENT_TARGETS.is_empty());
        let mut seen = std::collections::HashSet::new();
        for id in AGENT_TARGETS {
            assert!(seen.insert(*id), "duplicate agent target: {id}");
        }
    }

    #[test]
    fn model_and_agent_allowlists_are_disjoint() {
        for model in MODEL_TARGETS {
            assert!(!AGENT_TARGETS.contains(model), "{model} appears in both allowlists");
        }
    }

    #[test]
    fn documented_model_targets_present() {
        for expected in [
            "gemini-3.5-flash",
            "gemini-3.1-flash-lite",
            "gemini-3.1-pro-preview",
            "gemini-3-flash-preview",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "lyria-3-clip-preview",
            "lyria-3-pro-preview",
        ] {
            assert!(MODEL_TARGETS.contains(&expected), "missing model target: {expected}");
        }
    }

    #[test]
    fn documented_agent_targets_present() {
        for expected in [
            "deep-research-pro-preview-12-2025",
            "deep-research-preview-04-2026",
            "deep-research-max-preview-04-2026",
        ] {
            assert!(AGENT_TARGETS.contains(&expected), "missing agent target: {expected}");
        }
    }

    #[test]
    fn accessors_report_variant_and_identifier() {
        let model = InteractionTarget::Model("gemini-2.5-flash".to_string());
        assert_eq!(model.identifier(), "gemini-2.5-flash");
        assert!(model.is_model());
        assert!(!model.is_agent());

        let agent = InteractionTarget::Agent("deep-research-preview-04-2026".to_string());
        assert_eq!(agent.identifier(), "deep-research-preview-04-2026");
        assert!(agent.is_agent());
        assert!(!agent.is_model());
    }

    #[test]
    fn parse_accepts_every_model_target_as_model() {
        for id in MODEL_TARGETS {
            let target = InteractionTarget::parse(id).expect("model target should parse");
            assert_eq!(target, InteractionTarget::Model((*id).to_string()));
            assert!(target.is_model());
            assert_eq!(target.identifier(), *id);
        }
    }

    #[test]
    fn parse_accepts_every_agent_target_as_agent() {
        for id in AGENT_TARGETS {
            let target = InteractionTarget::parse(id).expect("agent target should parse");
            assert_eq!(target, InteractionTarget::Agent((*id).to_string()));
            assert!(target.is_agent());
            assert_eq!(target.identifier(), *id);
        }
    }

    #[test]
    fn parse_rejects_unsupported_targets_with_invalid_input() {
        for id in ["gpt-4", "gemini-2.0-flash", "", "deep-research", "GEMINI-2.5-FLASH"] {
            let err = InteractionTarget::parse(id).expect_err("unsupported target should error");
            assert_eq!(
                err.category,
                ErrorCategory::InvalidInput,
                "expected InvalidInput for '{id}'"
            );
        }
    }

    #[test]
    fn parse_error_names_supported_targets() {
        let err = InteractionTarget::parse("gpt-4").expect_err("unsupported target should error");
        let message = err.to_string();
        // Mentions a supported model target.
        assert!(message.contains("gemini-2.5-flash"), "message should name a model target");
        // Mentions a supported agent target.
        assert!(
            message.contains("deep-research-preview-04-2026"),
            "message should name an agent target"
        );
    }

    #[test]
    fn parse_error_uses_gemini_provider() {
        let err = InteractionTarget::parse("not-a-target").expect_err("should error");
        assert_eq!(err.details.provider.as_deref(), Some("gemini"));
        assert_eq!(err.code, "model.gemini.interactions.unsupported_target");
    }
}
