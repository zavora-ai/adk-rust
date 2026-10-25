//! Alarms by urgency and the weekly review (spec 3.5 §7.21, §7.11, §7.14, §7.20).
//!
//! P1 reaches the owner at once, inside quiet hours, and repeats until a person
//! acknowledges it; P2 waits for quiet hours to end, or for the weekly review on a
//! volunteer-run site; P3 waits for the review. The review carries KPIs against baselines
//! with trends, each agent's work, leads, experiments, incidents, promotion offers and
//! demotions with their reasons, the alarms that waited, and the owner's questions.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_core::{Clock, ManualClock};
use adk_kernel::charter::{
    CharterEvent, CharterEventSink, DecisionOutcome, InMemoryKpiStore, JobDecision, KpiSample,
    KpiStore, KpiTracker,
};
use adk_payments::wallet::{SpendCategory, WalletAlarm, WalletAlarmHook, WalletAlarmKind};
use adk_website::alarms::{Alarm, AlarmRouter, Routed};
use adk_website::incident::{InMemoryStatusBoard, IncidentPath};
use adk_website::kinds::CharterInputs;
use adk_website::review::{ReviewComposer, ReviewInputs, Trend};
use adk_website::team::AgentRole;
use adk_website::wallet::SiteOutcomes;
use awp_types::{Channel, LeadRef, QuietHours, Role, Signal, SignalKind, SiteKind, Urgency};
use chrono::{Datelike, NaiveTime, TimeZone, Utc};
use common::*;

fn nairobi_nights() -> QuietHours {
    QuietHours {
        start: NaiveTime::from_hms_opt(21, 0, 0).unwrap(),
        end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
        timezone: "Africa/Nairobi".into(),
    }
}

#[tokio::test]
async fn p1_breaks_quiet_hours_and_repeats_until_a_person_acknowledges_it() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Bread.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    inputs.p2_channels = vec![Channel::Whatsapp];
    inputs.quiet_hours = Some(nairobi_nights());
    let charter = site.template.draft_charter(&site.team, &inputs);
    // 20:00 UTC is 23:00 in Nairobi: quiet hours.
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 21, 20, 0, 0).unwrap());
    let (sms, whatsapp) = (Phone::new("sms"), Phone::new("whatsapp"));
    let router = AlarmRouter::new(site.site.id, &charter.reporting)
        .with_sink(sms.clone(), "owner-sms")
        .with_sink(whatsapp.clone(), "owner-wa")
        .with_clock(Arc::new(clock.clone()))
        .with_audit(AuditTrail::new(Arc::new(site.audit.clone())));
    let now = clock.now();

    let routed = router
        .raise(Alarm::new(
            site.site.id,
            Urgency::P1,
            "incident",
            "two files drifted; rolling back",
            now,
        ))
        .await;
    let Routed::Sent { deliveries } = routed else {
        panic!("a P1 alarm is sent at once: {routed:?}")
    };
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].channel, "sms");
    let id = deliveries[0].alarm.clone();
    assert!(router.tick().await.is_empty(), "no repeat before the interval");
    clock.advance(std::time::Duration::from_secs(16 * 60));
    let repeats = router.tick().await;
    assert_eq!(repeats.len(), 1);
    assert_eq!(repeats[0].attempt, 2, "repeated until acknowledged");
    assert!(
        router
            .acknowledge(&id, &SubjectRef::human(AgentRole::Guardian.principal(&site.site)))
            .await
            .is_err(),
        "no agent acknowledges"
    );
    router.acknowledge(&id, &SubjectRef::human(OWNER)).await.unwrap();
    clock.advance(std::time::Duration::from_secs(16 * 60));
    assert!(router.tick().await.is_empty(), "acknowledged alarms stop");

    // P2 waits for the morning; P3 waits for the review.
    let breach = CharterEvent::GuardrailBreached {
        site: site.site.id,
        guardrail: "unsubscribes".into(),
        metric: "email.unsubscribe_rate".into(),
        value: 0.04,
        threshold: 0.02,
        jobs: vec!["nurture".into()],
        at: clock.now(),
    };
    router.emit(&breach).await;
    assert!(whatsapp.messages().is_empty(), "not inside quiet hours");
    clock.set(Utc.with_ymd_and_hms(2026, 10, 22, 5, 0, 0).unwrap());
    let morning = router.tick().await;
    assert_eq!(morning.iter().map(|d| d.channel.as_str()).collect::<Vec<_>>(), ["whatsapp"]);
    assert!(
        whatsapp.messages()[0].contains("guardrail unsubscribes breached"),
        "{:?}",
        whatsapp.messages()
    );
    assert_eq!(
        router
            .raise(Alarm::new(
                site.site.id,
                Urgency::P3,
                "freshness",
                "two pages are stale",
                clock.now()
            ))
            .await,
        Routed::Review
    );

    // A payment anomaly is P1 through the wallet's hook.
    let anomaly = WalletAlarm {
        wallet: "juniper".into(),
        kind: WalletAlarmKind::Anomaly {
            category: SpendCategory::Models,
            today: 900,
            requested: 400,
            trailing_average: 300.0,
            multiple: 3.0,
        },
        raised_at: clock.now(),
    };
    WalletAlarmHook::raise(&router, &anomaly).await.unwrap();
    assert_eq!(sms.messages().len(), 3, "the anomaly reached the P1 channel");
    assert!(sms.messages()[2].contains("paused"), "{:?}", sms.messages());

    let actions = site.actions();
    assert_eq!(actions.iter().filter(|a| *a == "website.alarm.raised").count(), 4);
    assert_eq!(actions.iter().filter(|a| *a == "website.alarm.acknowledged").count(), 1);
}

