//! A thread links each held call to its own card when the source serves no fingerprints, as
//! a kernel's approvals inbox does (spec 3.3 §7.1, §11 S-I1): two agents holding the same call
//! get two links, a call whose arguments the route redacted for approvers still links, a call
//! held again after its decision links to its new request, a call held again before its
//! decision keeps its request, and a call with no request of its own takes no other call's.

use std::sync::Arc;

use adk_core::principal::ActingContext;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalState, Event,
    EventActions, SubjectKind, ToolConfirmationRequest,
};
use adk_surface::{Caller, Origin, Source, SurfaceEngine, SurfaceKind, ThreadItem};
use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use proptest::prelude::*;
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
    rejected.resolved_at = Some(t0() + Duration::seconds(20));
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

#[tokio::test]
async fn a_call_without_a_request_of_its_own_takes_no_other_calls_card() {
    let large = call("billing", "transfer", json!({ "amount": 5000 }));
    let small = call("billing", "transfer", json!({ "amount": 50 }));
    let source = Inbox {
        events: vec![
            // The route leaves the large transfer held in the store only: it has no request.
            held("billing", &large, 1),
            held("billing", &small, 2),
        ],
        requests: vec![request("apr-small", "billing", &small, 1_500, None)],
    };
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
    let thread = engine.thread(&me, "s1").await.unwrap();
    assert_eq!(links(&thread), [None, Some("apr-small".to_string())]);
}

/// Wave 2 review findings 24 and 27: a call whose request is missing takes no redacted card
/// of a later call either, whichever of the two is the larger transfer.
#[tokio::test]
async fn a_call_without_a_request_takes_no_redacted_card_of_a_later_call() {
    for (unrouted, routed) in [(50, 5_000), (5_000, 50)] {
        let missing = call("billing", "transfer", json!({ "iban": "DE89", "amount": unrouted }));
        let shown = call("billing", "transfer", json!({ "iban": "GB29", "amount": routed }));
        let source = Inbox {
            events: vec![
                // Its request is not in the caller's inbox: unrouted, or for other approvers.
                held("billing", &missing, 3),
                // A later turn holds the next transfer; its route masks the IBAN.
                held("billing", &shown, 7),
            ],
            requests: vec![request(
                "apr-shown",
                "billing",
                &shown,
                6_800,
                Some(json!({ "iban": "****", "amount": routed })),
            )],
        };
        let engine = SurfaceEngine::builder(Arc::new(source)).build();
        let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
        let thread = engine.thread(&me, "s1").await.unwrap();
        assert_eq!(links(&thread), [None, Some("apr-shown".to_string())], "{unrouted}");
    }
}

