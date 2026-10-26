//! S8 — lead to warm handoff (spec 3.5 §5 S8, §7.10, §7.13, §7.19, §7.21, A-I8).
//!
//! A visitor asks the bakery's Concierge about a wedding cake, leaves an email with consent to
//! be contacted and to follow-ups, and answers two qualifying questions; the lead reaches the
//! back office as one typed signal; a week of nurture follows from the Revenue agent through
//! the site's email outbox, whose consent is the edge's receipts and which sends nothing in the
//! owner's quiet hours; each send rescores the lead, and once it reaches the threshold the
//! Revenue agent hands it to the owner as one card on WhatsApp.
//!
//! Pass condition: lead record with consent receipt; two nurture emails sent within the
//! playbook and quiet hours; the lead's score crosses the charter threshold; the owner gets one
//! handoff card with the conversation summary on their chosen channel.

use std::sync::Arc;

use adk_awp::v2::store::LeadStore;
use adk_core::ManualClock;
use adk_drivers::Disclosure;
use adk_drivers::Side;
use adk_drivers::email::{EmailOutbox, InMemoryEmail, SenderPolicy, StaticTxt, email_tools};
use adk_kernel::charter::{SignalFilter, SignalStore};
use adk_model::ScriptedResponse;
use adk_website::alarms::{AlarmRouter, handoff_tool_for};
use adk_website::leads::{LeadScorer, Leads, NURTURE_SCOPE, SqliteLeadRecords, nurture_tools};
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{LeadRef, SignalKind, SiteKind};
use axum::http::StatusCode;
use chrono::{TimeZone, Utc};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::edge::{EdgeOptions, edge_with};
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";
const VISITOR: &str = "wanjiru@mail.example";

