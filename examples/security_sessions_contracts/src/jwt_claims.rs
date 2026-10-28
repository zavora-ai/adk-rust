//! JWT validators that require the `iss` and `aud` claims.
//!
//! A local issuer on 127.0.0.1 generates an ES256 key, serves its JWKS, and signs tokens that
//! carry or omit `iss` and `aud`. `jwt_validator_requires_iss_and_aud` presents them to
//! `JwtValidator`; `live_jwt_claims_server_run` serves a `claude-sonnet-5-5` `LlmAgent` through
//! `adk-server` behind `JwtRequestContextExtractor` and sends one `POST /api/run` per token.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use adk_agent::LlmAgentBuilder;
use adk_auth::auth_bridge::JwtRequestContextExtractor;
use adk_auth::sso::{JwtValidator, JwtValidatorBuilder, TokenClaims, TokenError, TokenValidator};
use adk_core::{Agent, Event, SingleAgentLoader};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{GetRequest, InMemorySessionService, SessionService};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use axum::Router;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;

use crate::{Verdict, verdict};

const KEY_ID: &str = "jwt-claims-key";
const AUDIENCE: &str = "jwt-claims-agent";
const FOREIGN_AUDIENCE: &str = "another-service";
const LIVE_MODEL: &str = "claude-sonnet-5-5";
const AGENT: &str = "claims_assistant";

/// A local issuer whose tokens can omit `iss` and `aud`.
struct ClaimsIssuer {
    url: String,
    key: EncodingKey,
    task: tokio::task::JoinHandle<()>,
}

