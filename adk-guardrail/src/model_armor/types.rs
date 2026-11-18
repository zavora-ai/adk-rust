//! Wire types of the Model Armor `sanitizeUserPrompt` and `sanitizeModelResponse` responses.
//!
//! Field names follow the REST reference (`SanitizationResult`, `FilterResult`, ...). Every
//! field defaults when absent and every enum accepts values it does not know, so a response
//! with fields added after this release still parses.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// Whether any filter of a template matched the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum FilterMatchState {
    /// The service did not say.
    #[default]
    #[serde(rename = "FILTER_MATCH_STATE_UNSPECIFIED")]
    Unspecified,
    /// No filter matched.
    NoMatchFound,
    /// At least one filter matched.
    MatchFound,
    /// A value this release does not know.
    #[serde(other)]
    Unknown,
}

/// Whether every filter of a template ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum InvocationResult {
    /// The service did not say.
    #[default]
    #[serde(rename = "INVOCATION_RESULT_UNSPECIFIED")]
    Unspecified,
    /// Every filter ran.
    Success,
    /// Some filters ran and some failed.
    Partial,
    /// No filter ran.
    Failure,
    /// A value this release does not know.
    #[serde(other)]
    Unknown,
}

/// Whether one filter ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum FilterExecutionState {
    /// The service did not say.
    #[default]
    #[serde(rename = "FILTER_EXECUTION_STATE_UNSPECIFIED")]
    Unspecified,
    /// The filter ran.
    ExecutionSuccess,
    /// The filter did not apply, such as a text filter on an image.
    ExecutionSkipped,
    /// A value this release does not know.
    #[serde(other)]
    Unknown,
}

/// The confidence at or above which a filter reports a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum DetectionConfidenceLevel {
    /// The service did not say; treated as `LowAndAbove`.
    #[default]
    #[serde(rename = "DETECTION_CONFIDENCE_LEVEL_UNSPECIFIED")]
    Unspecified,
    /// Low confidence or above.
    LowAndAbove,
    /// Medium confidence or above.
    MediumAndAbove,
    /// High confidence.
    High,
    /// A value this release does not know.
    #[serde(other)]
    Unknown,
}

impl DetectionConfidenceLevel {
    /// The REST name of the level, such as `HIGH`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "DETECTION_CONFIDENCE_LEVEL_UNSPECIFIED",
            Self::LowAndAbove => "LOW_AND_ABOVE",
            Self::MediumAndAbove => "MEDIUM_AND_ABOVE",
            Self::High => "HIGH",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// The result of screening one piece of content against a template.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::model_armor::{FilterMatchState, SanitizationResult};
/// use serde_json::json;
///
/// let result: SanitizationResult = serde_json::from_value(json!({
///     "filterMatchState": "MATCH_FOUND",
///     "invocationResult": "SUCCESS",
///     "filterResults": {
///         "pi_and_jailbreak": {
///             "piAndJailbreakFilterResult": {
///                 "executionState": "EXECUTION_SUCCESS",
///                 "matchState": "MATCH_FOUND",
///                 "confidenceLevel": "HIGH"
///             }
///         }
///     }
/// }))
/// .unwrap();
/// assert_eq!(result.filter_match_state, FilterMatchState::MatchFound);
/// assert_eq!(result.matched_filters()[0].to_string(), "pi_and_jailbreak (HIGH)");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct SanitizationResult {
    /// Whether any filter matched.
    pub filter_match_state: FilterMatchState,
    /// Whether every filter ran.
    pub invocation_result: InvocationResult,
    /// Each filter's result, keyed by filter (`rai`, `sdp`, `pi_and_jailbreak`,
    /// `malicious_uris`, `csam`, `virus_scan`).
    pub filter_results: BTreeMap<String, FilterResult>,
    /// Why the invocation failed or was partial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sanitization_metadata: Option<SanitizationMetadata>,
}