/// Runs S8.
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Business, "Juniper Bakery", DOMAIN, &["en"]).await?;
    let charter = site.launch("Juniper Bakery", "KES", Vec::new()).await?;

    // The visitor talks to the Concierge on the edge, then leaves an email with consent and
    // answers the two qualifying questions: the date and the number of guests.
    let workspace = tempfile::tempdir()?;
    let concierge_model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        vec![ScriptedResponse::text(
            "Yes, we bake wedding cakes to order. When is the wedding, and for how many guests? \
             Leave your email on the contact form and the bakery will write to you.",
        )],
    ));
    let model: Arc<dyn adk_core::Llm> =
        mode.live_model().unwrap_or_else(|| concierge_model.clone());
    let factory = adk_website::edge::concierge(
        &site.team,
        &site.partitions,
        site.table.clone(),
        model,
        workspace.path(),
    );
    // The contact book the product keeps beside the edge's leads (`data/edge.db` when served).
    let records = Arc::new(SqliteLeadRecords::in_memory().await?);
    let edge = edge_with(
        &site,
        DOMAIN,
        EdgeOptions {
            public_agent: Some(factory),
            contacts: Some(records.clone()),
            ..EdgeOptions::default()
        },
    )
    .await?;
    let ask = json!({
        "jsonrpc": "2.0", "id": "v-1", "method": "SendMessage",
        "params": { "message": { "messageId": "v-1", "role": "ROLE_USER", "parts": [{ "text": "Do you bake wedding cakes?" }] } },
    });
    let (_, _, reply) = edge.send(edge.post("/awp/v2/a2a", &ask)).await;
    let answer = reply["result"]["task"]["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if mode.is_live() {
        println!("      the Concierge: {answer}");
    }
    // The enquiry form asks for consent to be contacted and, separately, to receive
    // follow-ups: the visitor grants both, and the call carries both receipts.
    let contact = edge.consent("contact").await?;
    let nurture = edge.consent(NURTURE_SCOPE).await?;
    let enquiry = json!({
        "name": "Wanjiru",
        "email": VISITOR,
        "message": "A wedding cake. Date: 14 February 2027. Guests: 120.",
    });
    let (status, _, received) = edge
        .send(edge.call_with_receipts("submit_inquiry", "lead-1", &[&contact, &nurture], &enquiry))
        .await;
    let reference = received["reference"].as_str().unwrap_or_default().to_string();
    let lead_ref = LeadRef(reference.clone());
    let lead = edge.stores.leads.get(&lead_ref).await?;
    checks.check(
        "a lead record with its consent receipts",
        status == StatusCode::OK
            && !answer.is_empty()
            && lead.as_ref().is_some_and(|l| {
                l.consent.len() == 2
                    && !serde_json::to_string(l).unwrap_or_default().contains("wanjiru@")
            }),
        format!(
            "lead {reference}, {} consent receipt(s) (contact and nurture), no address in the lead",
            lead.as_ref().map_or(0, |l| l.consent.len())
        ),
    );
    let signals = site.signals.list(&SignalFilter::default()).await?;
    let leads: Vec<_> = signals.iter().filter(|s| s.kind == "lead_captured").collect();
    checks.check(
        "the back office receives one typed lead_captured signal",
        leads.len() == 1 && matches!(&leads[0].signal.kind, SignalKind::LeadCaptured { lead_ref, .. } if lead_ref.0 == reference),
        format!("{:?}", signals.iter().map(|s| &s.kind).collect::<Vec<_>>()),
    );

    // A week of nurture: the Revenue agent with the site's email outbox, whose consent is the
    // edge's receipts, on a clock at 10:00 UTC, outside the charter's quiet hours.
    let clock = ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).single().ok_or_else(|| anyhow!("a date"))?,
    );
    let leads_desk = Arc::new(
        Leads::new(
            Arc::new(edge.stores.leads.clone()),
            records.clone(),
            Arc::new(edge.stores.consent.clone()),
            LeadScorer::default(),
        )
        .with_clock(Arc::new(clock.clone())),
    );
    let mail = Arc::new(InMemoryEmail::new());
    let txt = StaticTxt::new()
        .with(DOMAIN, "v=spf1 include:_spf.mail.example ~all")
        .with(&format!("mail._domainkey.{DOMAIN}"), "v=DKIM1; k=rsa; p=MIIBIjANBgkq")
        .with(
            &format!("_dmarc.{DOMAIN}"),
            "v=DMARC1; p=quarantine; rua=mailto:dmarc@juniper.example",
        );
    let outbox = Arc::new(
        EmailOutbox::builder(
            mail.clone(),
            adk_drivers::email::Mailbox::new(format!("hello@{DOMAIN}")),
            Disclosure::new("Juniper Bakery")?,
        )
        .consent(leads_desk.clone())
        .domain_auth(Arc::new(txt), SenderPolicy::new(DOMAIN, "mail", "include:_spf.mail.example"))
        .build(),
    );
    let quiet = adk_website::alarms::quiet_hours(&charter.reporting);
    let mut tools = nurture_tools(
        email_tools(outbox, Side::BackOffice),
        leads_desk.clone(),
        quiet,
        Arc::new(clock.clone()),
    );
    // The owner's phone, on the charter's P2 route.
    let phone = Phone::new("whatsapp");
    let router = Arc::new(
        AlarmRouter::new(site.site.id, &charter.reporting)
            .with_sink(phone.clone(), "wa-owner")
            .with_clock(Arc::new(clock.clone())),
    );
    tools.push(handoff_tool_for(router.clone(), leads_desk.clone()));

    // A send inside the owner's quiet hours goes nowhere: 23:00 UTC.
    let step = tools
        .iter()
        .find(|t| t.name() == "email_send_sequence_step")
        .ok_or_else(|| anyhow!("the sequence step"))?
        .clone();
    clock.set(
        Utc.with_ymd_and_hms(2026, 10, 19, 23, 0, 0).single().ok_or_else(|| anyhow!("a date"))?,
    );
    let night = step
        .execute(
            Arc::new(adk_tool::SimpleToolContext::new("revenue")),
            json!({ "to": VISITOR, "subject": "Your wedding cake", "text": "Hello.", "consent_scope": NURTURE_SCOPE }),
        )
        .await;
    let refused_at_night =
        night.as_ref().err().is_some_and(|e| e.to_string().contains("quiet hours"))
            && mail.sent().is_empty();
    clock.set(
        Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).single().ok_or_else(|| anyhow!("a date"))?,
    );

    // After the first week the owner raised nurture and handoff to their template's level,
    // one rung per decision (D-7); each standing rule pins the tools the agent is given.
    let compiler = site.compiler(&charter).with_tool_origins(&tools);
    for job in ["nurture", "handoff"] {
        for n in 0..4 {
            if site.ladder.level(site.site.id, job).await?
                == Some(awp_types::AutonomyLevel::ActWithinBudget)
            {
                break;
            }
            let decision = adk_kernel::charter::OwnerDecision::new(
                format!("dec-{job}-{n}"),
                adk_core::principal::SubjectRef::human(OWNER),
                chrono::Utc::now(),
            );
            adk_harness::website::promote_job(
                &site.ladder,
                &compiler,
                site.rules.as_ref(),
                job,
                &decision,
                OWNER,
            )
            .await?;
        }
    }
    let levels =
        adk_harness::website::job_levels(&compiler, site.autonomy.as_ref(), site.site.id).await?;
    let revenue_mode = compiler.mode_for_agent("revenue", &levels);
    let sends = ScriptedPrompter::new();
    let summary = "Wanjiru asked the Concierge about a wedding cake; she wants one for 120 guests \
                   on 14 February 2027 and agreed to follow-ups. Two nurture emails went out.";
    let (revenue, _) = site.back_office(
        &mode,
        "revenue",
        &charter,
        vec![
            call("email_send_sequence_step", json!({ "to": VISITOR, "subject": "Your wedding cake", "text": "Thank you for asking about a wedding cake for 14 February. We bake to order for up to 150 guests; here are our three designs.", "consent_scope": NURTURE_SCOPE })),
            call("email_send_sequence_step", json!({ "to": VISITOR, "subject": "A tasting at the bakery", "text": "Would you like a tasting this Saturday at ten? Reply with a time that suits you.", "consent_scope": NURTURE_SCOPE })),
            call("lead_handoff", json!({ "leadRef": reference, "summary": summary })),
            ScriptedResponse::text("Two nurture emails sent; the lead is with the owner."),
        ],
        |b| {
            tools.into_iter().fold(b, |b, t| b.tool(t)).mode(revenue_mode).prompter(sends.clone()).instruction(format!(
                "{}\n\nA lead_captured signal arrived: lead {reference}, Wanjiru ({VISITOR}), asks \
                 for a wedding cake on 14 February 2027 for 120 guests. She consented to be \
                 contacted and to follow-ups (scope nurture). Send the first two steps of the \
                 nurture sequence with email_send_sequence_step and consent_scope nurture: one \
                 about the cake, one inviting her to a tasting. Each send reports the lead's \
                 score; when it says readyForHandoff, hand the lead to the owner with \
                 lead_handoff, leadRef {reference}, and a short summary of the conversation.",
                AgentRole::Revenue.prompt()
            ))
        },
    )?;
    let owner = user(OWNER);
    let session = revenue.open_session(&owner, None).await?;
    let events =
        collect(revenue.send(&owner, &session, "Nurture the new wedding-cake lead.").await?)
            .await?;
    if mode.is_live() {
        trace(&events);
    }
    let sent = mail.sent();
    checks.check(
        "the nurture job's standing rules let its sends run without a hold",
        sends.held().is_empty(),
        format!("{} call(s) held for the owner", sends.held().len()),
    );
    let disclosed = sent.iter().all(|m| {
        m.text.contains("written by an AI agent") && m.text.to_lowercase().contains("unsubscribe")
    });
    checks.check(
        "two nurture emails sent within the playbook",
        sent.len() == 2 && disclosed,
        format!(
            "{} emails, each disclosing AI and offering unsubscribe, the nurture consent checked \
             against the edge's receipts",
            sent.len()
        ),
    );
    checks.check(
        "the nurture emails respect quiet hours",
        refused_at_night && sent.len() == 2,
        format!(
            "a send at 23:00 UTC inside the charter's quiet hours {quiet:?} was refused; the \
             sends went out at 10:00 UTC"
        ),
    );
    let scored = edge.stores.leads.get(&lead_ref).await?.and_then(|l| l.score);
    checks.check(
        "the lead's score crosses the charter threshold",
        scored.is_some_and(|score| score >= leads_desk.scorer().threshold()),
        format!(
            "score {scored:?} against the threshold {} the kind sets (the charter has no member for it)",
            leads_desk.scorer().threshold()
        ),
    );
    let cards = phone.messages();
    checks.check(
        "the owner gets one handoff card with the conversation summary on their chosen channel",
        cards.len() == 1
            && cards[0].contains("warm lead")
            && cards[0].contains(&reference)
            && cards[0].to_lowercase().contains("wedding"),
        format!("{cards:?}"),
    );
    Ok(checks)
}
