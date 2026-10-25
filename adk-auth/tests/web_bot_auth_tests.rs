//! Web Bot Auth (spec 3.5 §7.3, A-I6, S6): RFC 9421 and draft test vectors, sign/verify
//! round trips, tampering, freshness, the directory cache, SSRF refusal and unsigned requests.

#![cfg(feature = "web-bot-auth")]

use std::sync::Arc;
use std::time::Duration;

use adk_auth::web_bot_auth::{
    ComponentId, DirectoryCachePolicy, Ed25519SigningKey, HttpKeyDirectorySource,
    HttpSignatureSigner, HttpSignatureVerifier, InMemoryKeyDirectorySource, KeyDirectory,
    RequestHead, SignatureInput, SigningParams, VerifiedRequester, WebBotAuthError,
};
use adk_core::webhook_delivery::EgressPolicy;
use adk_core::{AdkError, ErrorCategory, ManualClock};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, TimeZone, Utc};
use http::{HeaderValue, Method, Request, Uri};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// RFC 9421 Appendix B.1.4, `test-key-ed25519`.
const RFC_TEST_JWK: &str = r#"{"kty":"OKP","crv":"Ed25519","kid":"test-key-ed25519","d":"n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU","x":"JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs"}"#;
const RFC_TEST_THUMBPRINT: &str = "poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U";
const DRAFT_AGENT: &str = "https://signature-agent.test";
const BOT: &str = "https://bot.example";

fn rfc_key() -> Ed25519SigningKey {
    Ed25519SigningKey::from_jwk(RFC_TEST_JWK).unwrap()
}

/// RFC 9421 Appendix B.2 `test-request`.
fn rfc_test_request() -> Request<()> {
    Request::post("https://example.com/foo?param=Value&Pet=dog")
        .header("host", "example.com")
        .header("date", "Tue, 20 Apr 2021 02:07:55 GMT")
        .header("content-type", "application/json")
        .header(
            "content-digest",
            "sha-512=:WZDPaVn/7XgHaAy8pmojAkGWoRx2UFChF41A2svX+TaPm+AbwAgBWnrIiYllu7BNNyealdVLvRwEmTHWXvJwew==:",
        )
        .header("content-length", "18")
        .body(())
        .unwrap()
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).unwrap()
}

const T0: i64 = 1_792_900_000;

struct Fixture {
    key: Arc<Ed25519SigningKey>,
    clock: ManualClock,
    directories: Arc<InMemoryKeyDirectorySource>,
    verifier: HttpSignatureVerifier,
    signer: HttpSignatureSigner,
}

async fn fixture() -> Fixture {
    let key = Arc::new(Ed25519SigningKey::generate().unwrap());
    let clock = ManualClock::new(at(T0));
    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    directories.insert(BOT, KeyDirectory::from_keys([key.as_ref()]), None).await;
    let verifier =
        HttpSignatureVerifier::builder(directories.clone()).clock(Arc::new(clock.clone())).build();
    let signer = HttpSignatureSigner::builder(Arc::clone(&key), BOT)
        .clock(Arc::new(clock.clone()))
        .build()
        .unwrap();
    Fixture { key, clock, directories, verifier, signer }
}

fn signed_get(signer: &HttpSignatureSigner, uri: &str) -> Request<()> {
    let mut request = Request::get(uri).body(()).unwrap();
    signer.sign_request(&mut request).unwrap();
    request
}

#[test]
fn rfc_9421_b_2_6_ed25519_vector_reproduces_byte_for_byte() {
    let field = r#"sig-b26=("date" "@method" "@path" "@authority" "content-type" "content-length");created=1618884473;keyid="test-key-ed25519""#;
    let inputs = SignatureInput::parse_field(field).unwrap();
    let (label, input) = &inputs[0];
    assert_eq!(format!("{label}={}", input.serialize().unwrap()), field);

    let request = rfc_test_request();
    let base = RequestHead::from_request(&request).signature_base(input).unwrap();
    assert_eq!(
        base,
        concat!(
            "\"date\": Tue, 20 Apr 2021 02:07:55 GMT\n",
            "\"@method\": POST\n",
            "\"@path\": /foo\n",
            "\"@authority\": example.com\n",
            "\"content-type\": application/json\n",
            "\"content-length\": 18\n",
            "\"@signature-params\": (\"date\" \"@method\" \"@path\" \"@authority\" \"content-type\" \"content-length\");created=1618884473;keyid=\"test-key-ed25519\"",
        )
    );
    let signature = rfc_key().sign(base.as_bytes());
    assert_eq!(
        STANDARD.encode(signature),
        "wqcAqbmYJ2ji2glfAMaRy4gruYYnx2nEFN2HN6jrnDnQCK1u02Gb04v9EDgwUPiu4A0w6vuQv5lIp5WPpBKRCw=="
    );
}

#[test]
fn rfc_9421_test_key_thumbprint_matches_the_draft_keyid() {
    let key = rfc_key();
    assert_eq!(key.thumbprint(), RFC_TEST_THUMBPRINT);
    assert_eq!(key.public_jwk().x, "JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs");
}

const E21_SIGNATURE_INPUT: &str = r#"sig2=("@authority" "signature-agent";key="agent2");created=1735689600;keyid="poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U";alg="ed25519";expires=4889289600;nonce="n9p433xm+NJ3ph3upfBIGmsuwHw387YV7Q/F+6BSpGCVjYCqQw6rznNA8PVVLySrAWsv0hQtFioQb6E1YsauiA==";tag="web-bot-auth""#;
const E21_SIGNATURE: &str = "sig2=:RdNFx5Bj6au3YgAMQL/RzmUlZE8QZLIaXGRpw985hWnwPfMxT228NMk6ehRS1PSl4e8PhbNZACSanGdhEwYCCg==:";

