//! Agent identity federation: the issuer's documents and tokens, the AWS, Google Cloud and
//! Microsoft Entra exchanges against mock servers, the credential brokers, and a live run
//! against the three clouds (`live_cloud_federation`, ignored by default).

#![cfg(feature = "agent-federation")]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use adk_auth::federation::{
    AgentCoreIdentity, AgentToken, AgentTokenIssuer, AgentTokenSigner, AgentTokenSource,
    AwsSessionCredentials, AwsWebIdentity, CloudProvider, EntraAgentIdentity, FederationError,
    GoogleWorkloadIdentity, InboundAgentVerifier, RsaTokenSigner,
};
use adk_core::{
    ActingContext, AdkError, AgentId, AgentPrincipal, CredentialBroker, CredentialSecret,
    ErrorCategory, RevocationScope, Url, async_trait, resource_indicator,
};
use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUER: &str = "https://agents.example.com";
const KEY_ID: &str = "agents-test-1";
const CASHIER: &str = "did:web:shop.example.com:agents:cashier";
const TENANT: &str = "8f2c6a1e-0000-4000-8000-000000000001";
const BLUEPRINT: &str = "8f2c6a1e-0000-4000-8000-000000000002";
const AGENT_IDENTITY: &str = "8f2c6a1e-0000-4000-8000-000000000003";

/// The test issuer's PKCS#8 key in PEM form, generated once per test binary.
fn key_pem() -> &'static str {
    static PEM: OnceLock<String> = OnceLock::new();
    PEM.get_or_init(|| {
        let pair = KeyPair::generate(KeySize::Rsa2048).expect("RSA key generation");
        let encoded = STANDARD.encode(pair.as_der().expect("PKCS#8 encoding").as_ref());
        let lines: Vec<_> = encoded.as_bytes().chunks(64).map(String::from_utf8_lossy).collect();
        format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n"))
    })
}

fn issuer() -> Arc<AgentTokenIssuer> {
    static ISSUER_ONCE: OnceLock<Arc<AgentTokenIssuer>> = OnceLock::new();
    ISSUER_ONCE
        .get_or_init(|| {
            Arc::new(AgentTokenIssuer::from_rsa_pem(ISSUER, KEY_ID, key_pem()).unwrap())
        })
        .clone()
}

fn cashier() -> AgentPrincipal {
    AgentPrincipal::new(AgentId::new(CASHIER).unwrap()).with_tenant("shop")
}

/// The claims of `token`, after checking its signature against the issuer's JWK set and its
/// issuer, audience and lifetime.
fn verified_claims(token: &str, audience: &str) -> Value {
    let jwks = issuer().jwks();
    let key = DecodingKey::from_rsa_components(&jwks.keys[0].n, &jwks.keys[0].e).unwrap();
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[audience]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub", "nbf"]);
    validation.validate_nbf = true;
    jsonwebtoken::decode::<Value>(token, &key, &validation).unwrap().claims
}

