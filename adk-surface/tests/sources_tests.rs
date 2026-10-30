//! The engine over its other sources (spec 3.3 §6): a kernel reached through its control plane
//! (`ControlSource` over HTTP and over the kernel's Unix socket, with the approvals inbox read in
//! full), and a harness with no kernel, whose decided call resumes with an empty turn.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use std::collections::HashMap;
use std::sync::Mutex;

use adk_core::{ApprovalState, RunState};
use adk_kernel::control::{
    CommandCall, CommandHost, CommandInfo, ControlClient, ControlPlaneConfig,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::{
    Answer, AnswerLog, Caller, Command, CommandOutcome, ControlSource, InMemoryAnswerLog, Mode,
    Origin, Risk, Source, SurfaceEngine, SurfaceKind, ThreadItem,
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

/// Wave 2 review finding 1: over the control plane, a feed waits for the kernel's next event;
/// after the bus has reported one, a quiet kernel is not read again and again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_feed_over_the_control_plane_waits_for_the_kernels_events() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap());
    let (s, remote, _, source) =
        remote(control, || ControlClient::http(format!("http://127.0.0.1:{port}")), "quiet.txt")
            .await;
    let terminal = Caller::human(os_user(), SurfaceKind::Terminal, Origin::Keyboard);
    // Subscribed before anything happens, as a terminal's feed is.
    let _ =
        tokio::time::timeout(Duration::from_millis(200), source.changed(Duration::from_millis(10)))
            .await;
    remote
        .execute(&terminal, Command::Send { session_id: "q1".into(), text: "write it".into() })
        .await
        .unwrap();
    eventually("the process to park on its held write", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes.into_iter().find(|process| process.run.state == RunState::Waiting)
    })
    .await;
    // The bus has reported the spawn and the park; now the kernel is quiet.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let quiet = Duration::from_secs(1);
    let started = tokio::time::Instant::now();
    let mut woke = 0;
    while let Some(left) = quiet.checked_sub(started.elapsed()) {
        if tokio::time::timeout(left, source.changed(Duration::from_millis(10))).await.is_ok() {
            woke += 1;
        }
        if woke > 20 {
            break;
        }
    }
    assert!(woke <= 2, "a quiet kernel woke the feed {woke} times in a second");
    s.kernel.stop().await.unwrap();
}

/// Wave 2 review finding 11: every session read over the control plane is an attach the
/// kernel records, so a feed left open on a quiet kernel — no process running, the bus
/// connected — does not read the session every 15 seconds; it waits for the kernel's next
/// event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_feed_on_a_quiet_kernel_is_not_read_again_and_again() {
    use futures::StreamExt;

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap());
    let (s, remote, _, _) =
        remote(control, || ControlClient::http(format!("http://127.0.0.1:{port}")), "idle.txt")
            .await;
    let terminal = Caller::human(os_user(), SurfaceKind::Terminal, Origin::Keyboard);
    remote
        .execute(&terminal, Command::Send { session_id: "i1".into(), text: "write it".into() })
        .await
        .unwrap();
    eventually("the process to park on its held write", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes.into_iter().find(|process| process.run.state == RunState::Waiting)
    })
    .await;
    let attaches = || {
        s.audit
            .records()
            .iter()
            .filter(|record| {
                record.action == adk_kernel::audit::CONTROL && record.ext["action"] == "attach"
            })
            .count()
    };
    let mut feed = remote.feed(terminal.clone(), "i1".into(), None);
    // The snapshot, then the feed waits.
    tokio::time::timeout(WITHIN, feed.next()).await.unwrap().unwrap().unwrap();
    let reader = tokio::spawn(async move { while feed.next().await.is_some() {} });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = attaches();
    // Past the 15 seconds a feed waits while a process runs.
    tokio::time::sleep(Duration::from_secs(17)).await;
    let after = attaches();
    reader.abort();
    assert_eq!(after, before, "an idle feed read the session {} times", after - before);
    s.kernel.stop().await.unwrap();
}

