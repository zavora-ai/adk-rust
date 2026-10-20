//! A local OpenID issuer on 127.0.0.1: an ES256 signing key generated at start, its JWKS served
//! over HTTP, and signed ID tokens. The real `JwtValidator` fetches the JWKS and verifies them.

use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use axum::Router;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;

const KEY_ID: &str = "example-key-1";
pub const AUDIENCE: &str = "security-sessions-contracts";

pub struct Issuer {
    pub url: String,
    key: EncodingKey,
    task: tokio::task::JoinHandle<()>,
}

impl Issuer {
    pub async fn start() -> anyhow::Result<Self> {
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .map_err(|_| anyhow::anyhow!("P-256 key generation failed"))?;
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref())
            .map_err(|error| anyhow::anyhow!("generated key rejected: {error}"))?;
        // An uncompressed SEC1 point: 0x04, then the 32-byte x and y coordinates.
        let point = pair.public_key().as_ref().to_vec();
        let jwks = json!({ "keys": [{
            "kty": "EC",
            "crv": "P-256",
            "alg": "ES256",
            "use": "sig",
            "kid": KEY_ID,
            "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
        }]});
        let app = Router::new().route("/jwks", get(move || async move { axum::Json(jwks) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { url, key: EncodingKey::from_ec_der(pkcs8.as_ref()), task })
    }

    pub fn jwks_uri(&self) -> String {
        format!("{}/jwks", self.url)
    }

    /// A token for `subject` in `groups`, valid for ten minutes.
    pub fn token(&self, subject: &str, groups: &[&str]) -> anyhow::Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let claims = json!({
            "sub": subject,
            "iss": self.url,
            "aud": AUDIENCE,
            "iat": now,
            "exp": now + 600,
            "groups": groups,
        });
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(KEY_ID.to_string());
        Ok(jsonwebtoken::encode(&header, &claims, &self.key)?)
    }
}

impl Drop for Issuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