/// The fields of a form-encoded request body.
fn form(request: &wiremock::Request) -> HashMap<String, String> {
    let body = String::from_utf8_lossy(&request.body);
    Url::parse(&format!("http://form.invalid/?{body}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect()
}

// ---------------------------------------------------------------------------------------
// Issuer
// ---------------------------------------------------------------------------------------

#[test]
fn the_discovery_document_and_jwk_set_have_the_published_shape() {
    let issuer = AgentTokenIssuer::from_rsa_pem(format!("{ISSUER}/"), KEY_ID, key_pem()).unwrap();

    assert_eq!(
        serde_json::to_value(issuer.discovery_document()).unwrap(),
        json!({
            "issuer": ISSUER,
            "jwks_uri": "https://agents.example.com/jwks.json",
            "response_types_supported": ["id_token"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "claims_supported": ["iss", "sub", "aud", "exp", "iat", "jti"],
        })
    );

    let jwks = serde_json::to_value(issuer.jwks()).unwrap();
    let key = &jwks["keys"][0];
    assert_eq!(jwks["keys"].as_array().unwrap().len(), 1);
    assert_eq!(
        json!({ "kty": key["kty"], "use": key["use"], "alg": key["alg"], "kid": key["kid"] }),
        json!({ "kty": "RSA", "use": "sig", "alg": "RS256", "kid": KEY_ID })
    );
    assert_eq!(key.as_object().unwrap().len(), 6, "a public JWK holds no private member");
    assert_eq!(URL_SAFE_NO_PAD.decode(key["n"].as_str().unwrap()).unwrap().len(), 256);
    assert_eq!(key["e"], "AQAB");
}

#[tokio::test]
async fn an_issued_token_verifies_against_the_issuers_jwk_set() {
    let token = issuer().issue(&cashier(), "sts.amazonaws.com").await.unwrap();

    let header = jsonwebtoken::decode_header(token.secret.expose()).unwrap();
    assert_eq!(header.alg, Algorithm::RS256);
    assert_eq!(header.kid.as_deref(), Some(KEY_ID));
    assert_eq!(header.typ.as_deref(), Some("JWT"));

    let claims = verified_claims(token.secret.expose(), "sts.amazonaws.com");
    let issued_at = claims["iat"].as_i64().unwrap();
    assert!((chrono::Utc::now().timestamp() - issued_at).abs() <= 5);
    let jti = claims["jti"].as_str().unwrap().to_string();
    assert_eq!(
        claims,
        json!({
            "iss": ISSUER,
            "sub": CASHIER,
            "aud": "sts.amazonaws.com",
            "iat": issued_at,
            "nbf": issued_at - 5,
            "exp": issued_at + 600,
            "jti": jti,
        })
    );
    let groups: Vec<usize> = jti.split('-').map(str::len).collect();
    assert_eq!(groups, [8, 4, 4, 4, 12], "the jti is a UUID");
    assert_eq!(&jti[14..15], "4");

    assert_eq!(token.subject.as_str(), CASHIER);
    assert_eq!(token.audience, "sts.amazonaws.com");
    assert_eq!(token.expires_at.timestamp(), issued_at + 600);

    let again = issuer().issue(&cashier(), "sts.amazonaws.com").await.unwrap();
    assert_ne!(
        verified_claims(again.secret.expose(), "sts.amazonaws.com")["jti"],
        claims["jti"],
        "every token has its own jti"
    );
}

#[tokio::test]
async fn a_token_from_another_key_does_not_verify() {
    let other =
        AgentTokenIssuer::new(ISSUER, Arc::new(RsaTokenSigner::generate(KEY_ID).unwrap())).unwrap();
    let forged = other.issue(&cashier(), "sts.amazonaws.com").await.unwrap();

    let jwks = issuer().jwks();
    let key = DecodingKey::from_rsa_components(&jwks.keys[0].n, &jwks.keys[0].e).unwrap();
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&["sts.amazonaws.com"]);
    let refused = jsonwebtoken::decode::<Value>(forged.secret.expose(), &key, &validation);
    assert!(refused.is_err());
}

#[tokio::test]
async fn the_token_lifetime_is_capped() {
    let lifetime = |issuer: AgentTokenIssuer| async move {
        let token = issuer.issue(&cashier(), "audience").await.unwrap();
        let claims = verified_claims(token.secret.expose(), "audience");
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap()
    };
    let issuer = || AgentTokenIssuer::from_rsa_pem(ISSUER, KEY_ID, key_pem()).unwrap();

    assert_eq!(issuer().token_ttl(), AgentTokenIssuer::DEFAULT_TOKEN_TTL);
    assert_eq!(lifetime(issuer().with_token_ttl(Duration::from_secs(120))).await, 120);
    assert_eq!(lifetime(issuer().with_token_ttl(Duration::from_secs(86_400))).await, 3600);
    assert_eq!(lifetime(issuer().with_token_ttl(Duration::ZERO)).await, 1);
}

#[tokio::test]
async fn an_empty_audience_is_refused() {
    let refused = issuer().issue(&cashier(), "").await;
    assert!(matches!(&refused, Err(FederationError::InvalidConfig { .. })), "{refused:?}");
}

#[test]
fn the_issuer_url_must_be_https() {
    let signer = || -> Arc<dyn AgentTokenSigner> {
        Arc::new(RsaTokenSigner::from_pkcs8_pem(KEY_ID, key_pem()).unwrap())
    };
    for url in [
        "http://agents.example.com",
        "agents.example.com",
        "https://agents.example.com/?x=1",
        "https://agents.example.com/#keys",
        "https://user@agents.example.com",
        "",
    ] {
        let refused = AgentTokenIssuer::new(url, signer());
        assert!(
            matches!(&refused, Err(FederationError::InvalidConfig { .. })),
            "{url}: {refused:?}"
        );
    }
    let error: AdkError =
        AgentTokenIssuer::new("http://agents.example.com", signer()).unwrap_err().into();
    assert_eq!(error.code, "identity.federation.invalid_config");
    assert_eq!(error.category, ErrorCategory::InvalidInput);

    for url in ["http://localhost:8080", "http://127.0.0.1:8080/issuer", "http://[::1]:8080"] {
        assert!(AgentTokenIssuer::new(url, signer()).is_ok(), "{url}");
    }
    let nested =
        AgentTokenIssuer::new("https://storage.example.com/bucket/issuer/", signer()).unwrap();
    assert_eq!(nested.issuer(), "https://storage.example.com/bucket/issuer");
    assert_eq!(
        nested.discovery_document().jwks_uri,
        "https://storage.example.com/bucket/issuer/jwks.json"
    );
}

#[test]
fn keys_are_read_from_pkcs8_and_never_printed() {
    let pem = key_pem();
    let body: String = pem.lines().filter(|line| !line.starts_with("-----")).collect();
    let der = STANDARD.decode(&body).unwrap();

    let from_pem = RsaTokenSigner::from_pkcs8_pem(KEY_ID, pem).unwrap();
    let from_der = RsaTokenSigner::from_pkcs8_der(KEY_ID, &der).unwrap();
    assert_eq!(from_pem.public_jwk(), from_der.public_jwk());

    let signer_debug = format!("{from_pem:?}");
    assert_eq!(signer_debug, format!("RsaTokenSigner {{ kid: {KEY_ID:?}, .. }}"));
    let issuer_debug =
        format!("{:?}", AgentTokenIssuer::from_rsa_der(ISSUER, KEY_ID, &der).unwrap());
    assert!(issuer_debug.contains(KEY_ID) && issuer_debug.contains(ISSUER), "{issuer_debug}");
    for printed in [&signer_debug, &issuer_debug] {
        assert!(!printed.contains(&body[..24]) && !printed.contains("PRIVATE"), "{printed}");
    }

    let rejected = |pem: &str| match RsaTokenSigner::from_pkcs8_pem(KEY_ID, pem) {
        Err(FederationError::InvalidKey { reason }) => reason,
        other => panic!("expected an invalid key, got {other:?}"),
    };
    assert!(
        rejected("-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----")
            .contains("PKCS#1")
    );
    assert!(
        rejected(
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----"
        )
        .contains("encrypted")
    );
    assert!(rejected("not a key").contains("no `PRIVATE KEY` block"));
    assert!(
        rejected("-----BEGIN PRIVATE KEY-----\n!!!!\n-----END PRIVATE KEY-----").contains("base64")
    );
    let truncated = rejected("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----");
    assert!(!truncated.contains("AAAA"), "{truncated}");
    assert!(matches!(
        RsaTokenSigner::from_pkcs8_pem("", pem),
        Err(FederationError::InvalidKey { .. })
    ));
    let error: AdkError = RsaTokenSigner::from_pkcs8_pem(KEY_ID, "not a key").unwrap_err().into();
    assert_eq!(error.code, "identity.federation.invalid_key");
}

// ---------------------------------------------------------------------------------------
// AWS
// ---------------------------------------------------------------------------------------

const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/cashier";

fn sts_success() -> Value {
    json!({
        "AssumeRoleWithWebIdentityResponse": {
            "AssumeRoleWithWebIdentityResult": {
                "AssumedRoleUser": {
                    "Arn": "arn:aws:sts::111122223333:assumed-role/cashier/session",
                    "AssumedRoleId": "AROAEXAMPLE:session"
                },
                "Audience": "sts.amazonaws.com",
                "Credentials": {
                    "AccessKeyId": "ASIAEXAMPLE",
                    "Expiration": 1.8e9,
                    "SecretAccessKey": "secret-access-key-value",
                    "SessionToken": "session-token-value"
                },
                "PackedPolicySize": null,
                "Provider": "arn:aws:iam::111122223333:oidc-provider/agents.example.com",
                "SourceIdentity": null,
                "SubjectFromWebIdentityToken": CASHIER
            },
            "ResponseMetadata": { "RequestId": "request-1" }
        }
    })
}

fn sts(server: &MockServer) -> AwsWebIdentity {
    AwsWebIdentity::new("us-east-1", ROLE_ARN, issuer()).with_endpoint(format!("{}/", server.uri()))
}

#[tokio::test]
async fn aws_assume_role_sends_the_agents_token_and_reads_the_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sts_success()))
        .expect(1)
        .mount(&server)
        .await;

    let credentials = sts(&server).assume_role(&cashier()).await.unwrap();

    assert_eq!(
        credentials,
        AwsSessionCredentials {
            access_key_id: "ASIAEXAMPLE".into(),
            secret_access_key: CredentialSecret::new("secret-access-key-value"),
            session_token: CredentialSecret::new("session-token-value"),
            expires_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            assumed_role_arn: "arn:aws:sts::111122223333:assumed-role/cashier/session".into(),
            subject_from_web_identity_token: CASHIER.into(),
        }
    );
    let printed = format!("{credentials:?}");
    assert!(
        !printed.contains("secret-access-key-value") && !printed.contains("session-token-value")
    );

    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].headers.get("authorization").is_none(), "the request is unsigned");
    let mut sent = form(&requests[0]);
    let token = sent.remove("WebIdentityToken").unwrap();
    assert_eq!(verified_claims(&token, "sts.amazonaws.com")["sub"], CASHIER);
    assert_eq!(
        sent,
        fields(&[
            ("Action", "AssumeRoleWithWebIdentity"),
            ("Version", "2011-06-15"),
            ("RoleArn", ROLE_ARN),
            ("RoleSessionName", "did-web-shop.example.com-agents-cashier"),
            ("DurationSeconds", "3600"),
        ])
    );
}