async fn vector_verifier(now: i64) -> HttpSignatureVerifier {
    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    directories.insert(DRAFT_AGENT, KeyDirectory::from_keys([&rfc_key()]), None).await;
    HttpSignatureVerifier::builder(directories)
        .clock(Arc::new(ManualClock::new(at(now))))
        .max_signature_lifetime(None)
        .allow_test_keys(true)
        .build()
}

#[tokio::test]
async fn draft_e_2_1_signer_reproduces_the_vector_and_verifier_accepts_it() {
    let signer = HttpSignatureSigner::builder(Arc::new(rfc_key()), DRAFT_AGENT)
        .label("sig2")
        .agent_member("agent2")
        .components(vec![ComponentId::new("@authority")])
        .build()
        .unwrap();
    let mut request = rfc_test_request();
    let (method, uri) = (request.method().clone(), request.uri().clone());
    signer
        .sign_head_with(
            &method,
            &uri,
            request.headers_mut(),
            SigningParams {
                created: 1_735_689_600,
                expires: 4_889_289_600,
                nonce: Some("n9p433xm+NJ3ph3upfBIGmsuwHw387YV7Q/F+6BSpGCVjYCqQw6rznNA8PVVLySrAWsv0hQtFioQb6E1YsauiA==".into()),
            },
        )
        .unwrap();
    let headers = request.headers();
    assert_eq!(headers["signature-agent"], "agent2=\"https://signature-agent.test\"");
    assert_eq!(headers["signature-input"], E21_SIGNATURE_INPUT);
    assert_eq!(headers["signature"], E21_SIGNATURE);

    let (_, input) = &SignatureInput::parse_field(E21_SIGNATURE_INPUT).unwrap()[0];
    assert_eq!(
        RequestHead::from_request(&request).signature_base(input).unwrap(),
        concat!(
            "\"@authority\": example.com\n",
            "\"signature-agent\";key=\"agent2\": \"https://signature-agent.test\"\n",
            "\"@signature-params\": (\"@authority\" \"signature-agent\";key=\"agent2\");created=1735689600;keyid=\"poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U\";alg=\"ed25519\";expires=4889289600;nonce=\"n9p433xm+NJ3ph3upfBIGmsuwHw387YV7Q/F+6BSpGCVjYCqQw6rznNA8PVVLySrAWsv0hQtFioQb6E1YsauiA==\";tag=\"web-bot-auth\"",
        )
    );

    let requester = vector_verifier(1_735_689_700).await.verify_request(&request).await.unwrap();
    assert_eq!(
        requester,
        Some(VerifiedRequester {
            id: format!("wba:{DRAFT_AGENT}#{RFC_TEST_THUMBPRINT}"),
            directory_origin: DRAFT_AGENT.into(),
            key_thumbprint: RFC_TEST_THUMBPRINT.into(),
            created: at(1_735_689_600),
            expires: at(4_889_289_600),
        })
    );
}

#[tokio::test]
async fn draft_e_2_2_legacy_string_signature_agent_verifies() {
    let mut request = rfc_test_request();
    let headers = request.headers_mut();
    headers.insert("signature-agent", HeaderValue::from_static("\"https://signature-agent.test\""));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig2=("@authority" "signature-agent");created=1735689600;keyid="poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U";alg="ed25519";expires=1735693200;nonce="e8N7S2MFd/qrd6T2R3tdfAuuANngKI7LFtKYI/vowzk4lAZYadIX6wW25MwG7DCT9RUKAJ0qVkU0mEeLElW1qg==";tag="web-bot-auth""#,
        ),
    );
    headers.insert(
        "signature",
        HeaderValue::from_static(
            "sig2=:jdq0SqOwHdyHr9+r5jw3iYZH6aNGKijYp/EstF4RQTQdi5N5YYKrD+mCT1HA1nZDsi6nJKuHxUi/5Syp3rLWBA==:",
        ),
    );
    let (_, input) =
        &SignatureInput::parse_field(request.headers()["signature-input"].to_str().unwrap())
            .unwrap()[0];
    let base = RequestHead::from_request(&request).signature_base(input).unwrap();
    assert!(base.starts_with(
        "\"@authority\": example.com\n\"signature-agent\": \"https://signature-agent.test\"\n"
    ));

    let requester =
        vector_verifier(1_735_690_000).await.verify_request(&request).await.unwrap().unwrap();
    assert_eq!(requester.id, format!("wba:{DRAFT_AGENT}#{RFC_TEST_THUMBPRINT}"));
    assert_eq!(requester.expires, at(1_735_693_200));
}

#[tokio::test]
async fn the_rfc_test_key_is_rejected_unless_allowed() {
    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    directories.insert(DRAFT_AGENT, KeyDirectory::from_keys([&rfc_key()]), None).await;
    let verifier = HttpSignatureVerifier::builder(directories)
        .clock(Arc::new(ManualClock::new(at(1_735_689_700))))
        .max_signature_lifetime(None)
        .build();
    let mut request = rfc_test_request();
    request.headers_mut().insert(
        "signature-agent",
        HeaderValue::from_static("agent2=\"https://signature-agent.test\""),
    );
    request.headers_mut().insert("signature-input", HeaderValue::from_static(E21_SIGNATURE_INPUT));
    request.headers_mut().insert("signature", HeaderValue::from_static(E21_SIGNATURE));
    let error = verifier.verify_request(&request).await.unwrap_err();
    assert_eq!(error, WebBotAuthError::TestKey { keyid: RFC_TEST_THUMBPRINT.into() });
}

#[test]
fn draft_e_2_3_directory_response_reproduces_byte_for_byte() {
    let key = rfc_key();
    let response = KeyDirectory::from_keys([&key])
        .signed_response(&[&key], "signature-agent.test", 1_735_689_600, 4_889_289_600)
        .unwrap();
    assert_eq!(
        response.body,
        r#"{"keys":[{"kty":"OKP","crv":"Ed25519","kid":"poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U","x":"JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs","use":"sig"}]}"#
    );
    assert_eq!(response.content_digest, "sha-256=:CADMT2aBdV/rqQr/NIru64ERQkCobVvllA4V0fLFDu0=:");
    assert_eq!(
        response.signature_input,
        r#"binding=("@authority";req "content-digest");created=1735689600;expires=4889289600;keyid="poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U";tag="http-message-signatures-directory""#
    );
    assert_eq!(
        response.signature,
        "binding=:l6P8R67tm3kujAxbHWio7ll01qrEZ0dKD/WWlGhNYEmTnFZM8Wt0VQ9zqGfvo7T/UMkBxsigzChM1Gpz7gOVBg==:"
    );
}

