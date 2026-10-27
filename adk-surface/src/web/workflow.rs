//! The Runs tab's routes (spec 3.3 §12 developer track; features `web` and `workflow`): a
//! graph agent's runs, steps and forks over [`Workflows`].
//!
//! [`router`] serves them under `/surface/workflow`, relative to the `/api` prefix: mount it
//! with `ServerBuilder::add_api_routes` beside [`super::router`], so the server's auth
//! middleware puts the caller's `RequestContext` in every request. Every route needs that
//! caller (`401` without one) and reads or forks only the caller's own sessions: anything else
//! answers `404` (S-I8). A paired device's token reads and never forks (`403
//! surface.not_here`).
//!
//! | Route (under `/api`) | Answer |
//! |---|---|
//! | `GET /surface/workflow/graphs` | [`GraphsResponse`] |
//! | `GET /surface/workflow/sessions/{session}/runs` | [`RunsResponse`]: every run, from records only; `?graph=` names the graph |
//! | `GET /surface/workflow/sessions/{session}/run` | [`WorkflowRun`]: the latest run, with each waiting node's decision id; `?graph=` names the graph |
//! | `GET /surface/workflow/sessions/{session}/checkpoints/{id}` | [`StepDetail`]: one step, read from one checkpoint |
//! | `GET /surface/workflow/sessions/{session}/feed` | Server-sent events: `changed` (with `id` = the newest checkpoint version) whenever the thread writes a checkpoint; resumes after `Last-Event-ID` |
//! | `POST /surface/workflow/sessions/{session}/fork` | [`ForkRequest`] → [`ForkOutcome`] |
//! | `POST /surface/workflow/sessions/{session}/start` | [`ForkOutcome`]: starts a fork made and not started |
//! | `GET /surface/workflow/openapi.json` | This API's OpenAPI document |
//!
//! Decisions stay on the engine's routes: a waiting node's card is the inbox entry, answered
//! with `POST /surface/commands` `decide` (S-I1, S-I2). Errors are problem documents.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Context, Unauthenticated, caller, failed, unauthenticated};
use crate::engine::SurfaceEngine;
use crate::origin::{Caller, SurfaceKind};
use crate::workflow::{
    ForkOutcome, ForkRequest, GraphsResponse, RunsResponse, StepDetail, WorkflowRun, Workflows,
};

/// The route paths, relative to the `/api` prefix (spec 3.3 §12).
pub mod paths {
    /// `GET`: the graphs observed.
    pub const GRAPHS: &str = "/surface/workflow/graphs";
    /// `GET`: every run of a session.
    pub const RUNS: &str = "/surface/workflow/sessions/{session}/runs";
    /// `GET`: a session's latest run.
    pub const RUN: &str = "/surface/workflow/sessions/{session}/run";
    /// `GET`: one step, from one checkpoint.
    pub const CHECKPOINT: &str = "/surface/workflow/sessions/{session}/checkpoints/{id}";
    /// `GET`: a session's run changes, as server-sent events.
    pub const FEED: &str = "/surface/workflow/sessions/{session}/feed";
    /// `POST`: a fork.
    pub const FORK: &str = "/surface/workflow/sessions/{session}/fork";
    /// `POST`: start a fork made and not started.
    pub const START: &str = "/surface/workflow/sessions/{session}/start";
    /// `GET`: the OpenAPI document.
    pub const OPENAPI: &str = "/surface/workflow/openapi.json";
}

/// How often the feed reads the thread's newest checkpoint.
const FEED_POLL: Duration = Duration::from_millis(250);

/// A `changed` frame of the feed: the thread's newest checkpoint version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowChanged {
    /// The newest checkpoint version.
    pub version: u64,
}

/// Query of the run reads.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphQuery {
    /// The graph agent whose runs to read; the one with checkpoints on the session, else the
    /// only one.
    #[serde(default)]
    pub graph: Option<String>,
}

#[derive(Clone)]
struct Routes {
    workflows: Workflows,
    engine: SurfaceEngine,
}

/// The Runs tab's routes, relative to `/api` (spec 3.3 §12).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, SurfaceEngine};
/// use adk_surface::workflow::Workflows;
///
/// # fn example(source: LocalSource, workflows: Workflows, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).card_hints(Arc::new(workflows.clone())).build();
/// let builder = builder
///     .add_api_routes(adk_surface::web::router(engine.clone()))
///     .add_api_routes(adk_surface::web::workflow::router(workflows, engine));
/// # let _ = builder;
/// # }
/// ```
pub fn router(workflows: Workflows, engine: SurfaceEngine) -> Router {
    Router::new()
        .route(paths::GRAPHS, get(graphs))
        .route(paths::RUNS, get(runs))
        .route(paths::RUN, get(run))
        .route(paths::CHECKPOINT, get(checkpoint))
        .route(paths::FEED, get(feed))
        .route(paths::FORK, post(fork))
        .route(paths::START, post(start))
        .route(paths::OPENAPI, get(openapi_document))
        .with_state(Routes { workflows, engine })
}

