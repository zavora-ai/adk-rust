//! The AWP 2.0 protocol repository's Web Bot Auth vectors (spec 3.5 §7.3, §11
//! "Interoperability", A-I6), checked with this crate's verifier.
//!
//! `tests/fixtures/web-bot-auth/` holds byte-identical copies of the protocol repository's
//! `vectors/web-bot-auth/` (branch `awp-2.0` at `3451c1d`) and their lines of its
//! `vectors/SHA256SUMS`. Each case records the key, the key directory, the request, the
//! signature's input, base and value, the verifier's settings and the verdict; every case
//! reaches its verdict here, and every valid signature is reproduced from its key.

#![cfg(feature = "web-bot-auth")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_auth::web_bot_auth::{
    Ed25519SigningKey, HttpSignatureVerifier, InMemoryKeyDirectorySource, KeyDirectory,
    RequestHead, SignatureInput, WebBotAuthError, jwk_thumbprint,
};
use adk_core::ManualClock;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{TimeZone, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The nine cases of the protocol's `vectors/web-bot-auth/`.
const CASES: [&str; 9] = [
    "awp-get-manifest",
    "awp-post-capability",
    "draft-e-2-1",
    "expired",
    "not-yet-valid",
    "rfc9421-b-2-6",
    "signature-agent-not-covered",
    "tampered-path",
    "unsigned-claim",
];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web-bot-auth")
}

fn vector(case: &str) -> Value {
    let bytes = std::fs::read(dir().join(format!("{case}.json"))).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or_else(|| panic!("{pointer}"))
}

/// A recorded request — method, URI and header fields in order — as an `http::Request`.
fn request(recorded: &Value) -> http::Request<()> {
    let mut builder =
        http::Request::builder().method(text(recorded, "/method")).uri(text(recorded, "/uri"));
    for field in recorded["headers"].as_array().unwrap() {
        builder = builder.header(field[0].as_str().unwrap(), field[1].as_str().unwrap());
    }
    builder.body(()).unwrap()
}

/// A verifier with the case's settings, clock and key directory.
async fn verifier(case: &Value) -> HttpSignatureVerifier {
    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    if let Some(directory) = case.get("directory").filter(|d| !d.is_null()) {
        let document = serde_json::to_vec(&directory["document"]).unwrap();
        let parsed = KeyDirectory::parse(&document, 64).unwrap();
        directories.insert(text(directory, "/origin"), parsed, None).await;
    }
    let settings = &case["verifier"];
    let now = case["expected"]["now"].as_i64().unwrap();
    HttpSignatureVerifier::builder(directories)
        .clock(Arc::new(ManualClock::new(Utc.timestamp_opt(now, 0).unwrap())))
        .max_skew(Duration::from_secs(settings["maxSkewSeconds"].as_u64().unwrap()))
        .max_signature_lifetime(
            settings["maxSignatureLifetimeSeconds"].as_u64().map(Duration::from_secs),
        )
        .allow_test_keys(settings["allowRfc9421TestKey"].as_bool().unwrap())
        .build()
}

/// The verdict name the vectors use for a verification error (the draft's Appendix C.1).
fn reason(error: &WebBotAuthError) -> &'static str {
    match error {
        WebBotAuthError::BadSignature { .. } => "bad_signature",
        WebBotAuthError::Expired { .. } => "expired",
        WebBotAuthError::NotYetValid { .. } => "not_yet_valid",
        WebBotAuthError::MissingComponent { .. } => "missing_component",
        WebBotAuthError::UnknownKey { .. } => "unknown_key",
        WebBotAuthError::LifetimeTooLong { .. } => "lifetime_too_long",
        WebBotAuthError::TestKey { .. } => "test_key",
        other => panic!("no vector names {other:?}"),
    }
}

