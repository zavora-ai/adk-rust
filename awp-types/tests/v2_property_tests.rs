//! Property tests for the AWP 2.0 types (spec 3.5 §7.4, §7.6, §7.7, §7.8.3, §7.10).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use awp_types::*;
use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use common::round_trip;
use proptest::collection::{btree_map, btree_set, vec};
use proptest::option;
use proptest::prelude::*;
use serde_json::{Value, json};
use uuid::Uuid;

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(128)
}

fn ident() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_.-]{0,15}"
}

fn text() -> impl Strategy<Value = String> {
    "[ -~]{0,40}"
}

fn time() -> impl Strategy<Value = DateTime<Utc>> {
    (0i64..4_102_444_800).prop_map(|secs| Utc.timestamp_opt(secs, 0).unwrap())
}

fn uuid() -> impl Strategy<Value = Uuid> {
    any::<u128>().prop_map(Uuid::from_u128)
}

fn digest() -> impl Strategy<Value = Digest> {
    any::<[u8; 32]>().prop_map(Digest::from_sha256_bytes)
}

/// Values that survive a JSON round trip exactly: dyadic fractions.
fn number() -> impl Strategy<Value = f64> {
    (-1_000_000_000i64..1_000_000_000).prop_map(|n| n as f64 / 8.0)
}

fn seconds() -> impl Strategy<Value = Duration> {
    (0u64..10_000_000).prop_map(Duration::from_secs)
}

fn language() -> impl Strategy<Value = LanguageTag> {
    prop_oneof![Just("en"), Just("sw"), Just("pt-BR"), Just("zh-Hant-TW"), Just("es-419")]
        .prop_map(|tag| tag.parse().unwrap())
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        text().prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..4).prop_map(Value::Array),
            btree_map(ident(), inner, 0..4).prop_map(|map| json!(map)),
        ]
    })
}

/// A JSON value other than `null`, which an `Option<Value>` field reads back as `None`.
fn schema_value() -> impl Strategy<Value = Value> {
    json_value().prop_filter("null reads back as an absent field", |value| !value.is_null())
}

fn trust_tier() -> impl Strategy<Value = TrustTier> {
    proptest::sample::select(TrustTier::ALL.to_vec())
}

fn consent_scope() -> impl Strategy<Value = ConsentScope> {
    prop_oneof![
        proptest::sample::select(ConsentScope::KNOWN.to_vec()).prop_map(ConsentScope::from),
        ident().prop_map(ConsentScope::from),
    ]
}

fn channel() -> impl Strategy<Value = Channel> {
    prop_oneof![
        proptest::sample::select(Channel::KNOWN.to_vec()).prop_map(Channel::from),
        ident().prop_map(Channel::from),
    ]
}

fn bounded() -> impl Strategy<Value = Bounded> {
    ("\\PC{0,60}", 0usize..80).prop_map(|(text, max)| Bounded::truncated(&text, max))
}

fn requester() -> impl Strategy<Value = RequesterId> {
    prop_oneof![
        (ident(), ident()).prop_map(|(origin, thumb)| RequesterId::web_bot_auth(
            &format!("https://{origin}"),
            &thumb
        )),
        ident().prop_map(|id| RequesterId::did(&format!("did:web:{id}"))),
        ident().prop_map(|id| RequesterId::api_key(&id)),
    ]
}

// §7.4