/// Why a sanitization was incomplete.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct SanitizationMetadata {
    /// The service's error code.
    #[serde(deserialize_with = "string_or_number", skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// The service's error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// One filter's result. The service sets exactly one field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct FilterResult {
    /// Responsible AI filter: sexually explicit, hate speech, harassment, dangerous content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rai_filter_result: Option<RaiFilterResult>,
    /// Sensitive Data Protection filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdp_filter_result: Option<SdpFilterResult>,
    /// Prompt injection and jailbreak filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pi_and_jailbreak_filter_result: Option<FilterVerdict>,
    /// Malicious URI filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub malicious_uri_filter_result: Option<FilterVerdict>,
    /// Child sexual abuse material filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csam_filter_filter_result: Option<FilterVerdict>,
    /// Virus scan of file content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virus_scan_filter_result: Option<FilterVerdict>,
}

/// What one filter decided.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct FilterVerdict {
    /// Whether the filter ran.
    pub execution_state: FilterExecutionState,
    /// Whether it matched.
    pub match_state: FilterMatchState,
    /// The confidence of the match, for filters that report one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_level: Option<DetectionConfidenceLevel>,
    /// Messages the filter returned, such as why it was skipped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub message_items: Vec<MessageItem>,
}

/// The Responsible AI filter's result, with one entry per category.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct RaiFilterResult {
    /// Whether the filter ran.
    pub execution_state: FilterExecutionState,
    /// Whether any category matched.
    pub match_state: FilterMatchState,
    /// Each category's result, keyed by category (`dangerous`, `harassment`, ...).
    pub rai_filter_type_results: BTreeMap<String, FilterVerdict>,
    /// Messages the filter returned.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub message_items: Vec<MessageItem>,
}

/// The Sensitive Data Protection filter's result: an inspection, or a de-identification when
/// the template configures advanced Sensitive Data Protection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct SdpFilterResult {
    /// What an inspection found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspect_result: Option<SdpInspectResult>,
    /// The content with its sensitive data transformed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deidentify_result: Option<SdpDeidentifyResult>,
}

/// What a Sensitive Data Protection inspection found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct SdpInspectResult {
    /// Whether the inspection ran.
    pub execution_state: FilterExecutionState,
    /// Whether it found sensitive data.
    pub match_state: FilterMatchState,
    /// Each finding: its info type and likelihood. A finding's location is not kept.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<SdpFinding>,
    /// Messages the filter returned.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub message_items: Vec<MessageItem>,
}

/// One Sensitive Data Protection finding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct SdpFinding {
    /// The info type found, such as `US_SOCIAL_SECURITY_NUMBER`.
    pub info_type: String,
    /// How likely the finding is, such as `VERY_LIKELY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likelihood: Option<String>,
}

/// A Sensitive Data Protection de-identification.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct SdpDeidentifyResult {
    /// Whether the de-identification ran.
    pub execution_state: FilterExecutionState,
    /// Whether it found sensitive data.
    pub match_state: FilterMatchState,
    /// The de-identified content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<DataItem>,
    /// The info types it transformed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub info_types: Vec<String>,
    /// Messages the filter returned.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub message_items: Vec<MessageItem>,
}

/// Content sent to or returned by the service.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct DataItem {
    /// Plain text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A message a filter returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct MessageItem {
    /// `INFO`, `WARNING`, or `ERROR`.
    pub message_type: String,
    /// The message.
    pub message: String,
}

/// A filter that matched, as [`SanitizationResult::matched_filters`] reports it.
///
/// Its `Display` names the filter and, where the service reported them, the category,
/// confidence, or info types — never the content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchedFilter {
    /// The filter key, such as `pi_and_jailbreak` or `rai`.
    pub filter: String,
    /// The category within the filter, such as `dangerous` for `rai`.
    pub category: Option<String>,
    /// The confidence of the match.
    pub confidence: Option<DetectionConfidenceLevel>,
    /// The Sensitive Data Protection info types found.
    pub info_types: Vec<String>,
}

