//! Serde round trips, wire spellings and helpers of the AWP 2.0 types (spec 3.5 §7.2–§7.10).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use awp_types::*;
use chrono::{DateTime, TimeZone, Utc};
use common::{round_trip, wire};
use serde_json::json;
use uuid::Uuid;

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, 9, 0, 0).unwrap()
}

fn digest(byte: u8) -> Digest {
    Digest::from_sha256_bytes([byte; 32])
}

fn lang(tag: &str) -> LanguageTag {
    tag.parse().unwrap()
}

fn proof() -> Proof {
    Proof::eddsa("release-2026-10", "eyJhbGciOiJFZERTQSIsImtpZCI6InJlbGVhc2UtMjAyNi0xMCJ9..c2ln")
}

fn charter() -> Charter {
    serde_json::from_str(include_str!("fixtures/v2/charter.json")).unwrap()
}

// §7.2 — the site contract

/// The spec's §7.2 example, read from the spec itself, parses into `SiteContract` once its
/// two elisions (`"018f2c7e-…"`, `"sha256:…"`) are filled in.
#[test]
fn spec_7_2_example_parses_into_site_contract() {
    let spec = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/design/spec-3.5-agentic-web.md"
    ))
    .expect("spec 3.5 is in the repository")
    // A Windows checkout may convert the spec's line ends.
    .replace("\r\n", "\n");
    let section = &spec[spec.find("### 7.2 The site contract").expect("§7.2 exists")..];
    let start = section.find("```json\n").expect("§7.2 has a JSON example") + "```json\n".len();
    let end = start + section[start..].find("```").expect("the example ends");
    let example = section[start..end]
        .replace("018f2c7e-…", "018f2c7e-5d1a-7c3b-9e4f-0a1b2c3d4e5f")
        .replace("sha256:…", digest(0x9f).as_str());

    let contract: SiteContract = serde_json::from_str(&example).expect("§7.2 example parses");

    assert_eq!(contract.awp, VERSION_2_0);
    assert_eq!(contract.site.name, "Example Solar");
    assert_eq!(contract.site.languages, vec![lang("en"), lang("sw")]);
    assert_eq!(contract.profile, Some(Profile::StaticEdge));
    assert_eq!(
        contract.identity.as_ref().unwrap().accepts,
        vec![IdentityMethod::WebBotAuth, IdentityMethod::Oauth2ClientCredentials]
    );
    assert_eq!(contract.release.as_ref().unwrap().seq, 42);
    assert_eq!(contract.status.as_ref().unwrap().last_known, Some(SiteState::Operational));
    assert_eq!(contract.events.as_ref().unwrap().catalog.0.len(), 5);
    assert!(contract.events.as_ref().unwrap().catalog.contains(&EventType::ConsentRevoked));
    assert_eq!(contract.payments.as_ref().unwrap().rails, vec!["ap2", "acp"]);
    assert_eq!(contract.proof.as_ref().unwrap().alg, Proof::EDDSA);
    assert!(contract.surfaces.as_ref().unwrap().machine_views);

    // The serialized contract carries exactly the example's fields.
    let reparsed: serde_json::Value = serde_json::from_str(&example).unwrap();
    assert_eq!(round_trip(&contract), reparsed);

    // The committed fixture is the same example.
    let fixture: SiteContract =
        serde_json::from_str(include_str!("fixtures/v2/site-contract.json")).unwrap();
    assert_eq!(fixture.site, contract.site);
    assert_eq!(fixture.without_proof().proof, None);
}

#[test]
fn minimal_contract_has_only_awp_and_site() {
    let contract = SiteContract::new(SiteRef {
        id: Uuid::nil(),
        name: "Blog".into(),
        domain: "blog.example".into(),
        languages: vec![],
    });
    let json = round_trip(&contract);
    assert_eq!(
        json,
        json!({
            "awp": "2.0",
            "site": { "id": "00000000-0000-0000-0000-000000000000", "name": "Blog", "domain": "blog.example" }
        })
    );
}

#[test]
fn contract_ignores_unknown_fields() {
    let json = json!({
        "awp": "2.1",
        "site": { "id": Uuid::nil(), "name": "S", "domain": "s.example" },
        "futureSurface": { "anything": true }
    });
    let contract: SiteContract = serde_json::from_value(json).unwrap();
    assert_eq!(contract.awp, AwpVersion { major: 2, minor: 1 });
}

#[test]
fn contract_rejects_a_malformed_version() {
    let json =
        json!({ "awp": "two", "site": { "id": Uuid::nil(), "name": "S", "domain": "s.example" } });
    assert!(serde_json::from_value::<SiteContract>(json).is_err());
}

#[test]
fn profile_spellings() {
    assert_eq!(wire(&Profile::Static), "static");
    assert_eq!(wire(&Profile::StaticEdge), "static+edge");
    assert_eq!(wire(&Profile::Dynamic), "dynamic");
}

#[test]
fn well_known_paths() {
    use awp_types::v2::contract::*;
    assert_eq!(MEDIA_TYPE, "application/awp+json");
    assert_eq!(CONTRACT_PATH, "/.well-known/awp.json");
    assert_eq!(KEYS_PATH, "/.well-known/awp/keys.json");
    assert_eq!(CHARTER_PATH, "/.well-known/awp/charter.json");
    assert_eq!(CHECKPOINT_PATH, "/.well-known/awp/log/checkpoint");
    assert_eq!(release_manifest_path(7), "/.well-known/awp/releases/7.json");
}

// §7.2 — keys

fn key_set() -> KeySet {
    KeySet {
        root: RootKey {
            kid: "site-root-2026".into(),
            public_key: PublicKeyJwk::ed25519("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"),
            endorsement: Some(RootEndorsement {
                previous: PublicKeyJwk::ed25519("NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs"),
                proof: Proof::eddsa("site-root-2025", "eyJ9..c2ln"),
            }),
        },
        delegations: vec![
            Delegation {
                kid: "release-2026-10".into(),
                public_key: PublicKeyJwk::ed25519("aaaa"),
                scopes: vec![KeyScope::Release, KeyScope::Card],
                not_before: at(1),
                not_after: at(31),
                proof: Proof::eddsa("site-root-2026", "eyJ9..c2ln"),
            },
            Delegation {
                kid: "receipt-2026-10".into(),
                public_key: PublicKeyJwk::ed25519("bbbb"),
                scopes: vec![KeyScope::Receipt, KeyScope::Payments],
                not_before: at(10),
                not_after: at(20),
                proof: Proof::eddsa("site-root-2026", "eyJ9..c2ln"),
            },
        ],
        retired_roots: vec![RetiredRoot {
            kid: "site-root-2025".into(),
            public_key: PublicKeyJwk::ed25519("NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs"),
            retired_at: at(1),
        }],
        revocations: vec![Revocation {
            kid: "receipt-2026-10".into(),
            revoked_at: at(18),
            proof: Proof::eddsa("site-root-2026", "eyJ9..c2ln"),
        }],
    }
}

