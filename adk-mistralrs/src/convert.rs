//! Conversion layer between ADK types and mistral.rs types.

use adk_core::{Content, Part};
use image::DynamicImage;
use indexmap::IndexMap;
use mistralrs::AudioInput;
use serde_json::Value;

use crate::error::{MistralRsError, Result};

/// Convert ADK Content to mistral.rs message format
pub fn content_to_message(content: &Content) -> IndexMap<String, Value> {
    let mut message = IndexMap::new();

    // Convert role - map ADK roles to OpenAI-style roles
    let role = match content.role.as_str() {
        "user" => "user",
        "model" | "assistant" => "assistant",
        "system" => "system",
        "tool" | "function" => "tool",
        other => other, // Pass through unknown roles
    };
    message.insert("role".to_string(), Value::String(role.to_string()));

    // Convert content parts to text
    let text_parts: Vec<String> = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();

    if !text_parts.is_empty() {
        message.insert("content".to_string(), Value::String(text_parts.join("\n")));
    }

    // Handle function calls
    let tool_calls: Vec<Value> = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::FunctionCall { id, name, args, .. } => {
                let mut call = serde_json::Map::new();
                if let Some(id) = id {
                    call.insert("id".to_string(), Value::String(id.clone()));
                }
                call.insert("type".to_string(), Value::String("function".to_string()));

                let mut function = serde_json::Map::new();
                function.insert("name".to_string(), Value::String(name.clone()));
                function.insert(
                    "arguments".to_string(),
                    Value::String(serde_json::to_string(args).unwrap_or_default()),
                );
                call.insert("function".to_string(), Value::Object(function));

                Some(Value::Object(call))
            }
            _ => None,
        })
        .collect();

    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }

    // Handle function responses
    for part in &content.parts {
        if let Part::FunctionResponse { id, function_response } = part {
            message
                .insert("tool_call_id".to_string(), Value::String(id.clone().unwrap_or_default()));
            message.insert("name".to_string(), Value::String(function_response.name.clone()));
            message.insert("content".to_string(), function_response.response.clone());
        }
    }

    message
}

/// Convert ADK tool declarations to mistral.rs tool format
pub fn tools_to_mistralrs(tools: &serde_json::Map<String, Value>) -> Result<Vec<Value>> {
    let mut mistral_tools = Vec::new();

    for (name, tool_def) in tools {
        let tool_obj = tool_def.as_object().ok_or_else(|| {
            MistralRsError::tool_conversion(name, "Tool definition is not a JSON object")
        })?;

        let mut function = serde_json::Map::new();
        function.insert("name".to_string(), Value::String(name.clone()));

        if let Some(desc) = tool_obj.get("description") {
            function.insert("description".to_string(), desc.clone());
        }

        if let Some(params) = tool_obj.get("parameters") {
            function.insert("parameters".to_string(), params.clone());
        }

        let mut tool = serde_json::Map::new();
        tool.insert("type".to_string(), Value::String("function".to_string()));
        tool.insert("function".to_string(), Value::Object(function));

        mistral_tools.push(Value::Object(tool));
    }

    Ok(mistral_tools)
}

/// Extract tool name from mistral.rs tool format
pub fn extract_tool_name(tool: &Value) -> Option<String> {
    tool.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).map(|s| s.to_string())
}

/// Extract tool description from mistral.rs tool format
pub fn extract_tool_description(tool: &Value) -> Option<String> {
    tool.get("function")
        .and_then(|f| f.get("description"))
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
}