#[test]
fn directory_responses_label_each_key() {
    let (a, b) = (Ed25519SigningKey::generate().unwrap(), Ed25519SigningKey::generate().unwrap());
    let response = KeyDirectory::from_keys([&a, &b])
        .signed_response(&[&a, &b], "bot.example", T0, T0 + 86_400)
        .unwrap();
    assert!(response.signature_input.starts_with("binding0=("));
    assert!(response.signature_input.contains(", binding1=("));
    assert!(response.signature.contains(", binding1=:"));
    assert!(KeyDirectory::default().signed_response(&[], "bot.example", T0, T0).is_err());
}

#[tokio::test]
async fn sign_then_verify_round_trips_for_http_and_reqwest_requests() {
    let f = fixture().await;
    let request = signed_get(&f.signer, "https://shop.example/catalog?page=2&sort=price");
    let requester = f.verifier.verify_request(&request).await.unwrap().unwrap();
    assert_eq!(requester, VerifiedRequester::new(BOT, f.key.thumbprint(), at(T0), at(T0 + 300)));

    let mut outgoing = reqwest::Client::new()
        .post("https://shop.example/orders?ref=agent")
        .header("content-type", "application/json")
        .body("{}")
        .build()
        .unwrap();
    f.signer.sign_reqwest(&mut outgoing).unwrap();
    let mut received = Request::builder()
        .method(outgoing.method().clone())
        .uri(outgoing.url().as_str())
        .body(())
        .unwrap();
    *received.headers_mut() = outgoing.headers().clone();
    assert_eq!(f.verifier.verify_request(&received).await.unwrap().unwrap().id, requester.id);
}

#[tokio::test]
async fn the_signer_sends_the_default_fields_in_order() {
    let f = fixture().await;
    let request = signed_get(&f.signer, "https://shop.example/");
    let headers = request.headers();
    assert_eq!(headers["signature-agent"], "sig1=\"https://bot.example\"");
    let input = headers["signature-input"].to_str().unwrap();
    let prefix = format!(
        "sig1=(\"@authority\" \"@method\" \"@path\" \"@query\" \"signature-agent\";key=\"sig1\");created={T0};keyid=\"{}\";alg=\"ed25519\";expires={};nonce=\"",
        f.key.thumbprint(),
        T0 + 300
    );
    assert!(input.starts_with(&prefix), "{input}");
    assert!(input.ends_with("\";tag=\"web-bot-auth\""), "{input}");
    let (_, parsed) = SignatureInput::parse_field(input).unwrap().remove(0);
    assert_eq!(STANDARD.decode(parsed.nonce().unwrap()).unwrap().len(), 32);
    assert!(headers["signature"].to_str().unwrap().starts_with("sig1=:"));

    // A second signature leaves the first in place.
    let other = HttpSignatureSigner::builder(Arc::clone(&f.key), "https://other.example")
        .label("sig2")
        .nonce(false)
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let mut request = request;
    other.sign_request(&mut request).unwrap();
    assert_eq!(
        request.headers()["signature-agent"],
        "sig1=\"https://bot.example\", sig2=\"https://other.example\""
    );
    let inputs =
        SignatureInput::parse_field(request.headers()["signature-input"].to_str().unwrap())
            .unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[1].1.nonce(), None);
}

#[tokio::test]
async fn a_request_signed_by_reqwest_verifies_as_the_server_received_it() {
    let f = fixture().await;
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let signer = HttpSignatureSigner::builder(Arc::clone(&f.key), BOT)
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let client = reqwest::Client::new();
    let mut outgoing =
        client.get(format!("{}/products/42?currency=KES&lang=sw", server.uri())).build().unwrap();
    signer.sign_reqwest(&mut outgoing).unwrap();
    client.execute(outgoing).await.unwrap();

    let seen = server.received_requests().await.unwrap().remove(0);
    // A server sees an origin-form target and the authority in Host.
    let target = format!("{}?{}", seen.url.path(), seen.url.query().unwrap_or_default());
    let mut request = Request::builder().method(seen.method.clone()).uri(target).body(()).unwrap();
    *request.headers_mut() = seen.headers.clone();
    assert!(request.uri().authority().is_none());
    let verifier = HttpSignatureVerifier::builder(f.directories.clone())
        .clock(Arc::new(f.clock.clone()))
        .default_scheme("http")
        .build();
    let requester = verifier.verify_request(&request).await.unwrap().unwrap();
    assert_eq!(requester.key_thumbprint, f.key.thumbprint());
}

type Tamper = Box<dyn FnOnce(&mut Request<()>)>;

async fn verify_tampered(f: &Fixture, tamper: impl FnOnce(&mut Request<()>)) -> WebBotAuthError {
    let mut request = signed_get(&f.signer, "https://shop.example/catalog?page=2");
    tamper(&mut request);
    f.verifier.verify_request(&request).await.unwrap_err()
}

fn bad_signature() -> WebBotAuthError {
    WebBotAuthError::BadSignature { label: "sig1".into() }
}