#[test]
fn key_set_round_trips_and_walks_delegations() {
    let keys = key_set();
    let json = round_trip(&keys);
    assert_eq!(json["root"]["publicKey"]["crv"], "Ed25519");
    assert_eq!(json["delegations"][0]["notBefore"], "2026-10-01T09:00:00Z");
    assert_eq!(json["delegations"][1]["scopes"], json!(["receipt", "payments"]));

    let release: Vec<_> = keys.delegates_for(KeyScope::Release, at(15)).map(|d| &d.kid).collect();
    assert_eq!(release, vec!["release-2026-10"]);
    assert_eq!(keys.delegates_for(KeyScope::Receipt, at(25)).count(), 0, "outside its window");
    assert_eq!(keys.delegates_for(KeyScope::Payments, at(10)).count(), 1, "window is inclusive");
    assert_eq!(json["retiredRoots"][0]["retiredAt"], "2026-10-01T09:00:00Z");
    assert_eq!(json["revocations"][0]["revokedAt"], "2026-10-18T09:00:00Z");

    // A revoked key signs nothing dated at or after `revokedAt`.
    assert_eq!(keys.delegates_for(KeyScope::Receipt, at(17)).count(), 1);
    assert_eq!(keys.delegates_for(KeyScope::Receipt, at(18)).count(), 0);
    assert!(keys.is_revoked("receipt-2026-10", at(19)));
    assert!(!keys.is_revoked("release-2026-10", at(19)));

    // A key set without rotations or revocations omits both members.
    let fresh = KeySet { retired_roots: vec![], revocations: vec![], ..keys };
    let json = round_trip(&fresh);
    assert!(json.get("retiredRoots").is_none() && json.get("revocations").is_none());
}

#[test]
fn without_proof_drops_only_the_top_level_proof() {
    let status = SiteStatus::new(SiteState::Incident, at(3), proof());
    let value = without_proof(&status).unwrap();
    assert!(value.get("proof").is_none());
    assert_eq!(value["state"], "incident");
    let keys = key_set();
    let value = without_proof(&keys.delegations[0]).unwrap();
    assert!(value.get("proof").is_none());
    assert_eq!(value["kid"], "release-2026-10");
}

#[test]
fn signed_edge_documents_require_a_proof() {
    let status = json!({ "state": "operational", "since": "2026-10-01T00:00:00Z" });
    assert!(serde_json::from_value::<SiteStatus>(status).is_err());
    let mut receipt = serde_json::to_value(consent_receipt()).unwrap();
    receipt.as_object_mut().unwrap().remove("proof");
    assert!(serde_json::from_value::<ConsentReceipt>(receipt).is_err());
}

// §7.3 — identity and trust

#[test]
fn trust_rules_document_assigns_the_highest_matching_tier() {
    let rules = TrustRules {
        accepts: vec![IdentityMethod::WebBotAuth, IdentityMethod::Oauth2ClientCredentials],
        rules: vec![
            TrustRule {
                subject: "wba:https://agents.buyer.example#*".into(),
                tier: TrustTier::Verified,
                rate_class: Some("standard".into()),
                note: None,
            },
            TrustRule {
                subject: "wba:https://agents.buyer.example#partner-key".into(),
                tier: TrustTier::Partner,
                rate_class: Some("partner".into()),
                note: Some("Installer partner".into()),
            },
        ],
        rate_classes: BTreeMap::from([
            ("standard".to_owned(), RateClass { requests_per_minute: 300, calls_per_day: 1_000 }),
            ("partner".to_owned(), RateClass { requests_per_minute: 1_000, calls_per_day: 10_000 }),
        ]),
    };
    let json = round_trip(&rules);
    assert_eq!(json["rateClasses"]["partner"]["requestsPerMinute"], 1_000);
    assert_eq!(json["rules"][0]["rateClass"], "standard");
    let buyer = |key| RequesterId::web_bot_auth("https://agents.buyer.example", key);
    assert_eq!(rules.tier_for(&buyer("k1")), TrustTier::Verified);
    assert_eq!(rules.tier_for(&buyer("partner-key")), TrustTier::Partner);
    assert_eq!(rules.tier_for(&RequesterId::did("did:web:other.example")), TrustTier::Known);
}

#[test]
fn trust_tiers_are_ordered_and_lowercase() {
    assert!(TrustTier::Public < TrustTier::Known);
    assert!(TrustTier::Known < TrustTier::Verified);
    assert!(TrustTier::Verified < TrustTier::Partner);
    for tier in TrustTier::ALL {
        assert_eq!(wire(&tier), tier.as_str());
        round_trip(&tier);
    }
    assert_eq!(TrustTier::default(), TrustTier::Public);
}

#[test]
fn identity_methods_use_the_contract_spelling() {
    assert_eq!(wire(&IdentityMethod::WebBotAuth), "web-bot-auth");
    assert_eq!(wire(&IdentityMethod::Oauth2ClientCredentials), "oauth2-client-credentials");
    assert_eq!(wire(&IdentityMethod::Mtls), "mtls");
    assert_eq!(wire(&IdentityMethod::Did), "did");
    assert_eq!(wire(&IdentityMethod::ApiKey), "api-key");
    round_trip(&IdentityPolicy {
        accepts: vec![IdentityMethod::WebBotAuth, IdentityMethod::Did],
        trust_rules: Some("https://agent.example.com/awp/v2/trust".into()),
    });
}

#[test]
fn requester_ids_name_their_method() {
    let wba = RequesterId::web_bot_auth("https://bots.example.com", "thumb");
    assert_eq!(wba.as_str(), "wba:https://bots.example.com#thumb");
    assert_eq!(wba.method(), Some(IdentityMethod::WebBotAuth));
    assert_eq!(
        RequesterId::oauth_client("https://id.example", "c1").method(),
        Some(IdentityMethod::Oauth2ClientCredentials)
    );
    assert_eq!(RequesterId::mtls("ab12").method(), Some(IdentityMethod::Mtls));
    assert_eq!(RequesterId::did("did:web:agent.example").method(), Some(IdentityMethod::Did));
    assert_eq!(RequesterId::api_key("k1").method(), Some(IdentityMethod::ApiKey));
    assert_eq!(RequesterId::new("GPTBot").method(), None);
    assert_eq!(wire(&wba), "wba:https://bots.example.com#thumb");
    round_trip(&wba);
}

#[test]
fn trust_rules_match_exactly_or_by_prefix() {
    let exact = TrustRule {
        subject: "did:web:partner.example".into(),
        tier: TrustTier::Partner,
        rate_class: Some("partner".into()),
        note: Some("Wholesale partner".into()),
    };
    assert!(exact.matches(&RequesterId::did("did:web:partner.example")));
    assert!(!exact.matches(&RequesterId::did("did:web:partner.example.evil")));
    let prefix = TrustRule { subject: "wba:https://bots.example.com#*".into(), ..exact.clone() };
    assert!(prefix.matches(&RequesterId::web_bot_auth("https://bots.example.com", "k1")));
    assert!(!prefix.matches(&RequesterId::web_bot_auth("https://bots.example.co", "k1")));
    round_trip(&exact);
}

// §7.4 — capabilities, manifest, machine views, held calls

fn full_capability() -> CapabilityV2 {
    CapabilityV2 {
        name: "place_order".into(),
        description: "Place an order for catalogue items".into(),
        effect: CapabilityEffect::NonIdempotent,
        min_trust: TrustTier::Known,
        input_schema: Some(
            json!({ "type": "object", "properties": { "sku_list": { "type": "array" } } }),
        ),
        output_schema: Some(json!({ "type": "object" })),
        consent: Some(ConsentRequirement {
            scope: ConsentScope::PaymentData,
            policy_version: "2026-10".into(),
        }),
        price: Some(Price { amount: 25_000, currency: "KES".into(), rails: vec!["ap2".into()] }),
        approval: ApprovalHint::AboveAmount(100_000),
        idempotency: IdempotencySource::IdempotencyKey,
        rate_class: Some("orders".into()),
        deprecated: Some(Deprecation {
            successor: Some("place_order_v2".into()),
            sunset: Some(at(30)),
        }),
    }
}

