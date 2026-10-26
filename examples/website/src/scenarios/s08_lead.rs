//! S8 — lead to warm handoff (spec 3.5 §5 S8, §7.10, §7.13, §7.19, §7.21, A-I8).
//!
//! A visitor asks the bakery's Concierge about a wedding cake, leaves an email with consent
//! and answers two qualifying questions; the lead reaches the back office as one typed signal;
//! a week of nurture follows from the Revenue agent through the site's email outbox.
//!
//! Pass condition: lead record with consent receipt; two nurture emails sent within the
//! playbook and quiet hours; the lead's score crosses the charter threshold; the owner gets one
//! handoff card with the conversation summary on their chosen channel.

use std::sync::Arc;

use adk_awp::v2::store::LeadStore;
use adk_drivers::Side;
use adk_drivers::email::{EmailOutbox, InMemoryEmail, SenderPolicy, StaticTxt, email_tools};
use adk_drivers::{Disclosure, InMemoryConsent};
use adk_kernel::charter::{SignalFilter, SignalStore};
use adk_model::ScriptedResponse;
use adk_website::team::AgentRole;
use anyhow::Result;
use awp_types::{LeadRef, SignalKind, SiteKind};
use axum::http::StatusCode;
use chrono::Timelike;
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
    let edge = edge_with(
        &site,
        DOMAIN,
        EdgeOptions { public_agent: Some(factory), ..EdgeOptions::default() },
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
    let receipt = edge.consent("contact").await?;
    let enquiry = json!({
        "name": "Wanjiru",
        "email": VISITOR,
        "message": "A wedding cake. Date: 14 February 2027. Guests: 120.",
    });
    let (status, _, received) =
        edge.send(edge.call("submit_inquiry", "lead-1", Some(&receipt), &enquiry)).await;
    let reference = received["reference"].as_str().unwrap_or_default().to_string();
    let lead = edge.stores.leads.get(&LeadRef(reference.clone())).await?;
    checks.check(
        "a lead record with its consent receipt",
        status == StatusCode::OK
            && !answer.is_empty()
            && lead.as_ref().is_some_and(|l| {
                l.consent.len() == 1
                    && !serde_json::to_string(l).unwrap_or_default().contains("wanjiru@")
            }),
        format!(
            "lead {reference}, {} consent receipt(s), no address in the lead",
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

    // A week of nurture: the Revenue agent with the site's email outbox. The product gives the
    // outbox no consent source of its own; the visitor's receipt is mirrored into it here.
    checks.gap(
        "the product wires no email outbox: `adk-drivers`' `EmailOutbox` and `email_tools` reach \
         no agent, and no adapter turns the edge's consent receipts into the outbox's \
         `ConsentCheck`; the scenario wires both",
    );
    let mail = Arc::new(InMemoryEmail::new());
    let consent = Arc::new(InMemoryConsent::new());
    consent.grant(VISITOR, "contact");
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
        .consent(consent)
        .domain_auth(Arc::new(txt), SenderPolicy::new(DOMAIN, "mail", "include:_spf.mail.example"))
        .build(),
    );
    let tools = email_tools(outbox, Side::BackOffice);
    // After the first week the owner raised nurture to its template's level, one rung per
    // decision (D-7); each standing rule pins the outbox's tools the agent is given.
    let compiler = site.compiler(&charter).with_tool_origins(&tools);
    for n in 0..4 {
        if site.ladder.level(site.site.id, "nurture").await?
            == Some(awp_types::AutonomyLevel::ActWithinBudget)
        {
            break;
        }
        let decision = adk_kernel::charter::OwnerDecision::new(
            format!("dec-nurture-{n}"),
            adk_core::principal::SubjectRef::human(OWNER),
            chrono::Utc::now(),
        );
        adk_harness::website::promote_job(
            &site.ladder,
            &compiler,
            site.rules.as_ref(),
            "nurture",
            &decision,
            OWNER,
        )
        .await?;
    }
    let levels =
        adk_harness::website::job_levels(&compiler, site.autonomy.as_ref(), site.site.id).await?;
    let revenue_mode = compiler.mode_for_agent("revenue", &levels);
    let sends = ScriptedPrompter::new();
    sends.approve_once();
    sends.approve_once();
    let (revenue, _) = site.back_office(
        &mode,
        "revenue",
        &charter,
        vec![
            call("email_send_sequence_step", json!({ "to": VISITOR, "subject": "Your wedding cake", "text": "Thank you for asking about a wedding cake for 14 February. We bake to order for up to 150 guests; here are our three designs.", "consent_scope": "contact" })),
            call("email_send_sequence_step", json!({ "to": VISITOR, "subject": "A tasting at the bakery", "text": "Would you like a tasting this Saturday at ten? Reply with a time that suits you.", "consent_scope": "contact" })),
            ScriptedResponse::text("Two nurture emails sent."),
        ],
        |b| {
            tools.into_iter().fold(b, |b, t| b.tool(t)).mode(revenue_mode).prompter(sends.clone()).instruction(format!(
                "{}\n\nA lead_captured signal arrived: Wanjiru ({VISITOR}) asks for a wedding \
                 cake on 14 February 2027 for 120 guests, with consent to be contacted (scope \
                 contact). Send the first two steps of the nurture sequence with \
                 email_send_sequence_step, one about the cake and one inviting her to a tasting.",
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
    if mode.is_live() && sent.is_empty() {
        checks.gap(
            "the enquiry form asks for `contact` consent only: no nurture or marketing scope \
             exists for a sequence, so a careful live agent declines to nurture on a contact consent",
        );
    }
    checks.check(
        "the nurture job's standing rules let its sends run without a hold",
        sends.held().is_empty(),
        format!("{} send(s) held for the owner", sends.held().len()),
    );
    let quiet = adk_website::alarms::quiet_hours(&charter.reporting);
    let now = chrono::Utc::now();
    let disclosed = sent.iter().all(|m| {
        m.text.contains("written by an AI agent") && m.text.to_lowercase().contains("unsubscribe")
    });
    checks.check(
        "two nurture emails sent within the playbook",
        sent.len() == 2 && disclosed,
        format!(
            "{} emails, each disclosing AI and offering unsubscribe, consent checked",
            sent.len()
        ),
    );
    checks.gap(format!(
        "the email outbox does not enforce quiet hours: these sends went out at {:02}:{:02} UTC \
         whatever the owner's quiet hours {quiet:?} say",
        now.hour(),
        now.minute()
    ));
    checks.check(
        "the nurture emails respect quiet hours",
        false,
        "nothing between the agent and the outbox reads the quiet hours",
    );

    // The score and the handoff: no machinery on main.
    let scored = lead
        .as_ref()
        .and_then(|l| serde_json::to_value(l).ok())
        .and_then(|v| v.get("score").cloned())
        .filter(|v| !v.is_null());
    checks.gap(
        "no lead score: the edge records `score: None` and nothing in the charter, the kernel or \
         the product scores a lead or holds a handoff threshold, so nothing calls the handoff \
         job's `lead_handoff` to send the owner a card",
    );
    checks.check(
        "the lead's score crosses the charter threshold",
        scored.is_some(),
        format!("score {scored:?}"),
    );
    checks.check(
        "the owner gets one handoff card with the conversation summary on their chosen channel",
        false,
        "no handoff card is sent",
    );
    Ok(checks)
}
