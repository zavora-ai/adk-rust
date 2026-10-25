//! The voice view's routes and socket (spec 3.3 §5.6; features `voice` and `web`).

use std::time::Duration;

use adk_core::{AdkError, RequestContext};
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use futures::{SinkExt, StreamExt};

use super::hub::{TICKET_LIFETIME, Ticket};
use super::session::{VoiceInput, VoiceLink, VoiceOutput};
use super::wire::{
    FRAME_AUDIO, FRAME_CAMERA, FRAME_SCREEN, VisualSource, VoiceClientMessage, VoiceTicket,
    VoiceTicketRequest,
};
use super::{VoiceError, VoiceHub, paths};
use crate::error::SurfaceError;
use crate::origin::{Caller, Origin, SurfaceKind};

/// The voice socket's path, a root route (spec 3.3 §5.6).
pub const SOCKET_PATH: &str = paths::SOCKET;

/// How long a ticket for the voice socket works.
pub const TICKET_TTL: Duration = TICKET_LIFETIME;

/// How long the socket waits for its first message, the hello with the ticket.
const HELLO_WITHIN: Duration = Duration::from_secs(5);

/// The largest message the socket takes: a camera or screen JPEG fits.
const MAX_MESSAGE: usize = 1 << 20;

/// The close code of a socket without a valid ticket.
const CLOSE_NO_TICKET: u16 = 4401;

type Context = Option<Extension<Option<RequestContext>>>;

/// The caller the server's authentication admitted; voice reads nothing else from a request.
fn caller(context: Context) -> Option<Caller> {
    let Some(Extension(Some(context))) = context else {
        return None;
    };
    Some(
        Caller::human(context.user_id.clone(), SurfaceKind::Voice, Origin::Pointer)
            .with_groups(context.scopes.clone()),
    )
}

fn problem(error: SurfaceError) -> Response {
    let error = AdkError::from(error);
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(error.to_problem_json())).into_response()
}

fn unauthenticated() -> Response {
    problem(SurfaceError::Source(AdkError::new(
        adk_core::ErrorComponent::Server,
        adk_core::ErrorCategory::Unauthorized,
        "surface.unauthenticated",
        "voice needs an authenticated caller; mount it with ServerBuilder::add_api_routes \
         behind a RequestContextExtractor",
    )))
}

/// Voice's API routes, relative to `/api` (spec 3.3 §5.6): mount them with
/// `ServerBuilder::add_api_routes`, beside [`web::router`](crate::web::router), so every
/// request carries the caller the server's authentication admitted.
///
/// | Route (under `/api`) | Answer |
/// |---|---|
/// | `GET /surface/voice` | [`VoiceModels`](super::VoiceModels): the agent and the models, the default first |
/// | `POST /surface/voice/sessions` | [`VoiceTicketRequest`] → [`VoiceTicket`]; `401` without a caller, `403` for a `sessionId` that is not one of the caller's voice conversations |
///
/// A ticket works once, within [`TICKET_TTL`], for the caller it was issued to, and is never
/// part of a URL or a log line.
///
/// # Example
///
/// ```rust,no_run
/// # fn example(hub: adk_surface::voice::VoiceHub, builder: adk_server::ServerBuilder) {
/// let builder = builder
///     .add_api_routes(adk_surface::voice::router(hub.clone()))
///     .add_root_routes(adk_surface::voice::socket_routes(hub));
/// # let _ = builder;
/// # }
/// ```
pub fn router(hub: VoiceHub) -> Router {
    Router::new()
        .route(paths::VOICE, get(models))
        .route(paths::SESSIONS, post(ticket))
        .with_state(hub)
}

/// The voice socket, `GET /voice/socket`, as root routes (spec 3.3 §5.6): mount it with
/// `ServerBuilder::add_root_routes`.
///
/// The first message must be `{"type":"hello","ticket":…}` within 5 seconds, with a ticket
/// from [`router`]; otherwise the socket closes with code 4401. Messages are at most 1 MiB.
/// Binary messages carry a one-byte tag: [`FRAME_AUDIO`] (PCM16 mono, up at the model's
/// input rate, down at its output rate), [`FRAME_CAMERA`] and [`FRAME_SCREEN`] (one JPEG).
/// Text messages are [`VoiceClientMessage`]s up and
/// [`VoiceUpdate`](super::VoiceUpdate)s down.
pub fn socket_routes(hub: VoiceHub) -> Router {
    Router::new().route(paths::SOCKET, get(socket)).with_state(hub)
}

async fn models(State(hub): State<VoiceHub>, context: Context) -> Response {
    if caller(context).is_none() {
        return unauthenticated();
    }
    Json(hub.models()).into_response()
}