#[test]
fn capability_round_trips_with_every_field() {
    let json = round_trip(&full_capability());
    assert_eq!(json["effect"], "non_idempotent");
    assert_eq!(json["minTrust"], "known");
    assert_eq!(json["approval"], json!({ "mode": "above_amount", "amount": 100_000 }));
    assert_eq!(json["idempotency"], "idempotency_key");
    assert_eq!(json["consent"]["policyVersion"], "2026-10");
    assert_eq!(json["deprecated"]["successor"], "place_order_v2");
}

#[test]
fn capability_requires_its_policy_fields() {
    let json = json!({ "name": "x", "description": "x", "effect": "read_only" });
    assert!(serde_json::from_value::<CapabilityV2>(json).is_err());
}

#[test]
fn capability_enum_spellings() {
    assert_eq!(wire(&CapabilityEffect::ReadOnly), "read_only");
    assert_eq!(wire(&CapabilityEffect::Idempotent), "idempotent");
    assert_eq!(wire(&ApprovalHint::Never), json!({ "mode": "never" }));
    assert_eq!(wire(&ApprovalHint::Always), json!({ "mode": "always" }));
    assert_eq!(wire(&IdempotencySource::Binding), "binding");
    assert_eq!(wire(&IdempotencySource::A2aMessageId), "a2a_message_id");
    assert_eq!(wire(&IdempotencySource::McpRequestId), "mcp_request_id");
}

/// A `NonIdempotent` capability requires an idempotency key (A-I7).
#[test]
fn only_non_idempotent_capabilities_require_idempotency() {
    for (effect, required) in [
        (CapabilityEffect::ReadOnly, false),
        (CapabilityEffect::Idempotent, false),
        (CapabilityEffect::NonIdempotent, true),
    ] {
        assert_eq!(CapabilityV2::new("c", "c", effect).requires_idempotency(), required);
    }
}

#[test]
fn approval_hints_hold_calls() {
    assert!(!ApprovalHint::Never.holds(Some(u64::MAX)));
    assert!(ApprovalHint::Always.holds(None));
    assert!(!ApprovalHint::AboveAmount(100).holds(Some(100)));
    assert!(ApprovalHint::AboveAmount(100).holds(Some(101)));
    assert!(!ApprovalHint::AboveAmount(100).holds(None));
}

#[test]
fn manifest_lists_only_what_a_tier_may_see() {
    let manifest = CapabilityManifestV2::new(vec![
        CapabilityV2::new("search_site", "Search", CapabilityEffect::ReadOnly),
        CapabilityV2::new("get_quote", "Quote", CapabilityEffect::Idempotent)
            .with_min_trust(TrustTier::Known),
        CapabilityV2::new("wholesale", "Wholesale", CapabilityEffect::NonIdempotent)
            .with_min_trust(TrustTier::Partner),
    ]);
    let names = |tier| -> Vec<String> {
        manifest.visible_to(tier).capabilities.into_iter().map(|c| c.name).collect()
    };
    assert_eq!(names(TrustTier::Public), vec!["search_site"]);
    assert_eq!(names(TrustTier::Known), vec!["search_site", "get_quote"]);
    assert_eq!(names(TrustTier::Verified), vec!["search_site", "get_quote"]);
    assert_eq!(names(TrustTier::Partner), vec!["search_site", "get_quote", "wholesale"]);
    assert_eq!(manifest.get("get_quote").unwrap().min_trust, TrustTier::Known);
    let json = round_trip(&manifest.visible_to(TrustTier::Known));
    assert_eq!(json["tier"], "known");
    assert!(round_trip(&manifest).get("tier").is_none());
}

#[test]
fn machine_view_round_trips() {
    let view = MachineView {
        path: "/products/panel-400".into(),
        release_seq: 42,
        kind: Some(ContentKind::Product),
        language: Some(lang("en")),
        title: Some("400 W panel".into()),
        resource: BTreeMap::from([
            ("price".to_owned(), json!(25_000)),
            ("sku".to_owned(), json!("PANEL-400")),
        ]),
        capabilities: vec!["get_quote".into(), "place_order".into()],
        json_ld: Some(json!({ "@context": "https://schema.org", "@type": "Product" })),
        markdown: Some("/products/panel-400/index.md".into()),
    };
    let json = round_trip(&view);
    assert_eq!(json["releaseSeq"], 42);
    assert_eq!(json["jsonLd"]["@type"], "Product");
    assert_eq!(
        MachineView::twin_path("/products/panel-400/"),
        "/products/panel-400/index.awp.json"
    );
    assert_eq!(MachineView::markdown_path("/about"), "/about/index.md");
}

#[test]
fn held_calls_round_trip_in_every_state() {
    let mut call = HeldCall::pending("c-1", "book", at(3));
    assert_eq!(call.status, "/awp/v2/calls/c-1");
    round_trip(&call);
    call.state = HeldCallState::Completed;
    call.result = Some(json!({ "bookingId": "b-9" }));
    call.expires_at = Some(at(4));
    let json = round_trip(&call);
    assert_eq!(json["state"], "completed");
    assert_eq!(json["expiresAt"], "2026-10-04T09:00:00Z");
    call.state = HeldCallState::Rejected;
    call.result = None;
    call.error = Some(ErrorBody::new(ErrorCode::PaymentPolicyRejected, "the owner declined"));
    assert_eq!(round_trip(&call)["error"]["code"], "payment_policy_rejected");

    for (state, value, terminal) in [
        (HeldCallState::Pending, "pending", false),
        (HeldCallState::Approved, "approved", false),
        (HeldCallState::Rejected, "rejected", true),
        (HeldCallState::Expired, "expired", true),
        (HeldCallState::Completed, "completed", true),
    ] {
        assert_eq!(wire(&state), value);
        assert_eq!(state.is_terminal(), terminal);
    }
}

// §7.5 — events, status, consent, commerce, errors

#[test]
fn event_types_cover_the_wire_catalogue() {
    for (event_type, value) in [
        (EventType::SiteReleasePublished, "site.release.published"),
        (EventType::SiteStatusChanged, "site.status.changed"),
        (EventType::CatalogChanged, "catalog.changed"),
        (EventType::OrderUpdated, "order.updated"),
        (EventType::ConsentRevoked, "consent.revoked"),
        (EventType::SubscriptionSuspended, "subscription.suspended"),
    ] {
        assert_eq!(wire(&event_type), value);
        assert_eq!(EventType::from(value), event_type);
        assert!(event_type.is_known());
    }
    let custom = EventType::from("booking.reminder");
    assert_eq!(custom, EventType::Other("booking.reminder".into()));
    assert!(!custom.is_known());
    assert_eq!(EventType::Other("catalog.changed".into()), EventType::CatalogChanged);
    round_trip(&custom);
}

