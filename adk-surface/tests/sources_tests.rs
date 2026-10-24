//! The engine over its other sources (spec 3.3 §6): a kernel reached through its control plane
//! (`ControlSource` over HTTP and over the kernel's Unix socket, with the approvals inbox read in
//! full), and a harness with no kernel, whose decided call resumes with an empty turn.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{ApprovalState, RunState};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::{
    Answer, AnswerLog, Caller, Command, CommandOutcome, ControlSource, InMemoryAnswerLog, Origin,
    Risk, Source, SurfaceEngine, SurfaceKind, ThreadItem,
};
use common::*;
use serde_json::json;

const WITHIN: Duration = Duration::from_secs(15);

fn scripted(path: &str) -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": path, "content": "INV-1" }),
            ),
            ScriptedResponse::text("Written."),
        ],
    ))
}

fn decide(id: &str, version: u64) -> Command {
    Command::Decide {
        decision_id: id.to_string(),
        version,
        answer: Answer::AllowOnce,
        edited_args: None,
        reason: None,
        expanded: true,
    }
}

/// The user the kernel's loopback HTTP control plane authenticates callers as.
fn os_user() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

/// A remote engine over a kernel serving `control`, reached through `client`.
async fn remote(
    control: ControlPlaneConfig,
    client: impl FnOnce() -> ControlClient,
    path: &str,
) -> (Scenario, SurfaceEngine, Arc<InMemoryAnswerLog>, Arc<ControlSource>) {
    let s = scenario_with(scripted(path), control, true).await;
    let answers = Arc::new(InMemoryAnswerLog::new());
    let source = Arc::new(ControlSource::new(client(), APP, APP));
    let remote = SurfaceEngine::builder(source.clone())
        .answers(answers.clone())
        .poll_interval(Duration::from_millis(20))
        .build();
    (s, remote, answers, source)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_decides_through_the_control_plane() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap());
    let (s, remote, answers, source) =
        remote(control, || ControlClient::http(format!("http://127.0.0.1:{port}")), "terminal.txt")
            .await;
    let me = os_user();
    let terminal = Caller::human(me.clone(), SurfaceKind::Terminal, Origin::Keyboard);

    let sent = remote
        .execute(&terminal, Command::Send { session_id: "t1".into(), text: "write it".into() })
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "t1".into() });
    let card = eventually("the card over the control plane", WITHIN, || async {
        remote.session_decisions(&terminal, "t1").await.unwrap().into_iter().next()
    })
    .await;
    // The inbox beside the kernel routes serves the whole request: its risk, its agent.
    assert_eq!(card.risk, Risk::Medium);
    assert_eq!(card.agent.name, "finance");
    let offered: Vec<Answer> = card.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(offered, [Answer::AllowOnce, Answer::Edit, Answer::Deny]);
    assert_eq!(card.heading, "Write file terminal.txt");
    // The held call links to its card although the inbox serves no fingerprints.
    let thread = remote.thread(&terminal, "t1").await.unwrap();
    assert!(thread.iter().flat_map(|entry| &entry.items).any(|item| matches!(
        item,
        ThreadItem::Decision { decision_id: Some(id), .. } if *id == card.id
    )));
    assert!(remote.session_decisions(&terminal, "elsewhere").await.unwrap().is_empty());
    // The inbox query itself names the session (spec 3.3 §10 C1): a request of another
    // session never comes back from it.
    let other = adk_core::ApprovalRequest::new(
        "apr-other",
        adk_core::ApprovalScope::new(APP, me.clone(), "other"),
        &adk_core::ToolConfirmationRequest {
            tool_name: "write_file".into(),
            function_call_id: Some("c-other".into()),
            args: json!({ "path": "other.txt" }),
            origin: None,
            acting: None,
        },
        adk_core::ApprovalPolicy::new([adk_core::ApproverSelector::group("operator")])
            .allow_self_approval(),
    );
    s.service.submit(other).await.unwrap();
    let read = |session| {
        let source = source.clone();
        let terminal = terminal.clone();
        async move {
            source
                .requests(&terminal, session, true)
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(read(Some("t1")).await, [card.id.as_str()]);
    assert_eq!(read(Some("other")).await, ["apr-other"]);
    assert_eq!(read(None).await.len(), 2);

    let outcome = remote.execute(&terminal, decide(&card.id, card.version)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    // A second answer through the control plane: told who decided and how, not retried.
    let again = remote.execute(&terminal, decide(&card.id, card.version)).await.unwrap();
    let CommandOutcome::AlreadyDecided { decided, .. } = again else {
        panic!("a second answer is shown as decided: {again:?}");
    };
    assert!(decided.by_you);
    assert!(decided.text.starts_with("You said yes in the terminal at "), "{}", decided.text);
    eventually("the process to complete", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes.into_iter().find(|process| process.run.state == RunState::Completed)
    })
    .await;
    assert_eq!(std::fs::read_to_string(s.path.join("terminal.txt")).unwrap(), "INV-1");
    let stored = s.service.get(&card.id).await.unwrap().unwrap();
    assert_eq!((stored.state, stored.decisions.len()), (ApprovalState::Approved, 1));
    let answer = answers.find(&card.id).await.unwrap().unwrap();
    assert_eq!(answer.via.surface, SurfaceKind::Terminal);
    s.kernel.stop().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_local_terminal_edits_a_call_over_the_kernel_socket() {
    // A short directory: Unix socket paths are limited to about a hundred bytes.
    let home = tempfile::Builder::new().prefix("ks").tempdir_in("/tmp").unwrap();
    let socket = home.path().join("k.sock");
    let control = ControlPlaneConfig::default().with_socket(&socket);
    let path = socket.clone();
    let (s, remote, _, _) = remote(control, move || ControlClient::socket(path), "draft.txt").await;
    let terminal =
        Caller::human(os_user(), SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
    remote
        .execute(&terminal, Command::Send { session_id: "u1".into(), text: "write it".into() })
        .await
        .unwrap();
    let card = eventually("the card over the socket", WITHIN, || async {
        remote.session_decisions(&terminal, "u1").await.unwrap().into_iter().next()
    })
    .await;
    assert_eq!(card.risk, Risk::Medium);
    let edit = Command::Decide {
        decision_id: card.id.clone(),
        version: card.version,
        answer: Answer::Edit,
        edited_args: Some(json!({ "path": "final.txt", "content": "INV-2" })),
        reason: None,
        expanded: true,
    };
    let outcome = remote.execute(&terminal, edit).await.unwrap();
    let CommandOutcome::Answered { card: answered } = outcome else {
        panic!("the edit is recorded: {outcome:?}");
    };
    assert_eq!(answered.decided.unwrap().answer, adk_surface::DecidedAnswer::Edited);
    eventually("the edited call to run", WITHIN, || async {
        std::fs::read_to_string(s.path.join("final.txt")).ok()
    })
    .await;
    assert!(!s.path.join("draft.txt").exists(), "the call ran with the edited arguments only");
    s.kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_harness_without_a_kernel_resumes_the_decided_call() {
    let s =
        scenario_with(scripted("plain.txt"), ControlPlaneConfig::default().without_socket(), false)
            .await;
    let sent = s
        .engine
        .execute(&web(), Command::Send { session_id: "p1".into(), text: "write it".into() })
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "p1".into() });
    let card = eventually("the held call's card", WITHIN, || async {
        s.engine.decisions(&web()).await.unwrap().into_iter().next()
    })
    .await;
    assert!(!s.path.join("plain.txt").exists());
    let outcome = s.engine.execute(&telegram(), decide(&card.id, card.version)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    eventually("the resumed call to write the file", WITHIN, || async {
        std::fs::read_to_string(s.path.join("plain.txt")).ok()
    })
    .await;
    let thread = eventually("the turn to finish", WITHIN, || async {
        let thread = s.engine.thread(&web(), "p1").await.unwrap();
        thread
            .iter()
            .flat_map(|entry| &entry.items)
            .any(|item| matches!(item, ThreadItem::Message { text, .. } if text == "Written."))
            .then_some(thread)
    })
    .await;
    let writes = thread
        .iter()
        .flat_map(|entry| &entry.items)
        .filter(|item| matches!(item, ThreadItem::Result { tool, ok: true, .. } if tool == "write_file"))
        .count();
    assert_eq!(writes, 1);
}

/// Accepts `Authorization: Bearer s3cret` as the OS user with the `operator` scope.
struct Token;

#[async_trait::async_trait]
impl adk_server::auth_bridge::RequestContextExtractor for Token {
    async fn extract(
        &self,
        parts: &hyper::http::request::Parts,
    ) -> Result<adk_core::RequestContext, adk_server::auth_bridge::RequestContextError> {
        use adk_server::auth_bridge::RequestContextError;
        let header = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        if header != "Bearer s3cret" {
            return Err(RequestContextError::InvalidToken("wrong token".into()));
        }
        Ok(adk_core::RequestContext {
            user_id: os_user(),
            scopes: vec!["operator".into()],
            metadata: Default::default(),
        })
    }
}

/// A kernel that admits no caller without its token: the token given to the client once
/// reaches the approvals inbox too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_given_to_the_client_once_reaches_the_inbox() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap())
        .with_extractor(Arc::new(Token));
    let url = format!("http://127.0.0.1:{port}");
    let client_url = url.clone();
    let (s, remote, _, _) = remote(
        control,
        move || ControlClient::http(client_url).with_bearer_token("s3cret"),
        "token.txt",
    )
    .await;
    let terminal = Caller::human(os_user(), SurfaceKind::Terminal, Origin::Keyboard);
    remote
        .execute(&terminal, Command::Send { session_id: "k1".into(), text: "write it".into() })
        .await
        .unwrap();
    let card = eventually("the card through the token-protected inbox", WITHIN, || async {
        remote.session_decisions(&terminal, "k1").await.unwrap().into_iter().next()
    })
    .await;
    assert_eq!(card.risk, Risk::Medium);
    let outcome = remote.execute(&terminal, decide(&card.id, card.version)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    eventually("the decided call to run", WITHIN, || async {
        std::fs::read_to_string(s.path.join("token.txt")).ok()
    })
    .await;

    // Without the token the inbox refuses, as every other kernel route does.
    let anonymous = ControlSource::new(ControlClient::http(url.clone()), APP, APP);
    let error = anonymous.requests(&terminal, Some("k1"), true).await.unwrap_err();
    assert_eq!(adk_core::AdkError::from(error).category, adk_core::ErrorCategory::Unauthorized);
    // A token given to the source alone reaches the inbox, and replaces the client's.
    let inbox_only =
        ControlSource::new(ControlClient::http(url.clone()), APP, APP).with_bearer_token("s3cret");
    assert_eq!(inbox_only.requests(&terminal, Some("k1"), false).await.unwrap().len(), 1);
    let replaced =
        ControlSource::new(ControlClient::http(url).with_bearer_token("s3cret"), APP, APP)
            .with_bearer_token("stale");
    let error = replaced.requests(&terminal, Some("k1"), false).await.unwrap_err();
    assert_eq!(adk_core::AdkError::from(error).category, adk_core::ErrorCategory::Unauthorized);
    s.kernel.stop().await.unwrap();
}
