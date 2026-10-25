//! Scripted runs of driver tools on the governed path (spec 3.5 §7.13, §7.10, A-I17, A-I21):
//! real `LlmAgent`s through a `Runner`, with `ScriptedLlm` models, under a mode layer that
//! allows everything, as Auto does.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, Event, Part, PolicyDecision, RunConfig, SessionId, StrictestPolicy, Tool,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest, ToolPolicy,
    ToolPolicyRequest, UserId,
};
use adk_drivers::ads::{AdsDriver, CampaignDraft, InMemoryAds, ads_tools};
use adk_drivers::dns::{DnsDriver, DnsRecord, InMemoryDns, dns_tools};
use adk_drivers::email::{
    EmailOutbox, InMemoryEmail, InboundEmail, Mailbox, SenderPolicy, StaticTxt, email_tools,
};
use adk_drivers::{
    Disclosure, DriverBudget, DriverCapPolicy, ReplyInThread, ReplyTemplates, Side,
    SuppressionScope,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "bakery";

/// The mode layer in Auto: every call allowed.
#[derive(Debug)]
struct Auto;

#[async_trait]
impl ToolPolicy for Auto {
    async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

/// A person who approves every call they are asked about.
#[derive(Debug)]
struct Approves;

#[async_trait]
impl ToolConfirmationHandler for Approves {
    async fn decide(
        &self,
        _request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        Ok(ToolConfirmationDecision::Approve)
    }
}

fn policy(budget: DriverBudget) -> Arc<dyn ToolPolicy> {
    Arc::new(StrictestPolicy::new(vec![Arc::new(Auto), Arc::new(DriverCapPolicy::new(budget))]))
}

async fn runner(agent: Arc<dyn Agent>, policy: Arc<dyn ToolPolicy>) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "owner".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions as Arc<dyn SessionService>)
        .tool_policy(policy)
        .build()
        .unwrap()
}

async fn run(runner: &Runner, content: Content, config: Option<RunConfig>) -> Vec<Event> {
    let stream = runner
        .run_with_config(
            UserId::new("owner").unwrap(),
            SessionId::new("session").unwrap(),
            content,
            config,
        )
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("the run ends");
    events.into_iter().map(|event| event.expect("no run error")).collect()
}

fn held(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| {
            event.actions.tool_confirmation.as_ref().map(|request| request.tool_name.clone())
        })
        .collect()
}

fn responses(events: &[Event]) -> Vec<(String, Value)> {
    events
        .iter()
        .filter_map(|event| event.content())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

fn agent(model: ScriptedLlm, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn Agent> {
    let mut builder = LlmAgentBuilder::new("guardian").model(Arc::new(model));
    for tool in tools {
        builder = builder.tool(tool);
    }
    Arc::new(builder.build().unwrap())
}

#[tokio::test]
async fn a_dns_change_is_held_in_auto_and_runs_once_the_owner_approves() {
    let dns = Arc::new(InMemoryDns::new().with_zone(
        "bakery.example",
        [DnsRecord::new("www.bakery.example", "A", 300, "192.0.2.1")],
    ));
    let change = json!({
        "zone": "bakery.example",
        "changes": [{"op": "replace", "name": "www.bakery.example", "type": "A", "ttl": 300, "values": ["203.0.113.7"]}]
    });
    let model = ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call("dns_list_records", json!({"zone": "bakery.example"})),
            ScriptedResponse::function_call("dns_change_records", change),
            ScriptedResponse::text("www now points at the new host."),
        ],
    );
    let runner = runner(agent(model, dns_tools(dns.clone())), policy(DriverBudget::new())).await;

    let events =
        run(&runner, Content::new("user").with_text("Point www at 203.0.113.7"), None).await;
    let names: Vec<String> = responses(&events).into_iter().map(|(name, _)| name).collect();
    assert!(names.contains(&"dns_list_records".to_string()), "the read ran: {names:?}");
    assert_eq!(
        held(&events),
        ["dns_change_records"],
        "the change is held for the owner although the mode allows it"
    );
    let www = DnsRecord::new("www.bakery.example", "A", 300, "192.0.2.1").normalized().unwrap();
    assert!(
        dns.records("bakery.example").await.unwrap().contains(&www),
        "nothing changed while held"
    );

    let approved = RunConfig::builder().tool_confirmation_handler(Arc::new(Approves)).build();
    let events = run(&runner, Content::new("user"), Some(approved)).await;
    assert!(held(&events).is_empty(), "nothing is held after the decision");
    let records = dns.records("bakery.example").await.unwrap();
    let moved = DnsRecord::new("www.bakery.example", "A", 300, "203.0.113.7").normalized().unwrap();
    assert!(
        records.contains(&moved) && !records.contains(&www),
        "the approved change ran once: {records:?}"
    );
}