#[test]
fn event_envelopes_round_trip_with_each_payload() {
    let release = EventEnvelope::new(
        "evt_1",
        EventType::SiteReleasePublished,
        at(5),
        ReleasePublished {
            seq: 43,
            manifest: "https://example.com/.well-known/awp/releases/43.json".into(),
            digest: digest(1),
            base: Some(42),
        },
    );
    let json = round_trip(&release);
    assert_eq!(json["type"], "site.release.published");
    assert_eq!(json["data"]["base"], 42);

    let status = SiteStatus {
        state: SiteState::Incident,
        since: at(5),
        reason_code: Some("drift_detected".into()),
        release_seq: Some(42),
        proof: proof(),
    };
    round_trip(&EventEnvelope::new(
        "evt_2",
        EventType::SiteStatusChanged,
        at(5),
        StatusChanged { previous: Some(SiteState::Operational), status },
    ));

    let items: Vec<ContentId> = (0..1_500).map(|i| ContentId(format!("sku-{i}"))).collect();
    let catalog = CatalogChanged {
        release_seq: 44,
        items: items[..CatalogChanged::PAGE_SIZE].to_vec(),
        total: items.len() as u64,
        next: Some("https://agent.example.com/awp/v2/events/evt_3/items?page=2".into()),
    };
    assert!(catalog.has_more());
    let json = round_trip(&EventEnvelope::new("evt_3", EventType::CatalogChanged, at(5), catalog));
    assert_eq!(json["data"]["items"].as_array().unwrap().len(), 1_000);

    round_trip(&EventEnvelope::new(
        "evt_4",
        EventType::OrderUpdated,
        at(6),
        OrderUpdated { order_id: "o-1".into(), state: "paid".into(), receipt: Some(Uuid::nil()) },
    ));
    round_trip(&EventEnvelope::new(
        "evt_5",
        EventType::ConsentRevoked,
        at(6),
        ConsentRevoked { receipt: Uuid::nil(), scope: ConsentScope::Newsletter, revoked_at: at(6) },
    ));
    assert_eq!(SubscriptionSuspended::MAX_FAILURES, 6);
    round_trip(&EventEnvelope::new(
        "evt_6",
        EventType::SubscriptionSuspended,
        at(7),
        SubscriptionSuspended {
            subscription: "sub-1".into(),
            failures: 6,
            last_error: Some("HTTP 500".into()),
        },
    ));

    // Untyped data stays raw.
    let raw: EventEnvelope = serde_json::from_value(json!({
        "id": "evt_7", "type": "booking.reminder", "timestamp": "2026-10-07T09:00:00Z", "data": { "x": 1 }
    }))
    .unwrap();
    assert_eq!(raw.data["x"], 1);
    round_trip(&raw);
}

#[test]
fn subscriptions_and_catalog_pages_round_trip() {
    let mut subscription = EventSubscription {
        id: "sub_01JB6Z8Y7X6W5V4U3T2S1R0Q9P".into(),
        types: vec![EventType::CatalogChanged, EventType::OrderUpdated],
        url: "https://agents.buyer.example/hooks/awp".into(),
        state: SubscriptionState::Active,
        created_at: at(20),
        failures: 0,
        secret: Some("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw".into()),
    };
    let json = round_trip(&subscription);
    assert_eq!(json["types"], json!(["catalog.changed", "order.updated"]));
    assert_eq!(json["createdAt"], "2026-10-20T09:00:00Z");
    subscription.secret = None;
    subscription.state = SubscriptionState::Suspended;
    subscription.failures = SubscriptionSuspended::MAX_FAILURES;
    let json = round_trip(&subscription);
    assert_eq!(json["state"], "suspended");
    assert!(json.get("secret").is_none(), "the secret appears only on creation");

    let page = CatalogChangesPage {
        items: (1_000..1_500).map(|i| ContentId(format!("sku-{i}"))).collect(),
        next: None,
    };
    let json = round_trip(&page);
    assert_eq!(json["items"].as_array().unwrap().len(), 500);
    assert!(json.get("next").is_none());
}

#[test]
fn site_status_round_trips_in_every_state() {
    for (state, value) in [
        (SiteState::Operational, "operational"),
        (SiteState::Degraded, "degraded"),
        (SiteState::ReadOnly, "read_only"),
        (SiteState::Maintenance, "maintenance"),
        (SiteState::Incident, "incident"),
    ] {
        assert_eq!(wire(&state), value);
        let mut status = SiteStatus::new(state, at(2), proof());
        status.reason_code = Some(ReasonCode::from("planned"));
        status.release_seq = Some(9);
        let json = round_trip(&status);
        assert_eq!(json["reasonCode"], "planned");
        assert_eq!(json["releaseSeq"], 9);
    }
}

fn consent_receipt() -> ConsentReceipt {
    ConsentReceipt {
        id: Uuid::nil(),
        subject_ref: "subj_8f3a".into(),
        scope: ConsentScope::ProactiveOutreach,
        policy_version: "2026-10".into(),
        policy_digest: digest(3),
        legal_basis: LegalBasis::ExplicitConsent,
        jurisdiction: "KE".into(),
        granted_at: at(1),
        expires_at: Some(at(20)),
        channel: Channel::Whatsapp,
        proof: proof(),
    }
}

#[test]
fn consent_receipt_round_trips_with_the_spec_fields() {
    let json = round_trip(&consent_receipt());
    let mut keys: Vec<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "channel",
            "expiresAt",
            "grantedAt",
            "id",
            "jurisdiction",
            "legalBasis",
            "policyDigest",
            "policyVersion",
            "proof",
            "scope",
            "subjectRef"
        ]
    );
    assert_eq!(json["legalBasis"], "explicit_consent");
    assert_eq!(json["channel"], "whatsapp");
}

/// A consent-bound action refuses without a live receipt for its scope and policy version
/// (A-I8).
#[test]
fn consent_receipt_covers_only_its_scope_version_and_window() {
    let receipt = consent_receipt();
    let needed = ConsentRequirement {
        scope: ConsentScope::ProactiveOutreach,
        policy_version: "2026-10".into(),
    };
    assert!(receipt.covers(&needed, at(10)));
    assert!(!receipt.covers(&needed, at(20)), "expired at expiresAt");
    assert!(!receipt.covers(&needed, Utc.with_ymd_and_hms(2026, 9, 30, 0, 0, 0).unwrap()));
    assert!(!receipt.covers(
        &ConsentRequirement { policy_version: "2026-11".into(), ..needed.clone() },
        at(10)
    ));
    assert!(
        !receipt.covers(&ConsentRequirement { scope: ConsentScope::Newsletter, ..needed }, at(10))
    );
}

#[test]
fn consent_scopes_include_the_draft_scopes_newsletter_and_custom() {
    for (scope, value) in [
        (ConsentScope::SessionContinuity, "session_continuity"),
        (ConsentScope::ProactiveOutreach, "proactive_outreach"),
        (ConsentScope::PersistentProfile, "persistent_profile"),
        (ConsentScope::PaymentData, "payment_data"),
        (ConsentScope::Newsletter, "newsletter"),
        (ConsentScope::Custom("contact".into()), "contact"),
    ] {
        assert_eq!(wire(&scope), value);
        assert_eq!(round_trip(&scope), value);
    }
    for (basis, value) in [
        (LegalBasis::ExplicitConsent, "explicit_consent"),
        (LegalBasis::LegitimateInterest, "legitimate_interest"),
        (LegalBasis::ContractualNecessity, "contractual_necessity"),
        (LegalBasis::LegalObligation, "legal_obligation"),
        (LegalBasis::VitalInterest, "vital_interest"),
        (LegalBasis::PublicTask, "public_task"),
    ] {
        assert_eq!(wire(&basis), value);
    }
}

fn line(sku: &str, quantity: u32, unit_amount: u64) -> LineItem {
    LineItem { sku: sku.into(), description: Some(format!("{sku} item")), quantity, unit_amount }
}

