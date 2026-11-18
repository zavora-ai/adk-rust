//! The Bot Connector's token on every activity (Bot Framework, "Authenticate requests from
//! the Bot Connector service to your bot"): the signing keys from the OpenID metadata
//! document, cached for at most a day, and every check the protocol lists.
//!
//! | Check | Refused with |
//! |---|---|
//! | `Authorization: Bearer <JWT>` is present | `401` |
//! | Signed with `RS256` (the metadata's `id_token_signing_alg_values_supported`) by a key in the metadata's `jwks_uri` | `401` |
//! | `iss` is `https://api.botframework.com`, `aud` is the bot's app id, `exp` and `nbf` hold with five minutes of clock skew | `401` |
//! | The signing key endorses the activity's `channelId` | `403` |
//! | The token's `serviceurl` claim is the activity's `serviceUrl` | `403` |
//!
//! A key id the cache does not hold refreshes the keys at most once every five minutes, so a
//! flood of forged tokens cannot make the adapter hammer the metadata endpoint.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;

use crate::channels::shared::{REQUEST_TIMEOUT, checked_base};

/// The Bot Connector's OpenID metadata document.
pub(crate) const OPENID_CONFIGURATION: &str =
    "https://login.botframework.com/v1/.well-known/openidconfiguration";
/// The issuer of the Bot Connector's tokens.
pub(crate) const ISSUER: &str = "https://api.botframework.com";
/// The clock skew a token's validity period is given.
const CLOCK_SKEW_SECS: u64 = 5 * 60;
/// How long the signing keys are kept before they are fetched again.
const KEYS_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// The least time between two fetches an unknown key id causes.
const UNKNOWN_KEY_REFRESH: Duration = Duration::from_secs(5 * 60);
/// The most keys kept from one document.
const MAX_KEYS: usize = 64;