#[tokio::test]
async fn tampering_with_any_covered_component_fails() {
    let f = fixture().await;
    // A second origin publishing the same key, so a rewritten Signature-Agent reaches the
    // signature check instead of failing discovery.
    f.directories
        .insert("https://evil.example", KeyDirectory::from_keys([f.key.as_ref()]), None)
        .await;
    assert!(
        f.verifier
            .verify_request(&signed_get(&f.signer, "https://shop.example/catalog?page=2"))
            .await
            .unwrap()
            .is_some()
    );

    let cases: Vec<(&str, Tamper)> = vec![
        ("@method", Box::new(|r: &mut Request<()>| *r.method_mut() = Method::POST)),
        (
            "@authority",
            Box::new(|r: &mut Request<()>| {
                *r.uri_mut() = Uri::from_static("https://other.example/catalog?page=2")
            }),
        ),
        (
            "@path",
            Box::new(|r: &mut Request<()>| {
                *r.uri_mut() = Uri::from_static("https://shop.example/checkout?page=2")
            }),
        ),
        (
            "@query",
            Box::new(|r: &mut Request<()>| {
                *r.uri_mut() = Uri::from_static("https://shop.example/catalog?page=3")
            }),
        ),
        (
            "signature-agent",
            Box::new(|r: &mut Request<()>| {
                r.headers_mut().insert(
                    "signature-agent",
                    HeaderValue::from_static("sig1=\"https://evil.example\""),
                );
            }),
        ),
        (
            "@signature-params",
            Box::new(|r: &mut Request<()>| {
                let input = r.headers()["signature-input"]
                    .to_str()
                    .unwrap()
                    .replace(&format!("created={T0}"), &format!("created={}", T0 - 1));
                r.headers_mut().insert("signature-input", HeaderValue::from_str(&input).unwrap());
            }),
        ),
        (
            "signature",
            Box::new(|r: &mut Request<()>| {
                let value = r.headers()["signature"].to_str().unwrap().to_string();
                let bytes = STANDARD
                    .decode(value.trim_start_matches("sig1=:").trim_end_matches(':'))
                    .unwrap();
                let mut flipped = bytes.clone();
                flipped[10] ^= 0x01;
                r.headers_mut().insert(
                    "signature",
                    HeaderValue::from_str(&format!("sig1=:{}:", STANDARD.encode(flipped))).unwrap(),
                );
            }),
        ),
    ];
    for (component, tamper) in cases {
        assert_eq!(verify_tampered(&f, tamper).await, bad_signature(), "tampered {component}");
    }
}

#[tokio::test]
async fn expired_and_future_signatures_fail_outside_the_skew() {
    let f = fixture().await;
    let request = signed_get(&f.signer, "https://shop.example/");

    f.clock.set(at(T0 + 300 + 59));
    assert!(f.verifier.verify_request(&request).await.unwrap().is_some());
    f.clock.set(at(T0 + 300 + 61));
    assert_eq!(
        f.verifier.verify_request(&request).await.unwrap_err(),
        WebBotAuthError::Expired { label: "sig1".into(), expires: T0 + 300, now: T0 + 361 }
    );

    // The signer's clock runs ahead of the verifier's.
    f.clock.set(at(T0 + 120));
    let ahead = signed_get(&f.signer, "https://shop.example/");
    f.clock.set(at(T0));
    assert_eq!(
        f.verifier.verify_request(&ahead).await.unwrap_err(),
        WebBotAuthError::NotYetValid { label: "sig1".into(), created: T0 + 120, now: T0 }
    );
    f.clock.set(at(T0 + 30));
    let slightly_ahead = signed_get(&f.signer, "https://shop.example/");
    f.clock.set(at(T0));
    assert!(f.verifier.verify_request(&slightly_ahead).await.unwrap().is_some());
}

#[tokio::test]
async fn signatures_valid_for_longer_than_a_day_are_refused() {
    let f = fixture().await;
    let signer = HttpSignatureSigner::builder(Arc::clone(&f.key), BOT)
        .clock(Arc::new(f.clock.clone()))
        .lifetime(Duration::from_secs(2 * 86_400))
        .build()
        .unwrap();
    let error =
        f.verifier.verify_request(&signed_get(&signer, "https://shop.example/")).await.unwrap_err();
    assert!(
        matches!(error, WebBotAuthError::LifetimeTooLong { lifetime_secs: 172_800, .. }),
        "{error}"
    );
}

#[tokio::test]
async fn unknown_keys_and_origins_are_unverified() {
    let f = fixture().await;
    let stranger = Arc::new(Ed25519SigningKey::generate().unwrap());
    let signer = HttpSignatureSigner::builder(Arc::clone(&stranger), BOT)
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let error =
        f.verifier.verify_request(&signed_get(&signer, "https://shop.example/")).await.unwrap_err();
    assert_eq!(
        error,
        WebBotAuthError::UnknownKey { origin: BOT.into(), keyid: stranger.thumbprint().into() }
    );
    assert!(error.is_unverified());

    let elsewhere = HttpSignatureSigner::builder(Arc::clone(&f.key), "https://unknown.example")
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let error = f
        .verifier
        .verify_request(&signed_get(&elsewhere, "https://shop.example/"))
        .await
        .unwrap_err();
    assert!(matches!(error, WebBotAuthError::DirectoryUnavailable { .. }), "{error}");
    assert!(error.is_unverified());
}

#[tokio::test]
async fn expired_directory_keys_do_not_verify() {
    let f = fixture().await;
    let mut directory = KeyDirectory::from_keys([f.key.as_ref()]);
    directory.keys[0].exp = Some(T0 - 1);
    f.directories.insert("https://retired.example", directory, None).await;
    let signer = HttpSignatureSigner::builder(Arc::clone(&f.key), "https://retired.example")
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let error =
        f.verifier.verify_request(&signed_get(&signer, "https://shop.example/")).await.unwrap_err();
    assert!(matches!(error, WebBotAuthError::KeyNotCurrent { .. }), "{error}");
}

