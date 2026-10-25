//! What the HTTP host adapters share: percent-encoding, retries and error excerpts.

use std::time::Duration;

/// Percent-encodes `text`: everything but the RFC 3986 unreserved characters, and `/` too
/// unless `keep_slash` (the SigV4 canonical form, which every provider here accepts).
pub(crate) fn uri_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A short, credential-free description of a failed response.
pub(crate) async fn failure(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    format!("{status}: {}", body.trim())
}

/// Whether a response or transport error is worth another attempt.
pub(crate) fn retryable(result: &std::result::Result<reqwest::Response, reqwest::Error>) -> bool {
    match result {
        Ok(response) => {
            response.status().is_server_error()
                || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        }
        Err(error) => error.is_timeout() || error.is_connect() || error.is_request(),
    }
}

/// The delay before attempt `attempt` (from 1).
pub(crate) fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200 * 2u64.pow(attempt.min(6)))
}

/// How many times a request is sent before its failure is returned.
pub(crate) const ATTEMPTS: u32 = 3;
