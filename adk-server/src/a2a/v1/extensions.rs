//! The A2A extensions a caller asks for, as the `A2A-Extensions` request header names them
//! (A2A §3.2.4), read as a2a-go reads them.
//!
//! A server refuses a call that leaves out an extension its agent card marks `required`
//! with `ExtensionSupportRequired` (-32008), and hands the requested extensions to each
//! run's [`ExecutionContext`](super::hooks::ExecutionContext).

use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;

/// The request header naming the extensions a caller asks for.
pub const EXTENSIONS_HEADER: &str = "a2a-extensions";

/// The extension URIs a request asks for: every `A2A-Extensions` header, split on commas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestedExtensions(pub Vec<String>);

impl RequestedExtensions {
    /// The extensions `headers` ask for, each once, in order.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut uris: Vec<String> = Vec::new();
        for value in headers.get_all(EXTENSIONS_HEADER) {
            let Ok(value) = value.to_str() else { continue };
            for uri in value.split(',').map(str::trim).filter(|uri| !uri.is_empty()) {
                if !uris.iter().any(|known| known == uri) {
                    uris.push(uri.to_string());
                }
            }
        }
        Self(uris)
    }

    /// Whether the request asks for `uri`.
    pub fn contains(&self, uri: &str) -> bool {
        self.0.iter().any(|requested| requested == uri)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestedExtensions {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_header_and_comma_separated_uri_counts_once() {
        let mut headers = HeaderMap::new();
        headers.append(
            "A2A-Extensions",
            "https://a.example/ext, https://b.example/ext".parse().unwrap(),
        );
        headers.append("a2a-extensions", "https://a.example/ext".parse().unwrap());
        let requested = RequestedExtensions::from_headers(&headers);
        assert_eq!(requested.0, ["https://a.example/ext", "https://b.example/ext"]);
        assert!(requested.contains("https://b.example/ext"));
    }
}
