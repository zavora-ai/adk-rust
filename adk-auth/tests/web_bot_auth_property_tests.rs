//! Web Bot Auth property tests (spec 3.5 §7.3): RFC 8941 structured fields round-trip,
//! signature bases have the RFC 9421 §2.5 shape, every signed request verifies, and any change
//! to a covered component breaks the signature.

#![cfg(feature = "web-bot-auth")]

use std::sync::Arc;

use adk_auth::web_bot_auth::structured::{
    BareItem, Decimal, Dictionary, InnerList, Item, List, ListEntry, Parameters,
};
use adk_auth::web_bot_auth::{
    ComponentId, Ed25519SigningKey, HttpSignatureSigner, HttpSignatureVerifier,
    InMemoryKeyDirectorySource, KeyDirectory, RequestHead, SignatureInput, WebBotAuthError,
    jwk_thumbprint,
};
use adk_core::ManualClock;
use chrono::{TimeZone, Utc};
use http::{HeaderValue, Method, Request};
use proptest::prelude::*;

const MAX: i64 = 999_999_999_999_999;

fn key_strategy() -> impl Strategy<Value = String> {
    "[a-z*][a-z0-9_.*-]{0,8}"
}

fn bare_item() -> impl Strategy<Value = BareItem> {
    prop_oneof![
        (-MAX..=MAX).prop_map(BareItem::Integer),
        (-MAX..=MAX).prop_map(|t| BareItem::Decimal(Decimal::from_thousandths(t).unwrap())),
        "[ -~]{0,24}".prop_map(BareItem::String),
        "[A-Za-z*][A-Za-z0-9!#$%&'*+.^_`|~:/-]{0,12}".prop_map(BareItem::Token),
        proptest::collection::vec(any::<u8>(), 0..40).prop_map(BareItem::ByteSequence),
        any::<bool>().prop_map(BareItem::Boolean),
    ]
}

fn parameters() -> impl Strategy<Value = Parameters> {
    proptest::collection::vec((key_strategy(), bare_item()), 0..4).prop_map(|pairs| {
        let mut params = Parameters::new();
        for (key, value) in pairs {
            params.insert(key, value);
        }
        params
    })
}

fn item() -> impl Strategy<Value = Item> {
    (bare_item(), parameters()).prop_map(|(bare_item, params)| Item { bare_item, params })
}

fn list_entry() -> impl Strategy<Value = ListEntry> {
    prop_oneof![
        item().prop_map(ListEntry::Item),
        (proptest::collection::vec(item(), 0..4), parameters())
            .prop_map(|(items, params)| ListEntry::InnerList(InnerList { items, params })),
    ]
}

fn dictionary() -> impl Strategy<Value = Dictionary> {
    proptest::collection::vec((key_strategy(), list_entry()), 0..5).prop_map(|members| {
        let mut dict = Dictionary::new();
        for (key, member) in members {
            dict.insert(key, member);
        }
        dict
    })
}

/// Serialises with the optional whitespace RFC 8941 parsers must accept around members.
fn loosen(dict: &Dictionary) -> String {
    let members: Vec<String> = dict
        .iter()
        .map(|(key, member)| {
            let mut single = Dictionary::new();
            single.insert(key, member.clone());
            single.serialize().unwrap()
        })
        .collect();
    format!("  {}  ", members.join(" ,\t  "))
}

fn component_set() -> impl Strategy<Value = Vec<ComponentId>> {
    proptest::sample::subsequence(
        vec![
            "@method",
            "@path",
            "@query",
            "@target-uri",
            "@scheme",
            "@request-target",
            "x-a",
            "x-b",
        ],
        0..=8,
    )
    .prop_map(|names| {
        let mut components = vec![ComponentId::new("@authority")];
        components.extend(names.into_iter().map(ComponentId::new));
        components
    })
}

#[derive(Debug, Clone)]
struct RequestSpec {
    method: Method,
    uri: String,
    header_a: String,
    header_b: String,
}

fn request_spec() -> impl Strategy<Value = RequestSpec> {
    (
        proptest::sample::select(vec![
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ]),
        "[a-z]{1,10}",
        proptest::option::of(1024u16..65535),
        proptest::collection::vec("[A-Za-z0-9._~-]{0,8}", 0..4),
        proptest::collection::vec(("[a-z]{1,5}", "[A-Za-z0-9%]{0,6}"), 0..4),
        "[!-~]([ -~]{0,20}[!-~])?",
        "[!-~]([ -~]{0,20}[!-~])?",
    )
        .prop_map(|(method, host, port, segments, query, header_a, header_b)| {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            let path = segments.iter().map(|s| format!("/{s}")).collect::<String>();
            let query = if query.is_empty() {
                String::new()
            } else {
                format!(
                    "?{}",
                    query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
                )
            };
            RequestSpec {
                method,
                uri: format!("https://{host}.example{port}{path}{query}"),
                header_a,
                header_b,
            }
        })
        .prop_filter("valid URI", |spec| spec.uri.parse::<http::Uri>().is_ok())
}