impl std::fmt::Display for MatchedFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.filter)?;
        if let Some(category) = &self.category {
            write!(f, "/{category}")?;
        }
        let mut qualifiers: Vec<&str> = self.info_types.iter().map(String::as_str).collect();
        if let Some(confidence) = self.confidence {
            qualifiers.insert(0, confidence.as_str());
        }
        if !qualifiers.is_empty() {
            write!(f, " ({})", qualifiers.join(", "))?;
        }
        Ok(())
    }
}

impl SanitizationResult {
    /// Every filter that matched, in filter-key order.
    ///
    /// A result that reports `MATCH_FOUND` without naming a matching filter yields one entry
    /// whose filter is `unknown`, so a match is never lost to a filter this release does not
    /// know.
    pub fn matched_filters(&self) -> Vec<MatchedFilter> {
        let mut matched = Vec::new();
        for (key, result) in &self.filter_results {
            let filter = |category: Option<&str>, confidence, info_types| MatchedFilter {
                filter: key.clone(),
                category: category.map(str::to_string),
                confidence,
                info_types,
            };
            for verdict in [
                &result.pi_and_jailbreak_filter_result,
                &result.malicious_uri_filter_result,
                &result.csam_filter_filter_result,
                &result.virus_scan_filter_result,
            ]
            .into_iter()
            .flatten()
            {
                if verdict.match_state == FilterMatchState::MatchFound {
                    matched.push(filter(None, verdict.confidence_level, Vec::new()));
                }
            }
            if let Some(rai) = &result.rai_filter_result {
                let categories: Vec<_> = rai
                    .rai_filter_type_results
                    .iter()
                    .filter(|(_, verdict)| verdict.match_state == FilterMatchState::MatchFound)
                    .collect();
                for (category, verdict) in &categories {
                    matched.push(filter(Some(category), verdict.confidence_level, Vec::new()));
                }
                if categories.is_empty() && rai.match_state == FilterMatchState::MatchFound {
                    matched.push(filter(None, None, Vec::new()));
                }
            }
            if let Some(sdp) = &result.sdp_filter_result {
                if let Some(inspect) = &sdp.inspect_result
                    && inspect.match_state == FilterMatchState::MatchFound
                {
                    let mut info_types: Vec<String> =
                        inspect.findings.iter().map(|finding| finding.info_type.clone()).collect();
                    info_types.sort();
                    info_types.dedup();
                    matched.push(filter(None, None, info_types));
                }
                if let Some(deidentify) = &sdp.deidentify_result
                    && deidentify.match_state == FilterMatchState::MatchFound
                {
                    matched.push(filter(None, None, deidentify.info_types.clone()));
                }
            }
        }
        if matched.is_empty() && self.filter_match_state == FilterMatchState::MatchFound {
            matched.push(MatchedFilter {
                filter: "unknown".to_string(),
                category: None,
                confidence: None,
                info_types: Vec::new(),
            });
        }
        matched
    }

    /// Whether any filter matched.
    pub fn is_match(&self) -> bool {
        self.filter_match_state == FilterMatchState::MatchFound
            || !self.matched_filters().is_empty()
    }

    /// The de-identified text, when the only filter that matched is Sensitive Data Protection
    /// and the template de-identifies rather than inspects.
    pub fn deidentified_text(&self) -> Option<&str> {
        let matched = self.matched_filters();
        if matched.is_empty() || matched.iter().any(|m| m.filter != "sdp") {
            return None;
        }
        self.filter_results.get("sdp").and_then(|result| {
            result
                .sdp_filter_result
                .as_ref()?
                .deidentify_result
                .as_ref()
                .filter(|d| d.match_state == FilterMatchState::MatchFound)?
                .data
                .as_ref()?
                .text
                .as_deref()
        })
    }
}

/// Accepts an int64 error code as the JSON string proto3 encodes it, or as a number.
fn string_or_number<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::String(code)) => Some(code),
        Some(serde_json::Value::Number(code)) => Some(code.to_string()),
        Some(_) | None => None,
    })
}
