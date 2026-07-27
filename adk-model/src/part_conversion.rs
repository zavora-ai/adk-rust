//! What happened to each content part on the way to a provider.
//!
//! `Content` can express more than any single provider transport accepts, so every adapter
//! has to decide what to do with the remainder. The decisions themselves are legitimate;
//! making them invisible is not. Adapters used unrelated fallback policies — drop,
//! textualize, or encode — with no record, so a request could reach a provider without
//! material the caller supplied and the model could answer as though it had seen a document
//! it never received.
//!
//! Recording an outcome here emits a `tracing` event at the same moment, so an omission or
//! downgrade is always observable. [`ConversionReport::into_error`](crate::part_conversion::ConversionReport::into_error) lets a caller that
//! cannot tolerate loss turn omissions into a failure before dispatch.
//!
//! # Example
//!
//! ```rust
//! use adk_model::part_conversion::ConversionReport;
//!
//! let mut report = ConversionReport::new("bedrock");
//! report.converted("Text");
//! report.omitted("InlineData", Some("audio/wav"), "no Bedrock Converse block accepts audio");
//!
//! assert!(report.has_omissions());
//! assert_eq!(report.omitted_parts().count(), 1);
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// What an adapter did with one content part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartDisposition {
    /// Carried to the provider in an equivalent native form.
    Converted,
    /// Carried, but in a lossier form than the caller supplied — a file reference rendered
    /// as descriptive text, for instance, which the model reads but cannot open.
    Downgraded {
        /// The form actually sent.
        to: &'static str,
    },
    /// Not carried at all.
    Omitted,
}

/// One part's fate, with enough detail to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartOutcome {
    /// The `Part` variant name, such as `"InlineData"`.
    pub kind: &'static str,
    /// The part's MIME type where it has one.
    pub mime_type: Option<String>,
    /// What the adapter did.
    pub disposition: PartDisposition,
    /// Why, in terms a caller can act on.
    pub detail: String,
}

/// Every part outcome for one request conversion.
#[derive(Debug, Clone)]
pub struct ConversionReport {
    provider: &'static str,
    outcomes: Vec<PartOutcome>,
}

impl ConversionReport {
    /// Starts a report for `provider`.
    pub fn new(provider: &'static str) -> Self {
        Self { provider, outcomes: Vec::new() }
    }

    /// The provider this report describes.
    pub fn provider(&self) -> &'static str {
        self.provider
    }

    /// Records a part carried natively.
    pub fn converted(&mut self, kind: &'static str) {
        self.outcomes.push(PartOutcome {
            kind,
            mime_type: None,
            disposition: PartDisposition::Converted,
            detail: String::new(),
        });
    }

    /// Records a part carried in a lossier form, and warns.
    pub fn downgraded(
        &mut self,
        kind: &'static str,
        mime_type: Option<&str>,
        to: &'static str,
        detail: impl Into<String>,
    ) {
        let detail = detail.into();
        tracing::warn!(
            provider = self.provider,
            part.kind = kind,
            part.mime_type = mime_type.unwrap_or("none"),
            part.sent_as = to,
            reason = %detail,
            "content part downgraded for provider"
        );
        self.outcomes.push(PartOutcome {
            kind,
            mime_type: mime_type.map(str::to_string),
            disposition: PartDisposition::Downgraded { to },
            detail,
        });
    }

    /// Records a part left out entirely, and warns.
    pub fn omitted(
        &mut self,
        kind: &'static str,
        mime_type: Option<&str>,
        detail: impl Into<String>,
    ) {
        let detail = detail.into();
        tracing::warn!(
            provider = self.provider,
            part.kind = kind,
            part.mime_type = mime_type.unwrap_or("none"),
            reason = %detail,
            "content part omitted for provider"
        );
        self.outcomes.push(PartOutcome {
            kind,
            mime_type: mime_type.map(str::to_string),
            disposition: PartDisposition::Omitted,
            detail,
        });
    }

    /// Every recorded outcome, in the order the parts appeared.
    pub fn outcomes(&self) -> &[PartOutcome] {
        &self.outcomes
    }

    /// The parts that did not reach the provider.
    pub fn omitted_parts(&self) -> impl Iterator<Item = &PartOutcome> {
        self.outcomes.iter().filter(|outcome| outcome.disposition == PartDisposition::Omitted)
    }

    /// The parts that reached the provider in a lossier form.
    pub fn downgraded_parts(&self) -> impl Iterator<Item = &PartOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome.disposition, PartDisposition::Downgraded { .. }))
    }

    /// Whether any part was left out.
    pub fn has_omissions(&self) -> bool {
        self.omitted_parts().next().is_some()
    }

    /// Whether any part was omitted or downgraded.
    pub fn has_losses(&self) -> bool {
        self.has_omissions() || self.downgraded_parts().next().is_some()
    }

    /// An error naming every omitted part, for callers that must not send a partial request.
    ///
    /// Returns `None` when nothing was omitted. Downgrades are excluded: the material still
    /// reaches the model, and refusing them would reject the documented textual fallback.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_model::part_conversion::ConversionReport;
    ///
    /// let mut report = ConversionReport::new("bedrock");
    /// report.omitted("InlineData", Some("audio/wav"), "unsupported media type");
    ///
    /// let error = report.into_error().expect("an omission must produce an error");
    /// assert!(error.to_string().contains("audio/wav"));
    /// ```
    pub fn into_error(self) -> Option<AdkError> {
        if !self.has_omissions() {
            return None;
        }

        let omitted = self
            .omitted_parts()
            .map(|outcome| {
                let mime = outcome.mime_type.as_deref().unwrap_or("no mime type");
                format!("{} ({}): {}", outcome.kind, mime, outcome.detail)
            })
            .collect::<Vec<_>>()
            .join("; ");

        Some(AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unsupported,
            "model.content.parts_omitted",
            format!(
                "{} cannot carry every supplied content part, so the request would reach the \
                 model incomplete: {omitted}. Remove the part, convert it to a supported type, \
                 or choose a provider that accepts it.",
                self.provider
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_separates_omissions_from_downgrades() {
        let mut report = ConversionReport::new("bedrock");
        report.converted("Text");
        report.downgraded("FileData", Some("image/png"), "text", "only S3 URIs are native");
        report.omitted("InlineData", Some("audio/wav"), "no block accepts audio");

        assert_eq!(report.outcomes().len(), 3);
        assert_eq!(report.omitted_parts().count(), 1);
        assert_eq!(report.downgraded_parts().count(), 1);
        assert!(report.has_omissions());
        assert!(report.has_losses());
    }

    #[test]
    fn downgrades_alone_are_not_an_error() {
        let mut report = ConversionReport::new("gemini");
        report.downgraded("FileData", Some("application/pdf"), "text", "rendered as a reference");

        assert!(!report.has_omissions());
        assert!(report.has_losses());
        assert!(report.into_error().is_none(), "a downgrade still reaches the model");
    }

    #[test]
    fn an_omission_names_the_part_in_the_error() {
        let mut report = ConversionReport::new("bedrock");
        report.omitted("ServerToolCall", None, "Gemini-specific part");

        let error = report.into_error().expect("omission must error");
        let message = error.to_string();
        assert!(message.contains("ServerToolCall"), "{message}");
        assert!(message.contains("bedrock"), "{message}");
    }
}
