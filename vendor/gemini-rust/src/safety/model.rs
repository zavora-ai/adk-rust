use serde::{Deserialize, Serialize};

/// Setting for safety
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySetting {
    /// The category of content to filter
    pub category: HarmCategory,
    /// The threshold for filtering
    pub threshold: HarmBlockThreshold,
}

/// Category of harmful content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HarmCategory {
    /// Category is unspecified.
    #[serde(rename = "HARM_CATEGORY_UNSPECIFIED")]
    Unspecified,
    /// PaLM - Negative or harmful comments targeting identity and/or protected attribute.
    #[serde(rename = "HARM_CATEGORY_DEROGATORY")]
    Derogatory,
    /// PaLM - Content that is rude, disrespectful, or profane.
    #[serde(rename = "HARM_CATEGORY_TOXICITY")]
    Toxicity,
    /// PaLM - Describes scenarios depicting violence against an individual or group, or general descriptions of gore.
    #[serde(rename = "HARM_CATEGORY_VIOLENCE")]
    Violence,
    /// PaLM - Contains references to sexual acts or other lewd content.
    #[serde(rename = "HARM_CATEGORY_SEXUAL")]
    Sexual,
    /// PaLM - Promotes unchecked medical advice.
    #[serde(rename = "HARM_CATEGORY_MEDICAL")]
    Medical,
    /// PaLM - Dangerous content that promotes, facilitates, or encourages harmful acts.
    #[serde(rename = "HARM_CATEGORY_DANGEROUS")]
    Dangerous,
    /// Gemini - Harassment content.
    #[serde(rename = "HARM_CATEGORY_HARASSMENT")]
    Harassment,
    /// Gemini - Hate speech and content.
    #[serde(rename = "HARM_CATEGORY_HATE_SPEECH")]
    HateSpeech,
    /// Gemini - Sexually explicit content.
    #[serde(rename = "HARM_CATEGORY_SEXUALLY_EXPLICIT")]
    SexuallyExplicit,
    /// Gemini - Dangerous content.
    #[serde(rename = "HARM_CATEGORY_DANGEROUS_CONTENT")]
    DangerousContent,
}

/// Threshold for blocking harmful content
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HarmBlockThreshold {
    /// Threshold is unspecified.
    HarmBlockThresholdUnspecified,
    /// Content with NEGLIGIBLE will be allowed.
    BlockLowAndAbove,
    /// Content with NEGLIGIBLE and LOW will be allowed.
    BlockMediumAndAbove,
    /// Content with NEGLIGIBLE, LOW, and MEDIUM will be allowed.
    BlockOnlyHigh,
    /// All content will be allowed.
    BlockNone,
    /// Turn off the safety filter.
    Off,
}

/// Probability that content is harmful
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HarmProbability {
    /// Probability is unspecified.
    HarmProbabilityUnspecified,
    /// Content has a negligible chance of being unsafe.
    Negligible,
    /// Content has a low chance of being unsafe.
    Low,
    /// Content has a medium chance of being unsafe.
    Medium,
    /// Content has a high chance of being unsafe.
    High,
}

/// Safety rating for content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SafetyRating {
    /// The category of the safety rating
    pub category: HarmCategory,
    /// The probability that the content is harmful
    pub probability: HarmProbability,
}