fn capability() -> impl Strategy<Value = CapabilityV2> {
    let effect = prop_oneof![
        Just(CapabilityEffect::ReadOnly),
        Just(CapabilityEffect::Idempotent),
        Just(CapabilityEffect::NonIdempotent),
    ];
    let approval = prop_oneof![
        Just(ApprovalHint::Never),
        any::<u64>().prop_map(ApprovalHint::AboveAmount),
        Just(ApprovalHint::Always),
    ];
    let idempotency = prop_oneof![
        Just(IdempotencySource::Binding),
        Just(IdempotencySource::A2aMessageId),
        Just(IdempotencySource::McpRequestId),
        Just(IdempotencySource::IdempotencyKey),
    ];
    let consent = option::of((consent_scope(), ident())).prop_map(|consent| {
        consent.map(|(scope, policy_version)| ConsentRequirement { scope, policy_version })
    });
    let price = option::of((any::<u64>(), "[A-Z]{3}", vec(ident(), 0..3)))
        .prop_map(|price| price.map(|(amount, currency, rails)| Price { amount, currency, rails }));
    let deprecated = option::of((option::of(ident()), option::of(time())))
        .prop_map(|d| d.map(|(successor, sunset)| Deprecation { successor, sunset }));
    (
        (ident(), text(), effect, trust_tier()),
        (option::of(schema_value()), option::of(schema_value()), consent, price),
        (approval, idempotency, option::of(ident()), deprecated),
    )
        .prop_map(
            |(
                (name, description, effect, min_trust),
                (input_schema, output_schema, consent, price),
                (approval, idempotency, rate_class, deprecated),
            )| CapabilityV2 {
                name,
                description,
                effect,
                min_trust,
                input_schema,
                output_schema,
                consent,
                price,
                approval,
                idempotency,
                rate_class,
                deprecated,
            },
        )
}

// §7.7, §7.8.3

fn file_entry() -> impl Strategy<Value = FileEntry> {
    (digest(), any::<u64>(), "[a-z]+/[a-z+.-]+")
        .prop_map(|(sha256, size, content_type)| FileEntry { sha256, size, content_type })
}

fn path() -> impl Strategy<Value = String> {
    "(/[a-z0-9-]{1,8}){1,3}/index\\.(html|awp\\.json|md)"
}

fn release_source() -> impl Strategy<Value = ReleaseSource> {
    prop_oneof![
        Just(ReleaseSource::Onboarding),
        Just(ReleaseSource::Migration),
        (ident(), option::of(ident()))
            .prop_map(|(job, decision)| ReleaseSource::Edit { job, decision }),
        any::<u64>().prop_map(|to_seq| ReleaseSource::Rollback { to_seq }),
    ]
}

fn release_manifest() -> impl Strategy<Value = ReleaseManifest> {
    let redirect = (path(), path(), prop_oneof![Just(301u16), Just(302), Just(308), Just(410)])
        .prop_map(|(from, to, status)| Redirect { from, to, status });
    (
        (any::<u64>(), option::of(digest()), option::of(any::<u64>()), time()),
        (btree_map(path(), file_entry(), 0..6), btree_set(path(), 0..4)),
        (vec(redirect, 0..3), release_source(), (ident(), ident()), any::<u32>()),
    )
        .prop_map(
            |(
                (seq, previous, base, created_at),
                (files, removed),
                (redirects, source, (name, version), charter_version),
            )| {
                // A snapshot removes nothing.
                let removed = if base.is_some() { removed } else { BTreeSet::new() };
                ReleaseManifest {
                    seq,
                    previous,
                    base,
                    created_at,
                    files,
                    removed,
                    redirects,
                    source,
                    generator: GeneratorRef { name, version },
                    charter_version,
                }
            },
        )
}

// §7.6

fn measurement() -> impl Strategy<Value = Measurement> {
    (number(), seconds(), time()).prop_map(|(value, window, measured_at)| Measurement {
        value,
        window,
        measured_at,
    })
}

fn kpi() -> impl Strategy<Value = Kpi> {
    let source = prop_oneof![
        Just(SourceRef::Beacon),
        Just(SourceRef::HostLogs),
        ident().prop_map(SourceRef::Analytics),
        Just(SourceRef::Commerce),
        Just(SourceRef::Spend),
        Just(SourceRef::Email),
    ];
    let direction = prop_oneof![Just(Direction::Up), Just(Direction::Down)];
    let cadence = cadence();
    (
        (ident(), text(), ident(), source, direction),
        (option::of(measurement()), option::of((number(), time())), cadence),
    )
        .prop_map(|((id, name, metric, source, direction), (baseline, target, cadence))| Kpi {
            id,
            name,
            metric: MetricRef(metric),
            source,
            direction,
            baseline,
            target: target.map(|(value, horizon)| Target { value, horizon }),
            cadence,
        })
}