/// Wave 2 review finding 25: a held call's line says "Waiting for you" only while the
/// caller's own decision is open; a call waiting on someone else waits for an answer, and a
/// call answered since reads decided.
#[tokio::test]
async fn a_held_calls_line_says_who_it_waits_for() {
    let mine = call("billing", "send_email", json!({ "to": "[client email]" }));
    let theirs = call("billing", "transfer", json!({ "amount": 5_000 }));
    let answered = call("billing", "write_file", json!({ "path": "out.txt" }));
    let mut approved = request("apr-answered", "billing", &answered, 500, None);
    approved.state = ApprovalState::Approved;
    approved.resolved_at = Some(t0() + Duration::seconds(5));
    // The approved call ran: its response follows in the session.
    let mut ran = Event::new("inv");
    ran.author = "billing".into();
    ran.timestamp = t0() + Duration::seconds(6);
    ran.llm_response.content = Some(adk_core::Content {
        role: "function".into(),
        parts: vec![adk_core::Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "write_file",
                json!({ "status": "ok" }),
            ),
            id: answered.function_call_id.clone(),
            annotations: None,
        }],
    });
    let source = Inbox {
        events: vec![
            held("billing", &answered, 1),
            ran,
            held("billing", &mine, 10),
            // Routed to finance, which the caller is not in.
            held("billing", &theirs, 20),
        ],
        requests: vec![approved, request("apr-mine", "billing", &mine, 9_800, None)],
    };
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
    let thread = engine.thread(&me, "s1").await.unwrap();
    let lines: Vec<&str> = thread
        .iter()
        .flat_map(|entry| &entry.items)
        .filter_map(|item| match item {
            ThreadItem::Decision { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(lines, ["Decided", "Waiting for you", "Waiting for an answer"]);
}

#[tokio::test]
async fn a_call_held_again_before_its_decision_keeps_its_request() {
    let first = call("billing", "write_file", json!({ "path": "out.txt", "content": "x" }));
    let next = call("billing", "write_file", json!({ "path": "b.txt", "content": "y" }));
    let masked = |redacted: bool| redacted.then(|| json!({ "path": "****" }));
    // Arguments shown link in the exact pass; redacted ones in the order pass.
    for redacted in [false, true] {
        let mut approved = request("apr-billing", "billing", &first, 500, masked(redacted));
        approved.state = ApprovalState::Approved;
        approved.resolved_at = Some(t0() + Duration::seconds(20));
        let source = Inbox {
            events: vec![
                held("billing", &first, 1),
                // Held again in a later run, before the approval at 20 s.
                held("billing", &first, 10),
                // The approved call ran; the agent then holds its next call.
                held("billing", &next, 40),
            ],
            requests: vec![
                approved,
                request("apr-billing-2", "billing", &next, 39_500, masked(redacted)),
            ],
        };
        let engine = SurfaceEngine::builder(Arc::new(source)).build();
        let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
        let thread = engine.thread(&me, "s1").await.unwrap();
        let id = |id: &str| Some(id.to_string());
        assert_eq!(
            links(&thread),
            [id("apr-billing"), id("apr-billing"), id("apr-billing-2")],
            "redacted: {redacted}"
        );
    }
}

#[tokio::test]
async fn a_call_held_again_before_its_rejection_keeps_its_request_with_fingerprints() {
    let write = call("billing", "write_file", json!({ "path": "out.txt", "content": "x" }));
    let with_fingerprint = |mut request: ApprovalRequest| {
        request.fingerprint = write.fingerprint();
        request
    };
    let mut rejected = with_fingerprint(request("apr-first", "billing", &write, 500, None));
    rejected.state = ApprovalState::Rejected;
    rejected.resolved_at = Some(t0() + Duration::seconds(20));
    let source = Inbox {
        events: vec![
            held("billing", &write, 1),
            // Held again in a later run, before the rejection at 20 s.
            held("billing", &write, 10),
            // The model makes the rejected call again: a new hold and a new request.
            held("billing", &write, 30),
        ],
        requests: vec![
            rejected,
            with_fingerprint(request("apr-again", "billing", &write, 29_000, None)),
        ],
    };
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
    let thread = engine.thread(&me, "s1").await.unwrap();
    let id = |id: &str| Some(id.to_string());
    assert_eq!(links(&thread), [id("apr-first"), id("apr-first"), id("apr-again")]);
}

/// How a held call's request reaches the caller's inbox.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shown {
    /// Unrouted, or the caller is not an approver: no request.
    Missing,
    /// The request shows the call's arguments.
    Exact,
    /// The route shows approvers redacted arguments.
    Redacted,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the order of holds, a call whose request shows its arguments links to that
    /// request, and no request links two calls.
    #[test]
    fn no_call_takes_a_request_that_shows_another_calls_arguments(
        (calls, holds) in prop::collection::vec(
            prop_oneof![Just(Shown::Missing), Just(Shown::Exact), Just(Shown::Redacted)],
            1..8,
        )
        .prop_flat_map(|calls| {
            let count = calls.len();
            let holds = (0..count)
                .chain(0..count)
                .collect::<Vec<_>>();
            (Just(calls), Just(holds).prop_shuffle(), prop::collection::vec(any::<bool>(), count))
                .prop_map(|(calls, holds, again)| {
                    // Each call is held once, and some again before anyone answers.
                    let mut seen = vec![false; calls.len()];
                    let holds = holds
                        .into_iter()
                        .filter(|&call| !std::mem::replace(&mut seen[call], true) || again[call])
                        .collect::<Vec<_>>();
                    (calls, holds)
                })
        })
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let made = (0..calls.len())
                .map(|index| call("billing", "transfer", json!({ "amount": index })))
                .collect::<Vec<_>>();
            let events = holds
                .iter()
                .enumerate()
                .map(|(at, &index)| held("billing", &made[index], at as i64 + 1))
                .collect::<Vec<_>>();
            let requests = calls
                .iter()
                .enumerate()
                .filter(|(_, shown)| **shown != Shown::Missing)
                .map(|(index, shown)| {
                    let first = holds.iter().position(|&call| call == index).unwrap() as i64;
                    let redacted = (*shown == Shown::Redacted).then(|| json!({ "amount": "****" }));
                    request(&format!("apr-{index}"), "billing", &made[index], first * 1_000 + 500, redacted)
                })
                .collect::<Vec<_>>();
            let engine = SurfaceEngine::builder(Arc::new(Inbox { events, requests })).build();
            let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
            let thread = engine.thread(&me, "s1").await.unwrap();
            let linked = links(&thread);
            let mut owner = std::collections::HashMap::new();
            for (hold, &index) in holds.iter().enumerate() {
                if calls[index] == Shown::Exact {
                    prop_assert_eq!(linked[hold].clone(), Some(format!("apr-{index}")));
                }
                if let Some(id) = &linked[hold] {
                    let first = *owner.entry(id.clone()).or_insert(index);
                    // The request and the calls it links.
                    prop_assert_eq!((id.as_str(), first), (id.as_str(), index));
                }
            }
            Ok(())
        })?;
    }
}
