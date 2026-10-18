#[cfg(any(
    feature = "gemini",
    feature = "gemini-interactions",
    feature = "openai",
    feature = "anthropic",
    feature = "deepseek",
    feature = "groq",
    feature = "ollama",
    feature = "azure-ai"
))]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

/// Encode binary bytes as base64.
#[cfg(any(
    feature = "gemini",
    feature = "gemini-interactions",
    feature = "openai",
    feature = "anthropic",
    feature = "deepseek",
    feature = "groq",
    feature = "ollama",
    feature = "azure-ai"
))]
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

/// Name an inline PDF after the last segment of its source URI, or `document.pdf`
/// when the payload carries no usable name.
#[cfg(feature = "openai")]
pub(crate) fn pdf_filename(uri: Option<&str>) -> String {
    uri.and_then(|uri| uri.split(['?', '#']).next())
        .and_then(|path| path.rsplit('/').next())
        .filter(|name| !name.is_empty() && !name.contains(':'))
        .unwrap_or("document.pdf")
        .to_string()
}

/// Convert file URI attachments into a text payload for providers without URI-native attachment
/// support.
#[cfg(any(
    feature = "gemini",
    feature = "openai",
    feature = "anthropic",
    feature = "ollama",
    feature = "groq",
    feature = "deepseek",
    feature = "bedrock"
))]
pub(crate) fn file_attachment_to_text(mime_type: &str, file_uri: &str) -> String {
    format!("<attachment mime_type=\"{mime_type}\" uri=\"{file_uri}\" />")
}