fn cadence() -> impl Strategy<Value = Cadence> {
    prop_oneof![
        Just(Cadence::Daily),
        Just(Cadence::Weekly),
        Just(Cadence::Monthly),
        Just(Cadence::Quarterly),
    ]
}

fn role() -> impl Strategy<Value = Role> {
    prop_oneof![
        Just(Role::Owner),
        Just(Role::Publisher),
        Just(Role::Editor),
        Just(Role::Approver),
        Just(Role::Auditor),
    ]
}

fn governance() -> impl Strategy<Value = Governance> {
    let holder = (ident(), vec(role(), 0..3))
        .prop_map(|(name, roles)| RoleHolder { principal: format!("user:{name}"), roles });
    let action = prop_oneof![
        Just(ActionClass::Publish),
        Just(ActionClass::CharterChange),
        ident().prop_map(ActionClass::from),
    ];
    let chain =
        (action, vec(role(), 0..3)).prop_map(|(action, roles)| ApprovalChain { action, roles });
    (vec(holder, 0..4), vec(chain, 0..3))
        .prop_map(|(roles, approval_chains)| Governance { roles, approval_chains })
}

fn guardrail() -> impl Strategy<Value = Guardrail> {
    let breach = prop_oneof![Just(Breach::Above), Just(Breach::Below)];
    let action = prop_oneof![Just(GuardrailAction::PauseJob), Just(GuardrailAction::Alert)];
    (ident(), ident(), number(), breach, seconds(), action, vec(ident(), 0..3)).prop_map(
        |(id, metric, threshold, breach, window, action, jobs)| Guardrail {
            id,
            metric: MetricRef(metric),
            threshold,
            breach,
            window,
            action,
            jobs,
        },
    )
}

fn autonomy() -> impl Strategy<Value = AutonomyLevel> {
    prop_oneof![
        Just(AutonomyLevel::Observe),
        Just(AutonomyLevel::Propose),
        Just(AutonomyLevel::ActWithApproval),
        Just(AutonomyLevel::ActWithinBudget),
        Just(AutonomyLevel::ActReversibleOnly),
    ]
}

fn spend_category() -> impl Strategy<Value = SpendCategory> {
    prop_oneof![
        Just(SpendCategory::Hosting),
        Just(SpendCategory::Models),
        Just(SpendCategory::Advertising),
        Just(SpendCategory::Domains),
        Just(SpendCategory::Other),
    ]
}