#[tokio::test]
async fn s6_an_unsigned_request_claiming_gptbot_yields_no_identity() {
    let f = fixture().await;
    let request = Request::get("https://shop.example/pricing")
        .header("user-agent", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; +https://openai.com/gptbot)")
        .header("x-awp-channel", "agent")
        .body(())
        .unwrap();
    assert_eq!(f.verifier.verify_request(&request).await.unwrap(), None);
    assert_eq!(f.directories.fetch_count(), 0);

    // A signature for another protocol carries no Web Bot Auth identity either.
    let mut other = Request::get("https://shop.example/").body(()).unwrap();
    other.headers_mut().insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@authority");created=1;keyid="k";tag="other""#),
    );
    other.headers_mut().insert("signature", HeaderValue::from_static("sig1=:AAAA:"));
    assert_eq!(f.verifier.verify_request(&other).await.unwrap(), None);
}

#[tokio::test]
async fn required_components_are_enforced() {
    let f = fixture().await;
    let narrow = HttpSignatureSigner::builder(Arc::clone(&f.key), BOT)
        .clock(Arc::new(f.clock.clone()))
        .components(vec![ComponentId::new("@authority")])
        .build()
        .unwrap();
    let strict = HttpSignatureVerifier::builder(f.directories.clone())
        .clock(Arc::new(f.clock.clone()))
        .require_component(ComponentId::new("@method"))
        .build();
    let request = signed_get(&narrow, "https://shop.example/");
    assert!(f.verifier.verify_request(&request).await.unwrap().is_some());
    assert_eq!(
        strict.verify_request(&request).await.unwrap_err(),
        WebBotAuthError::MissingComponent { label: "sig1".into(), component: "\"@method\"".into() }
    );

    // A signature that leaves the Signature-Agent member uncovered names no directory.
    let mut request = Request::get("https://shop.example/").body(()).unwrap();
    let mut input = SignatureInput::new(vec![ComponentId::new("@authority")]);
    input.params.insert("created", adk_auth::web_bot_auth::structured::BareItem::Integer(T0));
    input.params.insert("expires", adk_auth::web_bot_auth::structured::BareItem::Integer(T0 + 60));
    input.params.insert(
        "keyid",
        adk_auth::web_bot_auth::structured::BareItem::String(f.key.thumbprint().into()),
    );
    input
        .params
        .insert("tag", adk_auth::web_bot_auth::structured::BareItem::String("web-bot-auth".into()));
    let base = RequestHead::from_request(&request).signature_base(&input).unwrap();
    let signature = STANDARD.encode(f.key.sign(base.as_bytes()));
    let headers = request.headers_mut();
    headers.insert("signature-agent", HeaderValue::from_str(&format!("sig1=\"{BOT}\"")).unwrap());
    headers.insert(
        "signature-input",
        HeaderValue::from_str(&format!("sig1={}", input.serialize().unwrap())).unwrap(),
    );
    headers.insert("signature", HeaderValue::from_str(&format!("sig1=:{signature}:")).unwrap());
    assert!(matches!(
        f.verifier.verify_request(&request).await.unwrap_err(),
        WebBotAuthError::MissingComponent { .. }
    ));
}

#[tokio::test]
async fn unsupported_discovery_types_and_non_origins_are_refused() {
    let f = fixture().await;
    let mut request = signed_get(&f.signer, "https://shop.example/");
    let mut jwks = request.clone();
    jwks.headers_mut().insert(
        "signature-agent",
        HeaderValue::from_static("sig1=\"https://bot.example/jwks.json\";type=jwks_uri"),
    );
    assert!(matches!(
        f.verifier.verify_request(&jwks).await.unwrap_err(),
        WebBotAuthError::UnsupportedDiscovery { .. }
    ));
    request
        .headers_mut()
        .insert("signature-agent", HeaderValue::from_static("sig1=\"https://bot.example/keys\""));
    assert!(matches!(
        f.verifier.verify_request(&request).await.unwrap_err(),
        WebBotAuthError::InvalidSignatureAgent { .. }
    ));
}

#[tokio::test]
async fn each_signature_is_verified_independently() {
    // Draft Appendix D.2: an agent signs; a remote browser signs the request it forwards and
    // covers the agent's signature fields.
    let f = fixture().await;
    let agent_key = Arc::new(Ed25519SigningKey::generate().unwrap());
    let agent = HttpSignatureSigner::builder(Arc::clone(&agent_key), "https://agent.alice.example")
        .label("agent")
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let browser = HttpSignatureSigner::builder(Arc::clone(&f.key), BOT)
        .label("browser")
        .clock(Arc::new(f.clock.clone()))
        .components(vec![
            ComponentId::new("@method"),
            ComponentId::new("@authority"),
            ComponentId::new("@path"),
            ComponentId::signature_agent("agent"),
            ComponentId::new("signature-input").with_param(
                "key",
                adk_auth::web_bot_auth::structured::BareItem::String("agent".into()),
            ),
            ComponentId::new("signature").with_param(
                "key",
                adk_auth::web_bot_auth::structured::BareItem::String("agent".into()),
            ),
        ])
        .build()
        .unwrap();
    let mut request = signed_get(&agent, "https://origin.example/resource");
    browser.sign_request(&mut request).unwrap();
    assert!(
        request.headers()["signature-agent"]
            .to_str()
            .unwrap()
            .contains("browser=\"https://bot.example\"")
    );

    // The agent's directory is unknown, so its signature is unverified; the browser's verifies.
    let requester = f.verifier.verify_request(&request).await.unwrap().unwrap();
    assert_eq!(requester.id, format!("wba:{BOT}#{}", f.key.thumbprint()));

    // Once the agent's directory resolves, its signature comes first.
    f.directories
        .insert("https://agent.alice.example", KeyDirectory::from_keys([agent_key.as_ref()]), None)
        .await;
    let fresh = HttpSignatureVerifier::builder(f.directories.clone())
        .clock(Arc::new(f.clock.clone()))
        .build();
    let requester = fresh.verify_request(&request).await.unwrap().unwrap();
    assert_eq!(requester.directory_origin, "https://agent.alice.example");
}