#[tokio::test]
async fn aws_session_name_and_duration_are_configurable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sts_success()))
        .mount(&server)
        .await;

    sts(&server)
        .with_session_name("cashier-shift-1")
        .with_duration(Duration::from_secs(900))
        .assume_role(&cashier())
        .await
        .unwrap();
    let long_id =
        AgentPrincipal::new(AgentId::new(format!("urn:agent:{}", "a/".repeat(60))).unwrap());
    sts(&server).assume_role(&long_id).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let first = form(&requests[0]);
    assert_eq!(first["RoleSessionName"], "cashier-shift-1");
    assert_eq!(first["DurationSeconds"], "900");
    let derived = &form(&requests[1])["RoleSessionName"];
    assert_eq!(derived.len(), 64);
    assert!(derived.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'), "{derived}");
}

#[tokio::test]
async fn an_aws_refusal_carries_the_sts_error_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "Error": {
                "Code": "AccessDenied",
                "Message": "Not authorized to perform sts:AssumeRoleWithWebIdentity",
                "Type": "Sender"
            },
            "RequestId": "request-2"
        })))
        .mount(&server)
        .await;

    let refused = sts(&server).assume_role(&cashier()).await.unwrap_err();
    assert!(
        matches!(
            &refused,
            FederationError::Refused { provider: CloudProvider::Aws, status: 403, code, message }
                if code == "AccessDenied" && message.contains("sts:AssumeRoleWithWebIdentity")
        ),
        "{refused:?}"
    );
    let error: AdkError = refused.into();
    assert_eq!(error.code, "identity.federation.exchange_refused");
    assert_eq!(error.category, ErrorCategory::Forbidden);
    assert_eq!(error.details.upstream_status_code, Some(403));
}