fn reporting() -> impl Strategy<Value = Reporting> {
    let urgency = prop_oneof![Just(Urgency::P1), Just(Urgency::P2), Just(Urgency::P3)];
    let route = (urgency, vec(channel(), 0..3))
        .prop_map(|(urgency, channels)| UrgencyRoute { urgency, channels });
    let quiet = option::of((0u32..24, 0u32..24, ident())).prop_map(|quiet| {
        quiet.map(|(start, end, timezone)| QuietHours {
            start: NaiveTime::from_hms_opt(start, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(end, 30, 0).unwrap(),
            timezone,
        })
    });
    (cadence(), vec(route, 0..3), quiet).prop_map(|(review, routes, quiet_hours)| Reporting {
        review,
        routes,
        quiet_hours,
    })
}

fn public_terms() -> impl Strategy<Value = PublicTerms> {
    let policy = (ident(), ident(), ident(), option::of(digest()))
        .prop_map(|(name, url, version, digest)| PolicyRef { name, url, version, digest });
    let contact = (option::of(ident()), option::of(ident()), option::of(ident()))
        .prop_map(|(security, email, page)| ContactLinks { security, email, page });
    (option::of(text()), vec(policy, 0..3), vec(consent_scope(), 0..4), contact).prop_map(
        |(usage, policies, consent_scopes, contact)| PublicTerms {
            usage,
            policies,
            consent_scopes,
            contact,
        },
    )
}

fn site_kind() -> impl Strategy<Value = SiteKind> {
    proptest::sample::select(SiteKind::ALL.to_vec())
}

fn charter() -> impl Strategy<Value = Charter> {
    let pillar = (text(), option::of(text()))
        .prop_map(|(name, description)| StrategyPillar { name, description });
    let goal =
        (ident(), text(), vec(ident(), 0..3)).prop_map(|(id, name, kpis)| Goal { id, name, kpis });
    let authority = (ident(), autonomy(), option::of(any::<u64>()), option::of(any::<u32>()))
        .prop_map(|(job, level, spend_cap, actions_per_day)| JobAuthority {
            job,
            level,
            limits: JobLimits { spend_cap, actions_per_day },
        });
    let budget = (cadence(), "[A-Z]{3}", btree_map(spend_category(), any::<u64>(), 0..5))
        .prop_map(|(period, currency, caps)| Budget { period, currency, caps });
    let voice = (text(), vec(language(), 0..3), text())
        .prop_map(|(tone, languages, disclosure)| Voice { tone, languages, disclosure });
    let red_line = (
        prop_oneof![Just(RedLineKind::Claim), Just(RedLineKind::Topic), Just(RedLineKind::Action)],
        text(),
    )
        .prop_map(|(kind, text)| RedLine { kind, text });
    let signed = option::of((ident(), ident(), text(), time())).prop_map(|signed| {
        signed.map(|(decision, principal, note, signed_at)| OwnerSignature {
            decision,
            principal,
            note,
            signed_at,
        })
    });
    let price_rule =
        (0u16..10_000).prop_map(|pct| PriceRule { auto_within_pct: f32::from(pct) / 8.0 });
    (
        (any::<u32>(), uuid(), site_kind(), governance(), text()),
        (vec(pillar, 0..3), vec(goal, 0..3), vec(kpi(), 0..4), vec(guardrail(), 0..3)),
        (vec(authority, 0..3), budget, voice, vec(red_line, 0..3)),
        (reporting(), public_terms(), price_rule, signed),
    )
        .prop_map(
            |(
                (version, site_id, kind, governance, mission),
                (strategy, goals, kpis, guardrails),
                (authority, budget, voice, red_lines),
                (reporting, public, price_rule, signed),
            )| Charter {
                version,
                site_id,
                kind,
                governance,
                mission,
                strategy,
                goals,
                kpis,
                guardrails,
                authority,
                budget,
                voice,
                red_lines,
                reporting,
                public,
                price_rule,
                signed,
            },
        )
}

// §7.10

fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        Just(Outcome::Completed),
        Just(Outcome::Held),
        Just(Outcome::Refused),
        Just(Outcome::Failed),
        Just(Outcome::Abandoned),
    ]
}

