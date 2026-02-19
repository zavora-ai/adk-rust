use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

/// Encode binary bytes as base64.
pub(crate) fn encode_base64(data: &[u8]) -> String {
    BASE64_STANDARD.encode(data)
}

/// Convert inline attachment bytes into a text payload for providers that don't support
/// the attachment MIME type natively.
#[cfg(any(
    feature = "openai",
    feature = "anthropic",
    feature = "deepseek",
    feature = "groq",
    feature = "ollama"
))]
pub(crate) fn inline_attachment_to_text(mime_type: &str, data: &[u8]) -> String {
    let encoded = encode_base64(data);
    format!("<attachment mime_type=\"{mime_type}\" encoding=\"base64\">{encoded}</attachment>")
}

/// Convert file URI attachments into a text payload for providers without URI-native attachment
/// support.
pub(crate) fn file_attachment_to_text(mime_type: &str, file_uri: &str) -> String {
    format!("<attachment mime_type=\"{mime_type}\" uri=\"{file_uri}\" />")
}
