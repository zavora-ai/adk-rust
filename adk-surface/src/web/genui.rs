//! The web home's agent-drawn UI routes (spec 3.3 §6.1 `genui`, §7.4 rule 5).
//!
//! | Route (under `/api`) | Answer |
//! |---|---|
//! | `GET /surface/genui/capabilities` | [`GenuiCapabilities`]: what this home claims of A2UI, AG-UI and MCP Apps, and the catalogs it renders, which the developer panel's Protocols tab shows |
//! | `GET /surface/genui/sessions/{session}/forms` | [`FormsResponse`]: the state of each form sent in the session, from its decision and its call's result |
//! | `GET /surface/genui/sessions/{session}/apps/{call}` | [`AppOpen`]: an app from an MCP server, framed, blocked, or not shown here (`genui-apps`) |
//! | `POST /surface/genui/sessions/{session}/apps/{call}/requests` | [`AppRequest`] → [`AppAnswer`]: what the app asked the home, which never approves anything (`genui-apps`) |
//! | `POST /surface/genui/sessions/{session}/apps/{call}/message` | [`AppMessageRequest`] → `CommandOutcome`: the person pressed **Send it** on what the app suggested (`genui-apps`) |
//!
//! A drawn form is sent with `POST /surface/commands` and a `submitDrawn` command. The routes
//! are mounted with `genui::router` by a host built with `genui`; the page probes them and hides
//! what a server built without `genui` lacks. Their types are in the OpenAPI document of
//! every `web` build.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::genui::FormState;
pub use crate::genui::{AppAnswer, AppMessageRequest, AppOpen, AppRequest, GenuiCapabilities};

/// The routes' paths, relative to `/api`.
pub mod paths {
    /// `GET`: what this home claims of the UI protocols agents draw in.
    pub const CAPABILITIES: &str = "/surface/genui/capabilities";
    /// `GET`: the state of each drawn form sent in a session.
    pub const FORMS: &str = "/surface/genui/sessions/{session}/forms";
    /// `GET`: an app from an MCP server, opened (spec 3.3 §6.1 `genui-apps`).
    pub const APP: &str = "/surface/genui/sessions/{session}/apps/{call}";
    /// `POST`: a request an app made of the home.
    pub const APP_REQUESTS: &str = "/surface/genui/sessions/{session}/apps/{call}/requests";
    /// `POST`: the person sends what an app suggested.
    pub const APP_MESSAGE: &str = "/surface/genui/sessions/{session}/apps/{call}/message";
}

/// `GET /surface/genui/sessions/{session}/forms` answer (spec 3.3 §7.4 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FormsResponse {
    /// The session.
    pub session_id: String,
    /// Each form sent, its latest send per version.
    pub forms: Vec<FormState>,
}

#[cfg(feature = "genui")]
mod routes {
    use axum::extract::{Path, Query, State};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};

    use super::{FormsResponse, paths};
    use crate::engine::SurfaceEngine;
    use crate::model::{DecidedAnswer, ThreadItem};
    use crate::origin::SurfaceKind;
    use crate::web::{Context, SurfaceQuery, Unauthenticated, caller, failed, unauthenticated};

    /// The agent-drawn UI routes, relative to `/api` (spec 3.3 §6.1 `genui`): mount them with
    /// `ServerBuilder::add_api_routes` beside [`crate::web::router`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::{LocalSource, SurfaceEngine};
    ///
    /// # fn example(source: LocalSource, builder: adk_server::ServerBuilder) {
    /// let engine = SurfaceEngine::builder(Arc::new(source)).build();
    /// let builder = builder
    ///     .add_api_routes(adk_surface::web::router(engine.clone()))
    ///     .add_api_routes(adk_surface::web::genui::router(engine));
    /// # let _ = builder;
    /// # }
    /// ```
    pub fn router(engine: SurfaceEngine) -> Router {
        Router::new()
            .route(paths::CAPABILITIES, get(capabilities))
            .route(paths::FORMS, get(forms))
            .with_state(engine)
    }

    async fn capabilities(context: Context) -> Response {
        if caller(context, SurfaceKind::Web, None).is_err() {
            return unauthenticated();
        }
        Json(crate::genui::capabilities()).into_response()
    }

    async fn forms(
        State(engine): State<SurfaceEngine>,
        context: Context,
        Path(session): Path<String>,
        Query(query): Query<SurfaceQuery>,
    ) -> Response {
        let caller = match caller(context, query.surface.unwrap_or(SurfaceKind::Web), None) {
            Ok(caller) => caller,
            Err(Unauthenticated) => return unauthenticated(),
        };
        let entries = match engine.thread(&caller, &session).await {
            Ok(entries) => entries,
            Err(error) => return failed(error),
        };
        // How each sent form's decision ended, read once per decision.
        let mut answers: Vec<(String, Option<DecidedAnswer>)> = Vec::new();
        for item in entries.iter().flat_map(|entry| &entry.items) {
            if let ThreadItem::DrawnSent { sent } = item
                && let Some(id) = &sent.decision_id
                && !answers.iter().any(|(known, _)| known == id)
            {
                let answer = engine
                    .decision(&caller, id)
                    .await
                    .ok()
                    .and_then(|card| card.decided)
                    .map(|decided| decided.answer);
                answers.push((id.clone(), answer));
            }
        }
        let forms = crate::genui::form_states(&entries, |id| {
            answers.iter().find(|(known, _)| known == id).and_then(|(_, answer)| *answer)
        });
        Json(FormsResponse { session_id: session, forms }).into_response()
    }
}

#[cfg(feature = "genui")]
pub use routes::router;