#[tokio::test]
async fn an_unreadable_aws_answer_and_an_unreachable_endpoint_are_told_apart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<AssumeRoleWithWebIdentityResponse/>"),
        )
        .mount(&server)
        .await;
    let unreadable = sts(&server).assume_role(&cashier()).await.unwrap_err();
    assert!(matches!(&unreadable, FederationError::InvalidResponse { .. }), "{unreadable:?}");
    assert_eq!(AdkError::from(unreadable).code, "identity.federation.invalid_response");

    let closed =
        AwsWebIdentity::new("us-east-1", ROLE_ARN, issuer()).with_endpoint("http://127.0.0.1:1/");
    let unreachable = closed.assume_role(&cashier()).await.unwrap_err();
    assert!(matches!(&unreachable, FederationError::Transport { .. }), "{unreachable:?}");
    let error: AdkError = unreachable.into();
    assert_eq!(error.code, "identity.federation.exchange_failed");
    assert!(error.is_retryable());
}

fn session_credentials() -> AwsSessionCredentials {
    AwsSessionCredentials {
        access_key_id: "ASIAEXAMPLE".into(),
        secret_access_key: CredentialSecret::new("secret-access-key-value"),
        session_token: CredentialSecret::new("session-token-value"),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        assumed_role_arn: "arn:aws:sts::111122223333:assumed-role/cashier/session".into(),
        subject_from_web_identity_token: CASHIER.into(),
    }
}

#[tokio::test]
async fn agentcore_signs_the_request_with_the_session_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/identities/GetWorkloadAccessTokenForJWT"))
        .and(header("content-type", "application/json"))
        .and(header("x-amz-security-token", "session-token-value"))
        .and(header("user-agent", concat!("adk-auth/", env!("CARGO_PKG_VERSION"))))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "workloadAccessToken": "opaque-workload-token" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let token = AgentCoreIdentity::new("us-east-1")
        .with_endpoint(server.uri())
        .workload_access_token(
            &session_credentials(),
            "cashier",
            &CredentialSecret::new("agent-jwt"),
        )
        .await
        .unwrap();

    assert_eq!(token.workload_name, "cashier");
    assert_eq!(token.secret.expose(), "opaque-workload-token");
    assert!(!format!("{token:?}").contains("opaque-workload-token"));

    let requests = server.received_requests().await.unwrap();
    let request = &requests[0];
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({ "workloadName": "cashier", "userToken": "agent-jwt" })
    );
    let date = request.headers["x-amz-date"].to_str().unwrap();
    assert_eq!(date.len(), 16, "{date}");
    let authorization = request.headers["authorization"].to_str().unwrap();
    let expected = format!(
        "AWS4-HMAC-SHA256 Credential=ASIAEXAMPLE/{}/us-east-1/bedrock-agentcore/aws4_request, \
         SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, Signature=",
        &date[..8]
    );
    let signature =
        authorization.strip_prefix(&expected).unwrap_or_else(|| panic!("{authorization}"));
    assert!(signature.len() == 64 && signature.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!authorization.contains("secret-access-key-value"));
}

#[tokio::test]
async fn an_agentcore_refusal_carries_the_exception_name() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header(
                    "x-amzn-errortype",
                    "UnauthorizedException:http://internal.amazon.com/",
                )
                .set_body_json(json!({ "message": "Invalid Signature for bearerToken" })),
        )
        .mount(&server)
        .await;

    let refused = AgentCoreIdentity::new("us-east-1")
        .with_endpoint(server.uri())
        .workload_access_token(
            &session_credentials(),
            "cashier",
            &CredentialSecret::new("agent-jwt"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            &refused,
            FederationError::Refused { provider: CloudProvider::AgentCore, status: 401, code, message }
                if code == "UnauthorizedException" && message == "Invalid Signature for bearerToken"
        ),
        "{refused:?}"
    );
    assert_eq!(AdkError::from(refused).category, ErrorCategory::Unauthorized);
}

// ---------------------------------------------------------------------------------------
// Google Cloud
// ---------------------------------------------------------------------------------------

const GOOGLE_AUDIENCE: &str = "//iam.googleapis.com/projects/123456789012/locations/global/workloadIdentityPools/agents-pool/providers/agents-issuer";

fn google(server: &MockServer) -> GoogleWorkloadIdentity {
    GoogleWorkloadIdentity::new("123456789012", "agents-pool", "agents-issuer", issuer())
        .with_endpoint(format!("{}/v1/token", server.uri()))
}

async fn mount_google_success(server: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.federated",
            "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
            "token_type": "Bearer",
            "expires_in": 3599
        })))
        .expect(times)
        .mount(server)
        .await;
}

