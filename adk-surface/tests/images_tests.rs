//! Images a person sends and the commands an agent runs, in the thread (spec 3.3 §5.1, §5.7):
//! a message's images reach the agent as checked bytes and come back in the thread, an image
//! that is not what it says never leaves the engine, a message with images waits for the next
//! turn rather than steering the running one, a command's or the sandbox's result reads as a
//! run with its last lines, and the roster says where each agent's commands run.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{ApprovalDecision, ApprovalRequest, Content, Event, FunctionResponseData, Part};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::{
    AgentState, Caller, Command, CommandOutcome, ImageData, LocalSource, MessageImage, Origin,
    RunStop, ShellPlace, Source, SurfaceEngine, SurfaceKind, ThreadItem,
};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::json;

const APP: &str = "home";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// The in-memory sessions; records each turn's text and images and every offer to a running
/// turn; its busy flag is set by the test.
struct Shots {
    inner: LocalSource,
    turns: Mutex<Vec<(String, Vec<MessageImage>)>>,
    offered: Mutex<Vec<String>>,
    busy: AtomicBool,
}

#[async_trait]
impl Source for Shots {
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        self.inner.transcript(user, session).await
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        self.inner.requests(caller, session, open_only).await
    }

    async fn request(&self, id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        self.inner.request(id).await
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        self.inner.decide(id, expected_version, decision).await
    }

    async fn busy(&self, _user: &str, _session: &str) -> adk_surface::Result<bool> {
        Ok(self.busy.load(Ordering::SeqCst))
    }

    async fn agents(&self, _user: &str) -> adk_surface::Result<Vec<AgentState>> {
        Ok(vec![AgentState {
            name: "code".into(),
            session_id: Some("s1".into()),
            state: adk_core::RunState::Running,
            waiting_on_decision: false,
            wake_at: None,
            suspended: false,
            subagent: None,
            shell: Some(ShellPlace::Sandbox { enforcer: Some("seatbelt".into()) }),
        }])
    }

    async fn steer(
        &self,
        _caller: &Caller,
        _session: &str,
        text: &str,
    ) -> adk_surface::Result<Option<adk_surface::Steering>> {
        self.offered.lock().unwrap().push(text.to_string());
        Ok(None)
    }

    async fn start_turn_images(
        &self,
        _caller: &Caller,
        _session: &str,
        text: &str,
        images: &[MessageImage],
    ) -> adk_surface::Result<()> {
        self.turns.lock().unwrap().push((text.to_string(), images.to_vec()));
        Ok(())
    }
}

struct Home {
    engine: SurfaceEngine,
    source: Arc<Shots>,
    sessions: Arc<dyn SessionService>,
}

async fn home() -> Home {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let inner = LocalSource::builder(APP).sessions(Arc::clone(&sessions)).build().unwrap();
    let source = Arc::new(Shots {
        inner,
        turns: Mutex::new(Vec::new()),
        offered: Mutex::new(Vec::new()),
        busy: AtomicBool::new(false),
    });
    let engine = SurfaceEngine::builder(Arc::clone(&source) as Arc<dyn Source>)
        .agent_name("code", "Code")
        .build();
    Home { engine, source, sessions }
}