#[cfg(feature = "genui-apps")]
mod apps {
    use axum::body::Body;
    use axum::extract::{Path, Query, Request, State};
    use axum::http::header;
    use axum::middleware::{self, Next};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::{AppMessageRequest, AppRequest, paths};
    use crate::engine::{CommandOutcome, SurfaceEngine};
    use crate::error::SurfaceError;
    use crate::genui::McpAppsHost;
    use crate::origin::SurfaceKind;
    use crate::web::{Context, SurfaceQuery, Unauthenticated, caller, failed, unauthenticated};

    /// The engine and the host an app route needs.
    #[derive(Clone)]
    struct Apps {
        engine: SurfaceEngine,
        host: McpAppsHost,
    }

    /// The MCP Apps routes, relative to `/api` (spec 3.3 §6.1 `genui-apps`): mount them with
    /// `ServerBuilder::add_api_routes` beside [`crate::web::router`], serve
    /// [`McpAppsHost::sandbox_router`] on the apps origin, and wrap the server's router in
    /// [`refuse_apps_origin`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::genui::McpAppsHost;
    /// use adk_surface::{LocalSource, SurfaceEngine};
    ///
    /// # fn example(source: LocalSource, manager: Arc<adk_tool::mcp::McpServerManager>, builder: adk_server::ServerBuilder) {
    /// let engine = SurfaceEngine::builder(Arc::new(source)).build();
    /// let host = McpAppsHost::builder().servers(manager).apps_origin("http://127.0.0.1:41234").build();
    /// let builder = builder
    ///     .add_api_routes(adk_surface::web::router(engine.clone()))
    ///     .add_api_routes(adk_surface::web::genui::apps_router(engine, host));
    /// # let _ = builder;
    /// # }
    /// ```
    pub fn apps_router(engine: SurfaceEngine, host: McpAppsHost) -> Router {
        Router::new()
            .route(paths::APP, get(open))
            .route(paths::APP_REQUESTS, post(request))
            .route(paths::APP_MESSAGE, post(message))
            .with_state(Apps { engine, host })
    }

    /// Refuses every request `router` serves whose `Origin` is `apps_origin`, or the opaque
    /// `null` a sandboxed frame sends, with `403 surface.not_here` (spec 3.3 §6.1
    /// `genui-apps`): an app's frame never acts on the home, whatever the server's
    /// `SecurityConfig` admits. Wrap the whole server's router, not only the surface's.
    ///
    /// # Example
    ///
    /// ```rust
    /// let app = adk_surface::web::genui::refuse_apps_origin(axum::Router::new(), "http://127.0.0.1:41234");
    /// # let _ = app;
    /// ```
    pub fn refuse_apps_origin(router: Router, apps_origin: &str) -> Router {
        let apps_origin = apps_origin.trim_end_matches('/').to_ascii_lowercase();
        router.layer(middleware::from_fn(move |request: Request<Body>, next: Next| {
            let apps_origin = apps_origin.clone();
            async move {
                let from_apps = request
                    .headers()
                    .get_all(header::ORIGIN)
                    .iter()
                    .filter_map(|origin| origin.to_str().ok())
                    .map(|origin| origin.trim_end_matches('/').to_ascii_lowercase())
                    .any(|origin| origin == apps_origin || origin == "null");
                if from_apps {
                    tracing::warn!(
                        http.path = %request.uri().path(),
                        "refused a request from an app's frame"
                    );
                    return failed(SurfaceError::NotHere(
                        "an app's frame never acts on this home".to_string(),
                    ));
                }
                next.run(request).await
            }
        }))
    }

    async fn open(
        State(apps): State<Apps>,
        context: Context,
        Path((session, call)): Path<(String, String)>,
        Query(query): Query<SurfaceQuery>,
    ) -> Response {
        let caller = match caller(context, query.surface.unwrap_or(SurfaceKind::Web), None) {
            Ok(caller) => caller,
            Err(Unauthenticated) => return unauthenticated(),
        };
        match apps.host.open(&apps.engine, &caller, &session, &call).await {
            Ok(open) => Json(open).into_response(),
            Err(error) => failed(error),
        }
    }

    async fn request(
        State(apps): State<Apps>,
        context: Context,
        Path((session, call)): Path<(String, String)>,
        Json(body): Json<AppRequest>,
    ) -> Response {
        // Only the web home frames apps, so only it relays their requests.
        let caller = match caller(context, SurfaceKind::Web, None) {
            Ok(caller) if caller.surface == SurfaceKind::Web => caller,
            Ok(_) => return failed(SurfaceError::NotHere("apps run in the web home".into())),
            Err(Unauthenticated) => return unauthenticated(),
        };
        match apps.host.request(&apps.engine, &caller, &session, &call, body).await {
            Ok(answer) => Json(answer).into_response(),
            Err(error) => failed(error),
        }
    }

    async fn message(
        State(apps): State<Apps>,
        context: Context,
        Path((session, call)): Path<(String, String)>,
        Json(body): Json<AppMessageRequest>,
    ) -> Response {
        let caller = match caller(context, SurfaceKind::Web, None) {
            Ok(caller) if caller.surface == SurfaceKind::Web => caller,
            Ok(_) => return failed(SurfaceError::NotHere("apps run in the web home".into())),
            Err(Unauthenticated) => return unauthenticated(),
        };
        match apps.host.send_message(&apps.engine, &caller, &session, &call, &body.message).await {
            Ok(outcome) => Json::<CommandOutcome>(outcome).into_response(),
            Err(error) => failed(error),
        }
    }
}

#[cfg(feature = "genui-apps")]
pub use apps::{apps_router, refuse_apps_origin};