/// An open quote keeps its price until its validity ends (A-I25).
#[test]
fn quotes_hold_their_price_until_valid_until() {
    let quote = QuoteDocument {
        id: Uuid::nil(),
        capability: "get_quote".into(),
        requester: Some(RequesterId::web_bot_auth("https://buyer.example", "k")),
        lines: vec![line("PANEL-400", 2, 25_000), line("INVERTER", 1, 80_000)],
        total: 130_000,
        currency: "KES".into(),
        release_seq: 42,
        issued_at: at(1),
        valid_until: at(8),
        proof: Some(proof()),
    };
    let json = round_trip(&quote);
    assert_eq!(json["validUntil"], "2026-10-08T09:00:00Z");
    assert_eq!(json["lines"][0]["unitAmount"], 25_000);
    assert!(quote.is_open(at(8)));
    assert!(!quote.is_open(at(9)));
    assert_eq!(quote.lines_total(), Some(quote.total));
    let overflow = QuoteDocument { lines: vec![line("X", u32::MAX, u64::MAX)], ..quote };
    assert_eq!(overflow.lines_total(), None);
}

#[test]
fn receipts_round_trip() {
    let receipt = Receipt {
        id: Uuid::nil(),
        capability: "place_order".into(),
        reference: "o-1".into(),
        requester: Some(RequesterId::web_bot_auth("https://buyer.example", "k")),
        quote: Some(Uuid::nil()),
        lines: vec![line("PANEL-400", 2, 25_000)],
        amount: 50_000,
        currency: "KES".into(),
        rail: Some("ap2".into()),
        release_seq: Some(42),
        issued_at: at(2),
        proof: proof(),
    };
    let json = round_trip(&receipt);
    assert_eq!(json["releaseSeq"], 42);
    assert_eq!(receipt.lines_total(), Some(50_000));
}

#[test]
fn error_envelope_matches_the_spec_shape() {
    let mut envelope = ErrorEnvelope::new(ErrorCode::RateLimitExceeded, "slow down");
    envelope.error.retry_after = Some(30);
    envelope.error.trace_id = Some("4bf92f3577b34da6".into());
    let json = round_trip(&envelope);
    assert_eq!(
        json,
        json!({
            "awp": "2.0",
            "error": {
                "code": "rate_limit_exceeded",
                "message": "slow down",
                "retryable": true,
                "retryAfter": 30,
                "traceId": "4bf92f3577b34da6"
            }
        })
    );
}

#[test]
fn read_only_errors_carry_the_status_document() {
    let status = SiteStatus {
        state: SiteState::ReadOnly,
        since: at(24),
        reason_code: Some("drift_detected".into()),
        release_seq: Some(101),
        proof: proof(),
    };
    let mut envelope = ErrorEnvelope::read_only("bookings resume when the incident ends", status);
    envelope.error.retry_after = Some(900);
    let json = round_trip(&envelope);
    assert_eq!(json["error"]["code"], "read_only");
    assert_eq!(json["error"]["retryable"], true);
    assert_eq!(json["status"]["reasonCode"], "drift_detected");
    assert_eq!(envelope.error.code.http_status(), 503);
}

#[test]
fn error_codes_map_to_statuses() {
    for (code, value, status, retryable) in [
        (ErrorCode::InvalidRequest, "invalid_request", 400, false),
        (ErrorCode::SchemaValidationFailed, "schema_validation_failed", 400, false),
        (ErrorCode::IdempotencyKeyRequired, "idempotency_key_required", 400, false),
        (ErrorCode::Unauthorized, "unauthorized", 401, false),
        (ErrorCode::PaymentPolicyRejected, "payment_policy_rejected", 402, false),
        (ErrorCode::Forbidden, "forbidden", 403, false),
        (ErrorCode::CapabilityNotFound, "capability_not_found", 404, false),
        (ErrorCode::ResourceNotFound, "resource_not_found", 404, false),
        (ErrorCode::VersionNotSupported, "version_not_supported", 406, false),
        (ErrorCode::IdempotencyConflict, "idempotency_conflict", 409, false),
        (ErrorCode::InventoryUnavailable, "inventory_unavailable", 409, true),
        (ErrorCode::QuoteExpired, "quote_expired", 409, false),
        (ErrorCode::SubscriptionSuspended, "subscription_suspended", 410, false),
        (ErrorCode::RateLimitExceeded, "rate_limit_exceeded", 429, true),
        (ErrorCode::ConsentRequired, "consent_required", 451, false),
        (ErrorCode::InternalError, "internal_error", 500, true),
        (ErrorCode::PaymentFailed, "payment_failed", 502, true),
        (ErrorCode::ServiceDegraded, "service_degraded", 503, true),
        (ErrorCode::ReadOnly, "read_only", 503, true),
        (ErrorCode::from("teapot"), "teapot", 500, false),
    ] {
        assert_eq!(wire(&code), value);
        assert_eq!(code.http_status(), status, "{value}");
        assert_eq!(code.is_retryable(), retryable, "{value}");
    }
    assert_eq!(ErrorCode::KNOWN.len(), 19);
}

// §7.7, §7.8.3 — releases

fn file(byte: u8, size: u64) -> FileEntry {
    FileEntry { sha256: digest(byte), size, content_type: "text/html".into() }
}

fn snapshot() -> ReleaseManifest {
    ReleaseManifest {
        seq: 41,
        previous: Some(digest(40)),
        base: None,
        created_at: at(4),
        files: BTreeMap::from([
            ("/index.html".to_owned(), file(1, 100)),
            ("/about/index.html".to_owned(), file(2, 200)),
            ("/old/index.html".to_owned(), file(3, 300)),
        ]),
        removed: BTreeSet::new(),
        redirects: vec![Redirect { from: "/old".into(), to: "/about".into(), status: 301 }],
        source: ReleaseSource::Migration,
        generator: GeneratorRef { name: "adk-site".into(), version: "3.0.0".into() },
        charter_version: 1,
    }
}

#[test]
fn release_manifests_round_trip_as_snapshot_and_delta() {
    let base = snapshot();
    let json = round_trip(&base);
    assert!(base.is_snapshot());
    assert!(json.get("base").is_none());
    assert!(json.get("removed").is_none(), "a snapshot omits `removed`");
    assert_eq!(json["files"]["/index.html"]["contentType"], "text/html");
    assert_eq!(json["source"], json!({ "kind": "migration" }));
    assert_eq!(json["charterVersion"], 1);

    let delta = ReleaseManifest {
        seq: 42,
        previous: Some(digest(41)),
        base: Some(41),
        files: BTreeMap::from([
            ("/index.html".to_owned(), file(9, 120)),
            ("/new/index.html".to_owned(), file(8, 80)),
        ]),
        removed: BTreeSet::from(["/old/index.html".to_owned()]),
        source: ReleaseSource::Edit { job: "freshness".into(), decision: Some("dec-7".into()) },
        ..base.clone()
    };
    let json = round_trip(&delta);
    assert!(!delta.is_snapshot());
    assert_eq!(json["base"], 41);
    assert_eq!(json["source"], json!({ "kind": "edit", "job": "freshness", "decision": "dec-7" }));

    let resolved = delta.apply_to(&base.files);
    assert_eq!(
        resolved.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["/about/index.html", "/index.html", "/new/index.html"]
    );
    assert_eq!(resolved["/index.html"], file(9, 120));
    assert_eq!(base.apply_to(&BTreeMap::new()), base.files, "a snapshot ignores its base");

    let rollback = ReleaseManifest { source: ReleaseSource::Rollback { to_seq: 40 }, ..base };
    assert_eq!(round_trip(&rollback)["source"], json!({ "kind": "rollback", "toSeq": 40 }));
    let onboarding = ReleaseManifest { source: ReleaseSource::Onboarding, ..rollback };
    assert_eq!(round_trip(&onboarding)["source"], json!({ "kind": "onboarding" }));
}

