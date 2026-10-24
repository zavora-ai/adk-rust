//! Spec 3.3 WP-A scenario: a scripted harness agent hosted by a kernel holds a call; the card
//! is projected in the words of §8; the web home (over HTTP, beside the kernel's own routes)
//! and a chat channel (in process, as its adapter would) answer at once; exactly one answer is
//! recorded and the other surface is told who decided and how (S-I2); the parked process
//! wakes and the tool runs exactly once; the audit record names the origin (§11).
//!
//! The model is `ScriptedLlm`; the kernel, its durable worker, the harness, the approval
//! service, the session store and the audit trail are real.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{ApprovalState, Part, RequestContext, RunState};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::web::{CommandRequest, DecisionsResponse};
use adk_surface::{
    Answer, Caller, Command, CommandOutcome, FeedFrame, Origin, SurfaceKind, ThreadItem,
};
use axum::http::StatusCode;
use common::*;
use futures::StreamExt;
use serde_json::json;

const WITHIN: Duration = Duration::from_secs(15);

fn scripted() -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "invoice.txt", "content": "INV-1" }),
            ),
            ScriptedResponse::text("The invoice is written."),
        ],
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_surfaces_answer_one_held_call_and_it_runs_once() {
    let model = scripted();
    let s = scenario(model.clone()).await;
    let router = api(&s.kernel, &s.engine);
    let (status, health) = call(&router, "GET", "/api/kernel/health", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the kernel's routes are served beside the surface's: {health}"
    );

    // The person asks from the web home; the kernel runs the turn as a process.
    let sent = s
        .engine
        .execute(
            &web(),
            Command::Send { session_id: "s1".into(), text: "Write the October invoice".into() },
        )
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "s1".into() });

    // The call is held and the process parks on it.
    let pid = eventually("the process to park on its held write", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes
            .into_iter()
            .find(|process| process.run.state == RunState::Waiting)
            .map(|process| process.meta.pid)
    })
    .await;
    assert!(!s.path.join("invoice.txt").exists(), "held, not written");

    // The card, from the inbox entry, in the words of §8 — the same on the web and in chat.
    let (_, other) = call(&router, "GET", "/api/surface/decisions?session=other", None).await;
    assert_eq!(other["decisions"], json!([]), "one session's decisions only");
    let (status, page) = call(&router, "GET", "/api/surface/decisions?session=s1", None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let page: DecisionsResponse = serde_json::from_value(page).unwrap();
    assert_eq!(page.decisions.len(), 1);
    let card = &page.decisions[0];
    assert_eq!(card.agent.display_name, "Finance");
    assert_eq!(card.status_text, "Waiting for you");
    assert_eq!(card.heading, "Write file invoice.txt");
    assert!(card.since_text.starts_with("since "));
    assert_eq!(card.timeline.last().unwrap().text, "Waiting for you");
    assert_eq!(card.risk_text, "Medium risk");
    assert_eq!(card.reversibility_text, "Can't be undone");
    assert_eq!(card.also_shown.as_deref(), Some("Also on your phone and in Telegram"));
    let web_answers: Vec<Answer> =
        card.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(web_answers, [Answer::AllowOnce, Answer::AlwaysAllow, Answer::Edit, Answer::Deny]);
    assert!(card.allowed.options[1].label.starts_with("Always allow for this "));
    let chat_card = &s.engine.decisions(&telegram()).await.unwrap()[0];
    let chat_answers: Vec<&str> =
        chat_card.allowed.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(chat_answers, ["Allow once", "Deny"]);
    let thread = s.engine.thread(&web(), "s1").await.unwrap();
    assert!(thread.iter().flat_map(|entry| &entry.items).any(|item| matches!(
        item,
        ThreadItem::Decision { decision_id: Some(id), .. } if *id == card.id
    )));
    // The runtime's notice on the held call is not shown as the agent's words (§8).
    assert!(!thread.iter().flat_map(|entry| &entry.items).any(|item| matches!(
        item,
        ThreadItem::Message { text, .. } if text.contains("confirmation")
    )));

    // Both surfaces answer the version they saw, at once.
    let decide = json!({
        "type": "decide",
        "decisionId": card.id,
        "version": card.version,
        "answer": "allowOnce",
    });
    let body = json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": decide });
    let chat = telegram();
    let by_web = call(&router, "POST", "/api/surface/commands", Some(body));
    let by_chat = s.engine.execute(&chat, serde_json::from_value(decide.clone()).unwrap());
    let ((status, web_outcome), chat_outcome) = tokio::join!(by_web, by_chat);
    assert_eq!(status, StatusCode::OK, "{web_outcome}");
    let web_outcome: CommandOutcome = serde_json::from_value(web_outcome).unwrap();
    let chat_outcome = chat_outcome.unwrap();
    let (winner, loser) = match (&web_outcome, &chat_outcome) {
        (CommandOutcome::Answered { .. }, CommandOutcome::AlreadyDecided { decided, .. }) => {
            (web().via(), decided)
        }
        (CommandOutcome::AlreadyDecided { decided, .. }, CommandOutcome::Answered { .. }) => {
            (telegram().via(), decided)
        }
        other => panic!("exactly one answer is recorded: {other:?}"),
    };
    assert_eq!(loser.via.as_ref(), Some(&winner), "the second surface is told how it was decided");
    assert!(loser.by_you);
    assert!(
        loser.text.starts_with(&format!("You said yes {} at ", winner.phrase())),
        "{}",
        loser.text
    );

    // The parked process wakes; the tool runs exactly once.
    eventually("the woken process to complete", WITHIN, || async {
        s.runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Completed)
    })
    .await;
    assert_eq!(std::fs::read_to_string(s.path.join("invoice.txt")).unwrap(), "INV-1");
    let events = s.engine.thread(&web(), "s1").await.unwrap();
    let results = events
        .iter()
        .flat_map(|entry| &entry.items)
        .filter(|item| matches!(item, ThreadItem::Result { tool, ok: true, .. } if tool == "write_file"))
        .count();
    assert_eq!(results, 1, "write_file ran once");
    assert_eq!(model.requests().len(), 2, "one turn before the hold, one after");
    let request = s.service.get(&card.id).await.unwrap().unwrap();
    assert_eq!((request.state, request.decisions.len()), (ApprovalState::Approved, 1));
    let transcript = s
        .sessions
        .get(adk_session::GetRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: "s1".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all();
    let writes = transcript
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter(|part| {
            matches!(
                part,
                Part::FunctionResponse { function_response, .. }
                    if function_response.name == "write_file"
                        && !adk_core::is_confirmation_pending(&function_response.response)
            )
        })
        .count();
    assert_eq!(writes, 1, "one answer from the tool besides the held call's placeholder");

    // The audit record of each answer names the surface and how it was given (§11).
    let answers: Vec<_> = s
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::SURFACE_AUDIT_DECIDE)
        .collect();
    assert_eq!(answers.len(), 2, "{answers:?}");
    let decided = answers.iter().find(|record| record.ext["outcome"] == "decided").unwrap();
    assert_eq!(decided.ext["origin"], serde_json::to_value(&winner.origin).unwrap());
    assert_eq!(decided.ext["surface"], serde_json::to_value(winner.surface).unwrap());
    assert_eq!(decided.actor.on_behalf_of.as_ref().unwrap().id, OWNER);
    assert!(answers.iter().any(|record| record.ext["outcome"] == "already_decided"));

    // The phone, opening the decision now, sees who decided and how.
    let phone = Caller::human(OWNER, SurfaceKind::Phone, Origin::Touch).with_groups(["owner"]);
    let seen = s.engine.decision(&phone, &card.id).await.unwrap();
    assert_eq!(seen.status_text, "Decided");
    assert_eq!(seen.decided.unwrap().via, Some(winner));

    // The feed replays the session from a cursor without a gap.
    let mut feed = s.engine.feed(web(), "s1".into(), Some(1));
    let mut seqs = Vec::new();
    while seqs.len() < transcript.len() - 1 {
        if let FeedFrame::Entry { entry } =
            tokio::time::timeout(Duration::from_secs(5), feed.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        {
            seqs.push(entry.seq);
        }
    }
    assert_eq!(seqs, (2..=transcript.len() as u64).collect::<Vec<_>>());
    s.kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_api_checks_the_session_owner() {
    let s = scenario(scripted()).await;
    let stranger = axum::Router::new()
        .nest("/api", adk_surface::web::router(s.engine.clone()))
        .layer(axum::Extension(Some(RequestContext {
            user_id: "bo".into(),
            scopes: vec!["owner".into()],
            metadata: Default::default(),
        })));
    s.engine
        .execute(
            &web(),
            Command::Send { session_id: "s1".into(), text: "Write the October invoice".into() },
        )
        .await
        .unwrap();
    let id = eventually("the card", WITHIN, || async {
        s.engine.decisions(&web()).await.unwrap().first().map(|card| card.id.clone())
    })
    .await;
    for uri in [
        "/api/surface/sessions/s1/thread".to_string(),
        "/api/surface/sessions/s1/feed".to_string(),
        format!("/api/surface/decisions/{id}"),
    ] {
        let (status, body) = call(&stranger, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
    }
    let (_, page) = call(&stranger, "GET", "/api/surface/decisions", None).await;
    assert_eq!(page["decisions"], json!([]));
    let body = serde_json::to_value(CommandRequest {
        surface: SurfaceKind::Web,
        origin: Origin::Pointer,
        command: Command::Decide {
            decision_id: id.clone(),
            version: 1,
            answer: Answer::AllowOnce,
            edited_args: None,
            reason: None,
            expanded: true,
        },
    })
    .unwrap();
    let (status, _) = call(&stranger, "POST", "/api/surface/commands", Some(body)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(s.service.get(&id).await.unwrap().unwrap().state, ApprovalState::Pending);

    // Unauthenticated callers get nothing; chat input does not arrive over HTTP.
    let anonymous = axum::Router::new().nest("/api", adk_surface::web::router(s.engine.clone()));
    let (status, _) = call(&anonymous, "GET", "/api/surface/decisions", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let owner = api(&s.kernel, &s.engine);
    let chat = json!({ "surface": "chat", "origin": { "kind": "chat", "channel": "Telegram" }, "command": { "type": "stopAll" } });
    let (status, _) = call(&owner, "POST", "/api/surface/commands", Some(chat)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    s.kernel.stop().await.unwrap();
}