#[tokio::test]
async fn google_exchange_sends_the_token_for_the_provider_audience() {
    let server = MockServer::start().await;
    mount_google_success(&server, 1).await;
    let client = google(&server);
    assert_eq!(client.audience(), GOOGLE_AUDIENCE);

    let token = client.access_token(&cashier()).await.unwrap();

    assert_eq!(token.secret.expose(), "ya29.federated");
    assert_eq!(token.scopes, ["https://www.googleapis.com/auth/cloud-platform"]);
    let lifetime = (token.expires_at - chrono::Utc::now()).num_seconds();
    assert!((3590..=3599).contains(&lifetime), "{lifetime}");
    assert!(!format!("{token:?}").contains("ya29"));

    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].headers.get("authorization").is_none(), "no client authentication");
    let mut sent = form(&requests[0]);
    let subject_token = sent.remove("subject_token").unwrap();
    let claims = verified_claims(&subject_token, &format!("https:{GOOGLE_AUDIENCE}"));
    assert_eq!(claims["sub"], CASHIER);
    assert_eq!(
        sent,
        fields(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
            ("audience", GOOGLE_AUDIENCE),
            ("scope", "https://www.googleapis.com/auth/cloud-platform"),
            ("requested_token_type", "urn:ietf:params:oauth:token-type:access_token"),
            ("subject_token_type", "urn:ietf:params:oauth:token-type:jwt"),
        ])
    );
}

#[tokio::test]
async fn a_google_refusal_carries_the_oauth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "The audience in ID Token does not match the expected audience."
        })))
        .mount(&server)
        .await;

    let refused = google(&server).access_token(&cashier()).await.unwrap_err();
    assert!(
        matches!(
            &refused,
            FederationError::Refused { provider: CloudProvider::Google, status: 400, code, message }
                if code == "invalid_grant" && message.contains("audience")
        ),
        "{refused:?}"
    );
    let error: AdkError = refused.into();
    assert_eq!(error.code, "identity.federation.exchange_refused");
    assert_eq!(error.category, ErrorCategory::Unauthorized);
}

#[tokio::test]
async fn a_google_api_style_error_and_a_token_without_expiry_are_mapped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("scope=fail"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "code": 503, "message": "The service is unavailable.", "status": "UNAVAILABLE" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "ya29.x" })))
        .mount(&server)
        .await;
    let client = google(&server);
    let acting = ActingContext::new(cashier());
    let storage = resource_indicator("https://storage.googleapis.com/").unwrap();

    let unavailable = client.token_for(&acting, &storage, &["fail".to_string()]).await.unwrap_err();
    assert_eq!(unavailable.code, "identity.federation.exchange_refused");
    assert_eq!(unavailable.category, ErrorCategory::Unavailable);
    assert!(unavailable.message.contains("UNAVAILABLE"), "{}", unavailable.message);

    let no_expiry = client.access_token(&cashier()).await.unwrap_err();
    assert!(matches!(&no_expiry, FederationError::InvalidResponse { .. }), "{no_expiry:?}");
}

#[tokio::test]
async fn the_google_broker_caches_per_agent_and_serves_google_apis_only() {
    let server = MockServer::start().await;
    mount_google_success(&server, 2).await;
    let broker = google(&server);
    let acting = ActingContext::new(cashier());
    let storage = resource_indicator("https://storage.googleapis.com/storage/v1").unwrap();

    let token = broker.token_for(&acting, &storage, &[]).await.unwrap();
    assert_eq!(token.agent.as_str(), CASHIER);
    assert_eq!(token.audience, storage);
    assert_eq!(token.scopes, ["https://www.googleapis.com/auth/cloud-platform"]);
    assert!(token.expires_at.is_some());
    assert_eq!(
        broker.token_for(&acting, &storage, &[]).await.unwrap(),
        token,
        "served from the cache"
    );
    assert_eq!(
        token
            .authorize_for(&Url::parse("https://storage.googleapis.com/storage/v1/b").unwrap())
            .unwrap()
            .expose(),
        "ya29.federated"
    );

    for foreign in [
        "https://api.example.com/",
        "https://googleapis.com.evil.example/",
        "http://storage.googleapis.com/",
    ] {
        let refused =
            broker.token_for(&acting, &Url::parse(foreign).unwrap(), &[]).await.unwrap_err();
        assert_eq!(refused.code, "identity.federation.request_unsupported", "{foreign}");
        assert_eq!(refused.category, ErrorCategory::Forbidden);
    }

    let unrelated = RevocationScope::Agent(AgentId::new("urn:adk:agent:other").unwrap());
    assert_eq!(broker.revoke(&unrelated).await.unwrap(), 0);
    assert_eq!(broker.revoke(&RevocationScope::Resource(storage.clone())).await.unwrap(), 1);
    broker.token_for(&acting, &storage, &[]).await.unwrap();
}

// ---------------------------------------------------------------------------------------
// Microsoft Entra
// ---------------------------------------------------------------------------------------

fn entra(server: &MockServer) -> EntraAgentIdentity {
    EntraAgentIdentity::new(TENANT, BLUEPRINT, AGENT_IDENTITY, issuer())
        .with_authority(server.uri())
}

async fn mount_entra_success(server: &MockServer, times: u64) {
    let endpoint = format!("/{TENANT}/oauth2/v2.0/token");
    Mock::given(method("POST"))
        .and(path(endpoint.clone()))
        .and(body_string_contains("fmi_path="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer",
            "expires_in": 3599,
            "access_token": "blueprint-exchange-token"
        })))
        .expect(times)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(endpoint))
        .and(body_string_contains("client_assertion=blueprint-exchange-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer",
            "expires_in": 3599,
            "access_token": "agent-identity-token"
        })))
        .expect(times)
        .mount(server)
        .await;
}

