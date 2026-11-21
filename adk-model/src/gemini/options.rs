//! Gemini request options carried through the provider-neutral request types: the media
//! resolution in `GenerateContentConfig::extensions["gemini"]`, and a video part's sampling
//! in its `annotations["gemini"]`.

use adk_core::GenerateContentConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use adk_gemini::{MediaResolution, VideoMetadata};

/// The namespace the Gemini client reads in `GenerateContentConfig::extensions` and in a
/// part's `annotations`.
pub const GEMINI_EXTENSION_NAMESPACE: &str = "gemini";

/// Gemini options for one request, in `GenerateContentConfig::extensions["gemini"]`.
///
/// # Example
///
/// ```
/// use adk_core::GenerateContentConfig;
/// use adk_model::gemini::{GeminiRequestOptions, MediaResolution};
///
/// let mut config = GenerateContentConfig::default();
/// GeminiRequestOptions::default().with_media_resolution(MediaResolution::Low).insert_into(&mut config);
/// assert_eq!(config.extensions["gemini"]["mediaResolution"], "MEDIA_RESOLUTION_LOW");
/// assert_eq!(
///     GeminiRequestOptions::from_config(&config).media_resolution,
///     Some(MediaResolution::Low)
/// );
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GeminiRequestOptions {
    /// The resolution images and video frames are tokenised at; video costs about 100
    /// tokens a second at `Low` and about 300 otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
}

impl GeminiRequestOptions {
    /// Tokenises images and video frames at `resolution`.
    pub fn with_media_resolution(mut self, resolution: MediaResolution) -> Self {
        self.media_resolution = Some(resolution);
        self
    }

    /// Writes these options into `config.extensions["gemini"]`, keeping other keys there.
    pub fn insert_into(&self, config: &mut GenerateContentConfig) {
        let entry = config
            .extensions
            .entry(GEMINI_EXTENSION_NAMESPACE.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let (Value::Object(target), Ok(Value::Object(options))) =
            (entry, serde_json::to_value(self))
        {
            target.extend(options);
        }
    }

    /// The options in `config.extensions["gemini"]`; an unreadable namespace reads as none.
    pub fn from_config(config: &GenerateContentConfig) -> Self {
        config
            .extensions
            .get(GEMINI_EXTENSION_NAMESPACE)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }
}

/// Annotations for a video `Part::InlineData` or `Part::FileData` that the Gemini client
/// sends as the part's `videoMetadata`: the frame rate it samples at and the span it reads.
///
/// # Example
///
/// ```
/// use adk_core::Part;
/// use adk_model::gemini::{VideoMetadata, video_part_annotations};
///
/// let part = Part::FileData {
///     mime_type: "video/mp4".into(),
///     file_uri: "https://generativelanguage.googleapis.com/v1beta/files/abc".into(),
///     annotations: Some(video_part_annotations(&VideoMetadata::default().with_fps(1.0))),
/// };
/// # let _ = part;
/// ```
pub fn video_part_annotations(metadata: &VideoMetadata) -> Value {
    serde_json::json!({ GEMINI_EXTENSION_NAMESPACE: { "videoMetadata": metadata } })
}

/// The `videoMetadata` a part's annotations carry, if any.
pub(crate) fn video_metadata(annotations: Option<&Value>) -> Option<VideoMetadata> {
    let value = annotations?.get(GEMINI_EXTENSION_NAMESPACE)?.get("videoMetadata")?;
    serde_json::from_value(value.clone()).ok()
}