fn build_request(spec: &RequestSpec) -> Request<()> {
    Request::builder()
        .method(spec.method.clone())
        .uri(&spec.uri)
        .header("x-a", HeaderValue::from_str(&spec.header_a).unwrap())
        .header("x-b", HeaderValue::from_str(&spec.header_b).unwrap())
        .body(())
        .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn dictionaries_round_trip(dict in dictionary()) {
        let serialized = dict.serialize().unwrap();
        let parsed = Dictionary::parse(&serialized).unwrap();
        prop_assert_eq!(&parsed, &dict);
        prop_assert_eq!(parsed.serialize().unwrap(), serialized.clone());
        prop_assert_eq!(Dictionary::parse(&loosen(&dict)).unwrap(), dict);
    }

    #[test]
    fn lists_and_items_round_trip(entries in proptest::collection::vec(list_entry(), 0..5), single in item()) {
        let list = List(entries);
        let serialized = list.serialize().unwrap();
        prop_assert_eq!(List::parse(&serialized).unwrap(), list);
        let serialized = single.serialize().unwrap();
        prop_assert_eq!(Item::parse(&serialized).unwrap(), single);
    }

    #[test]
    fn parsing_arbitrary_input_never_panics(input in "[ -~]{0,64}") {
        let _ = Dictionary::parse(&input);
        let _ = List::parse(&input);
        let _ = Item::parse(&input);
        let _ = SignatureInput::parse_field(&input);
    }

    #[test]
    fn signature_inputs_round_trip(
        components in component_set(),
        created in 0i64..4_000_000_000,
        lifetime in 1i64..86_400,
        keyid in "[A-Za-z0-9_-]{43}",
        nonce in proptest::option::of("[A-Za-z0-9+/=]{8,88}"),
        label in key_strategy(),
    ) {
        let mut input = SignatureInput::new(components);
        input.params.insert("created", BareItem::Integer(created));
        input.params.insert("keyid", BareItem::String(keyid));
        input.params.insert("alg", BareItem::String("ed25519".into()));
        input.params.insert("expires", BareItem::Integer(created + lifetime));
        if let Some(nonce) = nonce {
            input.params.insert("nonce", BareItem::String(nonce));
        }
        input.params.insert("tag", BareItem::String("web-bot-auth".into()));
        let field = format!("{label}={}", input.serialize().unwrap());
        let parsed = SignatureInput::parse_field(&field).unwrap();
        prop_assert_eq!(parsed, vec![(label, input)]);
    }

    #[test]
    fn signature_bases_have_the_rfc_9421_shape(spec in request_spec(), components in component_set()) {
        let request = build_request(&spec);
        let input = SignatureInput::new(components.clone());
        let base = RequestHead::from_request(&request).signature_base(&input).unwrap();
        let lines: Vec<&str> = base.split('\n').collect();
        prop_assert_eq!(lines.len(), components.len() + 1);
        for (line, component) in lines.iter().zip(&components) {
            let prefix = format!("{}: ", component.serialize().unwrap());
            prop_assert!(line.starts_with(&prefix), "{} lacks {}", line, prefix);
        }
        let params_line = format!("\"@signature-params\": {}", input.serialize().unwrap());
        prop_assert_eq!(*lines.last().unwrap(), params_line.as_str());
        prop_assert!(base.is_ascii());
        // The same request yields the same base.
        prop_assert_eq!(RequestHead::from_request(&request).signature_base(&input).unwrap(), base);
    }

    #[test]
    fn keys_thumbprint_and_publish_consistently(seed in any::<[u8; 32]>()) {
        let key = Ed25519SigningKey::from_seed(seed).unwrap();
        prop_assert_eq!(key.thumbprint(), jwk_thumbprint(key.public_key()));
        let restored = Ed25519SigningKey::from_jwk(&key.to_private_jwk()).unwrap();
        prop_assert_eq!(restored.public_key(), key.public_key());
        let published = KeyDirectory::from_keys([&key]);
        let parsed = KeyDirectory::parse(published.to_json().as_bytes(), 8).unwrap();
        prop_assert_eq!(&parsed, &published);
        prop_assert_eq!(parsed.find(key.thumbprint()).unwrap().thumbprint().unwrap(), key.thumbprint());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn signed_requests_verify_and_tampering_fails(
        seed in any::<[u8; 32]>(),
        spec in request_spec(),
        components in component_set(),
        other_host in "[a-z]{1,10}",
    ) {
        let now = 1_792_900_000;
        let clock = Arc::new(ManualClock::new(Utc.timestamp_opt(now, 0).unwrap()));
        let key = Arc::new(Ed25519SigningKey::from_seed(seed).unwrap());
        let signer = HttpSignatureSigner::builder(Arc::clone(&key), "https://bot.example")
            .components(components.clone())
            .clock(clock.clone())
            .build()
            .unwrap();
        let mut request = build_request(&spec);
        signer.sign_request(&mut request).unwrap();

        runtime().block_on(async {
            let directories = Arc::new(InMemoryKeyDirectorySource::new());
            directories.insert("https://bot.example", KeyDirectory::from_keys([key.as_ref()]), None).await;
            let verifier = HttpSignatureVerifier::builder(directories).clock(clock).build();

            let requester = verifier.verify_request(&request).await.unwrap().unwrap();
            prop_assert_eq!(requester.key_thumbprint, key.thumbprint());

            let mut moved = request.clone();
            let uri = spec.uri.replacen("://", &format!("://{other_host}x."), 1);
            *moved.uri_mut() = uri.parse().unwrap();
            let error = verifier.verify_request(&moved).await.unwrap_err();
            prop_assert_eq!(error, WebBotAuthError::BadSignature { label: "sig1".into() });

            if components.iter().any(|c| c.name() == "x-a") {
                let mut changed = request.clone();
                changed.headers_mut().insert("x-a", HeaderValue::from_str(&format!("{}!", spec.header_a)).unwrap());
                let error = verifier.verify_request(&changed).await.unwrap_err();
                prop_assert_eq!(error, WebBotAuthError::BadSignature { label: "sig1".into() });
            }
            Ok(())
        })?;
    }
}
