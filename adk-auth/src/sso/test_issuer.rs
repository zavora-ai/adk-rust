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

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_secs()
}

/// Claims for `alice`, valid for ten minutes, with `iss` and `aud` only when given.
fn claims(iss: Option<&str>, aud: Option<&str>) -> Value {
    let now = now();
    let mut claims = json!({ "sub": "alice", "iat": now, "exp": now + 600 });
    if let Some(iss) = iss {
        claims["iss"] = json!(iss);
    }
    if let Some(aud) = aud {
        claims["aud"] = json!(aud);
    }
    claims
}

/// `claims` signed RS256 by the test key, under `header` with the test key's id.
fn signed(mut header: Header, claims: &Value) -> String {
    header.kid = Some(KEY_ID.to_string());
    jsonwebtoken::encode(&header, claims, &key().0).expect("token signing")
}

/// An RS256 token for `alice`, valid for ten minutes, with `iss` and `aud` only when given.
pub(crate) fn token(iss: Option<&str>, aud: Option<&str>) -> String {
    signed(Header::new(Algorithm::RS256), &claims(iss, aud))
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

/// Asserts that `validator`, which accepts RS256 tokens naming `iss` and `aud`, refuses the
/// tokens an attacker or a stale client presents: an expired one, one not yet valid, one
/// from another issuer, an unsigned `alg: none` one, an HS256 one keyed with the public
/// JWK (RS/HS confusion), one whose signature does not match, and one with a critical
/// header extension it does not understand.
pub(crate) async fn assert_refuses_forged_and_stale_tokens(
    validator: &dyn TokenValidator,
    iss: &str,
    aud: &str,
) {
    let valid = claims(Some(iss), Some(aud));

    // An hour past expiry, well beyond the default 60-second leeway.
    let mut expired = valid.clone();
    expired["exp"] = json!(now() - 3600);
    let refused = validator.validate(&signed(Header::new(Algorithm::RS256), &expired)).await;
    assert!(matches!(&refused, Err(TokenError::Expired)), "expired: {refused:?}");

    let mut immature = valid.clone();
    immature["nbf"] = json!(now() + 3600);
    let refused = validator.validate(&signed(Header::new(Algorithm::RS256), &immature)).await;
    assert!(matches!(&refused, Err(TokenError::NotYetValid)), "nbf: {refused:?}");

    let foreign_iss = claims(Some("https://evil.example"), Some(aud));
    let refused = validator.validate(&signed(Header::new(Algorithm::RS256), &foreign_iss)).await;
    assert!(matches!(&refused, Err(TokenError::ValidationError(_))), "issuer: {refused:?}");

    let unsigned = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(json!({ "alg": "none", "kid": KEY_ID }).to_string()),
        URL_SAFE_NO_PAD.encode(valid.to_string()),
    );
    let refused = validator.validate(&unsigned).await;
    assert!(matches!(&refused, Err(TokenError::DecodingError(_))), "alg none: {refused:?}");

    // The published JWK is public, so an HMAC keyed with it is a signature anyone can forge.
    let public_jwk = serde_json::to_vec(&key().1).expect("JWK serialises");
    let mut hs_header = Header::new(Algorithm::HS256);
    hs_header.kid = Some(KEY_ID.to_string());
    let hs_key = EncodingKey::from_secret(&public_jwk).expect("non-empty HMAC key");
    let confused = jsonwebtoken::encode(&hs_header, &valid, &hs_key).expect("token signing");
    let refused = validator.validate(&confused).await;
    assert!(matches!(&refused, Err(TokenError::DecodingError(_))), "HS/RS: {refused:?}");

    // The same HMAC signature under an RS256 header fails the RSA signature check.
    let rs_header = URL_SAFE_NO_PAD.encode(json!({ "alg": "RS256", "kid": KEY_ID }).to_string());
    let (_, rest) = confused.split_once('.').expect("three-part token");
    let refused = validator.validate(&format!("{rs_header}.{rest}")).await;
    assert!(matches!(&refused, Err(TokenError::InvalidSignature)), "signature: {refused:?}");

    let mut crit_header = Header::new(Algorithm::RS256);
    crit_header.crit = Some(vec!["https://example.com/unknown".into()]);
    let refused = validator.validate(&signed(crit_header, &valid)).await;
    assert!(matches!(&refused, Err(TokenError::DecodingError(_))), "crit: {refused:?}");
}