#[test]
fn log_entries_are_exactly_seq_and_manifest() {
    let entry = LogEntry { seq: 101, manifest: digest(0x7a) };
    let json = round_trip(&entry);
    assert_eq!(json, json!({ "seq": 101, "manifest": digest(0x7a).as_str() }));
    assert_eq!(entry.leaf_index(), Some(100));
    assert_eq!(LogEntry { seq: 0, ..entry }.leaf_index(), None);
    let extra = json!({ "seq": 1, "manifest": digest(1).as_str(), "note": "x" });
    assert!(serde_json::from_value::<LogEntry>(extra).is_err(), "extra members change the leaf");
}

#[test]
fn digests_accept_only_sha256_lowercase_hex() {
    let good = digest(0xab);
    assert_eq!(good.as_str(), format!("sha256:{}", "ab".repeat(32)));
    assert_eq!(round_trip(&good), good.as_str());
    for bad in [
        "ab".repeat(32),
        format!("sha512:{}", "ab".repeat(32)),
        format!("sha256:{}", "AB".repeat(32)),
        format!("sha256:{}", "ab".repeat(31)),
        "sha256:zz".to_owned(),
    ] {
        assert!(bad.parse::<Digest>().is_err(), "{bad}");
        assert!(serde_json::from_value::<Digest>(json!(bad)).is_err(), "{bad}");
    }
    assert_eq!(Digest::parse("x"), Err(DigestError::MissingAlgorithm));
}

#[test]
fn host_limits_round_trip() {
    for (atomic, value) in [
        (Atomicity::Deploy, "deploy"),
        (Atomicity::PrefixPointer, "prefix_pointer"),
        (Atomicity::None, "none"),
    ] {
        let limits = HostLimits { max_publishes_per_hour: Some(60), delta_upload: true, atomic };
        assert_eq!(round_trip(&limits)["atomic"], value);
    }
}

// §7.6 — the charter

#[test]
fn charter_fixture_round_trips_and_validates() {
    let charter = charter();
    let json = round_trip(&charter);
    assert_eq!(json["kind"], "business");
    assert_eq!(json["governance"]["approvalChains"][1]["action"], "charter_change");
    assert_eq!(json["kpis"][1]["source"], "analytics:search_console");
    assert_eq!(json["authority"][0]["level"], "act_within_budget");
    assert!(json["authority"][1].get("limits").is_none(), "no limits, no member");
    assert_eq!(json["budget"]["caps"]["models"], 300_000);
    assert_eq!(json["reporting"]["routes"][0]["urgency"], "p1");
    assert_eq!(json["public"]["consentScopes"][2], "contact");
    assert_eq!(charter.validate(), Ok(()));
    assert_eq!(
        charter.approval_chain(&ActionClass::Publish).unwrap().roles,
        vec![Role::Editor, Role::Publisher]
    );
    assert!(charter.approval_chain(&ActionClass::from("dns_change")).is_none());
    round_trip(&charter.public);
}

/// A KPI has no target until it has a baseline (A-I14).
#[test]
fn a_target_without_a_baseline_is_invalid() {
    let mut charter = charter();
    charter.kpis[0].baseline = None;
    assert_eq!(
        charter.validate().unwrap_err().violations,
        vec![CharterViolation::TargetWithoutBaseline { kpi: "leads-qualified".into() }]
    );
    charter.kpis[0].target = None;
    assert_eq!(charter.validate(), Ok(()), "measure-only KPIs carry no target");
}

#[test]
fn a_charter_needs_an_owner() {
    let mut charter = charter();
    charter.governance.roles[0].roles = vec![Role::Editor];
    let violations = charter.validate().unwrap_err().violations;
    assert!(violations.contains(&CharterViolation::NoOwner));
    assert!(violations.contains(&CharterViolation::UnsatisfiableApprovalChain {
        action: ActionClass::CharterChange
    }));
}

/// No agent holds a role (spec 3.5 §7.6, A-I23).
#[test]
fn an_agent_cannot_hold_a_role() {
    let mut charter = charter();
    charter.governance.roles.push(RoleHolder {
        principal: "urn:adk:agent:example/steward".into(),
        roles: vec![Role::Publisher],
    });
    assert_eq!(
        charter.validate().unwrap_err().violations,
        vec![CharterViolation::AgentHoldsRole {
            principal: "urn:adk:agent:example/steward".into()
        }]
    );
}

/// A chain is satisfied only by distinct principals (spec 3.5 §7.6, A-I23).
#[test]
fn approval_chains_need_distinct_principals() {
    let mut charter = charter();
    // A charter change needing two owners is unsatisfiable with one owner …
    charter.governance.approval_chains[1].roles = vec![Role::Owner, Role::Owner];
    assert_eq!(
        charter.validate().unwrap_err().violations,
        vec![CharterViolation::UnsatisfiableApprovalChain { action: ActionClass::CharterChange }]
    );
    // … and satisfiable once a second person holds the role.
    charter
        .governance
        .roles
        .push(RoleHolder { principal: "user:chao".into(), roles: vec![Role::Owner] });
    assert_eq!(charter.validate(), Ok(()));

    let governance = &charter.governance;
    let two_owners = &governance.approval_chains[1];
    assert!(two_owners.is_satisfied_by(governance, &["user:amina", "user:chao"]));
    assert!(
        !two_owners.is_satisfied_by(governance, &["user:amina", "user:amina"]),
        "one person twice"
    );
    assert!(
        !two_owners.is_satisfied_by(governance, &["user:amina", "user:baraka"]),
        "not an owner"
    );

    // One person holding both roles of a chain cannot fill both.
    let publish =
        ApprovalChain { action: ActionClass::Publish, roles: vec![Role::Editor, Role::Publisher] };
    let solo = Governance {
        roles: vec![RoleHolder {
            principal: "user:dee".into(),
            roles: vec![Role::Owner, Role::Editor, Role::Publisher],
        }],
        approval_chains: vec![publish.clone()],
    };
    assert!(!publish.is_satisfied_by(&solo, &["user:dee"]));
    let mut solo_charter = charter.clone();
    solo_charter.governance = solo;
    assert_eq!(
        solo_charter.validate().unwrap_err().violations,
        vec![CharterViolation::UnsatisfiableApprovalChain { action: ActionClass::Publish }]
    );

    // The matching reassigns a person who holds two roles when that frees a slot.
    let flexible = Governance {
        roles: vec![
            RoleHolder { principal: "user:a".into(), roles: vec![Role::Editor, Role::Publisher] },
            RoleHolder { principal: "user:b".into(), roles: vec![Role::Editor] },
        ],
        approval_chains: vec![],
    };
    assert!(publish.is_satisfied_by(&flexible, &["user:a", "user:b"]));
    assert!(!publish.is_satisfied_by(&flexible, &["user:a", "urn:adk:agent:x/steward"]));
}