fn web_caller(context: Context) -> Result<Caller, Unauthenticated> {
    caller(context, SurfaceKind::Web, None)
}

macro_rules! caller_or_401 {
    ($context:expr) => {
        match web_caller($context) {
            Ok(caller) => caller,
            Err(Unauthenticated) => return unauthenticated(),
        }
    };
}

fn answer<T: Serialize>(result: crate::Result<T>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => failed(error),
    }
}

async fn graphs(State(routes): State<Routes>, context: Context) -> Response {
    let _caller = caller_or_401!(context);
    Json(GraphsResponse { graphs: routes.workflows.graphs() }).into_response()
}

async fn runs(
    State(routes): State<Routes>,
    context: Context,
    Path(session): Path<String>,
    Query(query): Query<GraphQuery>,
) -> Response {
    let caller = caller_or_401!(context);
    answer::<RunsResponse>(routes.workflows.runs(&caller, &session, query.graph.as_deref()).await)
}

async fn run(
    State(routes): State<Routes>,
    context: Context,
    Path(session): Path<String>,
    Query(query): Query<GraphQuery>,
) -> Response {
    let caller = caller_or_401!(context);
    answer::<WorkflowRun>(
        routes.workflows.run(&routes.engine, &caller, &session, query.graph.as_deref()).await,
    )
}

async fn checkpoint(
    State(routes): State<Routes>,
    context: Context,
    Path((session, id)): Path<(String, String)>,
) -> Response {
    let caller = caller_or_401!(context);
    answer::<StepDetail>(routes.workflows.step(&routes.engine, &caller, &session, &id).await)
}

async fn fork(
    State(routes): State<Routes>,
    context: Context,
    Path(session): Path<String>,
    Json(request): Json<ForkRequest>,
) -> Response {
    let caller = caller_or_401!(context);
    answer::<ForkOutcome>(routes.workflows.fork(&routes.engine, &caller, &session, request).await)
}

async fn start(
    State(routes): State<Routes>,
    context: Context,
    Path(session): Path<String>,
) -> Response {
    let caller = caller_or_401!(context);
    answer::<ForkOutcome>(routes.workflows.start(&routes.engine, &caller, &session).await)
}

async fn feed(
    State(routes): State<Routes>,
    context: Context,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let caller = caller_or_401!(context);
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    // A session that is not the caller's is refused before the stream starts (S-I8).
    if let Err(error) = routes.workflows.version(&caller, &session).await {
        return failed(error);
    }
    Sse::new(changes(routes.workflows, caller, session, after))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

/// A `changed` frame each time the thread's newest version passes `after`.
fn changes(
    workflows: Workflows,
    caller: Caller,
    session: String,
    after: Option<u64>,
) -> impl Stream<Item = Result<SseEvent, std::convert::Infallible>> {
    futures::stream::unfold(Some(after), move |state| {
        let (workflows, caller, session) = (workflows.clone(), caller.clone(), session.clone());
        async move {
            let after = state?;
            loop {
                match workflows.version(&caller, &session).await {
                    Ok(Some(version)) if after.is_none_or(|after| version > after) => {
                        let event = SseEvent::default()
                            .id(version.to_string())
                            .event("changed")
                            .json_data(WorkflowChanged { version })
                            .unwrap_or_else(|_| SseEvent::default().comment("unencodable"));
                        return Some((Ok(event), Some(Some(version))));
                    }
                    Ok(_) => tokio::time::sleep(FEED_POLL).await,
                    Err(error) => {
                        let event = SseEvent::default()
                            .event("error")
                            .json_data(adk_core::AdkError::from(error).to_problem_json())
                            .unwrap_or_else(|_| SseEvent::default().comment("unencodable"));
                        return Some((Ok(event), None));
                    }
                }
            }
        }
    })
}

async fn openapi_document() -> Response {
    Json(openapi()).into_response()
}

/// The OpenAPI 3.1 document of [`router`], its schemas generated from the workflow view types
/// (spec 3.3 §12).
///
/// # Example
///
/// ```rust
/// let document = adk_surface::web::workflow::openapi();
/// assert!(document["components"]["schemas"]["WorkflowRun"].is_object());
/// ```
pub fn openapi() -> serde_json::Value {
    super::openapi::workflow::openapi()
}

/// TypeScript declarations of every schema in [`openapi`], for the web home's Runs tab.
///
/// # Example
///
/// ```rust
/// assert!(adk_surface::web::workflow::typescript().contains("export interface WorkflowRun {"));
/// ```
pub fn typescript() -> String {
    super::openapi::workflow::typescript()
}