async fn ticket(
    State(hub): State<VoiceHub>,
    context: Context,
    Json(request): Json<VoiceTicketRequest>,
) -> Response {
    let Some(caller) = caller(context) else {
        return unauthenticated();
    };
    let Some(model) = hub.models().models.into_iter().find(|model| model.id == request.model)
    else {
        return problem(VoiceError::UnknownModel(request.model).into());
    };
    let session_id = match hub.session_for(&caller, request.session_id.as_deref()).await {
        Ok(session_id) => session_id,
        Err(error) => return problem(error.into()),
    };
    let ticket = hub.issue(&caller, &model.id, &session_id);
    Json(VoiceTicket {
        ticket,
        socket: SOCKET_PATH.to_string(),
        session_id,
        input_rate: model.input_rate,
        output_rate: model.output_rate,
    })
    .into_response()
}

async fn socket(
    State(hub): State<VoiceHub>,
    context: Context,
    upgrade: WebSocketUpgrade,
) -> Response {
    // A socket request the server authenticated must be the ticket's caller's.
    let user = caller(context).map(|caller| caller.user_id().to_string());
    upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve(hub, user, socket))
}

async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame { code, reason: Utf8Bytes::from_static(reason) })))
        .await;
}

/// The ticket the socket's first message carries, taken once.
async fn hello(hub: &VoiceHub, socket: &mut WebSocket, user: Option<&str>) -> Option<Ticket> {
    let first = tokio::time::timeout(HELLO_WITHIN, socket.recv()).await.ok()??.ok()?;
    let Message::Text(text) = first else {
        return None;
    };
    let VoiceClientMessage::Hello { ticket } = serde_json::from_str(text.as_str()).ok()? else {
        return None;
    };
    let ticket = hub.redeem(&ticket)?;
    user.is_none_or(|user| user == ticket.caller.user_id()).then_some(ticket)
}

async fn serve(hub: VoiceHub, user: Option<String>, mut socket: WebSocket) {
    let Some(ticket) = hello(&hub, &mut socket, user.as_deref()).await else {
        close(socket, CLOSE_NO_TICKET, "a valid voice ticket is needed").await;
        return;
    };
    let link = match hub.open_ticket(&ticket).await {
        Ok(link) => link,
        Err(error) => {
            let update = super::VoiceUpdate::Error {
                code: error.code().to_string(),
                message: error.to_string(),
            };
            if let Ok(text) = serde_json::to_string(&update) {
                let _ = socket.send(Message::Text(text.into())).await;
            }
            close(socket, 1011, "the voice conversation could not start").await;
            return;
        }
    };
    bridge(socket, link).await;
}

/// Carries the socket's messages to the conversation and back until either side ends.
async fn bridge(socket: WebSocket, link: VoiceLink) {
    let VoiceLink { input, mut output, .. } = link;
    let (mut sink, mut stream) = socket.split();
    let reader = async move {
        while let Some(Ok(message)) = stream.next().await {
            let input_message = match message {
                Message::Binary(bytes) => match bytes.split_first() {
                    Some((&FRAME_AUDIO, pcm)) => VoiceInput::Audio(pcm.to_vec()),
                    Some((&FRAME_CAMERA, jpeg)) => {
                        VoiceInput::Frame { source: VisualSource::Camera, jpeg: jpeg.to_vec() }
                    }
                    Some((&FRAME_SCREEN, jpeg)) => {
                        VoiceInput::Frame { source: VisualSource::Screen, jpeg: jpeg.to_vec() }
                    }
                    _ => continue,
                },
                Message::Text(text) => match serde_json::from_str(text.as_str()) {
                    Ok(message) => VoiceInput::Message(message),
                    Err(_) => continue,
                },
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            if input.send(input_message).await.is_err() {
                break;
            }
        }
        // Dropping `input` ends the conversation.
    };
    let writer = async {
        while let Some(out) = output.recv().await {
            let message = match out {
                VoiceOutput::Audio(pcm) => {
                    let mut frame = Vec::with_capacity(pcm.len() + 1);
                    frame.push(FRAME_AUDIO);
                    frame.extend_from_slice(&pcm);
                    Message::Binary(frame.into())
                }
                VoiceOutput::Update(update) => match serde_json::to_string(&update) {
                    Ok(text) => Message::Text(text.into()),
                    Err(_) => continue,
                },
            };
            if sink.send(message).await.is_err() {
                return;
            }
        }
        let _ = sink
            .send(Message::Close(Some(CloseFrame {
                code: 1000,
                reason: Utf8Bytes::from_static("the voice conversation ended"),
            })))
            .await;
    };
    tokio::select! {
        () = reader => {}
        () = writer => {}
    }
}
