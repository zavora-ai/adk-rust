use serde::{Deserialize, Serialize};

/// Setting for safety
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySetting {
    /// The category of content to filter
    pub category: HarmCategory,
    /// The threshold for filtering
    pub threshold: HarmBlockThreshold,
}

hybrid_enum! {
    /// Category of harmful content
    pub enum HarmCategory {
        /// Category is unspecified.
        Unspecified       => ("HARM_CATEGORY_UNSPECIFIED", 0),
        /// PaLM - Negative or harmful comments targeting identity and/or protected attribute.
        Derogatory        => ("HARM_CATEGORY_DEROGATORY", 1),
        /// PaLM - Content that is rude, disrespectful, or profane.
        Toxicity          => ("HARM_CATEGORY_TOXICITY", 2),
        /// PaLM - Describes scenarios depicting violence against an individual or group.
        Violence          => ("HARM_CATEGORY_VIOLENCE", 3),
        /// PaLM - Contains references to sexual acts or other lewd content.
        Sexual            => ("HARM_CATEGORY_SEXUAL", 4),
        /// PaLM - Promotes unchecked medical advice.
        Medical           => ("HARM_CATEGORY_MEDICAL", 5),
        /// PaLM - Dangerous content that promotes harmful acts.
        Dangerous         => ("HARM_CATEGORY_DANGEROUS", 6),
        /// Gemini - Harassment content.
        Harassment        => ("HARM_CATEGORY_HARASSMENT", 7),
        /// Gemini - Hate speech and content.
        HateSpeech        => ("HARM_CATEGORY_HATE_SPEECH", 8),
        /// Gemini - Sexually explicit content.
        SexuallyExplicit  => ("HARM_CATEGORY_SEXUALLY_EXPLICIT", 9),
        /// Gemini - Dangerous content.
        DangerousContent  => ("HARM_CATEGORY_DANGEROUS_CONTENT", 10),
        /// Gemini - Civic integrity content.
        CivicIntegrity    => ("HARM_CATEGORY_CIVIC_INTEGRITY", 11),
        /// Gemini - Jailbreak-related content.
        Jailbreak         => ("HARM_CATEGORY_JAILBREAK", 12),
    }
    fallback: Unspecified
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

hybrid_enum! {
    /// Probability that content is harmful
    pub enum HarmProbability {
        /// Probability is unspecified.
        HarmProbabilityUnspecified => ("HARM_PROBABILITY_UNSPECIFIED", 0),
        /// Content has a negligible chance of being unsafe.
        Negligible                => ("NEGLIGIBLE", 1),
        /// Content has a low chance of being unsafe.
        Low                       => ("LOW", 2),
        /// Content has a medium chance of being unsafe.
        Medium                    => ("MEDIUM", 3),
        /// Content has a high chance of being unsafe.
        High                      => ("HIGH", 4),
    }
    fallback: HarmProbabilityUnspecified
}

/// Safety rating for content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SafetyRating {
    /// The category of the safety rating
    pub category: HarmCategory,
    /// The probability that the content is harmful
    pub probability: HarmProbability,
}