/// Why an activity's token was refused; the webhook answers with its status and never says
/// more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No bearer token, or one that is not a valid Bot Connector token: `401`.
    Unauthenticated(&'static str),
    /// A valid token that does not vouch for this activity: `403`.
    Forbidden(&'static str),
    /// The signing keys could not be fetched: `503`, so the Bot Connector delivers again.
    Unavailable,
}

/// A signing key and the channels it endorses.
struct Key {
    decoding: DecodingKey,
    endorsements: Vec<String>,
}

#[derive(Default)]
struct Keys {
    by_id: HashMap<String, Arc<Key>>,
    algorithms: Vec<Algorithm>,
    fetched_at: Option<Instant>,
}

#[derive(Deserialize)]
struct Metadata {
    jwks_uri: String,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
    #[serde(default)]
    endorsements: Vec<String>,
}

/// The claims of a token that passed every check but the activity's.
#[derive(Debug, Clone)]
pub(crate) struct Verified {
    service_url: Option<String>,
    endorsements: Vec<String>,
}

impl Verified {
    /// Whether the token vouches for an activity of `channel` from `service_url`: the key
    /// endorses the channel, and the token names the same service address.
    pub(crate) fn vouches_for(&self, channel: &str, service_url: &str) -> Result<(), Refusal> {
        if !channel.is_empty() && !self.endorsements.iter().any(|endorsed| endorsed == channel) {
            return Err(Refusal::Forbidden("the signing key does not endorse the channel"));
        }
        let claimed = self.service_url.as_deref().map(|url| url.trim_end_matches('/'));
        if claimed.is_none_or(|claimed| claimed != service_url.trim_end_matches('/')) {
            return Err(Refusal::Forbidden("the token names another service address"));
        }
        Ok(())
    }
}

/// Checks Bot Connector tokens for one bot.
pub(crate) struct ConnectorAuth {
    client: reqwest::Client,
    metadata_url: String,
    issuer: String,
    app_id: String,
    unknown_key_refresh: Duration,
    keys: RwLock<Keys>,
    fetching: Mutex<()>,
}

impl fmt::Debug for ConnectorAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorAuth")
            .field("metadata_url", &self.metadata_url)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl ConnectorAuth {
    /// The checks for `app_id`'s tokens, issued by `issuer`, with keys from `metadata_url`.
    pub(crate) fn new(
        client: reqwest::Client,
        metadata_url: &str,
        issuer: &str,
        app_id: &str,
    ) -> Result<Self, String> {
        let metadata_url =
            checked_base(metadata_url, "the Bot Connector OpenID metadata address", false)?;
        Ok(Self {
            client,
            metadata_url,
            issuer: issuer.to_string(),
            app_id: app_id.to_string(),
            unknown_key_refresh: UNKNOWN_KEY_REFRESH,
            keys: RwLock::new(Keys::default()),
            fetching: Mutex::new(()),
        })
    }

    /// The least time between two fetches an unknown key id causes, shortened for tests.
    #[cfg(test)]
    fn with_unknown_key_refresh(mut self, interval: Duration) -> Self {
        self.unknown_key_refresh = interval;
        self
    }

    /// Checks the `Authorization` header `authorization` of an activity.
    pub(crate) async fn verify(&self, authorization: Option<&str>) -> Result<Verified, Refusal> {
        let token = authorization
            .and_then(|value| {
                let (scheme, token) = value.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
            })
            .filter(|token| !token.is_empty())
            .ok_or(Refusal::Unauthenticated("no bearer token"))?;
        let header = jsonwebtoken::decode_header(token)
            .map_err(|_| Refusal::Unauthenticated("not a JWT"))?;
        let kid = header.kid.ok_or(Refusal::Unauthenticated("the token names no key"))?;
        let (key, algorithms) = self.key(&kid).await?;
        if !algorithms.contains(&header.alg) {
            return Err(Refusal::Unauthenticated("the token's algorithm is not the connector's"));
        }
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.app_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        validation.validate_nbf = true;
        validation.leeway = CLOCK_SKEW_SECS;
        let data =
            jsonwebtoken::decode::<Value>(token, &key.decoding, &validation).map_err(|error| {
                tracing::debug!(error = %error, "a bot connector token was refused");
                Refusal::Unauthenticated("the token failed validation")
            })?;
        let service_url = ["serviceurl", "serviceUrl"]
            .iter()
            .find_map(|claim| data.claims[*claim].as_str())
            .map(str::to_string);
        Ok(Verified { service_url, endorsements: key.endorsements.clone() })
    }

    /// The key `kid` and the metadata's algorithms, fetching the keys when they are older
    /// than a day, or when `kid` is unknown and they were not fetched in the last minutes.
    async fn key(&self, kid: &str) -> Result<(Arc<Key>, Vec<Algorithm>), Refusal> {
        {
            let keys = self.keys.read().await;
            let fresh = keys.fetched_at.is_some_and(|at| at.elapsed() < KEYS_MAX_AGE);
            if fresh && let Some(key) = keys.by_id.get(kid) {
                return Ok((key.clone(), keys.algorithms.clone()));
            }
        }
        let _fetching = self.fetching.lock().await;
        {
            let keys = self.keys.read().await;
            let recent = keys.fetched_at.is_some_and(|at| at.elapsed() < self.unknown_key_refresh);
            let fresh = keys.fetched_at.is_some_and(|at| at.elapsed() < KEYS_MAX_AGE);
            match keys.by_id.get(kid) {
                Some(key) if fresh => return Ok((key.clone(), keys.algorithms.clone())),
                None if recent => return Err(Refusal::Unauthenticated("unknown signing key")),
                _ => {}
            }
        }
        let fetched = self.fetch().await.map_err(|reason| {
            tracing::warn!(error = %reason, "the bot connector's signing keys could not be fetched");
            Refusal::Unavailable
        })?;
        let mut keys = self.keys.write().await;
        *keys = fetched;
        let key =
            keys.by_id.get(kid).cloned().ok_or(Refusal::Unauthenticated("unknown signing key"))?;
        Ok((key, keys.algorithms.clone()))
    }

    async fn fetch(&self) -> Result<Keys, String> {
        let metadata: Metadata = self.get_json(&self.metadata_url).await?;
        let jwks_uri = checked_base(&metadata.jwks_uri, "the metadata's jwks_uri", false)?;
        let algorithms: Vec<Algorithm> = metadata
            .id_token_signing_alg_values_supported
            .iter()
            .filter_map(|name| match name.as_str() {
                "RS256" => Some(Algorithm::RS256),
                "RS384" => Some(Algorithm::RS384),
                "RS512" => Some(Algorithm::RS512),
                // Only RSA keys are read, so only RSA algorithms are taken.
                _ => None,
            })
            .collect();
        let algorithms = if algorithms.is_empty() { vec![Algorithm::RS256] } else { algorithms };
        let jwks: Jwks = self.get_json(&jwks_uri).await?;
        let mut by_id = HashMap::new();
        for jwk in jwks.keys.into_iter().take(MAX_KEYS) {
            let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e) else { continue };
            if jwk.kty != "RSA" {
                continue;
            }
            let Ok(decoding) = DecodingKey::from_rsa_components(&n, &e) else { continue };
            by_id.insert(kid, Arc::new(Key { decoding, endorsements: jwk.endorsements }));
        }
        tracing::debug!(keys.count = by_id.len(), "bot connector signing keys fetched");
        Ok(Keys { by_id, algorithms, fetched_at: Some(Instant::now()) })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, String> {
        let response = self
            .client
            .get(url)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?
            .error_for_status()
            .map_err(|error| error.without_url().to_string())?;
        response.json().await.map_err(|error| error.without_url().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified(service_url: Option<&str>, endorsements: &[&str]) -> Verified {
        Verified {
            service_url: service_url.map(str::to_string),
            endorsements: endorsements.iter().map(|e| (*e).to_string()).collect(),
        }
    }

    #[test]
    fn a_token_vouches_only_for_its_channel_and_service_address() {
        let token =
            verified(Some("https://smba.trafficmanager.net/amer/"), &["msteams", "webchat"]);
        assert_eq!(token.vouches_for("msteams", "https://smba.trafficmanager.net/amer/"), Ok(()));
        assert_eq!(token.vouches_for("msteams", "https://smba.trafficmanager.net/amer"), Ok(()));
        assert!(matches!(
            token.vouches_for("slack", "https://smba.trafficmanager.net/amer/"),
            Err(Refusal::Forbidden(_))
        ));
        assert!(matches!(
            token.vouches_for("msteams", "https://attacker.example/"),
            Err(Refusal::Forbidden(_))
        ));
        let unnamed = verified(None, &["msteams"]);
        assert!(unnamed.vouches_for("msteams", "https://smba.trafficmanager.net/amer/").is_err());
    }

    /// An RSA key with id `kid`, as a signer and the JWK its metadata publishes.
    fn key(kid: &str) -> (jsonwebtoken::EncodingKey, Value) {
        use aws_lc_rs::encoding::AsDer;
        use aws_lc_rs::rsa::{KeyPair, KeySize};
        use aws_lc_rs::signature::KeyPair as _;
        use base64::Engine;
        use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

        let pair = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let der = STANDARD.encode(pair.as_der().unwrap().as_ref());
        let lines: Vec<String> = der
            .as_bytes()
            .chunks(64)
            .map(|line| String::from_utf8_lossy(line).to_string())
            .collect();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            lines.join("\n")
        );
        let public = pair.public_key();
        let jwk = serde_json::json!({
            "kty": "RSA", "use": "sig", "kid": kid, "endorsements": ["msteams"],
            "n": URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
            "e": URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
        });
        (jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(), jwk)
    }

    fn token(kid: &str, key: &jsonwebtoken::EncodingKey) -> String {
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let claims = serde_json::json!({ "iss": ISSUER, "aud": "app-id", "exp": 4_102_444_800u64, "serviceurl": "https://smba.trafficmanager.net/teams/" });
        jsonwebtoken::encode(&header, &claims, key).unwrap()
    }

    /// The Bot Connector rotates its keys: a token naming a key the cache does not hold
    /// fetches them again, and the new key is then cached.
    #[tokio::test]
    async fn a_rotated_key_is_fetched_when_a_token_names_it() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let (old, old_jwk) = key("old");
        let (new, new_jwk) = key("new");
        Mock::given(method("GET"))
            .and(path("/openid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
                "id_token_signing_alg_values_supported": ["RS256"],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "keys": [old_jwk.clone()] })),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "keys": [old_jwk, new_jwk] })),
            )
            .with_priority(2)
            .mount(&server)
            .await;
        let auth = ConnectorAuth::new(
            reqwest::Client::new(),
            &format!("{}/openid", server.uri()),
            ISSUER,
            "app-id",
        )
        .unwrap()
        .with_unknown_key_refresh(Duration::ZERO);
        let fetches = || async {
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path() == "/keys")
                .count()
        };
        let bearer = |token: String| format!("Bearer {token}");
        let verified = auth.verify(Some(&bearer(token("old", &old)))).await.unwrap();
        assert_eq!(verified.endorsements, ["msteams"]);
        assert_eq!(fetches().await, 1);
        assert!(auth.verify(Some(&bearer(token("new", &new)))).await.is_ok(), "the rotated key");
        assert_eq!(fetches().await, 2);
        assert!(auth.verify(Some(&bearer(token("new", &new)))).await.is_ok());
        assert_eq!(fetches().await, 2, "cached");
        // A token naming the new key but signed by the old one is refused.
        assert!(matches!(
            auth.verify(Some(&bearer(token("new", &old)))).await,
            Err(Refusal::Unauthenticated(_))
        ));
    }

    #[tokio::test]
    async fn a_missing_or_malformed_header_is_refused_before_any_fetch() {
        let auth = ConnectorAuth::new(
            reqwest::Client::new(),
            "http://127.0.0.1:9/openid",
            ISSUER,
            "app-id",
        )
        .unwrap();
        for header in [None, Some(""), Some("Basic abc"), Some("Bearer "), Some("Bearer x.y")] {
            assert!(
                matches!(auth.verify(header).await, Err(Refusal::Unauthenticated(_))),
                "{header:?}"
            );
        }
        assert!(
            ConnectorAuth::new(reqwest::Client::new(), "http://login.example/x", ISSUER, "a")
                .is_err(),
            "plain http beyond loopback"
        );
    }
}
