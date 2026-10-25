//! HTTP plumbing shared by the reference adapters over web APIs (spec 3.5 §7.13).

use std::time::Duration;

use serde_json::Value;

use crate::error::{DriverError, Result};

/// The longest body read from a service, so a hostile or broken one cannot exhaust memory.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// An HTTP client for `service` with `timeout` per request.
pub(crate) fn http_client(service: &str, timeout: Duration) -> Result<reqwest::Client> {
    adk_core::ensure_crypto_provider();
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("adk-drivers/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| DriverError::unavailable(service, error))
}

/// `base` as an API address ending in `/`: `https`, or `http` on loopback only, since the
/// driver's token travels with every request.
pub(crate) fn checked_base(base: &str) -> Result<String> {
    let url = reqwest::Url::parse(base)
        .map_err(|error| DriverError::invalid(format!("`{base}` is not an address: {error}")))?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(DriverError::invalid(format!(
                "`{base}` must be https (plain http only on loopback), since the driver's credentials travel with each request"
            )));
        }
    }
    let text = url.to_string();
    Ok(if text.ends_with('/') { text } else { format!("{text}/") })
}

/// The JSON body of `response`, or the error its status means.
pub(crate) async fn read_json(service: &str, response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let body = response.bytes().await.map_err(|error| DriverError::unavailable(service, error))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(DriverError::protocol(service, "the answer is larger than 4 MiB"));
    }
    let detail = || {
        let text = String::from_utf8_lossy(&body);
        crate::outbound::bounded(text.trim(), 300)
    };
    match status.as_u16() {
        200..=299 => serde_json::from_slice(&body)
            .map_err(|error| DriverError::protocol(service, format!("not JSON: {error}"))),
        401 | 403 => Err(DriverError::Unauthorized {
            service: service.into(),
            reason: format!("{status}: {}", detail()),
        }),
        404 => Err(DriverError::not_found(format!("{service}: {}", detail()))),
        429 => Err(DriverError::RateLimited {
            reason: format!("{service} asked to slow down"),
            retry_after_secs: retry_after.unwrap_or(60),
        }),
        400 | 409 | 422 => Err(DriverError::invalid(format!(
            "{service} refused the request ({status}): {}",
            detail()
        ))),
        _ => Err(DriverError::unavailable(service, format!("{status}: {}", detail()))),
    }
}