#[test]
fn the_vectors_are_the_protocol_repositorys_bytes() {
    let sums = std::fs::read_to_string(dir().join("SHA256SUMS")).unwrap();
    let recorded: BTreeMap<&str, &str> = sums
        .lines()
        .map(|line| {
            let (digest, path) = line.split_once("  ").expect("`<sha256>  <path>` lines");
            (path.strip_prefix("web-bot-auth/").expect("a web-bot-auth path"), digest)
        })
        .collect();
    let expected: BTreeMap<String, &str> =
        CASES.iter().map(|case| (format!("{case}.json"), *case)).collect();
    assert_eq!(
        recorded.keys().copied().collect::<Vec<_>>(),
        expected.keys().map(String::as_str).collect::<Vec<_>>(),
        "SHA256SUMS lists each case once"
    );
    let mut on_disk: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    on_disk.sort();
    assert_eq!(
        on_disk,
        expected.keys().cloned().collect::<Vec<_>>(),
        "no case is missing or extra"
    );
    for (file, digest) in recorded {
        let bytes = std::fs::read(dir().join(file)).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            digest,
            "{file} differs from the protocol's"
        );
        assert_eq!(text(&serde_json::from_slice(&bytes).unwrap(), "/vector"), "web-bot-auth");
    }
}

#[test]
fn keys_bases_and_signatures_reproduce() {
    for case in CASES {
        let v = vector(case);
        assert_eq!(text(&v, "/case"), case);
        if v["signature"].is_null() {
            assert!(v["key"].is_null(), "{case}: a key without a signature");
            continue;
        }
        let key = Ed25519SigningKey::from_jwk(&v["key"]["jwk"].to_string()).unwrap();
        let x: [u8; 32] =
            URL_SAFE_NO_PAD.decode(text(&v, "/key/jwk/x")).unwrap().try_into().unwrap();
        assert_eq!(jwk_thumbprint(&x), text(&v, "/key/thumbprint"), "{case}: thumbprint");
        assert_eq!(key.thumbprint(), text(&v, "/key/thumbprint"), "{case}: key");

        let label = text(&v, "/signature/label");
        let field = format!("{label}={}", text(&v, "/signature/input"));
        let inputs = SignatureInput::parse_field(&field).unwrap();
        assert_eq!(inputs.len(), 1, "{case}");
        let (parsed_label, input) = &inputs[0];
        assert_eq!(parsed_label, label);
        assert_eq!(format!("{label}={}", input.serialize().unwrap()), field, "{case}: input");

        let signed = request(&v["request"]);
        let base = RequestHead::from_request(&signed).signature_base(input).unwrap();
        assert_eq!(base, text(&v, "/signature/base"), "{case}: signature base");
        assert_eq!(
            STANDARD.encode(key.sign(base.as_bytes())),
            text(&v, "/signature/value"),
            "{case}: Ed25519 is deterministic, so the signature reproduces"
        );
        let header = |name: &str| signed.headers().get(name).map(|h| h.to_str().unwrap());
        assert_eq!(header("signature-input"), Some(field.as_str()), "{case}");
        assert_eq!(
            header("signature"),
            Some(format!("{label}=:{}:", text(&v, "/signature/value")).as_str()),
            "{case}"
        );
        if let Some(body) = v["request"].get("body").and_then(Value::as_str) {
            let digest = format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body.as_bytes())));
            assert_eq!(header("content-digest"), Some(digest.as_str()), "{case}: Content-Digest");
        }
    }
}

#[tokio::test]
async fn each_case_reaches_its_verdict() {
    for case in CASES {
        let v = vector(case);
        let expected = &v["expected"];
        // A tampered case is verified as the server received it.
        let received = v.get("received").filter(|r| !r.is_null()).unwrap_or(&v["request"]);
        let outcome = verifier(&v).await.verify_request(&request(received)).await;
        match text(expected, "/verdict") {
            "valid" => {
                let requester = outcome
                    .unwrap_or_else(|e| panic!("{case}: {e}"))
                    .unwrap_or_else(|| panic!("{case}: no signature found"));
                assert_eq!(requester.id, text(expected, "/requesterId"), "{case}");
                assert_eq!(requester.directory_origin, text(expected, "/directoryOrigin"));
                assert_eq!(requester.key_thumbprint, text(expected, "/keyThumbprint"));
            }
            "unsigned" => {
                assert_eq!(outcome, Ok(None), "{case}: no identity, whatever the request claims");
                assert_eq!(text(expected, "/tier"), "public");
            }
            verdict @ ("invalid" | "unverified") => {
                let error = outcome.expect_err(case);
                assert_eq!(error.is_unverified(), verdict == "unverified", "{case}: {error}");
                assert_eq!(reason(&error), text(expected, "/reason"), "{case}: {error}");
            }
            other => panic!("{case}: unknown verdict {other}"),
        }
    }
}
