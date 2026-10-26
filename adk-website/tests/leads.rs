//! Leads (spec 3.5 §5 S8, §7.20, §7.21, A-I8, A-I17): the outbox's consent comes from the
//! edge's receipts, nurture is sent only outside the owner's quiet hours and warms the lead's
//! score, and the owner gets one handoff card once the score reaches the threshold.

use std::sync::Arc;

use adk_awp::v2::store::{
    ConsentReceiptStore, InMemoryConsentReceiptStore, InMemoryLeadStore, Lead, LeadStore,
};
use adk_core::ManualClock;
use adk_drivers::email::{
    EmailOutbox, InMemoryEmail, Mailbox, SenderPolicy, StaticTxt, email_tools,
};
use adk_drivers::{ConsentCheck, Disclosure, Side};
use adk_tool::SimpleToolContext;
use adk_website::alarms::{AlarmRouter, handoff_tool_for};
use adk_website::edge::handlers::{Contact, ContactBook};
use adk_website::leads::{
    LeadRecords, LeadScorer, Leads, NURTURE_SCOPE, SqliteLeadRecords, nurture_tools,
};
use awp_types::{
    Bounded, Cadence, Channel, ConsentReceipt, ConsentReceiptRef, ConsentScope, ContactRef, Digest,
    LeadRef, LegalBasis, Proof, Reporting,
};
use chrono::{NaiveTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

const DOMAIN: &str = "juniper.example";
const VISITOR: &str = "wanjiru@mail.example";

fn receipt(scope: &str) -> ConsentReceipt {
    ConsentReceipt {
        id: Uuid::new_v4(),
        subject_ref: "subj_1".into(),
        scope: ConsentScope::from(scope),
        policy_version: "1".into(),
        policy_digest: Digest::from_sha256_bytes([1; 32]),
        legal_basis: LegalBasis::ExplicitConsent,
        jurisdiction: "KE".into(),
        granted_at: Utc::now(),
        expires_at: None,
        channel: Channel::Web,
        proof: Proof::eddsa("edge", ""),
    }
}

struct Fixture {
    leads: Arc<Leads>,
    receipts: Arc<InMemoryConsentReceiptStore>,
    nurture: ConsentReceipt,
    lead: LeadRef,
    mail: Arc<InMemoryEmail>,
    outbox: Arc<EmailOutbox>,
    clock: ManualClock,
}

/// A lead captured with `contact` and `nurture` receipts, as the edge's form keeps it.
async fn fixture() -> Fixture {
    let store = Arc::new(InMemoryLeadStore::default());
    let receipts = Arc::new(InMemoryConsentReceiptStore::default());
    let records = Arc::new(SqliteLeadRecords::in_memory().await.unwrap());
    let (contact, nurture) = (receipt("contact"), receipt(NURTURE_SCOPE));
    for held in [&contact, &nurture] {
        receipts.insert(held.clone()).await.unwrap();
    }
    let contact_ref = ContactRef("contact:wanjiru".into());
    records
        .put(Contact {
            contact_ref: contact_ref.clone(),
            email: Some("Wanjiru@Mail.Example".into()),
            name: Some("Wanjiru".into()),
            consent: vec![contact.id, nurture.id],
            at: Utc::now(),
        })
        .await
        .unwrap();
    let lead = Lead {
        id: LeadRef("lead-1".into()),
        captured_at: Utc::now(),
        source: Channel::Http,
        contact_ref,
        consent: vec![ConsentReceiptRef(contact.id), ConsentReceiptRef(nurture.id)],
        answers: [(
            "message".to_string(),
            Bounded::truncated("A wedding cake. Date: 14 February 2027. Guests: 120.", 2000),
        )]
        .into_iter()
        .collect(),
        thread_ref: None,
        score: None,
    };
    store.insert(lead.clone()).await.unwrap();
    // 10:00 in Nairobi, outside the owner's quiet hours of 21:00 to 07:00.
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 19, 7, 0, 0).unwrap());
    let leads = Arc::new(
        Leads::new(store, records, receipts.clone(), LeadScorer::new(70.0))
            .with_clock(Arc::new(clock.clone())),
    );
    let mail = Arc::new(InMemoryEmail::new());
    let txt = StaticTxt::new()
        .with(DOMAIN, "v=spf1 include:_spf.mail.example ~all")
        .with(&format!("mail._domainkey.{DOMAIN}"), "v=DKIM1; k=rsa; p=MIIBIjANBgkq")
        .with(&format!("_dmarc.{DOMAIN}"), "v=DMARC1; p=quarantine; rua=mailto:d@juniper.example");
    let outbox = Arc::new(
        EmailOutbox::builder(
            mail.clone(),
            Mailbox::new(format!("hello@{DOMAIN}")),
            Disclosure::new("Juniper Bakery").unwrap(),
        )
        .consent(leads.clone())
        .domain_auth(Arc::new(txt), SenderPolicy::new(DOMAIN, "mail", "include:_spf.mail.example"))
        .build(),
    );
    Fixture { leads, receipts, nurture, lead: lead.id, mail, outbox, clock }
}