fn signal_kind() -> impl Strategy<Value = SignalKind> {
    let category = prop_oneof![
        Just(MessageCategory::Inquiry),
        Just(MessageCategory::Support),
        Just(MessageCategory::Complaint),
        Just(MessageCategory::Order),
        Just(MessageCategory::Booking),
        Just(MessageCategory::Feedback),
        Just(MessageCategory::Spam),
        Just(MessageCategory::Other),
    ];
    let abuse = prop_oneof![
        Just(AbuseKind::RateAbuse),
        Just(AbuseKind::InjectionAttempt),
        Just(AbuseKind::Scraping),
        Just(AbuseKind::Spam),
        Just(AbuseKind::Fraud),
        Just(AbuseKind::InvalidCredential),
        Just(AbuseKind::Other),
    ];
    prop_oneof![
        (ident(), channel(), vec(uuid(), 0..3), btree_map(ident(), bounded(), 0..3)).prop_map(
            |(lead, source, consent, answers)| SignalKind::LeadCaptured {
                lead_ref: LeadRef(lead),
                source,
                consent: consent.into_iter().map(ConsentReceiptRef).collect(),
                answers,
            }
        ),
        (bounded(), any::<u32>(), vec(bounded(), 0..3)).prop_map(
            |(topic, question_count, samples)| SignalKind::ContentGap {
                topic,
                question_count,
                samples
            }
        ),
        (ident(), outcome())
            .prop_map(|(capability, outcome)| SignalKind::VisitorIntent { capability, outcome }),
        (ident(), ident()).prop_map(|(reason, contact)| SignalKind::SupportEscalation {
            reason: ReasonCode(reason),
            contact_ref: ContactRef(contact),
        }),
        (ident(), 1u8..=5, bounded()).prop_map(|(platform, rating, excerpt)| {
            SignalKind::ReviewSeen { platform, rating, excerpt }
        }),
        (channel(), ident(), category).prop_map(|(channel, thread, category)| {
            SignalKind::InboundMessage { channel, thread_ref: ThreadRef(thread), category }
        }),
        (requester(), ident(), outcome()).prop_map(|(requester, capability, outcome)| {
            SignalKind::AgentInteraction { requester, capability, outcome }
        }),
        (requester(), abuse)
            .prop_map(|(requester, kind)| SignalKind::AbuseObserved { requester, kind }),
    ]
}

fn signal() -> impl Strategy<Value = Signal> {
    (uuid(), signal_kind(), ident(), time()).prop_map(|(id, kind, agent, emitted_at)| Signal {
        id,
        kind,
        emitted_by: format!("urn:adk:agent:site/{agent}"),
        emitted_at,
    })
}