#[tokio::test]
async fn a_campaign_launch_is_held_and_bids_are_held_only_beyond_the_budget() {
    let ads = Arc::new(InMemoryAds::new());
    let draft: CampaignDraft = serde_json::from_value(json!({
        "name": "Sourdough week", "adText": "Fresh sourdough", "landingPage": "https://bakery.example/",
        "dailyBudgetMinor": 2000, "bidMinor": 60, "currency": "USD"
    }))
    .unwrap();
    let campaign = ads.launch_campaign(&draft).await.unwrap();
    let model = ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_calls([
                (
                    "ads_set_bid",
                    json!({"campaign": campaign.id, "bid_minor": 80, "currency": "USD"}),
                ),
                (
                    "ads_set_bid",
                    json!({"campaign": campaign.id, "bid_minor": 400, "currency": "USD"}),
                ),
                (
                    "ads_launch_campaign",
                    json!({"name": "Cakes", "ad_text": "Cakes to order", "landing_page": "https://bakery.example/cakes",
                           "daily_budget_minor": 1000, "bid_minor": 50, "currency": "USD"}),
                ),
            ]),
            ScriptedResponse::text("Bid tuned; the rest waits for you."),
        ],
    );
    let budget = DriverBudget::new().cap("ads_set_bid", "USD", 100);
    let runner = runner(agent(model, ads_tools(ads.clone())), policy(budget)).await;
    let events = run(&runner, Content::new("user").with_text("Tune the ads"), None).await;
    let mut held = held(&events);
    held.sort();
    assert_eq!(
        held,
        ["ads_launch_campaign", "ads_set_bid"],
        "the launch and the bid beyond budget are held"
    );
    let campaigns = ads.campaigns();
    assert_eq!(campaigns.len(), 1, "no campaign launched");
    assert_eq!(campaigns[0].bid_minor, 80, "the bid within budget ran");
}

#[tokio::test]
async fn reply_in_thread_ends_with_the_disclosure_and_an_unsubscribe_stops_the_next() {
    let mail = Arc::new(InMemoryEmail::new());
    mail.deliver(InboundEmail::parse(
        b"From: Ana <ana@example.org>\r\nTo: hello@bakery.example\r\nSubject: Hours?\r\nMessage-ID: <q1@example.org>\r\n\r\nWhen do you open on Sunday?\r\n",
        chrono::Utc::now(),
    ));
    let outbox = Arc::new(
        EmailOutbox::builder(
            mail.clone(),
            Mailbox::new("hello@bakery.example"),
            Disclosure::new("Wanjiru's Bakery").unwrap(),
        )
        .domain_auth(
            Arc::new(StaticTxt::new()),
            SenderPolicy::new("bakery.example", "mail", "include:_spf.mail.example"),
        )
        .templates(
            ReplyTemplates::new().template("hours", "Hi {name}, on Sundays we open at {time}."),
        )
        .build(),
    );
    let mut tools = email_tools(outbox.clone(), Side::Public);
    tools.push(Arc::new(ReplyInThread::new([
        outbox.clone() as Arc<dyn adk_drivers::ThreadReplier>
    ])));
    let reply = json!({"thread": "email:q1@example.org", "template": "hours", "fields": {"name": "Ana", "time": "8am"}});
    let model = ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call("email_list_inbox", json!({})),
            ScriptedResponse::function_call("reply_in_thread", reply.clone()),
            ScriptedResponse::text("Answered Ana."),
            ScriptedResponse::function_call("reply_in_thread", reply),
            ScriptedResponse::text("Ana unsubscribed, so I did not write."),
        ],
    );
    let runner = runner(agent(model, tools), policy(DriverBudget::new())).await;
    let events = run(&runner, Content::new("user").with_text("Answer the inbox"), None).await;
    assert!(held(&events).is_empty(), "a reply within the playbook runs");
    let sent = mail.sent();
    assert_eq!(sent.len(), 1, "one reply");
    assert_eq!(
        sent[0].to,
        [Mailbox::new("ana@example.org").with_name("Ana")],
        "to the sender only"
    );
    assert!(
        sent[0].text.ends_with("This message was written by an AI agent for Wanjiru's Bakery."),
        "the disclosure line: {:?}",
        sent[0].text
    );

    outbox.unsubscribe("ana@example.org", SuppressionScope::All, "owner").await.unwrap();
    let events = run(&runner, Content::new("user").with_text("Answer again"), None).await;
    let (_, answer) = responses(&events)
        .into_iter()
        .find(|(name, _)| name == "reply_in_thread")
        .expect("the call was answered");
    assert!(
        answer.to_string().contains("unsubscribed or revoked"),
        "the model is told why: {answer}"
    );
    assert_eq!(mail.sent().len(), 1, "nothing more was sent");
}