fn quiet() -> adk_kernel::QuietHours {
    adk_kernel::QuietHours::new(
        NaiveTime::from_hms_opt(21, 0, 0).unwrap(),
        NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
    )
    .with_time_zone(chrono_tz::Africa::Nairobi)
}

fn send(step: u32) -> serde_json::Value {
    json!({
        "to": VISITOR,
        "subject": format!("Your wedding cake, part {step}"),
        "text": "We bake to order for up to 150 guests.",
        "consent_scope": NURTURE_SCOPE,
    })
}

#[tokio::test]
async fn the_outbox_reads_consent_from_the_edges_receipts() {
    let f = fixture().await;
    assert!(f.leads.is_live(VISITOR, "contact").await.unwrap());
    assert!(f.leads.is_live(VISITOR, NURTURE_SCOPE).await.unwrap());
    assert!(!f.leads.is_live("someone@else.example", NURTURE_SCOPE).await.unwrap());
    let before = Utc.with_ymd_and_hms(2026, 10, 19, 6, 0, 0).unwrap();
    f.receipts.revoke(f.nurture.id, before).await.unwrap();
    assert!(
        !f.leads.is_live(VISITOR, NURTURE_SCOPE).await.unwrap(),
        "a revocation stops the sequence at once (A-I8)"
    );
}

#[tokio::test]
async fn nurture_waits_out_quiet_hours_and_warms_the_lead_to_one_handoff() {
    let f = fixture().await;
    let tools = nurture_tools(
        email_tools(f.outbox.clone(), Side::BackOffice),
        f.leads.clone(),
        Some(quiet()),
        Arc::new(f.clock.clone()),
    );
    let step = tools.iter().find(|t| t.name() == "email_send_sequence_step").unwrap().clone();
    let ctx = || Arc::new(SimpleToolContext::new("revenue"));
    let reporting = Reporting { review: Cadence::Weekly, routes: vec![], quiet_hours: None };
    let router = Arc::new(AlarmRouter::new(Uuid::nil(), &reporting));
    let handoff = handoff_tool_for(router.clone(), f.leads.clone());
    let hand_off = || {
        handoff.execute(ctx(), json!({ "leadRef": "lead-1", "summary": "A wedding cake for 120 guests on 14 February 2027; asked about a tasting." }))
    };

    // 23:00 in Nairobi: inside quiet hours, nothing is sent.
    f.clock.set(Utc.with_ymd_and_hms(2026, 10, 19, 20, 0, 0).unwrap());
    let refused = step.execute(ctx(), send(1)).await.unwrap_err();
    assert!(refused.to_string().contains("quiet hours"), "{refused}");
    assert!(f.mail.sent().is_empty());

    // The captured lead scores 60: below the threshold, so no card yet.
    f.clock.set(Utc.with_ymd_and_hms(2026, 10, 20, 7, 0, 0).unwrap());
    let early = hand_off().await.unwrap_err();
    assert!(early.to_string().contains("scores 60 of the 70"), "{early}");

    let first = step.execute(ctx(), send(1)).await.unwrap();
    assert_eq!(first["leads"]["lead-1"]["readyForHandoff"], false, "{first}");
    let second = step.execute(ctx(), send(2)).await.unwrap();
    assert_eq!(second["leads"]["lead-1"]["score"], 70.0, "{second}");
    assert_eq!(second["leads"]["lead-1"]["readyForHandoff"], true, "{second}");
    assert_eq!(f.mail.sent().len(), 2);
    assert!(f.mail.sent().iter().all(|m| m.text.contains("written by an AI agent")));

    let card = hand_off().await.unwrap();
    assert_eq!(card["handedOff"], true);
    let raised = router.raised_since(chrono::DateTime::UNIX_EPOCH).await;
    assert_eq!(raised.len(), 1);
    assert!(raised[0].summary.contains("score 70 of 70"), "{}", raised[0].summary);
    let again = hand_off().await.unwrap_err();
    assert!(again.to_string().contains("handed to the owner already"), "{again}");
    assert_eq!(router.raised_since(chrono::DateTime::UNIX_EPOCH).await.len(), 1, "one card");
    assert_eq!(f.leads.rescore(&f.lead).await.unwrap().value, 70.0);
}

#[tokio::test]
async fn the_contact_book_survives_a_restart_and_erasure_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("edge.db").display());
    let open = || async {
        let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
        let records = SqliteLeadRecords::new(pool);
        records.migrate().await.unwrap();
        records
    };
    let receipt = Uuid::new_v4();
    let records = open().await;
    let contact = Contact {
        contact_ref: ContactRef("contact:1".into()),
        email: Some("ana@example.com".into()),
        name: None,
        consent: vec![receipt],
        at: Utc::now(),
    };
    records.put(contact.clone()).await.unwrap();
    assert_eq!(records.note_nurture(&contact.contact_ref).await.unwrap(), 1);
    drop(records);
    let records = open().await;
    assert_eq!(records.by_email("ANA@example.com").await.unwrap(), std::slice::from_ref(&contact));
    assert_eq!(records.nurtured(&contact.contact_ref).await.unwrap(), 1);
    assert_eq!(records.erase(&[receipt]).await.unwrap(), 1);
    assert!(records.by_email("ana@example.com").await.unwrap().is_empty());
}