impl ClaimsIssuer {
    async fn start() -> anyhow::Result<Self> {
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

    fn validator(&self) -> JwtValidatorBuilder {
        JwtValidator::builder()
            .issuer(&self.url)
            .jwks_uri(format!("{}/jwks", self.url))
            .algorithm(Algorithm::ES256)
    }

    /// A token for `alice`, valid for ten minutes, with `iss` and `aud` only when given.
    fn token(&self, iss: Option<&str>, aud: Option<&str>) -> anyhow::Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut claims = json!({ "sub": "alice", "iat": now, "exp": now + 600 });
        if let Some(iss) = iss {
            claims["iss"] = json!(iss);
        }
        if let Some(aud) = aud {
            claims["aud"] = json!(aud);
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(KEY_ID.to_string());
        Ok(jsonwebtoken::encode(&header, &claims, &self.key)?)
    }

    /// `(label, token, accepted by a validator pinned to AUDIENCE)` for each case.
    fn cases(&self) -> anyhow::Result<Vec<(&'static str, String, bool)>> {
        Ok(vec![
            ("valid", self.token(Some(&self.url), Some(AUDIENCE))?, true),
            ("no-aud", self.token(Some(&self.url), None)?, false),
            ("no-iss", self.token(None, Some(AUDIENCE))?, false),
            ("foreign-aud", self.token(Some(&self.url), Some(FOREIGN_AUDIENCE))?, false),
        ])
    }
}

impl Drop for ClaimsIssuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn outcome(result: &Result<TokenClaims, TokenError>) -> String {
    match result {
        Ok(claims) => format!("accepted (sub {})", claims.sub),
        Err(error) => format!("refused ({error})"),
    }
}

/// The opt-out for an issuer whose tokens carry no `aud` claim.
fn without_audience(builder: JwtValidatorBuilder) -> JwtValidatorBuilder {
    builder.without_audience()
}

pub async fn jwt_validator_requires_iss_and_aud() -> anyhow::Result<Verdict> {
    let issuer = ClaimsIssuer::start().await?;
    let mut checks = Vec::new();

    let pinned = issuer.validator().audience(AUDIENCE).build()?;
    for (label, token, accept) in issuer.cases()? {
        let result = pinned.validate(&token).await;
        let expected = if accept { "accepted" } else { "refused" };
        checks.push((
            result.is_ok() == accept,
            format!("pinned audience: {label} token {} (expected {expected})", outcome(&result)),
        ));
    }

    let no_aud = issuer.token(Some(&issuer.url), None)?;
    match issuer.validator().build() {
        Err(error) => {
            checks.push((true, format!("no audience configured: build() refused ({error})")))
        }
        Ok(validator) => {
            let result = validator.validate(&no_aud).await;
            checks.push((
                false,
                format!(
                    "no audience configured: build() succeeded and a no-aud token is {}",
                    outcome(&result)
                ),
            ));
        }
    }

    let unpinned = without_audience(issuer.validator()).build()?;
    let opted_out = [
        ("no-aud", no_aud, true),
        ("valid", issuer.token(Some(&issuer.url), Some(AUDIENCE))?, false),
        ("no-iss, no-aud", issuer.token(None, None)?, false),
    ];
    for (label, token, accept) in opted_out {
        let result = unpinned.validate(&token).await;
        let expected = if accept { "accepted" } else { "refused" };
        checks.push((
            result.is_ok() == accept,
            format!("without_audience: {label} token {} (expected {expected})", outcome(&result)),
        ));
    }
    Ok(verdict(checks))
}

/// The text the agent answered with, from a `/api/run` response body.
fn reply_text(body: &str) -> Option<String> {
    let events: Vec<Event> = serde_json::from_str(body).ok()?;
    let text = events
        .iter()
        .filter(|event| event.author == AGENT)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
        .collect::<String>();
    (!text.trim().is_empty()).then_some(text)
}

pub async fn live_jwt_claims_server_run() -> anyhow::Result<Verdict> {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    let issuer = ClaimsIssuer::start().await?;
    let extractor = JwtRequestContextExtractor::builder()
        .validator(issuer.validator().audience(AUDIENCE).build()?)
        .build()
        .map_err(anyhow::Error::msg)?;
    let model = AnthropicClient::new(AnthropicConfig::new(key, LIVE_MODEL).with_max_tokens(64))?;
    let agent = LlmAgentBuilder::new(AGENT)
        .instruction("Answer in one short sentence.")
        .model(Arc::new(model))
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(agent) as Arc<dyn Agent>));
    let config =
        ServerConfig::new(loader, sessions.clone()).with_request_context(Arc::new(extractor));
    let app = ServerBuilder::new(config).build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = reqwest::Client::new();
    let mut checks = Vec::new();
    let mut cases: Vec<(&str, Option<String>, bool)> = issuer
        .cases()?
        .into_iter()
        .map(|(label, token, accept)| (label, Some(token), accept))
        .collect();
    cases.push(("no-bearer", None, false));
    for (label, token, accept) in cases {
        let session_id = format!("claims-{label}");
        // The body names `mallory`; an authenticated run belongs to the token's subject.
        let body = json!({
            "appName": AGENT,
            "userId": "mallory",
            "sessionId": session_id,
            "newMessage": { "role": "user", "parts": [{ "text": "Say hello in five words." }] }
        });
        let mut request = client.post(format!("{base}/api/run")).json(&body);
        if let Some(token) = &token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let reply = reply_text(&text);
        let alice_session = sessions
            .get(GetRequest {
                app_name: AGENT.to_string(),
                user_id: "alice".to_string(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .is_ok();
        let observed = match &reply {
            Some(reply) => format!("{status}, {LIVE_MODEL} answered {:?}", reply.trim()),
            None => format!("{status} {}", text.chars().take(120).collect::<String>()),
        };
        let holds = if accept {
            status == 200 && reply.is_some() && alice_session
        } else {
            status == 401 && reply.is_none() && !alice_session
        };
        let expected = if accept { "200, a reply, run as alice" } else { "401, no run" };
        checks.push((
            holds,
            format!(
                "{label}: {observed}; session under alice: {alice_session} (expected {expected})"
            ),
        ));
    }
    server.abort();
    Ok(verdict(checks))
}