/// Extract tool parameters from mistral.rs tool format
pub fn extract_tool_parameters(tool: &Value) -> Option<Value> {
    tool.get("function").and_then(|f| f.get("parameters")).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_content_to_message_user() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Hello, world!".to_string() }],
        };

        let message = content_to_message(&content);
        assert_eq!(message.get("role").unwrap(), "user");
        assert_eq!(message.get("content").unwrap(), "Hello, world!");
    }

    #[test]
    fn test_content_to_message_assistant() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "Hi there!".to_string() }],
        };

        let message = content_to_message(&content);
        assert_eq!(message.get("role").unwrap(), "assistant");
    }

    #[test]
    fn test_content_to_message_with_function_call() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                id: Some("call_123".to_string()),
                name: "get_weather".to_string(),
                args: json!({"location": "Tokyo"}),
                thought_signature: None,
            }],
        };

        let message = content_to_message(&content);
        assert_eq!(message.get("role").unwrap(), "assistant");
        let tool_calls = message.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
    }

    #[test]
    fn test_tools_to_mistralrs() {
        let mut tools = serde_json::Map::new();
        tools.insert(
            "get_weather".to_string(),
            json!({
                "description": "Get weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }
            }),
        );

        let result = tools_to_mistralrs(&tools).unwrap();
        assert_eq!(result.len(), 1);

        let tool = &result[0];
        assert_eq!(extract_tool_name(tool), Some("get_weather".to_string()));
        assert_eq!(extract_tool_description(tool), Some("Get weather for a location".to_string()));
        assert!(extract_tool_parameters(tool).is_some());
    }

    #[test]
    fn test_tool_conversion_roundtrip() {
        let original_name = "test_function";
        let original_desc = "A test function";
        let original_params = json!({
            "type": "object",
            "properties": {
                "arg1": {"type": "string"},
                "arg2": {"type": "number"}
            }
        });

        let mut tools = serde_json::Map::new();
        tools.insert(
            original_name.to_string(),
            json!({
                "description": original_desc,
                "parameters": original_params.clone()
            }),
        );

        let converted = tools_to_mistralrs(&tools).unwrap();
        let tool = &converted[0];

        // Verify roundtrip preserves structure
        assert_eq!(extract_tool_name(tool), Some(original_name.to_string()));
        assert_eq!(extract_tool_description(tool), Some(original_desc.to_string()));
        assert_eq!(extract_tool_parameters(tool), Some(original_params));
    }
}

// ============================================================================
// Image Conversion Functions
// ============================================================================

/// Supported image formats for vision models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// JPEG format
    Jpeg,
    /// PNG format
    Png,
    /// WebP format
    WebP,
    /// GIF format
    Gif,
}

impl ImageFormat {
    /// Get the MIME type for this format.
    pub fn mime_type(&self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Gif => "image/gif",
        }
    }

    /// Try to detect format from MIME type.
    pub fn from_mime_type(mime_type: &str) -> Option<Self> {
        match mime_type.to_lowercase().as_str() {
            "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
            "image/png" => Some(ImageFormat::Png),
            "image/webp" => Some(ImageFormat::WebP),
            "image/gif" => Some(ImageFormat::Gif),
            _ => None,
        }
    }

    /// Check if a MIME type is a supported image format.
    pub fn is_supported_mime_type(mime_type: &str) -> bool {
        Self::from_mime_type(mime_type).is_some()
    }
}