#[tokio::test]
async fn the_directory_cache_honours_max_age() {
    let f = fixture().await;
    f.directories
        .insert(BOT, KeyDirectory::from_keys([f.key.as_ref()]), Some(Duration::from_secs(120)))
        .await;
    let verify = || async {
        let request = signed_get(&f.signer, "https://shop.example/");
        f.verifier.verify_request(&request).await.unwrap().unwrap()
    };
    verify().await;
    verify().await;
    assert_eq!(f.directories.fetch_count(), 1);
    f.clock.advance(Duration::from_secs(119));
    verify().await;
    assert_eq!(f.directories.fetch_count(), 1);
    f.clock.advance(Duration::from_secs(2));
    verify().await;
    assert_eq!(f.directories.fetch_count(), 2);
}

#[tokio::test]
async fn cache_lifetimes_are_clamped_to_the_policy() {
    let f = fixture().await;
    f.directories
        .insert(BOT, KeyDirectory::from_keys([f.key.as_ref()]), Some(Duration::from_secs(1)))
        .await;
    let verify = || async {
        let request = signed_get(&f.signer, "https://shop.example/");
        f.verifier.verify_request(&request).await.unwrap().unwrap()
    };
    verify().await;
    f.clock.advance(Duration::from_secs(30));
    verify().await;
    assert_eq!(f.directories.fetch_count(), 1, "a 1 s max-age is raised to the 60 s minimum");
    f.clock.advance(Duration::from_secs(31));
    verify().await;
    assert_eq!(f.directories.fetch_count(), 2);
}

