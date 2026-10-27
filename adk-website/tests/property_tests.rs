//! Property tests (spec 3.5 §11 "Property tests"): charter drafts never start a job above
//! Propose but onboarding and never give a KPI a target (A-I13, A-I14, §7.18); every planned
//! page serves a goal of its charter; kind detection is a deterministic function of its
//! evidence; a P1 alarm reaches the owner at every time of day, quiet hours or not; a capability
//! pack's signed content cannot change unnoticed, its JSON layout does not matter, and an
//! installed pack's version never goes down (§7.23).

use std::sync::Arc;

use adk_harness::website::SiteRef;
use adk_kernel::charter::ladder_rank;
use adk_site::migrate::FeatureKind;
use adk_website::alarms::{Alarm, AlarmRouter, Routed};
use adk_website::kinds::{CharterInputs, KindEvidence, detect, template};
use adk_website::onboarding::build::SitePlan;
use adk_website::team::SiteTeam;
use awp_types::{
    AutonomyLevel, Cadence, Channel, Goal, QuietHours, Reporting, SiteKind, Urgency, UrgencyRoute,
};
use chrono::{NaiveTime, TimeZone, Utc};
use proptest::prelude::*;

fn kind() -> impl Strategy<Value = SiteKind> {
    prop::sample::select(SiteKind::ALL.to_vec())
}

