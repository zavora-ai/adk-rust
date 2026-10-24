//! A thread links each held call to its own card when the source serves no fingerprints, as
//! a kernel's approvals inbox does (spec 3.3 §7.1, §11 S-I1): two agents holding the same call
//! get two links, a call whose arguments the route redacted for approvers still links, and a
//! call held again after its decision links to its new request.

use std::sync::Arc;

use adk_core::principal::ActingContext;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalState, Event,
    EventActions, SubjectKind, ToolConfirmationRequest,
};
use adk_surface::{Caller, Origin, Source, SurfaceEngine, SurfaceKind, ThreadItem};
use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Value, json};

/// A session with held calls, and an inbox that serves requests without fingerprints.
struct Inbox {
    events: Vec<Event>,
    requests: Vec<ApprovalRequest>,
}

#[async_trait]
impl Source for Inbox {
    fn app_name(&self) -> &str {
        "app"
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        Ok((user == "ana" && session == "s1").then(|| self.events.clone()))
    }

    async fn requests(
        &self,
        _caller: &Caller,
        session: Option<&str>,
        _open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        Ok(self
            .requests
            .iter()
            .filter(|request| session.is_none_or(|session| request.scope.session_id == session))
            .cloned()
            .collect())
    }

    async fn request(&self, id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        Ok(self.requests.iter().find(|request| request.id == id).cloned())
    }

    async fn decide(
        &self,
        id: &str,
        _version: u64,
        _decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        Err(adk_surface::SurfaceError::not_found(id))
    }
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 24, 9, 0, 0).unwrap()
}

fn acting(agent: &str) -> ActingContext {
    ActingContext::derived("app", agent, "ana", SubjectKind::Human)
}

fn call(agent: &str, tool: &str, args: Value) -> ToolConfirmationRequest {
    ToolConfirmationRequest {
        tool_name: tool.into(),
        function_call_id: Some(format!("{agent}-{tool}")),
        args,
        origin: None,
        acting: Some(Box::new(acting(agent))),
    }
}

/// The event an agent writes when it holds `call`, `seconds` after `t0`.
fn held(agent: &str, call: &ToolConfirmationRequest, seconds: i64) -> Event {
    let mut event = Event::new("inv");
    event.author = agent.into();
    event.timestamp = t0() + Duration::seconds(seconds);
    event.actions =
        EventActions { tool_confirmation: Some(call.clone()), ..EventActions::default() };
    event
}

/// The inbox entry for `call`, created `millis` after `t0`, as a kernel's inbox serves it.
fn request(
    id: &str,
    agent: &str,
    call: &ToolConfirmationRequest,
    millis: i64,
    shown: Option<Value>,
) -> ApprovalRequest {
    let mut request = ApprovalRequest::new(
        id,
        ApprovalScope::new("app", "ana", "s1"),
        call,
        ApprovalPolicy::new([]),
    )
    .with_acting(acting(agent));
    if let Some(shown) = shown {
        request = request.with_redacted_args(shown);
    }
    request.fingerprint = String::new();
    request.created_at = t0() + Duration::milliseconds(millis);
    request
}

fn links(thread: &[adk_surface::FeedEntry]) -> Vec<Option<String>> {
    thread
        .iter()
        .flat_map(|entry| &entry.items)
        .filter_map(|item| match item {
            ThreadItem::Decision { decision_id, .. } => Some(decision_id.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn each_held_call_links_to_its_own_card_without_fingerprints() {
    let write = json!({ "path": "out.txt", "content": "x" });
    let by_billing = call("billing", "write_file", write.clone());
    let by_refunds = call("refunds", "write_file", write);
    let transfer =
        call("billing", "transfer", json!({ "iban": "DE89370400440532013000", "amount": 5 }));
    let source = Inbox {
        events: vec![
            held("billing", &by_billing, 1),
            held("refunds", &by_refunds, 2),
            held("billing", &transfer, 3),
            // The same call held again in a later run, before anyone answered it.
            held("billing", &by_billing, 10),
        ],
        requests: vec![
            request("apr-billing", "billing", &by_billing, 500, None),
            request("apr-refunds", "refunds", &by_refunds, 1_500, None),
            // The route shows approvers the IBAN masked.
            request(
                "apr-transfer",
                "billing",
                &transfer,
                2_500,
                Some(json!({ "iban": "****3000", "amount": 5 })),
            ),
        ],
    };
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
    let thread = engine.thread(&me, "s1").await.unwrap();
    let id = |id: &str| Some(id.to_string());
    assert_eq!(
        links(&thread),
        [id("apr-billing"), id("apr-refunds"), id("apr-transfer"), id("apr-billing")]
    );
}

#[tokio::test]
async fn redacted_calls_of_one_agent_link_in_the_order_they_were_held() {
    let first =
        call("billing", "transfer", json!({ "iban": "DE89370400440532013000", "amount": 5 }));
    let second =
        call("billing", "transfer", json!({ "iban": "GB29NWBK60161331926819", "amount": 7 }));
    let masked = |amount: u64| Some(json!({ "iban": "****", "amount": amount }));
    let mut rejected = request("apr-first", "billing", &first, 500, masked(5));
    rejected.state = ApprovalState::Rejected;
    let source = Inbox {
        events: vec![
            // Both calls of one model turn are held before either event is written.
            held("billing", &first, 1),
            held("billing", &second, 1),
            // The first call, rejected, is made again and held under a new request.
            held("billing", &first, 30),
        ],
        requests: vec![
            request("apr-again", "billing", &first, 29_000, masked(5)),
            request("apr-second", "billing", &second, 600, masked(7)),
            rejected,
        ],
    };
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
    let thread = engine.thread(&me, "s1").await.unwrap();
    let id = |id: &str| Some(id.to_string());
    assert_eq!(links(&thread), [id("apr-first"), id("apr-second"), id("apr-again")]);
}