/// Wave 2 review findings 8 and 9: "Always allow" from a surface says all its rule covers,
/// and the rule is created through the harness, which records `harness.rule.created` with
/// the surface before the rule is stored (spec 3.1 §7.5, §9.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_from_a_surface_says_what_it_covers_and_is_on_record() {
    let s =
        scenario_with(scripted("notes.txt"), ControlPlaneConfig::default().without_socket(), false)
            .await;
    s.engine
        .execute(&web(), Command::Send { session_id: "r1".into(), text: "write it".into() })
        .await
        .unwrap();
    let card = eventually("the held call's card", WITHIN, || async {
        s.engine.decisions(&web()).await.unwrap().into_iter().next()
    })
    .await;
    let always = card.allowed.option(Answer::AlwaysAllow).expect("always allow is offered");
    assert!(
        always
            .label
            .starts_with("Always allow for this folder (file writes anywhere in the workspace, "),
        "{}",
        always.label
    );
    let outcome = s
        .engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AlwaysAllow,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    let created: Vec<_> = s
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == "harness.rule.created")
        .collect();
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0].ext["surface"], "web");
    eventually("the resumed call to write the file", WITHIN, || async {
        std::fs::read_to_string(s.path.join("notes.txt")).ok()
    })
    .await;
}

/// A kernel's command host as `adk-rust up` gives it, its modes in a map: `/mode` sets the
/// session's, and "Always allow" fails once, as a kernel out of reach for a moment would.
#[derive(Default)]
struct KernelHost {
    modes: Mutex<HashMap<String, String>>,
    runs: Mutex<Vec<(String, String, String)>>,
    offers: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl CommandHost for KernelHost {
    fn commands(&self) -> Vec<CommandInfo> {
        vec![CommandInfo::new("mode", "Show or set the mode: /mode read-only|plan|ask|auto")]
    }

    fn changes(&self, name: &str, args: &str) -> bool {
        name == "mode" && !args.is_empty()
    }

    async fn run(&self, call: &CommandCall) -> adk_kernel::Result<String> {
        self.runs.lock().unwrap().push((
            call.name.clone(),
            call.args.clone(),
            call.session.clone(),
        ));
        self.modes.lock().unwrap().insert(call.session.clone(), call.args.replace('-', "_"));
        Ok(format!("mode: {}", call.args))
    }

    async fn session_mode(&self, _: &str, session: &str) -> adk_kernel::Result<Option<String>> {
        Ok(Some(self.modes.lock().unwrap().get(session).cloned().unwrap_or_else(|| "ask".into())))
    }

    async fn always_allow_offer(
        &self,
        _: &adk_core::ApprovalRequest,
    ) -> adk_kernel::Result<Option<String>> {
        if self.offers.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Err(adk_kernel::KernelError::Core(adk_core::AdkError::new(
                adk_core::ErrorComponent::Server,
                adk_core::ErrorCategory::Unavailable,
                "test.unavailable",
                "the harness is restarting",
            )));
        }
        Ok(Some("Always allow for this folder".into()))
    }
}

/// A kernel served over loopback HTTP with `host` as its command host, and the terminal's
/// engine over it, which writes its records on the kernel's log.
async fn on_a_kernel(host: Arc<KernelHost>) -> (Scenario, SurfaceEngine, Arc<ControlSource>) {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap())
        .with_commands(host);
    let (s, _, _, source) =
        remote(control, || ControlClient::http(format!("http://127.0.0.1:{port}")), "kernel.txt")
            .await;
    let terminal = SurfaceEngine::builder(source.clone())
        .audit(source.audit_trail())
        .poll_interval(Duration::from_millis(20))
        .build();
    (s, terminal, source)
}

/// Wave 3 review findings 3–5 and 11: on a kernel, the terminal sets the mode — on a thread
/// with no message yet too — through the kernel's `/mode`, and its status shows the mode the
/// kernel reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_on_a_kernel_sets_the_mode_of_a_new_thread() {
    let host = Arc::new(KernelHost::default());
    let (s, remote, _) = on_a_kernel(host.clone()).await;
    let terminal = Caller::human(os_user(), SurfaceKind::Terminal, Origin::Keyboard);
    let status = remote.status(&terminal, Some("fresh")).await.unwrap();
    assert_eq!(status.mode.map(|view| view.mode), Some(Mode::AskFirst));

    let set = Command::SetMode { session_id: "fresh".into(), mode: Mode::JustLook };
    let outcome = remote.execute(&terminal, set).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::ModeSet { .. }), "{outcome:?}");
    assert_eq!(
        *host.runs.lock().unwrap(),
        [("mode".to_string(), "read-only".to_string(), "fresh".to_string())],
        "the REPL's words, in the shown thread"
    );
    let status = remote.status(&terminal, Some("fresh")).await.unwrap();
    assert_eq!(status.mode.map(|view| view.mode), Some(Mode::JustLook));
    s.kernel.stop().await.unwrap();
}