#[tokio::test]
async fn entra_exchanges_through_the_blueprint_then_the_agent_identity() {
    let server = MockServer::start().await;
    mount_entra_success(&server, 1).await;

    let token =
        entra(&server).access_token(&cashier(), "https://graph.microsoft.com").await.unwrap();

    assert_eq!(token.secret.expose(), "agent-identity-token");
    assert_eq!(token.scope, "https://graph.microsoft.com/.default");
    assert!(token.expires_at > chrono::Utc::now() + chrono::Duration::minutes(59));
    assert!(!format!("{token:?}").contains("agent-identity-token"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let mut first = form(&requests[0]);
    let assertion = first.remove("client_assertion").unwrap();
    assert_eq!(verified_claims(&assertion, "api://AzureADTokenExchange")["sub"], CASHIER);
    assert_eq!(
        first,
        fields(&[
            ("client_id", BLUEPRINT),
            ("scope", "api://AzureADTokenExchange/.default"),
            ("grant_type", "client_credentials"),
            ("fmi_path", AGENT_IDENTITY),
            ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
        ])
    );
    assert_eq!(
        form(&requests[1]),
        fields(&[
            ("client_id", AGENT_IDENTITY),
            ("scope", "https://graph.microsoft.com/.default"),
            ("grant_type", "client_credentials"),
            ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
            ("client_assertion", "blueprint-exchange-token"),
        ])
    );
}

#[tokio::test]
async fn an_entra_refusal_carries_the_aadsts_message_and_stops_the_exchange() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "AADSTS700213: No matching federated identity record found for presented assertion subject.",
            "error_codes": [700213]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let impostor =
        AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:impostor").unwrap());
    let refused =
        entra(&server).access_token(&impostor, "https://graph.microsoft.com").await.unwrap_err();
    assert!(
        matches!(
            &refused,
            FederationError::Refused { provider: CloudProvider::Entra, status: 401, code, message }
                if code == "invalid_client" && message.starts_with("AADSTS700213")
        ),
        "{refused:?}"
    );
    assert_eq!(AdkError::from(refused).category, ErrorCategory::Unauthorized);
}

#[tokio::test]
async fn the_entra_broker_requests_the_default_scope_of_the_resource() {
    let server = MockServer::start().await;
    mount_entra_success(&server, 1).await;
    let broker = entra(&server);
    let acting = ActingContext::new(cashier());
    let graph = resource_indicator("https://graph.microsoft.com").unwrap();

    let token = broker.token_for(&acting, &graph, &[]).await.unwrap();
    assert_eq!(token.agent.as_str(), CASHIER);
    assert_eq!(token.audience, graph);
    assert_eq!(token.scopes, ["https://graph.microsoft.com/.default"]);
    assert_eq!(
        broker.token_for(&acting, &graph, &[]).await.unwrap(),
        token,
        "served from the cache"
    );
    assert_eq!(
        token
            .authorize_for(&Url::parse("https://graph.microsoft.com/v1.0/me").unwrap())
            .unwrap()
            .expose(),
        "agent-identity-token"
    );

    let scoped = broker.token_for(&acting, &graph, &["User.Read".to_string()]).await.unwrap_err();
    assert_eq!(scoped.code, "identity.federation.request_unsupported");
    assert_eq!(broker.revoke(&RevocationScope::All).await.unwrap(), 1);
}

// ---------------------------------------------------------------------------------------
// Token sources
// ---------------------------------------------------------------------------------------

/// A source with no token to give.
#[derive(Debug)]
struct NoToken;

#[async_trait]
impl AgentTokenSource for NoToken {
    async fn agent_token(
        &self,
        _agent: &AgentPrincipal,
        _audience: &str,
    ) -> Result<AgentToken, FederationError> {
        Err(FederationError::Signing { reason: "the key service is unreachable".into() })
    }
}

#[tokio::test]
async fn a_source_without_a_token_sends_nothing() {
    let server = MockServer::start().await;
    let aws = AwsWebIdentity::new("us-east-1", ROLE_ARN, Arc::new(NoToken))
        .with_endpoint(format!("{}/", server.uri()));
    let google = GoogleWorkloadIdentity::new("1", "pool", "provider", Arc::new(NoToken))
        .with_endpoint(format!("{}/v1/token", server.uri()));
    let entra = EntraAgentIdentity::new(TENANT, BLUEPRINT, AGENT_IDENTITY, Arc::new(NoToken))
        .with_authority(server.uri());

    let failures = [
        aws.assume_role(&cashier()).await.map(drop),
        google.access_token(&cashier()).await.map(drop),
        entra.access_token(&cashier(), "https://graph.microsoft.com").await.map(drop),
    ];
    for failure in failures {
        assert!(matches!(&failure, Err(FederationError::Signing { .. })), "{failure:?}");
        assert_eq!(AdkError::from(failure.unwrap_err()).code, "identity.federation.signing_failed");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------
// Live
// ---------------------------------------------------------------------------------------

/// Issues every token for one fixed audience, whatever the exchange asks for.
#[derive(Debug)]
struct FixedAudience {
    issuer: Arc<AgentTokenIssuer>,
    audience: &'static str,
}

#[async_trait]
impl AgentTokenSource for FixedAudience {
    async fn agent_token(
        &self,
        agent: &AgentPrincipal,
        _audience: &str,
    ) -> Result<AgentToken, FederationError> {
        self.issuer.issue(agent, self.audience).await
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name} for the live run")).trim().to_string()
}

/// The code and status of a refusal; the message is not printed.
fn refusal<T: std::fmt::Debug>(outcome: Result<T, FederationError>) -> (u16, String, String) {
    match outcome {
        Err(FederationError::Refused { status, code, message, .. }) => (status, code, message),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The claim names of a JWT's payload, read without verification.
fn claim_names(jwt: &str) -> Vec<String> {
    let payload = jwt.split('.').nth(1).expect("a JWT has three parts");
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    claims.as_object().unwrap().keys().cloned().collect()
}

/// The exchanges and the inbound checks against the real clouds, through this crate's code.
///
/// The issuer's discovery document and JWK set must already be published at the issuer URL,
/// and each cloud told to trust that issuer and `ADK_FEDERATION_SUBJECT`. Prints outcomes and
/// non-secret facts only.
///
/// ```text
/// ADK_FEDERATION_ISSUER_URL=... ADK_FEDERATION_ISSUER_KID=... ADK_FEDERATION_ISSUER_KEY_FILE=... \
/// ADK_FEDERATION_SUBJECT=... ADK_FEDERATION_IMPOSTOR_SUBJECT=... \
/// ADK_FEDERATION_AWS_REGION=... ADK_FEDERATION_AWS_ROLE_ARN=... ADK_FEDERATION_AGENTCORE_WORKLOAD=... \
/// ADK_FEDERATION_GCP_PROJECT_NUMBER=... ADK_FEDERATION_GCP_POOL=... ADK_FEDERATION_GCP_PROVIDER=... \
/// ADK_FEDERATION_ENTRA_TENANT_ID=... ADK_FEDERATION_ENTRA_BLUEPRINT_APP_ID=... \
/// ADK_FEDERATION_ENTRA_AGENT_IDENTITY_ID=... \
/// cargo nextest run -p adk-auth --features agent-federation --run-ignored only \
///     -E 'test(live_cloud_federation)' --no-capture
/// ```
///
/// With `ADK_FEDERATION_GOOGLE_AGENT_TOKEN_FILE`, `ADK_FEDERATION_GOOGLE_AGENT_ISSUER` and
/// `ADK_FEDERATION_GOOGLE_AGENT_AUDIENCE` set, the identity token of an agent hosted on Google
/// Cloud is verified too.
#[tokio::test]
#[ignore = "reaches AWS, Google Cloud and Microsoft Entra"]
async fn live_cloud_federation() {
    let pem = std::fs::read_to_string(env("ADK_FEDERATION_ISSUER_KEY_FILE")).expect("issuer key");
    let issuer = Arc::new(
        AgentTokenIssuer::from_rsa_pem(
            env("ADK_FEDERATION_ISSUER_URL"),
            env("ADK_FEDERATION_ISSUER_KID"),
            &pem,
        )
        .unwrap(),
    );
    let subject = env("ADK_FEDERATION_SUBJECT");
    let cashier = AgentPrincipal::new(AgentId::new(subject.as_str()).unwrap());
    let impostor =
        AgentPrincipal::new(AgentId::new(env("ADK_FEDERATION_IMPOSTOR_SUBJECT")).unwrap());

    // The published key set is the one this key signs for.
    let published: Value = reqwest::get(issuer.discovery_document().jwks_uri)
        .await
        .expect("the issuer's JWK set is reachable")
        .json()
        .await
        .unwrap();
    assert_eq!(published, serde_json::to_value(issuer.jwks()).unwrap());
    println!("issuer: published JWK set matches the signing key");

    // AWS IAM.
    let region = env("ADK_FEDERATION_AWS_REGION");
    let sts = AwsWebIdentity::new(&region, env("ADK_FEDERATION_AWS_ROLE_ARN"), issuer.clone());
    let credentials = sts.assume_role(&cashier).await.unwrap();
    assert_eq!(credentials.subject_from_web_identity_token, subject);
    assert!(credentials.expires_at > chrono::Utc::now());
    println!(
        "aws sts: ACCEPTED; SubjectFromWebIdentityToken = {}; credentials valid for {} minutes; \
         session token of {} characters",
        credentials.subject_from_web_identity_token,
        (credentials.expires_at - chrono::Utc::now()).num_minutes(),
        credentials.session_token.expose().len()
    );
    let (status, code, _) = refusal(sts.assume_role(&impostor).await);
    assert_eq!(code, "AccessDenied");
    println!("aws sts: impostor subject REFUSED ({status} {code})");

    // Amazon Bedrock AgentCore Identity.
    let agentcore = AgentCoreIdentity::new(&region);
    let workload = env("ADK_FEDERATION_AGENTCORE_WORKLOAD");
    let own_token = issuer.issue(&cashier, AwsWebIdentity::AUDIENCE).await.unwrap();
    let workload_token =
        agentcore.workload_access_token(&credentials, &workload, &own_token.secret).await.unwrap();
    println!(
        "agentcore: ACCEPTED; workload access token of {} characters for workload {}",
        workload_token.secret.expose().len(),
        workload_token.workload_name
    );
    let (signing_input, signature) = own_token.secret.expose().rsplit_once('.').unwrap();
    let reversed: String = signature.chars().rev().collect();
    let tampered = CredentialSecret::new(format!("{signing_input}.{reversed}"));
    let (status, code, _) =
        refusal(agentcore.workload_access_token(&credentials, &workload, &tampered).await);
    println!("agentcore: tampered signature REFUSED ({status} {code})");

    // Google Cloud.
    let google_parts = (
        env("ADK_FEDERATION_GCP_PROJECT_NUMBER"),
        env("ADK_FEDERATION_GCP_POOL"),
        env("ADK_FEDERATION_GCP_PROVIDER"),
    );
    let google = GoogleWorkloadIdentity::new(
        &google_parts.0,
        &google_parts.1,
        &google_parts.2,
        issuer.clone(),
    );
    let google_token = google.access_token(&cashier).await.unwrap();
    println!(
        "google sts: ACCEPTED; access token of {} characters, valid for {} minutes",
        google_token.secret.expose().len(),
        (google_token.expires_at - chrono::Utc::now()).num_minutes()
    );
    let wrong_audience = GoogleWorkloadIdentity::new(
        &google_parts.0,
        &google_parts.1,
        &google_parts.2,
        Arc::new(FixedAudience { issuer: issuer.clone(), audience: AwsWebIdentity::AUDIENCE }),
    );
    let (status, code, _) = refusal(wrong_audience.access_token(&cashier).await);
    println!("google sts: wrong audience REFUSED ({status} {code})");
    let storage = resource_indicator("https://storage.googleapis.com/").unwrap();
    let brokered =
        google.token_for(&ActingContext::new(cashier.clone()), &storage, &[]).await.unwrap();
    assert_eq!(brokered.agent, cashier.id);
    println!("google sts: CredentialBroker issued a token bound to {}", brokered.audience);

    // Microsoft Entra Agent ID.
    let tenant = env("ADK_FEDERATION_ENTRA_TENANT_ID");
    let blueprint = env("ADK_FEDERATION_ENTRA_BLUEPRINT_APP_ID");
    let agent_identity = env("ADK_FEDERATION_ENTRA_AGENT_IDENTITY_ID");
    let entra = EntraAgentIdentity::new(&tenant, &blueprint, &agent_identity, issuer.clone());
    let entra_token = entra.access_token(&cashier, &format!("api://{blueprint}")).await.unwrap();
    println!(
        "entra: ACCEPTED; agent identity token of {} characters, valid for {} minutes, claims {:?}",
        entra_token.secret.expose().len(),
        (entra_token.expires_at - chrono::Utc::now()).num_minutes(),
        claim_names(entra_token.secret.expose())
    );
    let (status, code, message) =
        refusal(entra.access_token(&impostor, &format!("api://{blueprint}")).await);
    let aadsts = message.split(':').next().unwrap_or_default();
    assert_eq!(aadsts, "AADSTS700213");
    println!("entra: impostor subject REFUSED ({status} {code}, {aadsts})");

    // Inbound: the Entra token addressed to this service, checked against Entra's keys.
    let verifier = InboundAgentVerifier::entra(&tenant, &blueprint).unwrap();
    let principal = verifier.verify(entra_token.secret.expose()).await.unwrap();
    assert_eq!(principal.id.as_str(), format!("urn:entra:{tenant}:agent:{agent_identity}"));
    assert_eq!(principal.tenant.as_deref(), Some(tenant.as_str()));
    println!(
        "entra inbound: VERIFIED; principal is urn:entra:<tenant>:agent:<agent identity>; \
         class set: {}; attributes {:?}",
        principal.class.is_some(),
        principal.attributes.keys().collect::<Vec<_>>()
    );
    let other_audience = InboundAgentVerifier::entra(&tenant, &agent_identity).unwrap();
    let refused = other_audience.verify(entra_token.secret.expose()).await;
    assert!(matches!(&refused, Err(FederationError::Token(_))), "{refused:?}");
    println!("entra inbound: a verifier for another audience REFUSED the token");
    let google_as_entra = verifier.verify(google_token.secret.expose()).await;
    assert!(google_as_entra.is_err());
    println!("entra inbound: a Google access token REFUSED");

    // Inbound: the identity token of an agent hosted on Google Cloud.
    let Ok(token_file) = std::env::var("ADK_FEDERATION_GOOGLE_AGENT_TOKEN_FILE") else {
        println!("google inbound: skipped, ADK_FEDERATION_GOOGLE_AGENT_TOKEN_FILE is not set");
        return;
    };
    let agent_token = std::fs::read_to_string(token_file).expect("google agent token");
    let agent_token = agent_token.trim();
    let agent_issuer = env("ADK_FEDERATION_GOOGLE_AGENT_ISSUER");
    let agent_audience = env("ADK_FEDERATION_GOOGLE_AGENT_AUDIENCE");
    let verifier = InboundAgentVerifier::google(&agent_issuer, &agent_audience).await.unwrap();
    let principal = verifier.verify(agent_token).await.unwrap();
    assert!(principal.id.as_str().starts_with("spiffe://"));
    assert!(principal.tenant.as_deref().is_some_and(|tenant| agent_issuer.ends_with(tenant)));
    println!(
        "google inbound: VERIFIED; claims {:?}; principal id is a SPIFFE id in the issuing pool; \
         service {:?}, resource type {:?}, bound token {:?}; attributes {:?}",
        claim_names(agent_token),
        principal.attributes.get("google.service"),
        principal.attributes.get("google.resource_type"),
        principal.attributes.get("google.bound_token"),
        principal.attributes.keys().collect::<Vec<_>>()
    );
    let other_audience =
        InboundAgentVerifier::google(&agent_issuer, "https://another-service.example")
            .await
            .unwrap();
    let refused = other_audience.verify(agent_token).await;
    assert!(matches!(&refused, Err(FederationError::Token(_))), "{refused:?}");
    println!("google inbound: a verifier for another audience REFUSED the token");
}
