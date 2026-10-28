//! A local identity provider for the validators' tests: an RSA key, its JWKS served by a mock
//! server, and RS256 tokens that carry or omit the `iss` and `aud` claims.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{TokenError, TokenValidator};

const KEY_ID: &str = "test-key-1";

/// The signing key and its public JWK, generated once per test binary.
fn key() -> &'static (EncodingKey, Value) {
    static KEY: OnceLock<(EncodingKey, Value)> = OnceLock::new();
    KEY.get_or_init(|| {
        let pair = KeyPair::generate(KeySize::Rsa2048).expect("RSA key generation");
        let pkcs8 = STANDARD.encode(pair.as_der().expect("PKCS#8 encoding").as_ref());
        let lines = pkcs8.as_bytes().chunks(64).map(String::from_utf8_lossy).collect::<Vec<_>>();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            lines.join("\n")
        );
        let public = pair.public_key();
        let jwk = json!({
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": KEY_ID,
            "n": URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
            "e": URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
        });
        (EncodingKey::from_rsa_pem(pem.as_bytes()).expect("PKCS#8 PEM key"), jwk)
    })
}

/// A mock server publishing the test key's JWKS at `/jwks`.
pub(crate) struct TestIssuer {
    server: MockServer,
}

impl TestIssuer {
    pub(crate) async fn start() -> Self {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "keys": [key().1] })))
            .mount(&server)
            .await;
        Self { server }
    }

    pub(crate) fn jwks_uri(&self) -> String {
        format!("{}/jwks", self.server.uri())
    }
}

/// An RS256 token for `alice`, valid for ten minutes, with `iss` and `aud` only when given.
pub(crate) fn token(iss: Option<&str>, aud: Option<&str>) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_secs();
    let mut claims = json!({ "sub": "alice", "iat": now, "exp": now + 600 });
    if let Some(iss) = iss {
        claims["iss"] = json!(iss);
    }
    if let Some(aud) = aud {
        claims["aud"] = json!(aud);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KEY_ID.to_string());
    jsonwebtoken::encode(&header, &claims, &key().0).expect("token signing")
}

/// Asserts that `validator` accepts a token naming `iss` and `aud`, and refuses one without
/// `aud`, one without `iss`, and one for another audience.
pub(crate) async fn assert_requires_iss_and_aud(
    validator: &dyn TokenValidator,
    iss: &str,
    aud: &str,
) {
    let accepted = validator.validate(&token(Some(iss), Some(aud))).await;
    assert!(accepted.as_ref().is_ok_and(|claims| claims.sub == "alice"), "{accepted:?}");

    let no_aud = validator.validate(&token(Some(iss), None)).await;
    assert!(
        matches!(&no_aud, Err(TokenError::MissingClaim(claim)) if claim == "aud"),
        "{no_aud:?}"
    );

    let no_iss = validator.validate(&token(None, Some(aud))).await;
    assert!(
        matches!(&no_iss, Err(TokenError::MissingClaim(claim)) if claim == "iss"),
        "{no_iss:?}"
    );

    let foreign_aud = validator.validate(&token(Some(iss), Some("another-service"))).await;
    assert!(matches!(&foreign_aud, Err(TokenError::ValidationError(_))), "{foreign_aud:?}");
}
