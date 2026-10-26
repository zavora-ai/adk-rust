//! The voice view's routes and socket (spec 3.3 §5.6, §6, §11 S-I8): models, single-use
//! tickets, the socket's hello, a spoken "allow once" over the socket, frames only while their
//! switch is on, and the page's policy naming the socket's host. On `origin/main` none of
//! these routes exist.

#![cfg(all(feature = "voice", feature = "web"))]

mod voice_common;

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::RequestContext;
use adk_realtime::audio::{AudioChunk, AudioFormat};
use adk_realtime::config::RealtimeConfig;
use adk_realtime::session::{BoxedSession, ContextMutationOutcome, RealtimeSession};
use adk_realtime::{ClientEvent, RealtimeModel, ServerEvent, ToolResponse};
use adk_surface::voice::{FRAME_AUDIO, FRAME_CAMERA, VoiceTicket, VoiceUpdate};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures::{SinkExt, Stream, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;
use voice_common::*;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn context(user: &str) -> axum::Extension<Option<RequestContext>> {
    axum::Extension(Some(RequestContext {
        user_id: user.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata: Default::default(),
    }))
}

/// The voice routes under `/api` for `api_user` (none: unauthenticated), the socket and the
/// page at the root, as `adk-rust up` mounts them; `socket_user` authenticates the socket's
/// request too, when set.
fn app(voice: &Voice, api_user: Option<&str>, socket_user: Option<&str>) -> Router {
    let mut api = Router::new().nest(
        "/api",
        adk_surface::voice::router(voice.hub.clone())
            .merge(adk_surface::web::router(voice.engine.clone())),
    );
    if let Some(user) = api_user {
        api = api.layer(context(user));
    }
    let mut root = adk_surface::voice::socket_routes(voice.hub.clone());
    if let Some(user) = socket_user {
        root = root.layer(context(user));
    }
    api.merge(root).merge(adk_surface::web::home())
}

struct Server {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(app: Router) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { addr, task }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn ticket(app: &Router, body: Value) -> (StatusCode, Value) {
    call(app, "POST", "/api/surface/voice/sessions", Some(body)).await
}

async fn open(server: &Server) -> Socket {
    let (socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{}/voice/socket", server.addr))
            .await
            .unwrap();
    socket
}

async fn hello(socket: &mut Socket, ticket: &str) {
    let hello = json!({ "type": "hello", "ticket": ticket }).to_string();
    socket.send(Message::Text(hello.into())).await.unwrap();
}

/// The close code the server ends `socket` with.
async fn closed_with(socket: &mut Socket) -> u16 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, socket.next()).await.expect("the socket closes") {
            Some(Ok(Message::Close(Some(frame)))) => return u16::from(frame.code),
            Some(Ok(_)) => {}
            other => panic!("the socket ended without a close frame: {other:?}"),
        }
    }
}

/// The next update `pick` accepts on `socket`.
async fn update(
    socket: &mut Socket,
    what: &str,
    pick: impl Fn(&VoiceUpdate) -> bool,
) -> VoiceUpdate {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|| panic!("the socket ended before {what}"))
            .unwrap();
        if let Message::Text(text) = message {
            let update: VoiceUpdate = serde_json::from_str(text.as_str()).unwrap();
            if pick(&update) {
                return update;
            }
        }
    }
}

async fn audio(socket: &mut Socket) {
    let mut frame = vec![FRAME_AUDIO];
    frame.extend_from_slice(&[0; 640]);
    socket.send(Message::Binary(frame.into())).await.unwrap();
}