fn web() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn image(mime_type: &str, bytes: &[u8]) -> ImageData {
    ImageData {
        mime_type: mime_type.into(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

fn send(text: &str, images: Vec<ImageData>) -> Command {
    Command::Send { session_id: "s1".into(), text: text.into(), images }
}

#[tokio::test]
async fn a_message_s_images_reach_the_agent_as_checked_bytes() {
    let home = home().await;
    let outcome = home
        .engine
        .execute(&web(), send("Why does CI fail like this?", vec![image("image/png", PNG)]))
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Sent { .. }));
    let turns = home.source.turns.lock().unwrap().clone();
    assert_eq!(
        turns,
        [(
            "Why does CI fail like this?".to_string(),
            vec![MessageImage { mime_type: "image/png".into(), data: PNG.to_vec() }]
        )]
    );
    // An image alone is a message; nothing at all is not.
    home.engine.execute(&web(), send(" ", vec![image("image/png", PNG)])).await.unwrap();
    let empty = home.engine.execute(&web(), send(" ", Vec::new())).await.unwrap_err();
    assert_eq!(empty.code(), "surface.invalid");
}

#[tokio::test]
async fn an_image_that_is_not_what_it_says_never_leaves_the_engine() {
    let home = home().await;
    for bad in [
        image("image/svg+xml", b"<svg onload=alert(1)>"),
        image("image/png", b"<html><script>"),
        image("image/jpeg", PNG),
    ] {
        let error = home.engine.execute(&web(), send("look", vec![bad])).await.unwrap_err();
        assert_eq!(error.code(), "surface.invalid");
    }
    let five = vec![image("image/png", PNG); adk_surface::MAX_IMAGES + 1];
    assert!(home.engine.execute(&web(), send("look", five)).await.is_err());
    assert!(home.source.turns.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_message_with_images_waits_for_the_next_turn_and_is_never_steered() {
    let home = home().await;
    home.source.busy.store(true, Ordering::SeqCst);
    let outcome = home
        .engine
        .execute(&web(), send("and this one", vec![image("image/png", PNG)]))
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Queued { .. }), "{outcome:?}");
    assert!(home.source.offered.lock().unwrap().is_empty(), "never offered to the running turn");
    // Text alone, with nothing queued before it, is still offered to the running turn.
    let other = self::home().await;
    other.source.busy.store(true, Ordering::SeqCst);
    other.engine.execute(&web(), send("also", Vec::new())).await.unwrap();
    assert_eq!(other.source.offered.lock().unwrap().as_slice(), ["also"]);
}

#[tokio::test]
async fn the_thread_shows_the_person_s_images_and_each_run() {
    let home = home().await;
    let mut said = Event::new("inv-1");
    said.author = "user".into();
    said.llm_response.content = Some(
        Content::new("user")
            .with_text("Why?")
            .with_inline_data("image/png", PNG.to_vec())
            // Never shown: it is not what it says.
            .with_inline_data("image/png", b"<svg/>".to_vec()),
    );
    home.sessions.append_event("s1", said).await.unwrap();
    let mut ran = Event::new("inv-1");
    ran.author = "code".into();
    let stdout = (1..=50).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
    ran.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![
            Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "bash",
                    json!({ "command": "cargo test", "exit_code": 101, "stdout": stdout, "stderr": "panicked", "truncated": true }),
                ),
                id: Some("call-1".into()),
                annotations: None,
            },
            Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "sandbox_exec",
                    json!({ "status": "timeout", "stderr": "execution timed out after 30s", "duration_ms": 30000 }),
                ),
                id: Some("call-2".into()),
                annotations: None,
            },
        ],
    });
    home.sessions.append_event("s1", ran).await.unwrap();

    let thread = home.engine.thread(&web(), "s1").await.unwrap();
    let ThreadItem::Message { text, images, .. } = &thread[0].items[0] else {
        panic!("{:?}", thread[0].items)
    };
    assert_eq!(text, "Why?");
    assert_eq!(images.as_slice(), [image("image/png", PNG)]);

    let ThreadItem::Run { tool, ok, text, run, .. } = &thread[1].items[0] else {
        panic!("{:?}", thread[1].items)
    };
    assert_eq!((tool.as_str(), *ok, text.as_str()), ("bash", false, "Failed"));
    assert_eq!(run.command.as_deref(), Some("cargo test"));
    assert_eq!(run.exit_code, Some(101));
    assert_eq!((run.lines, run.output.len(), run.truncated), (51, adk_surface::RUN_LINES, true));
    assert_eq!(
        run.output.last().map(|line| (line.text.as_str(), line.stderr)),
        Some(("panicked", true))
    );
    assert_eq!(run.output[0].text, "line 12");

    let ThreadItem::Run { ok, run, .. } = &thread[1].items[1] else {
        panic!("{:?}", thread[1].items)
    };
    assert!(!ok);
    assert_eq!(
        (run.stopped, run.duration_ms, run.command.as_deref()),
        (Some(RunStop::Timeout), Some(30_000), None)
    );
}

#[tokio::test]
async fn the_roster_says_where_an_agent_s_commands_run() {
    let home = home().await;
    let rows = home.engine.agents(&web()).await.unwrap();
    let shell = rows[0].shell.as_ref().expect("the source says");
    assert_eq!(shell.place, ShellPlace::Sandbox { enforcer: Some("seatbelt".into()) });
    assert_eq!(shell.text, "Commands run in the sandbox (Seatbelt)");
    assert!(shell.place.isolated());
    let degraded =
        ShellPlace::Degraded { requested: "os_sandbox".into(), reason: "no bubblewrap".into() };
    assert!(!degraded.isolated());
    assert_eq!(
        adk_surface::words::commands_run(&degraded),
        "The sandbox is unavailable, so commands run on this computer: no bubblewrap"
    );
}