fn language() -> impl Strategy<Value = Vec<&'static str>> {
    prop::sample::subsequence(vec!["en", "sw", "fr", "ar"], 1..=3)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn drafts_start_every_job_at_observe_or_propose_and_no_kpi_has_a_target(kind in kind(), languages in language(), mission in "[A-Za-z ]{1,60}") {
        let id = uuid::Uuid::new_v4();
        let template = template(kind);
        let team = SiteTeam::new(SiteRef::new(id, "site", "https://site.example"), &template);
        let inputs = CharterInputs::new(id, "Site", "user:owner", mission, languages.iter().map(|l| l.parse().unwrap()).collect(), "USD");
        let charter = template.draft_charter(&team, &inputs);
        for authority in &charter.authority {
            if authority.job != "onboarding" {
                prop_assert!(ladder_rank(authority.level) <= ladder_rank(AutonomyLevel::Propose), "{} at {:?}", authority.job, authority.level);
            }
        }
        prop_assert!(charter.kpis.iter().all(|kpi| kpi.target.is_none() && kpi.baseline.is_none()));
        let plan = SitePlan::new(&charter, "Site");
        prop_assert!(plan.unmapped(&charter).is_empty());
        let items = plan.items(&charter, &[], &[]);
        prop_assert_eq!(items.len(), plan.pages.len() * languages.len());
    }

    #[test]
    fn every_planned_page_serves_one_of_its_charters_goals(goals in prop::collection::btree_set("[a-z]{1,10}", 1..8)) {
        let id = uuid::Uuid::new_v4();
        let template = template(SiteKind::Business);
        let team = SiteTeam::new(SiteRef::new(id, "site", "https://site.example"), &template);
        let mut charter = template.draft_charter(&team, &CharterInputs::new(id, "Site", "user:owner", "A mission.", vec!["en".parse().unwrap()], "USD"));
        charter.goals = goals.iter().map(|id| Goal { id: id.clone(), name: id.clone(), kpis: Vec::new() }).collect();
        let plan = SitePlan::new(&charter, "Site");
        prop_assert!(plan.unmapped(&charter).is_empty());
        let mut slugs: Vec<&str> = plan.pages.iter().map(|p| p.slug.as_str()).collect();
        let count = slugs.len();
        slugs.sort_unstable();
        slugs.dedup();
        prop_assert_eq!(slugs.len(), count, "one page per slug");
        prop_assert!(plan.pages.iter().any(|p| p.legal.as_deref() == Some("privacy")));
    }

    #[test]
    fn detection_is_a_deterministic_function_of_its_evidence(
        domain in prop::option::of("[a-z]{1,8}(\\.[a-z]{2,3}){1,3}"),
        features in prop::collection::vec(prop::sample::select(vec![FeatureKind::Shop, FeatureKind::Bookings, FeatureKind::ContactForm, FeatureKind::Newsletter, FeatureKind::Comments, FeatureKind::Search]), 0..4),
        markers in prop::collection::vec("[a-z-]{1,12}", 0..4),
        types in prop::collection::vec("[A-Za-z]{1,20}", 0..4),
    ) {
        let evidence = KindEvidence { domain, features, types, markers, ..KindEvidence::default() };
        let first = detect(&evidence);
        prop_assert_eq!(&first, &detect(&evidence));
        if let Some(proposal) = first {
            prop_assert!(!proposal.reasons.is_empty());
            prop_assert!(!proposal.alternatives.contains(&proposal.kind));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn a_p1_alarm_reaches_the_owner_at_every_time_of_day(minute in 0u32..(24 * 60)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let routed = runtime.block_on(async {
            let reporting = Reporting {
                review: Cadence::Weekly,
                routes: vec![UrgencyRoute { urgency: Urgency::P1, channels: vec![Channel::Sms] }],
                quiet_hours: Some(QuietHours { start: NaiveTime::from_hms_opt(21, 0, 0).unwrap(), end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(), timezone: "Africa/Nairobi".into() }),
            };
            let at = Utc.with_ymd_and_hms(2026, 10, 21, 0, 0, 0).unwrap() + chrono::Duration::minutes(i64::from(minute));
            let phone = Arc::new(Sms);
            let router = AlarmRouter::new(uuid::Uuid::nil(), &reporting)
                .with_sink(phone.clone(), "owner")
                .with_clock(Arc::new(adk_core::ManualClock::new(at)));
            router.raise(Alarm::new(uuid::Uuid::nil(), Urgency::P1, "incident", "down", at)).await
        });
        prop_assert!(matches!(routed, Routed::Sent { ref deliveries } if deliveries.len() == 1), "{:?}", routed);
    }
}

#[derive(Debug, Default)]
struct Sms;

#[async_trait::async_trait]
impl adk_kernel::channels::ChannelSink for Sms {
    fn channel(&self) -> &str {
        "sms"
    }

    async fn send_message(&self, _thread: &str, _text: &str) -> adk_kernel::Result<()> {
        Ok(())
    }

    async fn send_decision(
        &self,
        _: &str,
        _: &adk_kernel::channels::DecisionCard,
    ) -> adk_kernel::Result<()> {
        Ok(())
    }

    async fn send_stopped(
        &self,
        _: &str,
        _: &adk_kernel::channels::StopNotice,
    ) -> adk_kernel::Result<()> {
        Ok(())
    }
}

/// Capability packs as signed bundles (spec 3.5 §7.23).
mod pack_bundles {
    use std::sync::LazyLock;

    use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
    use adk_website::packs::bundle::{HandlerConfig, PackBundle, PackPublisher};
    use adk_website::packs::installed::{Admission, PackDir};
    use adk_website::packs::trust::{TrustList, TrustedKey};
    use awp_types::{CapabilityEffect, CapabilityV2, ContentKind};
    use proptest::prelude::*;
    use serde::Serialize as _;
    use serde_json::{Value, json};

    fn key() -> Ed25519DocumentSigner {
        Ed25519DocumentSigner::from_seed("menus-2026", [0x4d; 32]).unwrap()
    }

    fn trusted() -> TrustedKey {
        TrustedKey::new("menus.example", "menus-2026", key().public_key())
    }

    fn sealed(version: &str) -> Vec<u8> {
        let listing = HandlerConfig::Listing { content: ContentKind::Product };
        let bundle =
            PackBundle::new("menu", version, PackPublisher::new("menus.example"), "The menu")
                .capability(
                    CapabilityV2::new("list_menu", "Tonight's menu", CapabilityEffect::ReadOnly),
                    listing,
                )
                .capability(
                    CapabilityV2::new("list_specials", "Specials", CapabilityEffect::ReadOnly),
                    listing,
                );
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(bundle.seal(&key())).unwrap().to_json().unwrap()
    }

    static SEALED: LazyLock<Vec<u8>> = LazyLock::new(|| sealed("1.0.0"));

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn any_change_to_signed_content_is_refused(member in 0usize..5, text in "[ -~]{1,40}") {
            let trust = TrustList::new().with(trusted());
            let mut value: Value = serde_json::from_slice(&SEALED).unwrap();
            let target = match member {
                0 => &mut value["description"],
                1 => &mut value["capabilities"][0]["description"],
                2 => &mut value["capabilities"][1]["name"],
                3 => &mut value["publisher"]["name"],
                _ => &mut value["version"],
            };
            prop_assume!(target.as_str() != Some(text.as_str()));
            *target = json!(text);
            let refused = PackBundle::verify(&serde_json::to_vec(&value).unwrap(), &trust).unwrap_err();
            prop_assert_eq!(refused.code(), "tampered");
        }

        #[test]
        fn the_json_layout_does_not_change_what_was_signed(indent in 0usize..8, compact in any::<bool>()) {
            let trust = TrustList::new().with(trusted());
            let value: Value = serde_json::from_slice(&SEALED).unwrap();
            let bytes = if compact {
                serde_json::to_vec(&value).unwrap()
            } else {
                let indent = " ".repeat(indent);
                let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
                let mut serializer = serde_json::Serializer::with_formatter(Vec::new(), formatter);
                value.serialize(&mut serializer).unwrap();
                serializer.into_inner()
            };
            prop_assert!(PackBundle::verify(&bytes, &trust).is_ok());
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn an_installed_packs_version_never_goes_down(versions in prop::collection::vec((0u64..3, 0u64..3, 0u64..3), 1..6)) {
            let project = tempfile::tempdir().unwrap();
            let packs = PackDir::new(project.path());
            packs.trust_key(trusted()).unwrap();
            let mut highest: Option<semver::Version> = None;
            for (major, minor, patch) in versions {
                let offered = semver::Version::new(major, minor, patch);
                let outcome = packs.install(&sealed(&offered.to_string()));
                match &highest {
                    None => prop_assert_eq!(outcome.unwrap().admission, Admission::New),
                    Some(held) if offered < *held => {
                        prop_assert_eq!(outcome.unwrap_err().code(), "downgrade");
                    }
                    Some(held) if offered == *held => {
                        prop_assert_eq!(outcome.unwrap().admission, Admission::Unchanged);
                    }
                    Some(held) => prop_assert_eq!(
                        outcome.unwrap().admission,
                        Admission::Upgrade { from: held.to_string() }
                    ),
                }
                if highest.as_ref().is_none_or(|held| offered > *held) {
                    highest = Some(offered);
                }
                let installed = packs.load().unwrap().get("menu").unwrap().bundle().version.clone();
                prop_assert_eq!(Some(installed), highest.as_ref().map(ToString::to_string));
            }
        }
    }
}
