//! AWS Signature Version 4 for the requests this module signs with temporary credentials.
//!
//! Covers what a JSON data-plane call needs: one region and service, headers signed by
//! value, and a payload hashed in full. Paths and query strings are taken as the caller
//! wrote them, so they must hold only unreserved characters, `/`, `=` and `&`.

use aws_lc_rs::{digest, hmac};
use chrono::{DateTime, Utc};
use zeroize::Zeroizing;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The request being signed.
pub(crate) struct SigningRequest<'a> {
    /// The HTTP method, upper case.
    pub(crate) method: &'a str,
    /// The `Host` header.
    pub(crate) host: &'a str,
    /// The absolute path.
    pub(crate) path: &'a str,
    /// The query string without `?`, parameters sorted by name.
    pub(crate) query: &'a str,
    /// Further headers to sign, as lower-case name and value. `host`, `x-amz-date` and
    /// `x-amz-security-token` are added by [`sign`].
    pub(crate) headers: &'a [(&'a str, &'a str)],
    /// The request body.
    pub(crate) payload: &'a [u8],
}

/// The credentials and scope a request is signed under.
pub(crate) struct SigningScope<'a> {
    pub(crate) access_key_id: &'a str,
    pub(crate) secret_access_key: &'a str,
    /// The session token of temporary credentials, sent and signed as `x-amz-security-token`.
    pub(crate) session_token: Option<&'a str>,
    pub(crate) region: &'a str,
    pub(crate) service: &'a str,
    pub(crate) time: DateTime<Utc>,
}

/// The headers that authenticate a signed request.
pub(crate) struct SignedHeaders {
    /// The `X-Amz-Date` header.
    pub(crate) amz_date: String,
    /// The `Authorization` header.
    pub(crate) authorization: String,
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, data))
}

/// Signs `request` under `scope` (Signature Version 4, header-based).
pub(crate) fn sign(request: &SigningRequest<'_>, scope: &SigningScope<'_>) -> SignedHeaders {
    let amz_date = scope.time.format("%Y%m%dT%H%M%SZ").to_string();
    let date = &amz_date[..8];

    let mut headers: Vec<(&str, &str)> = request.headers.to_vec();
    headers.push(("host", request.host));
    headers.push(("x-amz-date", &amz_date));
    if let Some(token) = scope.session_token {
        headers.push(("x-amz-security-token", token));
    }
    headers.sort_unstable();
    let canonical_headers: String =
        headers.iter().map(|(name, value)| format!("{name}:{}\n", value.trim())).collect();
    let signed_headers = headers.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        request.method,
        request.path,
        request.query,
        sha256_hex(request.payload)
    );
    let credential_scope = format!("{date}/{}/{}/aws4_request", scope.region, scope.service);
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let secret = Zeroizing::new(format!("AWS4{}", scope.secret_access_key));
    let date_key = hmac_sha256(secret.as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(date_key.as_ref(), scope.region.as_bytes());
    let service_key = hmac_sha256(region_key.as_ref(), scope.service.as_bytes());
    let signing_key = hmac_sha256(service_key.as_ref(), b"aws4_request");
    let signature = hex::encode(hmac_sha256(signing_key.as_ref(), string_to_sign.as_bytes()));

    let authorization = format!(
        "{ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        scope.access_key_id
    );
    SignedHeaders { amz_date, authorization }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    /// The credentials, scope and time of the AWS Signature Version 4 test suite
    /// (`aws-sig-v4-test-suite`).
    fn suite_scope(session_token: Option<&'static str>) -> SigningScope<'static> {
        SigningScope {
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            session_token,
            region: "us-east-1",
            service: "service",
            time: Utc.with_ymd_and_hms(2015, 8, 30, 12, 36, 0).unwrap(),
        }
    }

    fn suite_request(method: &'static str) -> SigningRequest<'static> {
        SigningRequest {
            method,
            host: "example.amazonaws.com",
            path: "/",
            query: "",
            headers: &[],
            payload: b"",
        }
    }

    #[test]
    fn get_vanilla_matches_the_aws_test_suite() {
        let signed = sign(&suite_request("GET"), &suite_scope(None));
        assert_eq!(signed.amz_date, "20150830T123600Z");
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn post_vanilla_matches_the_aws_test_suite() {
        let signed = sign(&suite_request("POST"), &suite_scope(None));
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b"
        );
    }

    /// The shape of the AgentCore call: temporary credentials, a path, and a JSON body. The
    /// expected value is what the AWS CLI's signer (botocore `SigV4Auth`) produces for the
    /// same request under the suite's credentials and time.
    #[test]
    fn a_json_post_with_a_session_token_matches_the_aws_cli_signer() {
        let request = SigningRequest {
            method: "POST",
            host: "example.amazonaws.com",
            path: "/identities/GetWorkloadAccessTokenForJWT",
            query: "",
            headers: &[("content-type", "application/json")],
            payload: br#"{"workloadName":"w","userToken":"t"}"#,
        };
        let signed = sign(&request, &suite_scope(Some("session-token")));
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, \
             Signature=8d73c7bf2f1eedaf8b30827f17c0349b3d1b181dbfec808c3e22e0b15b4446ee"
        );
    }
}