#[tokio::test]
async fn a_rotated_key_is_found_after_the_minimum_ttl() {
    let f = fixture().await;
    let next = Arc::new(Ed25519SigningKey::generate().unwrap());
    let signer = HttpSignatureSigner::builder(Arc::clone(&next), BOT)
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    assert!(
        f.verifier.verify_request(&signed_get(&f.signer, "https://shop.example/")).await.is_ok()
    );
    f.directories.insert(BOT, KeyDirectory::from_keys([f.key.as_ref(), next.as_ref()]), None).await;
    // Within the minimum TTL an unknown keyid does not trigger a refetch.
    let error =
        f.verifier.verify_request(&signed_get(&signer, "https://shop.example/")).await.unwrap_err();
    assert!(matches!(error, WebBotAuthError::UnknownKey { .. }), "{error}");
    assert_eq!(f.directories.fetch_count(), 1);
    f.clock.advance(Duration::from_secs(61));
    let requester = f
        .verifier
        .verify_request(&signed_get(&signer, "https://shop.example/"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requester.key_thumbprint, next.thumbprint());
    assert_eq!(f.directories.fetch_count(), 2);
}

#[tokio::test]
async fn a_removed_key_stops_verifying_when_the_directory_refreshes() {
    let f = fixture().await;
    assert!(
        f.verifier.verify_request(&signed_get(&f.signer, "https://shop.example/")).await.is_ok()
    );
    let replacement = Ed25519SigningKey::generate().unwrap();
    f.directories.insert(BOT, KeyDirectory::from_keys([&replacement]), None).await;
    // The cached copy verifies until its 5 min default lifetime ends.
    f.clock.advance(Duration::from_secs(299));
    assert!(
        f.verifier.verify_request(&signed_get(&f.signer, "https://shop.example/")).await.is_ok()
    );
    f.clock.advance(Duration::from_secs(2));
    let error = f
        .verifier
        .verify_request(&signed_get(&f.signer, "https://shop.example/"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WebBotAuthError::UnknownKey { origin: BOT.into(), keyid: f.key.thumbprint().into() }
    );
}

/// A source that takes 50 ms per fetch and counts them.
#[derive(Default)]
struct SlowSource {
    inner: InMemoryKeyDirectorySource,
}

#[async_trait::async_trait]
impl adk_auth::web_bot_auth::KeyDirectorySource for SlowSource {
    async fn fetch(
        &self,
        origin: &str,
    ) -> Result<adk_auth::web_bot_auth::FetchedDirectory, WebBotAuthError> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.inner.fetch(origin).await
    }
}

#[tokio::test]
async fn concurrent_misses_share_one_directory_fetch() {
    let f = fixture().await;
    let source = Arc::new(SlowSource::default());
    source.inner.insert(BOT, KeyDirectory::from_keys([f.key.as_ref()]), None).await;
    let verifier =
        HttpSignatureVerifier::builder(source.clone()).clock(Arc::new(f.clock.clone())).build();
    let requests: Vec<_> = (0..6).map(|_| signed_get(&f.signer, "https://shop.example/")).collect();
    let (a, b, c, d, e, g) = tokio::join!(
        verifier.verify_request(&requests[0]),
        verifier.verify_request(&requests[1]),
        verifier.verify_request(&requests[2]),
        verifier.verify_request(&requests[3]),
        verifier.verify_request(&requests[4]),
        verifier.verify_request(&requests[5]),
    );
    for result in [a, b, c, d, e, g] {
        assert!(result.unwrap().is_some());
    }
    assert_eq!(source.inner.fetch_count(), 1);
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_stale_directory_and_failures_are_cached() {
    let f = fixture().await;
    let verifier = HttpSignatureVerifier::builder(f.directories.clone())
        .clock(Arc::new(f.clock.clone()))
        .cache_policy(DirectoryCachePolicy {
            stale_if_error: Duration::from_secs(600),
            negative_ttl: Duration::from_secs(30),
            ..DirectoryCachePolicy::default()
        })
        .build();
    let verify = || async {
        let request = signed_get(&f.signer, "https://shop.example/");
        verifier.verify_request(&request).await
    };
    assert!(verify().await.unwrap().is_some());
    f.directories.remove(BOT).await;
    // Past the 5 min default lifetime the refresh fails; the stale copy keeps verifying.
    f.clock.advance(Duration::from_secs(301));
    assert!(verify().await.unwrap().is_some());
    assert_eq!(f.directories.fetch_count(), 2);
    // The failure is remembered for the negative TTL.
    f.clock.advance(Duration::from_secs(10));
    assert!(verify().await.unwrap().is_some());
    assert_eq!(f.directories.fetch_count(), 2);
    // Beyond stale-if-error nothing verifies.
    f.clock.advance(Duration::from_secs(600));
    assert!(matches!(verify().await.unwrap_err(), WebBotAuthError::DirectoryUnavailable { .. }));
    assert_eq!(f.directories.fetch_count(), 3);
    assert!(verify().await.is_err());
    assert_eq!(f.directories.fetch_count(), 3);
}

const MEDIA_TYPE: &str = "application/http-message-signatures-directory+json";

fn directory_response(directory: &KeyDirectory) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("cache-control", "max-age=120")
        .set_body_raw(directory.to_json(), MEDIA_TYPE)
}

fn local_source() -> Arc<HttpKeyDirectorySource> {
    Arc::new(
        HttpKeyDirectorySource::builder()
            .egress_policy(EgressPolicy::allow_private_networks())
            .max_bytes(4096)
            .build()
            .unwrap(),
    )
}

#[tokio::test]
async fn the_http_source_fetches_and_caches_for_max_age() {
    let f = fixture().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/http-message-signatures-directory"))
        .respond_with(directory_response(&KeyDirectory::from_keys([f.key.as_ref()])))
        .mount(&server)
        .await;
    let signer = HttpSignatureSigner::builder(Arc::clone(&f.key), server.uri())
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let verifier =
        HttpSignatureVerifier::builder(local_source()).clock(Arc::new(f.clock.clone())).build();

    let requester = verifier
        .verify_request(&signed_get(&signer, "https://shop.example/"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requester.directory_origin, server.uri());
    verifier.verify_request(&signed_get(&signer, "https://shop.example/")).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let fetch = &server.received_requests().await.unwrap()[0];
    assert_eq!(fetch.headers["accept"], "application/http-message-signatures-directory+json");

    f.clock.advance(Duration::from_secs(121));
    verifier.verify_request(&signed_get(&signer, "https://shop.example/")).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn the_http_source_refuses_redirects_wrong_types_and_oversized_bodies() {
    let key = Ed25519SigningKey::generate().unwrap();
    let directory = KeyDirectory::from_keys([&key]);
    let cases = [
        (
            ResponseTemplate::new(302).insert_header("location", "https://elsewhere.example/"),
            "answered 302",
        ),
        (ResponseTemplate::new(404), "answered 404"),
        (
            ResponseTemplate::new(200).set_body_raw(directory.to_json(), "application/json"),
            "media type is application/json",
        ),
        (
            ResponseTemplate::new(200).set_body_raw("x".repeat(5000), MEDIA_TYPE),
            "larger than 4096 bytes",
        ),
        (ResponseTemplate::new(200).set_body_raw("{\"not\":\"a jwks\"}", MEDIA_TYPE), "not a JWKS"),
    ];
    // The control: the same source accepts a well-formed directory.
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(directory_response(&directory)).mount(&server).await;
    let fetched =
        adk_auth::web_bot_auth::KeyDirectorySource::fetch(local_source().as_ref(), &server.uri())
            .await
            .unwrap();
    assert_eq!(fetched.directory, directory);
    assert_eq!(fetched.max_age, Some(Duration::from_secs(120)));

    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(response).mount(&server).await;
        let error = adk_auth::web_bot_auth::KeyDirectorySource::fetch(
            local_source().as_ref(),
            &server.uri(),
        )
        .await
        .unwrap_err();
        let kind_matches = if expected.starts_with("answered") {
            matches!(error, WebBotAuthError::DirectoryUnavailable { .. })
        } else {
            matches!(error, WebBotAuthError::MalformedDirectory { .. })
        };
        assert!(
            kind_matches && error.to_string().contains(expected),
            "expected {expected}, got {error}"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "redirects are not followed"
        );
    }
}

#[tokio::test]
async fn directory_fetches_to_private_addresses_are_refused() {
    let f = fixture().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(directory_response(&KeyDirectory::from_keys([f.key.as_ref()])))
        .mount(&server)
        .await;
    let verifier = HttpSignatureVerifier::builder(Arc::new(HttpKeyDirectorySource::new().unwrap()))
        .clock(Arc::new(f.clock.clone()))
        .build();
    let loopback_https = server.uri().replace("http://", "https://");
    for agent in [
        server.uri(),
        loopback_https,
        "https://localhost:8443".into(),
        "https://169.254.169.254".into(),
    ] {
        let signer = HttpSignatureSigner::builder(Arc::clone(&f.key), agent.clone())
            .clock(Arc::new(f.clock.clone()))
            .build()
            .unwrap();
        let error = verifier
            .verify_request(&signed_get(&signer, "https://shop.example/"))
            .await
            .unwrap_err();
        assert!(matches!(error, WebBotAuthError::EgressRefused { .. }), "{agent}: {error}");
        assert!(error.is_unverified());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn errors_map_to_adk_error_categories() {
    let category = |error: WebBotAuthError| AdkError::from(error).category;
    assert_eq!(
        category(WebBotAuthError::BadSignature { label: "sig1".into() }),
        ErrorCategory::Unauthorized
    );
    assert_eq!(
        category(WebBotAuthError::DirectoryUnavailable {
            origin: BOT.into(),
            reason: "down".into()
        }),
        ErrorCategory::Unavailable
    );
    assert_eq!(
        category(WebBotAuthError::EgressRefused { origin: BOT.into(), reason: "private".into() }),
        ErrorCategory::Forbidden
    );
    assert_eq!(
        category(WebBotAuthError::MalformedField { field: "signature".into(), reason: "x".into() }),
        ErrorCategory::InvalidInput
    );
    let adk = AdkError::from(WebBotAuthError::Expired { label: "sig1".into(), expires: 1, now: 2 });
    assert_eq!(adk.code, "auth.web_bot_auth.expired");
    assert!(adk.message.contains("sign each request afresh"));
}

#[test]
fn signer_configuration_is_validated() {
    let key = Arc::new(Ed25519SigningKey::generate().unwrap());
    for agent in
        ["not a url", "https://bot.example/keys", "ftp://bot.example", "https://bot.example/?q=1"]
    {
        assert!(HttpSignatureSigner::builder(Arc::clone(&key), agent).build().is_err(), "{agent}");
    }
    assert!(HttpSignatureSigner::builder(Arc::clone(&key), BOT).label("Sig").build().is_err());
    assert!(
        HttpSignatureSigner::builder(Arc::clone(&key), BOT)
            .components(vec![ComponentId::new("@method")])
            .build()
            .is_err()
    );
    assert!(
        HttpSignatureSigner::builder(Arc::clone(&key), BOT)
            .components(vec![ComponentId::new("@authority"), ComponentId::signature_agent("sig1")])
            .build()
            .is_err()
    );
}

#[test]
fn private_keys_never_reach_debug_output() {
    let key = Arc::new(rfc_key());
    let signer = HttpSignatureSigner::builder(Arc::clone(&key), BOT).build().unwrap();
    let seed = "n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU";
    assert!(!format!("{key:?}").contains(seed));
    assert!(!format!("{signer:?}").contains(seed));
    assert!(key.to_private_jwk().contains(seed));
}

const CLOUDFLARE_RESEARCH: &str = "https://http-message-signatures-example.research.cloudflare.com";

/// Interoperability with Cloudflare's public Web Bot Auth research server (draft Appendix F),
/// which publishes the RFC 9421 test key and verifies signatures made with it. Needs network:
/// `cargo nextest run -p adk-auth --features web-bot-auth --run-ignored only -E 'test(live_)'`.
///
/// 1. The verifier fetches the server's directory over HTTPS under the default egress policy
///    and verifies a request our signer made for that directory.
/// 2. The server, an external verifier, accepts our RFC 9421 Ed25519 signature. It implements
///    the earlier architecture draft, so the request uses the legacy string `Signature-Agent`
///    (draft Appendix E.2.2).
/// 3. The server's directory response proof (draft Appendix B), an external signature, verifies
///    over the signature base this module reconstructs from its `Signature-Input`.
#[tokio::test]
#[ignore = "reaches a public server"]
async fn live_cloudflare_research_interop() {
    use adk_auth::web_bot_auth::structured::BareItem;
    let key = Arc::new(rfc_key());
    let client = reqwest::Client::new();

    // 1. Our signer and verifier against the live directory.
    let verifier = HttpSignatureVerifier::builder(Arc::new(HttpKeyDirectorySource::new().unwrap()))
        .allow_test_keys(true)
        .build();
    let signer =
        HttpSignatureSigner::builder(Arc::clone(&key), CLOUDFLARE_RESEARCH).build().unwrap();
    let requester = verifier
        .verify_request(&signed_get(&signer, "https://site.example/"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requester.id, format!("wba:{CLOUDFLARE_RESEARCH}#{RFC_TEST_THUMBPRINT}"));

    // 2. The external verifier accepts our signature.
    let now = Utc::now().timestamp();
    let mut request = client.get(format!("{CLOUDFLARE_RESEARCH}/")).build().unwrap();
    request.headers_mut().insert(
        "signature-agent",
        HeaderValue::from_str(&format!("\"{CLOUDFLARE_RESEARCH}\"")).unwrap(),
    );
    let mut input = SignatureInput::new(vec![
        ComponentId::new("@authority"),
        ComponentId::new("signature-agent"),
    ]);
    input.params.insert("created", BareItem::Integer(now));
    input.params.insert("keyid", BareItem::String(RFC_TEST_THUMBPRINT.into()));
    input.params.insert("alg", BareItem::String("ed25519".into()));
    input.params.insert("expires", BareItem::Integer(now + 300));
    input.params.insert("tag", BareItem::String("web-bot-auth".into()));
    let uri: Uri = request.url().as_str().parse().unwrap();
    let base =
        RequestHead::new(request.method(), &uri, request.headers()).signature_base(&input).unwrap();
    let signature = STANDARD.encode(key.sign(base.as_bytes()));
    let headers = request.headers_mut();
    headers.insert(
        "signature-input",
        HeaderValue::from_str(&format!("sig1={}", input.serialize().unwrap())).unwrap(),
    );
    headers.insert("signature", HeaderValue::from_str(&format!("sig1=:{signature}:")).unwrap());
    let page = client.execute(request).await.unwrap().text().await.unwrap();
    assert!(
        page.contains("You successfully authenticated as owning the test public key"),
        "the external verifier rejected the signature"
    );

    // 3. The external directory proof verifies over our reconstructed base.
    let response = client
        .get(format!("{CLOUDFLARE_RESEARCH}/.well-known/http-message-signatures-directory"))
        .send()
        .await
        .unwrap();
    let field = |name: &str| response.headers()[name].to_str().unwrap().to_string();
    let (label, input) = SignatureInput::parse_field(&field("signature-input")).unwrap().remove(0);
    assert_eq!(input.tag(), Some("http-message-signatures-directory"));
    let mut base = String::new();
    for component in &input.components {
        let value = match component.name() {
            "@authority" => {
                reqwest::Url::parse(CLOUDFLARE_RESEARCH).unwrap().host_str().unwrap().to_string()
            }
            other => field(other),
        };
        base.push_str(&format!("{}: {value}\n", component.serialize().unwrap()));
    }
    base.push_str(&format!("\"@signature-params\": {}", input.serialize().unwrap()));
    let signatures =
        adk_auth::web_bot_auth::structured::Dictionary::parse(&field("signature")).unwrap();
    let Some(adk_auth::web_bot_auth::structured::ListEntry::Item(item)) = signatures.get(&label)
    else {
        panic!("no signature for {label}");
    };
    let proof = item.bare_item.as_bytes().unwrap();
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, key.public_key())
        .verify(base.as_bytes(), proof)
        .expect("the directory proof verifies");
}
