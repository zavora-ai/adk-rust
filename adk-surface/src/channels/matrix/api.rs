//! The Matrix client-server API client (`/_matrix/client/v3`): the access token travels in
//! the `Authorization` header and is kept out of every error and log line. Room and event ids
//! are percent-encoded as path segments.

use std::fmt;
use std::time::Duration;

use reqwest::{Method, Url};
use serde_json::Value;

use super::MatrixError;
use crate::channels::shared::{
    MAX_RATE_LIMIT_WAIT, RATE_LIMIT_ATTEMPTS, REQUEST_TIMEOUT, checked_base, checked_token,
};

/// The client-server API of one account on one homeserver.
pub(crate) struct MatrixApi {
    client: reqwest::Client,
    base: Url,
    token: String,
}

impl fmt::Debug for MatrixApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixApi").field("base", &self.base.as_str()).finish_non_exhaustive()
    }
}

impl MatrixApi {
    /// The API of `homeserver` for the account with `token`, or why they cannot be used.
    pub(crate) fn new(homeserver: &str, token: &str) -> Result<Self, String> {
        checked_token(token, "the Matrix access token (MATRIX_ACCESS_TOKEN)")?;
        let base =
            checked_base(homeserver, "the Matrix homeserver address (MATRIX_HOMESERVER)", false)?;
        let base = Url::parse(&base).map_err(|error| error.to_string())?;
        let client = reqwest::Client::builder().build().map_err(|error| {
            format!("the Matrix HTTP client could not be built: {}", error.without_url())
        })?;
        Ok(Self { client, base, token: token.to_string() })
    }

    /// The address of `/_matrix/client/v3/` and `segments`, each percent-encoded.
    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(["_matrix", "client", "v3"]).extend(segments);
        }
        url
    }

    /// Calls `segments` with `method`, `query` and `body`, waiting `wait` at most, and waiting
    /// out a short rate limit when `patient`.
    pub(crate) async fn call(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
        wait: Duration,
        patient: bool,
    ) -> Result<Value, MatrixError> {
        let mut attempt = 1;
        loop {
            match self.call_once(method.clone(), segments, query, body, wait).await {
                Err(MatrixError::RateLimited { retry_after_ms, .. })
                    if patient
                        && attempt < RATE_LIMIT_ATTEMPTS
                        && Duration::from_millis(retry_after_ms) <= MAX_RATE_LIMIT_WAIT =>
                {
                    tracing::debug!(retry.after_ms = retry_after_ms, "matrix rate limit, waiting");
                    tokio::time::sleep(Duration::from_millis(retry_after_ms.max(100))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn call_once(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
        wait: Duration,
    ) -> Result<Value, MatrixError> {
        // The endpoint without its ids, for errors and logs.
        let endpoint = segments.first().copied().unwrap_or_default().to_string();
        let unreachable = |error: reqwest::Error| MatrixError::Unreachable {
            endpoint: endpoint.clone(),
            reason: self.redact(&error.without_url().to_string()),
        };
        let mut request = self
            .client
            .request(method, self.url(segments))
            .bearer_auth(&self.token)
            .query(query)
            .timeout(wait);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreachable)?;
        let status = response.status();
        let text = response.text().await.map_err(unreachable)?;
        let reply: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            if reply.is_null() && !text.trim().is_empty() {
                return Err(MatrixError::InvalidResponse {
                    endpoint,
                    reason: format!("HTTP {status}, not JSON"),
                });
            }
            return Ok(reply);
        }
        let errcode = reply["errcode"].as_str().unwrap_or_default().to_string();
        tracing::debug!(matrix.endpoint = %endpoint, matrix.errcode = %errcode, "matrix refused a call");
        Err(match (status.as_u16(), errcode.as_str()) {
            (401, _) | (_, "M_UNKNOWN_TOKEN" | "M_MISSING_TOKEN") => MatrixError::Unauthorized,
            (429, _) | (_, "M_LIMIT_EXCEEDED") => MatrixError::RateLimited {
                endpoint,
                retry_after_ms: reply["retry_after_ms"].as_u64().unwrap_or(1000),
            },
            (status, _) => MatrixError::Refused {
                endpoint,
                status,
                errcode,
                error: self.redact(reply["error"].as_str().unwrap_or("no description")),
            },
        })
    }

    /// Downloads `file`, an `mxc://` image, through the authenticated media endpoint
    /// (`/_matrix/client/v1/media/download`), falling back to the legacy one
    /// (`/_matrix/media/v3/download`) when the homeserver gives nothing there; with the access
    /// token, to this homeserver only.
    pub(crate) async fn download(
        &self,
        file: crate::channels::media::FileRef,
    ) -> Option<(String, Vec<u8>)> {
        let (server, id) = file.url.strip_prefix("mxc://")?.split_once('/')?;
        let server_ok = !server.is_empty()
            && server.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')
            });
        let id_ok = !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if !server_ok || !id_ok {
            return None;
        }
        let endpoints: [&[&str]; 2] = [
            &["_matrix", "client", "v1", "media", "download"],
            &["_matrix", "media", "v3", "download"],
        ];
        for endpoint in endpoints {
            let mut url = self.base.clone();
            if let Ok(mut path) = url.path_segments_mut() {
                path.pop_if_empty().extend(endpoint).extend([server, id]);
            }
            let request = self.client.get(url).bearer_auth(&self.token);
            let found = crate::channels::media::download(request, file.media_type.as_deref()).await;
            if found.is_some() {
                return found;
            }
        }
        None
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.token, "[token]")
    }
}

/// How long any call other than a long poll may take.
pub(crate) const CALL: Duration = REQUEST_TIMEOUT;
