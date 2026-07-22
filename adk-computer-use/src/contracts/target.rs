//! Target evidence and value-free accessibility sensitivity contracts.
//!
//! These types bind a proposed action to a *fresh* desktop observation and
//! carry only digests and structured signals — never raw field values — so the
//! wire payload cannot leak sensitive content across the ADK/runtime boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

/// Evidence binding an action to a fresh desktop observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetEvidence {
    /// Platform identifier for the observed target (e.g. `darwin`, `windows`).
    pub platform: String,
    /// Application/bundle identifier of the observed target.
    pub app_id: String,
    /// Process identifier of the observed target, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Platform-specific window identifier, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Value>,
    /// Digest of the window title (never the raw title).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title_digest: Option<String>,
    /// Display identifier hosting the target, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
    /// Accessibility role of the target element, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Digest of the element label (never the raw label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_digest: Option<String>,
    /// Bounding box of the target element, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Identifier of the observation frame this evidence came from.
    pub observation_id: String,
    /// Hash of the screenshot backing this observation, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_hash: Option<String>,
    /// Revision of the UI tree backing this observation, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_tree_revision: Option<String>,
    /// Observation confidence in `0.0..=1.0`.
    pub confidence: f64,
    /// RFC 3339 timestamp of the observation.
    pub captured_at: String,
}

/// Axis-aligned bounding box in display coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    /// Left edge in display coordinates.
    pub x: f64,
    /// Top edge in display coordinates.
    pub y: f64,
    /// Width in display units.
    pub width: f64,
    /// Height in display units.
    pub height: f64,
}

/// Conclusion of an accessibility-based target sensitivity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivityAssessment {
    /// The target holds sensitive content (e.g. a password field).
    Sensitive,
    /// The target is confirmed non-sensitive.
    NonSensitive,
    /// Sensitivity could not be determined.
    Unknown,
}

/// Source of a [`TargetSensitivityEvidence`] assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivitySource {
    /// Derived from platform accessibility APIs.
    Accessibility,
    /// Native sensitivity signals were unavailable.
    Unavailable,
}

/// A single value-free signal contributing to a sensitivity assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivitySignal {
    /// Element reports a secure accessibility role.
    SecureRole,
    /// Element reports a secure accessibility subrole.
    SecureSubrole,
    /// Element is marked as protected content.
    ProtectedContent,
    /// UI Automation reports the element as a password field.
    UiaIsPassword,
    /// Element label matched a sensitive pattern.
    SensitiveLabel,
    /// Multiple candidate elements matched ambiguously.
    AmbiguousMatch,
    /// The referenced element was not found.
    ElementNotFound,
    /// Inspection raised an error.
    InspectionError,
    /// The field was invalid for inspection.
    InvalidField,
    /// Native sensitivity signals were unavailable.
    NativeSignalUnavailable,
}

/// Value-free native accessibility evidence used for action risk and revalidation.
///
/// Construct with [`TargetSensitivityEvidence::try_new`]; the constructor and the
/// deserializer enforce the same invariants (bounded/unique signals, conclusive
/// assessments require accessibility evidence for a checked field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawTargetSensitivityEvidence", into = "RawTargetSensitivityEvidence")]
pub struct TargetSensitivityEvidence {
    assessment: TargetSensitivityAssessment,
    source: TargetSensitivitySource,
    signals: Vec<TargetSensitivitySignal>,
    fields_checked: u32,
    observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTargetSensitivityEvidence {
    assessment: TargetSensitivityAssessment,
    source: TargetSensitivitySource,
    signals: Vec<TargetSensitivitySignal>,
    fields_checked: u32,
    observed_at: String,
}

impl TargetSensitivityEvidence {
    /// Build validated sensitivity evidence.
    ///
    /// # Errors
    ///
    /// Returns a message describing the violated invariant when signals exceed
    /// 10, contain duplicates, `fields_checked` exceeds 100, a conclusive
    /// assessment lacks accessibility evidence for a checked field, a
    /// `Sensitive` assessment has no signals, or `observed_at` is blank.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_computer_use::{
    ///     TargetSensitivityAssessment, TargetSensitivityEvidence, TargetSensitivitySignal,
    ///     TargetSensitivitySource,
    /// };
    ///
    /// let evidence = TargetSensitivityEvidence::try_new(
    ///     TargetSensitivityAssessment::Sensitive,
    ///     TargetSensitivitySource::Accessibility,
    ///     vec![TargetSensitivitySignal::UiaIsPassword],
    ///     1,
    ///     "2026-07-13T12:00:00Z",
    /// )
    /// .unwrap();
    /// assert_eq!(evidence.fields_checked(), 1);
    /// ```
    pub fn try_new(
        assessment: TargetSensitivityAssessment,
        source: TargetSensitivitySource,
        signals: Vec<TargetSensitivitySignal>,
        fields_checked: u32,
        observed_at: impl Into<String>,
    ) -> Result<Self, String> {
        RawTargetSensitivityEvidence {
            assessment,
            source,
            signals,
            fields_checked,
            observed_at: observed_at.into(),
        }
        .try_into()
    }

    /// The conclusion of the sensitivity check.
    pub fn assessment(&self) -> TargetSensitivityAssessment {
        self.assessment
    }

    /// The source of the assessment.
    pub fn source(&self) -> TargetSensitivitySource {
        self.source
    }

    /// The distinct signals that contributed to the assessment.
    pub fn signals(&self) -> &[TargetSensitivitySignal] {
        &self.signals
    }

    /// The number of fields inspected during the check.
    pub fn fields_checked(&self) -> u32 {
        self.fields_checked
    }

    /// RFC 3339 timestamp of the observation.
    pub fn observed_at(&self) -> &str {
        &self.observed_at
    }
}

impl TryFrom<RawTargetSensitivityEvidence> for TargetSensitivityEvidence {
    type Error = String;

    fn try_from(raw: RawTargetSensitivityEvidence) -> Result<Self, Self::Error> {
        if raw.signals.len() > 10 {
            return Err("target sensitivity supports at most 10 signals".into());
        }
        if raw.signals.iter().copied().collect::<HashSet<_>>().len() != raw.signals.len() {
            return Err("target sensitivity signals must be unique".into());
        }
        if raw.fields_checked > 100 {
            return Err("target sensitivity supports at most 100 checked fields".into());
        }
        if matches!(
            raw.assessment,
            TargetSensitivityAssessment::Sensitive | TargetSensitivityAssessment::NonSensitive
        ) && (raw.source != TargetSensitivitySource::Accessibility || raw.fields_checked == 0)
        {
            return Err(
                "conclusive target sensitivity requires accessibility evidence for a checked field"
                    .into(),
            );
        }
        if raw.assessment == TargetSensitivityAssessment::Sensitive && raw.signals.is_empty() {
            return Err("sensitive target evidence requires at least one signal".into());
        }
        if raw.observed_at.trim().is_empty() {
            return Err("target sensitivity observedAt must not be empty".into());
        }
        Ok(Self {
            assessment: raw.assessment,
            source: raw.source,
            signals: raw.signals,
            fields_checked: raw.fields_checked,
            observed_at: raw.observed_at,
        })
    }
}

impl From<TargetSensitivityEvidence> for RawTargetSensitivityEvidence {
    fn from(value: TargetSensitivityEvidence) -> Self {
        Self {
            assessment: value.assessment,
            source: value.source,
            signals: value.signals,
            fields_checked: value.fields_checked,
            observed_at: value.observed_at,
        }
    }
}