/// Convert an ADK Part containing image data to a DynamicImage.
///
/// Supports:
/// - InlineData with base64-encoded or raw bytes
/// - FileData with URL (requires `reqwest` feature)
/// - JPEG, PNG, WebP, GIF formats
///
/// # Arguments
///
/// * `part` - The ADK Part to convert
///
/// # Returns
///
/// A DynamicImage if the part contains valid image data, None otherwise.
pub fn image_part_to_mistralrs(part: &Part) -> Option<DynamicImage> {
    match part {
        Part::InlineData { mime_type, data } => {
            if ImageFormat::is_supported_mime_type(mime_type) {
                image::load_from_memory(data).ok()
            } else {
                None
            }
        }
        Part::FileData { mime_type, file_uri } => {
            if ImageFormat::is_supported_mime_type(mime_type) {
                image_from_uri(file_uri).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Load an image from a URI (file path or URL).
///
/// # Arguments
///
/// * `uri` - File path or URL to the image
///
/// # Returns
///
/// A Result containing the DynamicImage or an error.
pub fn image_from_uri(uri: &str) -> Result<DynamicImage> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        #[cfg(feature = "reqwest")]
        {
            // Use blocking reqwest for URL fetching
            let bytes = reqwest::blocking::get(uri)
                .map_err(|e| {
                    MistralRsError::image_processing(format!(
                        "Failed to fetch URL '{}': {}",
                        uri, e
                    ))
                })?
                .bytes()
                .map_err(|e| {
                    MistralRsError::image_processing(format!("Failed to read response: {}", e))
                })?;
            image::load_from_memory(&bytes).map_err(|e| {
                MistralRsError::image_processing(format!("Failed to decode image: {}", e))
            })
        }
        #[cfg(not(feature = "reqwest"))]
        {
            Err(MistralRsError::image_processing(
                "URL loading requires the 'reqwest' feature. Enable it in Cargo.toml.",
            ))
        }
    } else if uri.starts_with("gs://") || uri.starts_with("s3://") {
        // Cloud storage URIs not supported yet
        Err(MistralRsError::image_processing(format!(
            "Cloud storage URIs not yet supported: {}",
            uri
        )))
    } else {
        // Treat as local file path
        image_from_path(uri)
    }
}

/// Load an image from a URI asynchronously (file path or URL).
///
/// # Arguments
///
/// * `uri` - File path or URL to the image
///
/// # Returns
///
/// A Result containing the DynamicImage or an error.
#[cfg(feature = "reqwest")]
pub async fn image_from_uri_async(uri: &str) -> Result<DynamicImage> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        let bytes = reqwest::get(uri)
            .await
            .map_err(|e| {
                MistralRsError::image_processing(format!("Failed to fetch URL '{}': {}", uri, e))
            })?
            .bytes()
            .await
            .map_err(|e| {
                MistralRsError::image_processing(format!("Failed to read response: {}", e))
            })?;
        image::load_from_memory(&bytes)
            .map_err(|e| MistralRsError::image_processing(format!("Failed to decode image: {}", e)))
    } else {
        // For local files, use sync loading (file I/O is fast)
        image_from_path(uri)
    }
}

/// Convert raw image bytes to a DynamicImage.
///
/// # Arguments
///
/// * `data` - Raw image bytes
///
/// # Returns
///
/// A Result containing the DynamicImage or an error.
pub fn image_from_bytes(data: &[u8]) -> Result<DynamicImage> {
    image::load_from_memory(data)
        .map_err(|e| MistralRsError::image_processing(format!("Failed to decode image: {}", e)))
}

/// Convert a base64-encoded image string to a DynamicImage.
///
/// # Arguments
///
/// * `base64_data` - Base64-encoded image data
///
/// # Returns
///
/// A Result containing the DynamicImage or an error.
pub fn image_from_base64(base64_data: &str) -> Result<DynamicImage> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| MistralRsError::image_processing(format!("Invalid base64 encoding: {}", e)))?;

    image_from_bytes(&bytes)
}

/// Load an image from a file path.
///
/// # Arguments
///
/// * `path` - Path to the image file
///
/// # Returns
///
/// A Result containing the DynamicImage or an error.
pub fn image_from_path(path: impl AsRef<std::path::Path>) -> Result<DynamicImage> {
    let path = path.as_ref();
    image::open(path).map_err(|e| {
        MistralRsError::image_processing(format!(
            "Failed to load image from '{}': {}",
            path.display(),
            e
        ))
    })
}

// ============================================================================
// Audio Conversion Functions
// ============================================================================

/// Supported audio formats for multimodal models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// WAV format
    Wav,
    /// MP3 format
    Mp3,
    /// FLAC format
    Flac,
    /// OGG format
    Ogg,
}

impl AudioFormat {
    /// Get the MIME type for this format.
    pub fn mime_type(&self) -> &'static str {
        match self {
            AudioFormat::Wav => "audio/wav",
            AudioFormat::Mp3 => "audio/mpeg",
            AudioFormat::Flac => "audio/flac",
            AudioFormat::Ogg => "audio/ogg",
        }
    }

    /// Try to detect format from MIME type.
    pub fn from_mime_type(mime_type: &str) -> Option<Self> {
        match mime_type.to_lowercase().as_str() {
            "audio/wav" | "audio/wave" | "audio/x-wav" => Some(AudioFormat::Wav),
            "audio/mp3" | "audio/mpeg" => Some(AudioFormat::Mp3),
            "audio/flac" | "audio/x-flac" => Some(AudioFormat::Flac),
            "audio/ogg" => Some(AudioFormat::Ogg),
            _ => None,
        }
    }

    /// Check if a MIME type is a supported audio format.
    pub fn is_supported_mime_type(mime_type: &str) -> bool {
        Self::from_mime_type(mime_type).is_some()
    }
}