/// Wave 3 review finding 1: a terminal on a kernel answers and changes the mode, and each
/// record — with the surface and how it was given — is on the kernel's log, as the caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_on_a_kernel_writes_its_answers_on_the_kernels_log() {
    let (s, remote, _) = on_a_kernel(Arc::new(KernelHost::default())).await;
    let me = os_user();
    let terminal = Caller::human(me.clone(), SurfaceKind::Terminal, Origin::Keyboard);
    remote
        .execute(&terminal, Command::Send { session_id: "t2".into(), text: "write it".into() })
        .await
        .unwrap();
    let card = eventually("the card over the control plane", WITHIN, || async {
        remote.session_decisions(&terminal, "t2").await.unwrap().into_iter().next()
    })
    .await;
    let outcome = remote.execute(&terminal, decide(&card.id, card.version)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    remote
        .execute(&terminal, Command::SetMode { session_id: "t2".into(), mode: Mode::Suggest })
        .await
        .unwrap();

    let of_terminal = |action: &str| -> Vec<adk_core::audit_log::AuditRecordDraft> {
        s.audit
            .records()
            .into_iter()
            .filter(|record| {
                record.action == action && record.ext.get("surface") == Some(&json!("terminal"))
            })
            .collect()
    };
    let decided = of_terminal("surface.decide");
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0].actor.on_behalf_of, Some(adk_core::principal::SubjectRef::human(me)));
    assert_eq!(decided[0].ext["origin"], json!({ "kind": "keyboard" }));
    assert_eq!(decided[0].ext["approvalId"], json!(card.id));
    assert_eq!(decided[0].ext["outcome"], "decided");
    assert_eq!(decided[0].ext["transport"], "http");
    let mode = of_terminal("surface.set_mode");
    assert_eq!(mode.len(), 1, "{mode:?}");
    assert_eq!(mode[0].ext["mode"], "plan");
    s.kernel.stop().await.unwrap();
}

/// Wave 3 review finding 10: a kernel that could not say what "Always allow" creates, for a
/// moment, is asked again; only its answer is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_is_asked_again_after_the_kernel_could_not_answer() {
    let (s, _, source) = on_a_kernel(Arc::new(KernelHost::default())).await;
    let call = adk_core::ToolConfirmationRequest {
        tool_name: "write_file".into(),
        function_call_id: Some("c-offer".into()),
        args: json!({ "path": "offer.txt" }),
        origin: None,
        acting: None,
    };
    let request = adk_core::ApprovalRequest::new(
        "apr-offer",
        adk_core::ApprovalScope::new(APP, os_user(), "t3"),
        &call,
        adk_core::ApprovalPolicy::new([adk_core::ApproverSelector::group("operator")])
            .allow_self_approval(),
    );
    let request = s.service.submit(request).await.unwrap();
    assert!(source.always_allow_offer(&request).await.is_none(), "the kernel failed once");
    let offer = source.always_allow_offer(&request).await.expect("asked again");
    assert_eq!(offer.label, "Always allow for this folder");
    s.kernel.stop().await.unwrap();
}

