//! Base URL validation shared by every client in this crate.
//!
//! Each client attaches the Anthropic API key to every outbound request, so a
//! base URL that is not encrypted would transmit the credential in cleartext.
//! The validator lives here — outside any feature gate — so the default
//! [`crate::Anthropic`] client and the feature-gated Managed Agents client
//! enforce exactly the same rule.

use crate::error::{Error, Result};

/// How a client treats `http://` to a non-loopback host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsecureHttp {
    /// Rejected. `opt_in` is how the caller acknowledges plain HTTP, for a
    /// client that offers a way; the error names it.
    Rejected { opt_in: Option<&'static str> },
    /// Accepted with a warning naming the host: the caller acknowledged plain HTTP.
    Acknowledged,
}

/// Reject base URLs that would transmit the API key in cleartext.
///
/// Accepts `https://` unconditionally and `http://` when the host is loopback
/// (`localhost`, `127.0.0.0/8`, `[::1]`) for local development, or when
/// `insecure_http` is [`InsecureHttp::Acknowledged`]. Everything else — other
/// schemes, unparseable input — is a validation error naming the offending
/// scheme.
pub(crate) fn validate_base_url(base_url: &str, insecure_http: InsecureHttp) -> Result<()> {
    let parsed = url::Url::parse(base_url).map_err(|e| {
        Error::validation(
            format!(
                "base URL '{base_url}' is not a valid absolute URL ({e}). \
                 Provide a full URL such as https://api.anthropic.com."
            ),
            Some("base_url".to_string()),
        )
    })?;

    if parsed.scheme().eq_ignore_ascii_case("https") {
        return Ok(());
    }

    let is_http = parsed.scheme().eq_ignore_ascii_case("http");
    if is_http && is_loopback_host(&parsed) {
        return Ok(());
    }

    let opt_in = match insecure_http {
        InsecureHttp::Acknowledged if is_http => {
            // Only the host is logged: the path or userinfo may carry credentials.
            tracing::warn!(
                base_url.host = parsed.host_str().unwrap_or_default(),
                "accepting a plain http anthropic base url; the api key is sent unencrypted"
            );
            return Ok(());
        }
        InsecureHttp::Rejected { opt_in: Some(opt_in) } if is_http => format!(
            " For a trusted internal gateway that is reachable only over plain HTTP, {opt_in}."
        ),
        InsecureHttp::Rejected { .. } | InsecureHttp::Acknowledged => String::new(),
    };

    Err(Error::validation(
        format!(
            "base URL '{base_url}' uses scheme '{}', which would send the Anthropic API key \
             over an unencrypted connection. Use https://, or http:// with a loopback host \
             (localhost, 127.0.0.1, [::1]) for local development.{opt_in}",
            parsed.scheme()
        ),
        Some("base_url".to_string()),
    ))
}

/// True when the URL host is a loopback address or `localhost`.
fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_OPT_IN: InsecureHttp = InsecureHttp::Rejected { opt_in: None };
    const NOT_ACKNOWLEDGED: InsecureHttp = InsecureHttp::Rejected {
        opt_in: Some("call `allow_insecure_http()` on the client before setting the base URL"),
    };

    #[test]
    fn test_validate_base_url_accepts_https() {
        for insecure_http in [NO_OPT_IN, NOT_ACKNOWLEDGED, InsecureHttp::Acknowledged] {
            assert!(validate_base_url("https://api.anthropic.com", insecure_http).is_ok());
        }
    }

    #[test]
    fn test_validate_base_url_accepts_loopback_http() {
        for url in ["http://localhost:8080", "http://127.0.0.1:8080", "http://[::1]:8080"] {
            for insecure_http in [NO_OPT_IN, NOT_ACKNOWLEDGED] {
                assert!(
                    validate_base_url(url, insecure_http).is_ok(),
                    "loopback url {url} should be accepted without the opt-in"
                );
            }
        }
    }

    #[test]
    fn test_validate_base_url_rejects_non_loopback_http() {
        let err = validate_base_url("http://gateway.internal.example.com", NO_OPT_IN)
            .expect_err("non-loopback http must be rejected");
        assert!(err.is_validation(), "expected a validation error, got {err}");
        let message = err.to_string();
        assert!(message.contains("unencrypted"), "should explain the risk, got: {message}");
        assert!(message.contains("'http'"), "should name the scheme, got: {message}");
        assert!(
            !message.contains("allow_insecure_http"),
            "a client without the opt-in must not name it, got: {message}"
        );
    }

    #[test]
    fn test_validate_base_url_rejection_names_the_opt_in() {
        let err =
            validate_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic", NOT_ACKNOWLEDGED)
                .expect_err("non-loopback http without the opt-in must be rejected");
        assert!(err.is_validation(), "expected a validation error, got {err}");
        let message = err.to_string();
        assert!(message.contains("unencrypted"), "should explain the risk, got: {message}");
        assert!(message.contains("allow_insecure_http"), "should name the opt-in, got: {message}");
    }

    #[test]
    fn test_validate_base_url_accepts_non_loopback_http_with_opt_in() {
        for url in ["http://10.60.1.20:8080/api/v1/llm/anthropic", "http://gateway.corp.internal"] {
            assert!(
                validate_base_url(url, InsecureHttp::Acknowledged).is_ok(),
                "acknowledged http url {url} should be accepted"
            );
        }
    }

    #[test]
    fn test_validate_base_url_rejects_other_schemes_and_garbage() {
        for url in ["ftp://files.example.com", "ws://gateway.example.com", "not-a-url"] {
            for insecure_http in [NO_OPT_IN, NOT_ACKNOWLEDGED, InsecureHttp::Acknowledged] {
                let err =
                    validate_base_url(url, insecure_http).expect_err("{url} must be rejected");
                assert!(err.is_validation(), "expected a validation error for {url}, got {err}");
                assert!(
                    !err.to_string().contains("allow_insecure_http"),
                    "the opt-in does not apply to {url}, got: {err}"
                );
            }
        }
    }
}