/// Convert an ADK Part containing audio data to an AudioInput.
///
/// Supports:
/// - InlineData with raw bytes
/// - WAV, MP3, FLAC, OGG formats
///
/// # Arguments
///
/// * `part` - The ADK Part to convert
///
/// # Returns
///
/// An AudioInput if the part contains valid audio data, None otherwise.
pub fn audio_part_to_mistralrs(part: &Part) -> Option<AudioInput> {
    match part {
        Part::InlineData { mime_type, data } => {
            if AudioFormat::is_supported_mime_type(mime_type) {
                AudioInput::from_bytes(data).ok()
            } else {
                None
            }
        }
        Part::FileData { mime_type, file_uri } => {
            if AudioFormat::is_supported_mime_type(mime_type) {
                audio_from_uri(file_uri).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Load audio from a URI (file path or URL).
///
/// # Arguments
///
/// * `uri` - File path or URL to the audio file
///
/// # Returns
///
/// A Result containing the AudioInput or an error.
pub fn audio_from_uri(uri: &str) -> Result<AudioInput> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        #[cfg(feature = "reqwest")]
        {
            let bytes = reqwest::blocking::get(uri)
                .map_err(|e| {
                    MistralRsError::audio_processing(format!(
                        "Failed to fetch URL '{}': {}",
                        uri, e
                    ))
                })?
                .bytes()
                .map_err(|e| {
                    MistralRsError::audio_processing(format!("Failed to read response: {}", e))
                })?;
            AudioInput::from_bytes(&bytes).map_err(|e| {
                MistralRsError::audio_processing(format!("Failed to decode audio: {}", e))
            })
        }
        #[cfg(not(feature = "reqwest"))]
        {
            Err(MistralRsError::audio_processing(
                "URL loading requires the 'reqwest' feature. Enable it in Cargo.toml.",
            ))
        }
    } else if uri.starts_with("gs://") || uri.starts_with("s3://") {
        Err(MistralRsError::audio_processing(format!(
            "Cloud storage URIs not yet supported: {}",
            uri
        )))
    } else {
        audio_from_path(uri)
    }
}

/// Convert raw audio bytes to an AudioInput.
///
/// # Arguments
///
/// * `data` - Raw audio bytes
///
/// # Returns
///
/// A Result containing the AudioInput or an error.
pub fn audio_from_bytes(data: &[u8]) -> Result<AudioInput> {
    AudioInput::from_bytes(data)
        .map_err(|e| MistralRsError::audio_processing(format!("Failed to decode audio: {}", e)))
}

/// Convert a base64-encoded audio string to an AudioInput.
///
/// # Arguments
///
/// * `base64_data` - Base64-encoded audio data
///
/// # Returns
///
/// A Result containing the AudioInput or an error.
pub fn audio_from_base64(base64_data: &str) -> Result<AudioInput> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| MistralRsError::audio_processing(format!("Invalid base64 encoding: {}", e)))?;

    audio_from_bytes(&bytes)
}

/// Load audio from a file path.
///
/// # Arguments
///
/// * `path` - Path to the audio file
///
/// # Returns
///
/// A Result containing the AudioInput or an error.
pub fn audio_from_path(path: impl AsRef<std::path::Path>) -> Result<AudioInput> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| {
        MistralRsError::audio_processing(format!(
            "Failed to read audio file '{}': {}",
            path.display(),
            e
        ))
    })?;

    audio_from_bytes(&bytes)
}

// ============================================================================
// Content Extraction Helpers
// ============================================================================

