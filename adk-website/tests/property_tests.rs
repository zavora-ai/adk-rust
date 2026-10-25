//! Property tests (spec 3.5 §11 "Property tests"): charter drafts never start a job above
//! Propose but onboarding and never give a KPI a target (A-I13, A-I14, §7.18); every planned
//! page serves a goal of its charter; kind detection is a deterministic function of its
//! evidence; a P1 alarm reaches the owner at every time of day, quiet hours or not.

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
