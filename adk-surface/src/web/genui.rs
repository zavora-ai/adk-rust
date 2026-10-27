//! The web home's agent-drawn UI routes (spec 3.3 §6.1 `genui`, §7.4 rule 5).
//!
//! | Route (under `/api`) | Answer |
//! |---|---|
//! | `GET /surface/genui/capabilities` | [`GenuiCapabilities`]: what this home claims of A2UI, AG-UI and MCP Apps, and the catalogs it renders, which the developer panel's Protocols tab shows |
//! | `GET /surface/genui/sessions/{session}/forms` | [`FormsResponse`]: the state of each form sent in the session, from its decision and its call's result |
//!
//! A drawn form is sent with `POST /surface/commands` and a `submitDrawn` command. The routes
//! are mounted with [`router`] by a host built with `genui`; the page probes them and hides
//! what a server built without `genui` lacks. Their types are in the OpenAPI document of
//! every `web` build.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::genui::FormState;
pub use crate::genui::GenuiCapabilities;

/// The routes' paths, relative to `/api`.
pub mod paths {
    /// `GET`: what this home claims of the UI protocols agents draw in.
    pub const CAPABILITIES: &str = "/surface/genui/capabilities";
    /// `GET`: the state of each drawn form sent in a session.
    pub const FORMS: &str = "/surface/genui/sessions/{session}/forms";
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