/// Extract all images from an ADK Content.
///
/// # Arguments
///
/// * `content` - The ADK Content to extract images from
///
/// # Returns
///
/// A vector of DynamicImages found in the content.
pub fn extract_images_from_content(content: &Content) -> Vec<DynamicImage> {
    content.parts.iter().filter_map(image_part_to_mistralrs).collect()
}

/// Extract all audio inputs from an ADK Content.
///
/// # Arguments
///
/// * `content` - The ADK Content to extract audio from
///
/// # Returns
///
/// A vector of AudioInputs found in the content.
pub fn extract_audio_from_content(content: &Content) -> Vec<AudioInput> {
    content.parts.iter().filter_map(audio_part_to_mistralrs).collect()
}

/// Extract text from an ADK Content.
///
/// # Arguments
///
/// * `content` - The ADK Content to extract text from
///
/// # Returns
///
/// A string containing all text parts joined by newlines.
pub fn extract_text_from_content(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod image_audio_tests {
    use super::*;

    #[test]
    fn test_image_format_from_mime_type() {
        assert_eq!(ImageFormat::from_mime_type("image/jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_mime_type("image/jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_mime_type("image/png"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::from_mime_type("image/webp"), Some(ImageFormat::WebP));
        assert_eq!(ImageFormat::from_mime_type("image/gif"), Some(ImageFormat::Gif));
        assert_eq!(ImageFormat::from_mime_type("IMAGE/JPEG"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_mime_type("text/plain"), None);
    }

    #[test]
    fn test_image_format_mime_type() {
        assert_eq!(ImageFormat::Jpeg.mime_type(), "image/jpeg");
        assert_eq!(ImageFormat::Png.mime_type(), "image/png");
        assert_eq!(ImageFormat::WebP.mime_type(), "image/webp");
        assert_eq!(ImageFormat::Gif.mime_type(), "image/gif");
    }

    #[test]
    fn test_audio_format_from_mime_type() {
        assert_eq!(AudioFormat::from_mime_type("audio/wav"), Some(AudioFormat::Wav));
        assert_eq!(AudioFormat::from_mime_type("audio/wave"), Some(AudioFormat::Wav));
        assert_eq!(AudioFormat::from_mime_type("audio/x-wav"), Some(AudioFormat::Wav));
        assert_eq!(AudioFormat::from_mime_type("audio/mp3"), Some(AudioFormat::Mp3));
        assert_eq!(AudioFormat::from_mime_type("audio/mpeg"), Some(AudioFormat::Mp3));
        assert_eq!(AudioFormat::from_mime_type("audio/flac"), Some(AudioFormat::Flac));
        assert_eq!(AudioFormat::from_mime_type("audio/ogg"), Some(AudioFormat::Ogg));
        assert_eq!(AudioFormat::from_mime_type("AUDIO/WAV"), Some(AudioFormat::Wav));
        assert_eq!(AudioFormat::from_mime_type("text/plain"), None);
    }

    #[test]
    fn test_audio_format_mime_type() {
        assert_eq!(AudioFormat::Wav.mime_type(), "audio/wav");
        assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
        assert_eq!(AudioFormat::Flac.mime_type(), "audio/flac");
        assert_eq!(AudioFormat::Ogg.mime_type(), "audio/ogg");
    }

    #[test]
    fn test_image_part_to_mistralrs_non_image() {
        let part = Part::Text { text: "hello".to_string() };
        assert!(image_part_to_mistralrs(&part).is_none());
    }

    #[test]
    fn test_audio_part_to_mistralrs_non_audio() {
        let part = Part::Text { text: "hello".to_string() };
        assert!(audio_part_to_mistralrs(&part).is_none());
    }

    #[test]
    fn test_extract_text_from_content() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Hello".to_string() },
                Part::Text { text: "World".to_string() },
            ],
        };
        assert_eq!(extract_text_from_content(&content), "Hello\nWorld");
    }

    #[test]
    fn test_image_from_base64_invalid() {
        let result = image_from_base64("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_audio_from_base64_invalid() {
        let result = audio_from_base64("not-valid-base64!!!");
        assert!(result.is_err());
    }
}