proptest! {
    #![proptest_config(config())]

    /// Release manifests round-trip (spec 3.5 §7.7, §11).
    #[test]
    fn release_manifests_round_trip(manifest in release_manifest()) {
        round_trip(&manifest);
        prop_assert_eq!(manifest.is_snapshot(), manifest.base.is_none());
    }

    /// A delta built from two file sets resolves to the newer set: no file lost, none
    /// duplicated, every change carried (spec 3.5 §7.8.3).
    #[test]
    fn deltas_resolve_to_the_full_release(
        base in btree_map(path(), file_entry(), 0..12),
        next in btree_map(path(), file_entry(), 0..12),
        seq in 1u64..u64::MAX,
    ) {
        let files: BTreeMap<_, _> = next
            .iter()
            .filter(|(path, entry)| base.get(*path) != Some(*entry))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect();
        let removed: BTreeSet<_> = base.keys().filter(|path| !next.contains_key(*path)).cloned().collect();
        let delta = ReleaseManifest {
            seq,
            previous: None,
            base: Some(seq - 1),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            files,
            removed,
            redirects: vec![],
            source: ReleaseSource::Edit { job: "feed".into(), decision: None },
            generator: GeneratorRef { name: "adk-site".into(), version: "3.0.0".into() },
            charter_version: 1,
        };
        let delta: ReleaseManifest = serde_json::from_value(round_trip(&delta)).unwrap();
        prop_assert_eq!(delta.apply_to(&base), next);
    }

    /// Charters round-trip (spec 3.5 §7.6).
    #[test]
    fn charters_round_trip(charter in charter()) {
        round_trip(&charter);
    }

    /// `validate` reports a target without a baseline for exactly the KPIs that have one
    /// (A-I14).
    #[test]
    fn validation_flags_every_target_without_a_baseline(charter in charter()) {
        let expected: Vec<String> = charter
            .kpis
            .iter()
            .filter(|kpi| kpi.target.is_some() && kpi.baseline.is_none())
            .map(|kpi| kpi.id.clone())
            .collect();
        let flagged: Vec<String> = match charter.validate() {
            Ok(()) => vec![],
            Err(error) => error
                .violations
                .into_iter()
                .filter_map(|violation| match violation {
                    CharterViolation::TargetWithoutBaseline { kpi } => Some(kpi),
                    _ => None,
                })
                .collect(),
        };
        prop_assert_eq!(flagged, expected);
    }

    /// A chain is satisfiable only when as many distinct people as it has roles hold them
    /// (spec 3.5 §7.6, A-I23).
    #[test]
    fn a_satisfied_chain_has_a_distinct_holder_per_role(governance in governance(), roles in vec(role(), 1..4)) {
        let chain = ApprovalChain { action: ActionClass::Publish, roles };
        let people: Vec<&str> = governance.roles.iter().map(|holder| holder.principal.as_str()).collect();
        if chain.is_satisfied_by(&governance, &people) {
            let distinct: BTreeSet<&str> = people.iter().copied().collect();
            prop_assert!(distinct.len() >= chain.roles.len());
            for role in &chain.roles {
                prop_assert!(governance.holders(*role).next().is_some());
            }
        }
    }

    /// Capabilities round-trip, and only `NonIdempotent` ones require a key (A-I7).
    #[test]
    fn capabilities_round_trip(capability in capability()) {
        round_trip(&capability);
        prop_assert_eq!(
            capability.requires_idempotency(),
            capability.effect == CapabilityEffect::NonIdempotent
        );
    }

    /// Manifest filtering never shows a capability above the caller's tier (spec 3.5 §11).
    #[test]
    fn manifests_never_show_a_capability_above_the_tier(
        capabilities in vec(capability(), 0..8),
        tier in trust_tier(),
    ) {
        let manifest = CapabilityManifestV2::new(capabilities.clone());
        let visible = manifest.visible_to(tier);
        prop_assert!(visible.capabilities.iter().all(|capability| capability.min_trust <= tier));
        prop_assert_eq!(
            visible.capabilities.len(),
            capabilities.iter().filter(|capability| capability.min_trust <= tier).count()
        );
        round_trip(&visible);
    }

    /// Signals round-trip (spec 3.5 §7.10).
    #[test]
    fn signals_round_trip(signal in signal()) {
        round_trip(&signal);
    }

    /// `Bounded` never holds more than its limit and always serializes as untrusted (A-I3).
    #[test]
    fn bounded_never_exceeds_its_limit(text in "\\PC{0,200}", max in 0usize..300) {
        let truncated = Bounded::truncated(&text, max);
        prop_assert!(truncated.as_str().chars().count() <= max);
        prop_assert!(text.starts_with(truncated.as_str()));
        match Bounded::new(text.clone(), max) {
            Ok(bounded) => prop_assert_eq!(bounded.as_str(), text.as_str()),
            Err(BoundedError::TooLong { chars, .. }) => prop_assert!(chars > max),
            Err(other) => prop_assert!(false, "unexpected error {other}"),
        }
        let json = serde_json::to_value(&truncated).unwrap();
        prop_assert_eq!(&json["untrusted"], &json!(true));
    }

    /// Digests round-trip through their string form.
    #[test]
    fn digests_round_trip(bytes in any::<[u8; 32]>()) {
        let digest = Digest::from_sha256_bytes(bytes);
        prop_assert_eq!(digest.as_str().parse::<Digest>().unwrap(), digest.clone());
        prop_assert_eq!(digest.hex().len(), 64);
        round_trip(&digest);
    }

    /// The price rule allows a change exactly when it is within its percentage (A-I25).
    #[test]
    fn price_rule_matches_its_percentage(old in 1u64..10_000_000, new in 0u64..10_000_000, pct in 0u16..200) {
        let rule = PriceRule { auto_within_pct: f32::from(pct) };
        let within = u128::from(old.abs_diff(new)) * 100 <= u128::from(pct) * u128::from(old);
        prop_assert_eq!(rule.allows(old, new), within);
    }
}