/// A kernel whose areas are the partition records `areas` (id and daily spending limit), or
/// only `default` with `cap` as its limit, the primary agent's target bound to `primary`, and
/// one process of the owner per entry of `spent` (area and spend today); its control plane on
/// loopback HTTP, and the web home's source in its process.
async fn spent_in_areas(
    areas: &[(&str, u64)],
    primary: Option<&str>,
    cap: Option<u64>,
    spent: &[(&str, u64)],
) -> (adk_kernel::Kernel, adk_kernel::control::ControlPlane, adk_surface::LocalSource) {
    use adk_kernel::{
        InMemoryProcessStore, KernelConfig, Partition, PartitionId, ProcessKind, ProcessSpec,
        ProcessStore, QueueClass, Quotas, SpendDelta,
    };

    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let processes = Arc::new(InMemoryProcessStore::new());
    let mut config = KernelConfig::default().with_app_name(APP).with_control(
        ControlPlaneConfig::default().without_socket().with_http("127.0.0.1:0".parse().unwrap()),
    );
    if let Some(cap) = cap {
        config = config.with_quotas(Quotas::new().with_daily_spend_cap(cap));
    }
    let mut builder = adk_kernel::Kernel::builder()
        .config(config)
        .run_store(Arc::new(adk_core::InMemoryRunStore::new()))
        .process_store(processes.clone() as Arc<dyn ProcessStore>)
        .session_service(sessions.clone());
    if let Some(area) = primary {
        let agent = adk_agent::LlmAgentBuilder::new("primary")
            .model(Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new())))
            .build()
            .unwrap();
        let runner = adk_runner::Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(sessions.clone())
            .build()
            .unwrap();
        builder = builder.partition_agent(area, APP, Arc::new(runner));
    }
    let kernel = builder.build().unwrap();
    let by = adk_core::audit_log::AuditActor::subject(adk_core::SubjectRef::human(OWNER));
    for (id, limit) in areas {
        let record =
            Partition::new(*id, *id).with_quotas(Quotas::new().with_daily_spend_cap(*limit));
        kernel.partitions().put(record, by.clone()).await.unwrap();
    }
    let today = chrono::Utc::now().date_naive();
    for (index, (area, amount)) in spent.iter().enumerate() {
        let target = if Some(*area) == primary || primary.is_none() {
            APP.to_string()
        } else {
            format!("{APP}@{area}")
        };
        let run = adk_core::NewRun::new(
            QueueClass::Interactive.as_str(),
            APP,
            OWNER,
            target,
            json!({ "text": format!("turn {index}") }),
        );
        let spec = ProcessSpec::new(ProcessKind::Interactive).with_partition(*area);
        let pid = kernel.processes().spawn(run, spec).await.unwrap().meta.pid;
        processes
            .add_spend(&pid, &PartitionId::new(*area), today, SpendDelta::new(*amount))
            .await
            .unwrap();
    }
    let plane = adk_kernel::control::ControlPlane::start(&kernel).await.unwrap();
    let local = adk_surface::LocalSource::builder(APP)
        .sessions(sessions)
        .kernel(kernel.clone(), APP)
        .build()
        .unwrap();
    (kernel, plane, local)
}

/// Spend today and the daily spending limit `source` shows, as micro-USD.
async fn shown(source: &dyn Source) -> (u64, Option<u64>) {
    let spend = source.spend(OWNER).await.unwrap().expect("a kernel reports spend");
    (spend.spent, spend.limit)
}

/// Two areas, the primary agent working in `work` (spec 3.3 §10 K3, §13 Q7): the terminal on
/// the kernel and the web home in its process both show work's spend today against work's
/// daily spending limit — neither every area's spend added up, nor the default area's limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_two_areas_the_surfaces_show_the_primary_agents_area() {
    let (kernel, plane, home) = spent_in_areas(
        &[("errands", 5_000_000), ("work", 2_000_000)],
        Some("work"),
        None,
        &[("errands", 700_000), ("work", 300_000), ("work", 150_000)],
    )
    .await;
    let terminal = ControlSource::new(
        ControlClient::http(format!("http://{}", plane.http_addr().unwrap())),
        APP,
        APP,
    );
    let work = (450_000, Some(2_000_000));
    assert_eq!((shown(&terminal).await, shown(&home).await), (work, work), "(terminal, web home)");

    // A limit the person changes while the kernel runs is the one both show.
    ControlClient::http(format!("http://{}", plane.http_addr().unwrap()))
        .set_spending_limit("work", Some(1_000_000))
        .await
        .unwrap();
    let work = (450_000, Some(1_000_000));
    assert_eq!((shown(&terminal).await, shown(&home).await), (work, work), "(terminal, web home)");
    plane.shutdown().await.unwrap();
    drop(kernel);
}

/// One area: both surfaces show every process's spend today against the kernel's daily
/// spending limit, as before areas were marked (spec 3.3 §13 Q7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_one_area_the_surfaces_show_it_as_before() {
    let (kernel, plane, home) =
        spent_in_areas(&[], None, Some(5_000_000), &[("default", 300_000), ("default", 700_000)])
            .await;
    let terminal = ControlSource::new(
        ControlClient::http(format!("http://{}", plane.http_addr().unwrap())),
        APP,
        APP,
    );
    let all = (1_000_000, Some(5_000_000));
    assert_eq!((shown(&terminal).await, shown(&home).await), (all, all), "(terminal, web home)");
    // A limit set on the source holds over the kernel's.
    let capped = terminal.with_daily_limit(2_500_000);
    assert_eq!(shown(&capped).await, (1_000_000, Some(2_500_000)));
    plane.shutdown().await.unwrap();
    drop(kernel);
}