#[tokio::test]
async fn the_models_are_listed_to_a_caller_and_tickets_need_one() {
    let voice = voice(Vec::new()).await;
    let app = app(&voice, Some(OWNER), None);
    let (status, models) = call(&app, "GET", "/api/surface/voice", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        models,
        json!({
            "agent": "Chief of staff",
            "agentName": "chief_of_staff",
            "models": [{ "id": "scripted", "name": "Scripted", "inputRate": 16000, "outputRate": 24000 }],
        })
    );
    let (status, problem) = ticket(&app, json!({ "model": "unheard-of" })).await;
    assert_eq!(
        (status, &problem["error"]["code"]),
        (StatusCode::BAD_REQUEST, &json!("surface.voice.unknown_model"))
    );

    let anonymous = self::app(&voice, None, None);
    assert_eq!(
        call(&anonymous, "GET", "/api/surface/voice", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ticket(&anonymous, json!({ "model": "scripted" })).await.0,
        StatusCode::UNAUTHORIZED
    );
}

/// A caller continues only their own voice conversation (S-I8).
#[tokio::test]
async fn another_users_voice_session_is_refused() {
    let voice = voice(Vec::new()).await;
    let (status, mine) =
        ticket(&app(&voice, Some(OWNER), None), json!({ "model": "scripted" })).await;
    assert_eq!(status, StatusCode::OK);
    let mine: VoiceTicket = serde_json::from_value(mine).unwrap();
    assert_eq!(mine.socket, "/voice/socket");
    assert!(mine.session_id.starts_with("voice-"));
    let again = ticket(
        &app(&voice, Some(OWNER), None),
        json!({ "model": "scripted", "sessionId": mine.session_id }),
    )
    .await;
    assert_eq!(again.0, StatusCode::OK);
    let (status, problem) = ticket(
        &app(&voice, Some("bo"), None),
        json!({ "model": "scripted", "sessionId": mine.session_id }),
    )
    .await;
    assert_eq!(
        (status, &problem["error"]["code"]),
        (StatusCode::FORBIDDEN, &json!("surface.voice.not_your_session"))
    );
}

/// A bad ticket, a used one, another user's, or none in time closes the socket with 4401.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_without_a_valid_ticket_closes_with_4401() {
    let voice = voice(Vec::new()).await;
    let server = serve(app(&voice, Some(OWNER), None)).await;
    let mut bad = open(&server).await;
    hello(&mut bad, "made-up").await;
    assert_eq!(closed_with(&mut bad).await, 4401);

    let (_, issued) = ticket(&app(&voice, Some(OWNER), None), json!({ "model": "scripted" })).await;
    let issued: VoiceTicket = serde_json::from_value(issued).unwrap();
    let mut first = open(&server).await;
    hello(&mut first, &issued.ticket).await;
    update(&mut first, "ready", |u| matches!(u, VoiceUpdate::Ready { .. })).await;
    let mut reused = open(&server).await;
    hello(&mut reused, &issued.ticket).await;
    assert_eq!(closed_with(&mut reused).await, 4401);
    first.close(None).await.unwrap();

    // A socket the server authenticated as someone else cannot use ana's ticket.
    let bo = serve(app(&voice, Some(OWNER), Some("bo"))).await;
    let (_, issued) = ticket(&app(&voice, Some(OWNER), None), json!({ "model": "scripted" })).await;
    let mut other = open(&bo).await;
    hello(&mut other, issued["ticket"].as_str().unwrap()).await;
    assert_eq!(closed_with(&mut other).await, 4401);

    let mut silent = open(&server).await;
    assert_eq!(closed_with(&mut silent).await, 4401, "no hello within five seconds");
}

/// Audio up, the transcript, the pending answer and its outcome down; the call runs once.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spoken_allow_once_over_the_socket() {
    let mut script = asks_for(SEND);
    script.push(says("allow once"));
    let voice = voice(script).await;
    let server = serve(app(&voice, Some(OWNER), None)).await;
    let (_, issued) = ticket(&app(&voice, Some(OWNER), None), json!({ "model": "scripted" })).await;
    let mut socket = open(&server).await;
    hello(&mut socket, issued["ticket"].as_str().unwrap()).await;
    update(&mut socket, "ready", |u| matches!(u, VoiceUpdate::Ready { .. })).await;
    audio(&mut socket).await;
    let prompted = update(&mut socket, "the prompt", |u| {
        matches!(u, VoiceUpdate::Prompted { decision_id: Some(_), .. })
    })
    .await;
    let VoiceUpdate::Prompted { decision_id: Some(id), .. } = prompted else { unreachable!() };
    tokio::time::sleep(DWELL).await;
    audio(&mut socket).await;
    assert_eq!(
        update(&mut socket, "heard", |u| matches!(u, VoiceUpdate::Heard { .. })).await,
        VoiceUpdate::Heard { text: "allow once".into() }
    );
    update(
        &mut socket,
        "pending",
        |u| matches!(u, VoiceUpdate::Pending { decision_id, .. } if *decision_id == id),
    )
    .await;
    assert_eq!(voice.sent(), 0);
    let outcome =
        update(&mut socket, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
    assert_eq!(outcome, VoiceUpdate::Outcome { decision_id: id, decided: true, text: None });
    eventually("the call ran", async || (voice.sent() == 1).then_some(())).await;
    socket.close(None).await.unwrap();
}

/// Counts the camera and screen frames that reach the provider.
struct Framed {
    inner: Arc<dyn RealtimeModel>,
    frames: Arc<AtomicUsize>,
}

struct FramedSession {
    inner: BoxedSession,
    frames: Arc<AtomicUsize>,
}

#[async_trait]
impl RealtimeModel for Framed {
    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        self.inner.supported_input_formats()
    }

    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        self.inner.supported_output_formats()
    }

    fn available_voices(&self) -> Vec<&str> {
        self.inner.available_voices()
    }

    async fn connect(&self, config: RealtimeConfig) -> adk_realtime::Result<BoxedSession> {
        let inner = self.inner.connect(config).await?;
        Ok(Box::new(FramedSession { inner, frames: self.frames.clone() }))
    }
}