#[tokio::test]
async fn a_volunteer_run_site_hears_of_p2_in_the_weekly_review() {
    let site = Site::new(SiteKind::Community, "St Mark's", "stmarks.example", &["en"]).await;
    let mut inputs = CharterInputs::new(
        site.site.id,
        "St Mark's",
        OWNER,
        "A parish.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    inputs.p2_channels = vec![Channel::Whatsapp];
    let charter = site.template.draft_charter(&site.team, &inputs);
    assert!(
        charter.reporting.routes.iter().all(|route| route.urgency != Urgency::P2),
        "weekly review only"
    );
    let whatsapp = Phone::new("whatsapp");
    let router =
        AlarmRouter::new(site.site.id, &charter.reporting).with_sink(whatsapp.clone(), "owner-wa");
    let routed = router
        .raise(Alarm::new(
            site.site.id,
            Urgency::P2,
            "failed_renewal",
            "the domain renewal failed",
            Utc::now(),
        ))
        .await;
    assert_eq!(routed, Routed::Review);
    assert!(whatsapp.messages().is_empty());
}

#[tokio::test]
async fn the_weekly_review_holds_kpis_work_leads_incidents_autonomy_alarms_and_questions() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Fresh bread every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    let today = Utc::now().date_naive();
    let week = today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()));

    // KPIs: qualified leads up on the week before; response time down (better).
    let kpis = Arc::new(InMemoryKpiStore::new());
    for day in 0..14 {
        let at = (week - chrono::Duration::days(7) + chrono::Duration::days(day))
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        let (leads, hours) = if day < 7 { (2.0, 9.0) } else { (3.0, 6.0) };
        kpis.record(KpiSample::new(site.site.id, "qualified_leads", at, leads, "commerce"))
            .await
            .unwrap();
        kpis.record(KpiSample::new(site.site.id, "response_time", at, hours, "commerce"))
            .await
            .unwrap();
    }

    // Autonomy: twenty unchanged approvals earn freshness an offer; two rejections lower seo.
    let owner = SubjectRef::human(OWNER);
    for n in 0..20 {
        site.ladder
            .record_decision(JobDecision::new(
                site.site.id,
                "freshness",
                format!("fresh-{n}"),
                DecisionOutcome::ApprovedUnchanged,
                owner.clone(),
                Utc::now(),
            ))
            .await
            .unwrap();
    }
    for n in 0..2 {
        site.ladder
            .record_decision(JobDecision::new(
                site.site.id,
                "seo",
                format!("seo-{n}"),
                DecisionOutcome::Rejected,
                owner.clone(),
                Utc::now(),
            ))
            .await
            .unwrap();
    }

    // A lead through the back office's signals; an incident; an alarm for the review.
    let sink = site.partitions.signal_sink(site.signals.clone(), site.agenda.clone());
    sink.emit(Signal {
        id: uuid::Uuid::new_v4(),
        kind: SignalKind::LeadCaptured {
            lead_ref: LeadRef("lead-1".into()),
            source: Channel::Chat,
            consent: vec![],
            answers: BTreeMap::new(),
        },
        emitted_by: AgentRole::Concierge.principal(&site.site),
        emitted_at: Utc::now(),
    })
    .await
    .unwrap();
    let alarms = Arc::new(AlarmRouter::new(site.site.id, &charter.reporting));
    alarms
        .raise(Alarm::new(
            site.site.id,
            Urgency::P3,
            "freshness",
            "two pages are stale",
            Utc::now(),
        ))
        .await;
    let incidents = Arc::new(IncidentPath::new(
        site.runtime.clone(),
        site.ladder.clone(),
        alarms.clone(),
        Arc::new(InMemoryStatusBoard::new()),
    ));
    incidents.open(Vec::new(), 1, None).await.unwrap();

    let tracker = KpiTracker::new(site.site.id, site.charters.clone(), kpis.clone());
    let composer =
        ReviewComposer::new(site.site.id, tracker, site.ladder.clone(), site.charters.clone())
            .with_outcomes(Arc::new(SiteOutcomes::new(
                site.signals.clone(),
                site.partitions.back_office().id.clone(),
            )))
            .with_incidents(incidents)
            .with_alarms(alarms.clone())
            .with_audit(AuditTrail::new(Arc::new(site.audit.clone())));
    let mut inputs = ReviewInputs::default();
    inputs.work.insert(AgentRole::Growth, vec!["Drafted an FAQ for delivery questions".into()]);
    inputs.experiments.push("Headline B: +12% sign-ups (95% interval 3% to 21%)".into());
    inputs.handed_off.push("A café ordering bread daily".into());
    inputs.questions.push("Should we deliver on Saturdays?".into());
    let review = composer.compose(week, inputs).await.unwrap();

    let leads = review.kpis.iter().find(|k| k.review.kpi == "qualified_leads").unwrap();
    assert_eq!(
        (leads.review.week, leads.review.previous_week, leads.trend),
        (Some(3.0), Some(2.0), Trend::Up)
    );
    let response = review.kpis.iter().find(|k| k.review.kpi == "response_time").unwrap();
    assert_eq!(response.trend, Trend::Up, "fewer hours is better");
    assert_eq!(review.leads, Some(1));
    assert_eq!(review.incidents.len(), 1);
    assert!(
        review.promotions.iter().any(|p| p.job == "freshness" && p.eligible),
        "{:?}",
        review.promotions
    );
    let seo =
        review.demotions.iter().find(|d| d.job == "seo").expect("the demotion and its reason");
    assert!(!seo.reason.describe().is_empty());
    assert_eq!(review.alarms.len(), 1, "the P3 alarm waited for the review");
    assert!(alarms.review_items().await.is_empty(), "the review carried it");
    assert_eq!(review.questions, ["Should we deliver on Saturdays?"]);
    assert_eq!(review.charter_version, Some(1));

    let phone = Phone::new("whatsapp");
    let document = composer.send(&review, phone.as_ref(), "owner-wa").await.unwrap();
    for part in [
        "## KPIs",
        "Qualified enquiries",
        "## Work",
        "## Experiments",
        "## Incidents",
        "## Leads",
        "## Autonomy",
        "promote freshness",
        "Lowered seo",
        "## Questions for you",
    ] {
        assert!(document.contains(part), "{part} in\n{document}");
    }
    assert_eq!(phone.messages().len(), 1);
    assert!(phone.messages()[0].contains("promotion offer"), "{:?}", phone.messages());
    assert!(site.actions().iter().any(|a| a == "website.review.sent"));
}