/// The owner's settings are optional charter members: a charter without them reads and
/// writes as before, and with them they travel in camelCase (spec 3.5 §5 S8, S10, §7.4).
#[test]
fn the_owners_settings_are_optional_members() {
    let charter = charter();
    assert_eq!(
        (charter.handoff_threshold, charter.publish_delay_hours, charter.amount_thresholds.len()),
        (None, None, 0)
    );
    let json = round_trip(&charter);
    for member in ["handoffThreshold", "publishDelayHours", "amountThresholds"] {
        assert!(json.get(member).is_none(), "{member} is left out when not set");
    }

    let mut set = charter.clone();
    set.handoff_threshold = Some(60);
    set.publish_delay_hours = Some(48);
    set.amount_thresholds = BTreeMap::from([("place_order".to_owned(), 20_000)]);
    let json = round_trip(&set);
    assert_eq!(json["handoffThreshold"], 60);
    assert_eq!(json["publishDelayHours"], 48);
    assert_eq!(json["amountThresholds"], json!({ "place_order": 20_000 }));
    assert_eq!(set.validate(), Ok(()));

    set.handoff_threshold = Some(101);
    assert_eq!(
        set.validate().unwrap_err().violations,
        vec![CharterViolation::HandoffThresholdOutOfRange { threshold: 101 }]
    );
}

#[test]
fn charter_rejects_empty_and_duplicate_chains_and_bad_kpi_references() {
    let mut charter = charter();
    charter
        .governance
        .approval_chains
        .push(ApprovalChain { action: ActionClass::Publish, roles: vec![] });
    charter.kpis.push(charter.kpis[0].clone());
    charter.goals[0].kpis.push("missing".into());
    let error = charter.validate().unwrap_err();
    assert_eq!(
        error.violations,
        vec![
            CharterViolation::DuplicateApprovalChain { action: ActionClass::Publish },
            CharterViolation::EmptyApprovalChain { action: ActionClass::Publish },
            CharterViolation::DuplicateKpi { kpi: "leads-qualified".into() },
            CharterViolation::UnknownKpi { goal: "enquiries".into(), kpi: "missing".into() },
        ]
    );
    assert!(error.to_string().starts_with("charter is invalid: "));
}

#[test]
fn charter_enum_spellings() {
    let kinds: Vec<_> = SiteKind::ALL.iter().map(wire).collect();
    assert_eq!(
        kinds,
        vec![
            "personal",
            "business",
            "ecommerce",
            "government",
            "nonprofit",
            "education",
            "media",
            "health",
            "community",
            "events"
        ]
    );
    for (level, value) in [
        (AutonomyLevel::Observe, "observe"),
        (AutonomyLevel::Propose, "propose"),
        (AutonomyLevel::ActWithApproval, "act_with_approval"),
        (AutonomyLevel::ActWithinBudget, "act_within_budget"),
        (AutonomyLevel::ActReversibleOnly, "act_reversible_only"),
    ] {
        assert_eq!(wire(&level), value);
    }
    for (role, value) in [
        (Role::Owner, "owner"),
        (Role::Publisher, "publisher"),
        (Role::Editor, "editor"),
        (Role::Approver, "approver"),
        (Role::Auditor, "auditor"),
    ] {
        assert_eq!(wire(&role), value);
    }
    assert_eq!(wire(&Urgency::P1), "p1");
    assert_eq!(wire(&Urgency::P3), "p3");
    assert_eq!(wire(&GuardrailAction::PauseJob), "pause_job");
    assert_eq!(wire(&SpendCategory::Domains), "domains");
    assert_eq!(wire(&Cadence::Quarterly), "quarterly");
    assert_eq!(wire(&Direction::Down), "down");
    assert_eq!(wire(&RedLineKind::Topic), "topic");
    for source in
        ["beacon", "host_logs", "analytics:ga4", "commerce", "spend", "email", "aeo_queries"]
    {
        assert_eq!(round_trip(&SourceRef::from(source)), source);
    }
    assert_eq!(SourceRef::from("aeo_queries"), SourceRef::Other("aeo_queries".into()));
}

#[test]
fn guardrails_breach_on_their_side_of_the_threshold() {
    let charter = charter();
    let guardrail = &charter.guardrails[0];
    assert!(guardrail.is_breached_by(0.03));
    assert!(!guardrail.is_breached_by(0.02));
    let floor = Guardrail { breach: Breach::Below, ..guardrail.clone() };
    assert!(floor.is_breached_by(0.01));
    assert_eq!(guardrail.window, Duration::from_secs(7 * 24 * 3600));
}

/// A price change outside the charter's price rule is held (A-I25).
#[test]
fn price_rule_allows_changes_within_its_percentage() {
    let rule = PriceRule::default();
    assert_eq!(rule.auto_within_pct, 10.0);
    assert!(rule.allows(10_000, 10_500), "5% publishes");
    assert!(rule.allows(10_000, 11_000), "10% is within");
    assert!(!rule.allows(10_000, 11_500), "15% is held");
    assert!(!rule.allows(10_000, 8_500), "a 15% cut is held");
    assert!(!rule.allows(0, 100), "a price from zero is held");
    assert!(rule.allows(0, 0));
    assert_eq!(round_trip(&rule), json!({ "autoWithinPct": 10.0 }));
}

// §7.10 — signals

fn signals() -> Vec<SignalKind> {
    let requester = RequesterId::web_bot_auth("https://bots.example.com", "k");
    vec![
        SignalKind::LeadCaptured {
            lead_ref: LeadRef("lead-1".into()),
            source: Channel::Chat,
            consent: vec![ConsentReceiptRef(Uuid::nil())],
            answers: BTreeMap::from([("roof_type".to_owned(), Bounded::new("tin", 40).unwrap())]),
        },
        SignalKind::ContentGap {
            topic: Bounded::new("warranty", 40).unwrap(),
            question_count: 4,
            samples: vec![Bounded::new("How long is the warranty?", 200).unwrap()],
        },
        SignalKind::VisitorIntent { capability: "book".into(), outcome: Outcome::Abandoned },
        SignalKind::SupportEscalation {
            reason: "billing_dispute".into(),
            contact_ref: ContactRef("contact-7".into()),
        },
        SignalKind::ReviewSeen {
            platform: "maps".into(),
            rating: 4,
            excerpt: Bounded::new("Quick install", 200).unwrap(),
        },
        SignalKind::InboundMessage {
            channel: Channel::Email,
            thread_ref: ThreadRef("t-1".into()),
            category: MessageCategory::Inquiry,
        },
        SignalKind::AgentInteraction {
            requester: requester.clone(),
            capability: "get_quote".into(),
            outcome: Outcome::Completed,
        },
        SignalKind::AbuseObserved { requester, kind: AbuseKind::InjectionAttempt },
    ]
}

#[test]
fn every_signal_kind_round_trips() {
    let names: Vec<&str> = signals().iter().map(SignalKind::name).collect();
    assert_eq!(
        names,
        vec![
            "lead_captured",
            "content_gap",
            "visitor_intent",
            "support_escalation",
            "review_seen",
            "inbound_message",
            "agent_interaction",
            "abuse_observed"
        ]
    );
    for kind in signals() {
        let signal = Signal {
            id: Uuid::nil(),
            kind: kind.clone(),
            emitted_by: "urn:adk:agent:example/concierge".into(),
            emitted_at: at(9),
        };
        let json = round_trip(&signal);
        assert_eq!(json["kind"]["type"], kind.name());
        assert_eq!(json["emittedBy"], "urn:adk:agent:example/concierge");
    }
    let json = round_trip(&signals()[0]);
    assert_eq!(json["leadRef"], "lead-1");
    assert_eq!(json["answers"]["roof_type"], json!({ "text": "tin", "untrusted": true }));
    assert_eq!(round_trip(&signals()[7])["kind"], "injection_attempt");
}