#[async_trait]
impl RealtimeSession for FramedSession {
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }
    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }
    async fn send_audio(&self, audio: &AudioChunk) -> adk_realtime::Result<()> {
        self.inner.send_audio(audio).await
    }
    async fn send_audio_base64(&self, audio: &str) -> adk_realtime::Result<()> {
        self.inner.send_audio_base64(audio).await
    }
    async fn send_text(&self, text: &str) -> adk_realtime::Result<()> {
        self.inner.send_text(text).await
    }
    async fn send_video_frame(&self, _mime_type: &str, _data: &str) -> adk_realtime::Result<()> {
        self.frames.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send_tool_response(&self, response: ToolResponse) -> adk_realtime::Result<()> {
        self.inner.send_tool_response(response).await
    }
    async fn send_tool_output(&self, response: ToolResponse) -> adk_realtime::Result<()> {
        self.inner.send_tool_output(response).await
    }
    async fn commit_audio(&self) -> adk_realtime::Result<()> {
        self.inner.commit_audio().await
    }
    async fn clear_audio(&self) -> adk_realtime::Result<()> {
        self.inner.clear_audio().await
    }
    async fn create_response(&self) -> adk_realtime::Result<()> {
        self.inner.create_response().await
    }
    async fn interrupt(&self) -> adk_realtime::Result<()> {
        self.inner.interrupt().await
    }
    async fn send_event(&self, event: ClientEvent) -> adk_realtime::Result<()> {
        self.inner.send_event(event).await
    }
    async fn next_event(&self) -> Option<adk_realtime::Result<ServerEvent>> {
        self.inner.next_event().await
    }
    fn events(&self) -> Pin<Box<dyn Stream<Item = adk_realtime::Result<ServerEvent>> + Send + '_>> {
        self.inner.events()
    }
    async fn close(&self) -> adk_realtime::Result<()> {
        self.inner.close().await
    }
    async fn mutate_context(
        &self,
        config: RealtimeConfig,
    ) -> adk_realtime::Result<ContextMutationOutcome> {
        self.inner.mutate_context(config).await
    }
}

/// A camera frame sent after its switch went off never reaches the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_frame_after_switch_off_is_not_forwarded() {
    let frames = Arc::new(AtomicUsize::new(0));
    let counted = frames.clone();
    let voice = voice_with(Vec::new(), move |model| {
        Arc::new(Framed { inner: Arc::new(model), frames: counted })
    })
    .await;
    let server = serve(app(&voice, Some(OWNER), None)).await;
    let (_, issued) = ticket(&app(&voice, Some(OWNER), None), json!({ "model": "scripted" })).await;
    let mut socket = open(&server).await;
    hello(&mut socket, issued["ticket"].as_str().unwrap()).await;
    update(&mut socket, "ready", |u| matches!(u, VoiceUpdate::Ready { .. })).await;
    let camera = |byte: u8| Message::Binary(vec![FRAME_CAMERA, 0xFF, 0xD8, byte].into());
    let switch = |on: bool| {
        Message::Text(json!({ "type": "switch", "source": "camera", "on": on }).to_string().into())
    };

    socket.send(camera(1)).await.unwrap();
    socket.send(switch(true)).await.unwrap();
    socket.send(camera(2)).await.unwrap();
    eventually("the frame went through", async || {
        (frames.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    socket.send(switch(false)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    socket.send(camera(3)).await.unwrap();
    socket.send(camera(4)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(frames.load(Ordering::SeqCst), 1, "only the frame sent while the camera was on");
    socket.close(None).await.unwrap();
}

/// The page lets its script open the voice socket on the host that served it.
#[tokio::test]
async fn the_pages_policy_names_the_socket_host() {
    let home = adk_surface::web::home();
    let request = Request::builder()
        .uri("/ui/")
        .header(header::HOST, "127.0.0.1:8791")
        .body(Body::empty())
        .unwrap();
    let response = home.clone().oneshot(request).await.unwrap();
    let policy = response.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
    assert!(
        policy.contains("connect-src 'self' ws://127.0.0.1:8791 wss://127.0.0.1:8791;"),
        "{policy}"
    );
    // A host that is not a plain host and port adds nothing to the policy.
    let request = Request::builder()
        .uri("/ui/voice")
        .header(header::HOST, "evil; script-src *")
        .body(Body::empty())
        .unwrap();
    let response = home.oneshot(request).await.unwrap();
    let policy = response.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
    assert!(policy.contains("connect-src 'self';") && !policy.contains("evil"), "{policy}");
}