#[test]
fn bounded_enforces_its_limit_and_stays_untrusted() {
    assert_eq!(Bounded::new("abc", 3).unwrap().as_str(), "abc");
    assert_eq!(Bounded::new("abcd", 3), Err(BoundedError::TooLong { chars: 4, max: 3 }));
    assert_eq!(
        Bounded::new("a", Bounded::MAX_CHARS + 1),
        Err(BoundedError::LimitTooLarge { max: Bounded::MAX_CHARS + 1 })
    );
    // Characters, not bytes.
    assert_eq!(Bounded::new("ñandú", 5).unwrap().as_str(), "ñandú");
    assert_eq!(Bounded::truncated("ñandú", 2).as_str(), "ña");
    assert_eq!(
        Bounded::truncated(&"x".repeat(10_000), usize::MAX).as_str().len(),
        Bounded::MAX_CHARS
    );

    // No input can mark a value trusted.
    let parsed: Bounded =
        serde_json::from_value(json!({ "text": "hi", "untrusted": false })).unwrap();
    assert!(parsed.is_untrusted());
    assert_eq!(wire(&parsed), json!({ "text": "hi", "untrusted": true }));
    let parsed: Bounded = serde_json::from_value(json!({ "text": "hi" })).unwrap();
    assert_eq!(parsed.into_inner(), "hi");
    // Deserialization enforces the ceiling.
    let long = json!({ "text": "x".repeat(Bounded::MAX_CHARS + 1) });
    assert!(serde_json::from_value::<Bounded>(long).is_err());
}

#[test]
fn channels_are_open() {
    assert_eq!(wire(&Channel::Whatsapp), "whatsapp");
    assert_eq!(wire(&Channel::A2a), "a2a");
    assert_eq!(Channel::from("line"), Channel::Other("line".into()));
    assert_eq!(round_trip(&Channel::from("line")), "line");
}

// §7.8.2, §7.8.3 — content, batching, feeds

#[test]
fn content_items_round_trip() {
    let item = ContentItem {
        id: "panel-400".into(),
        kind: ContentKind::Product,
        slug: "panel-400".into(),
        language: lang("sw"),
        fields: BTreeMap::from([
            ("price".to_owned(), json!(25_000)),
            ("valid_until".to_owned(), json!("2026-12-31")),
        ]),
        volatile: vec!["stock".into()],
        body: Some(Markdown("# Paneli ya 400 W".into())),
        media: vec![MediaRef {
            path: "media/panel.webp".into(),
            content_type: "image/webp".into(),
            sha256: Some(digest(5)),
            alt: Some("A 400 W panel on a roof".into()),
            alt_confirmed: true,
            generated: false,
        }],
        sources: vec![FactRef("fact-price-panel-400".into())],
        schedule: Some(Schedule {
            publish_at: Some(at(10)),
            embargo_until: None,
            expires_at: Some(at(31)),
            revert_at: None,
        }),
        state: ContentState::Scheduled,
    };
    let json = round_trip(&item);
    assert_eq!(json["kind"], "product");
    assert_eq!(json["state"], "scheduled");
    assert_eq!(json["schedule"]["publishAt"], "2026-10-10T09:00:00Z");
    assert_eq!(json["media"][0]["altConfirmed"], true);
    assert!(json["media"][0].get("generated").is_none(), "false flags are omitted");
    assert!(item.is_volatile("stock"));

    for (kind, value) in [
        (ContentKind::Page, "page"),
        (ContentKind::Post, "post"),
        (ContentKind::Article, "article"),
        (ContentKind::Event, "event"),
        (ContentKind::Notice, "notice"),
        (ContentKind::Service, "service"),
        (ContentKind::Person, "person"),
        (ContentKind::Collection, "collection"),
    ] {
        assert_eq!(wire(&kind), value);
    }
    for (state, value) in [
        (ContentState::Draft, "draft"),
        (ContentState::InReview, "in_review"),
        (ContentState::Approved, "approved"),
        (ContentState::Published, "published"),
        (ContentState::Corrected, "corrected"),
        (ContentState::Withdrawn, "withdrawn"),
    ] {
        assert_eq!(wire(&state), value);
    }
}

#[test]
fn stream_entries_round_trip() {
    let entry = StreamEntry {
        stream: "solar-fair-2026-live".into(),
        n: 17,
        published_at: at(25),
        markdown: Markdown("The installers' panel opens in Hall B at 14:00.".into()),
        corrects: Some(16),
        proof: proof(),
    };
    let json = round_trip(&entry);
    assert_eq!(json["publishedAt"], "2026-10-25T09:00:00Z");
    assert_eq!(json["corrects"], 16);
    assert_eq!(StreamEntry::FOLD_INTERVAL, Duration::from_secs(300));
}

#[test]
fn language_tags_are_checked() {
    for good in ["en", "sw", "pt-BR", "zh-Hant-TW", "es-419"] {
        assert_eq!(round_trip(&lang(good)), good);
    }
    for bad in ["", "en_US", "-en", "en-", "toolongprimary", "1en", "en-toolongsubtag"] {
        assert!(bad.parse::<LanguageTag>().is_err(), "{bad}");
        assert!(serde_json::from_value::<LanguageTag>(json!(bad)).is_err(), "{bad}");
    }
}

#[test]
fn batch_policy_and_feed_bounds_round_trip() {
    let policy = BatchPolicy::default();
    let json = round_trip(&policy);
    assert_eq!(json["window"], 30);
    assert_eq!(json["maxItems"], 10_000);
    assert_eq!(
        json["flushNow"],
        json!(["correction", "takedown", "emergency_notice", "owner_request"])
    );
    assert!(BatchPolicy::WINDOW_RANGE.contains(&Duration::from_secs(5)));
    assert!(!BatchPolicy::WINDOW_RANGE.contains(&Duration::from_secs(301)));

    let bounds = FeedBounds {
        max_change_fraction: FeedBounds::DEFAULT_MAX_CHANGE_FRACTION,
        price_rule: PriceRule::default(),
        field_bounds: BTreeMap::from([
            (
                "price".to_owned(),
                Bound::Range {
                    minimum: None,
                    exclusive_minimum: Some(0.0),
                    maximum: None,
                    exclusive_maximum: None,
                },
            ),
            (
                "stock".to_owned(),
                Bound::Range {
                    minimum: Some(0.0),
                    exclusive_minimum: None,
                    maximum: Some(100_000.0),
                    exclusive_maximum: None,
                },
            ),
            ("starts_at".to_owned(), Bound::DateWithin { days: 365 }),
        ]),
        max_rate: Rate { per_minute: 60_000 },
    };
    let json = round_trip(&bounds);
    assert_eq!(json["fieldBounds"]["price"], json!({ "kind": "range", "exclusiveMinimum": 0.0 }));
    assert_eq!(json["fieldBounds"]["starts_at"], json!({ "kind": "date_within", "days": 365 }));
    assert_eq!(json["maxRate"]["perMinute"], 60_000);

    let stock = &bounds.field_bounds["stock"];
    assert!(stock.allows_number(100_000.0));
    assert!(!stock.allows_number(100_001.0));
    assert!(!stock.allows_number(-1.0));
    assert!(!stock.allows_date(at(1), at(1)));
    assert!(!bounds.field_bounds["starts_at"].allows_number(1.0));
}
