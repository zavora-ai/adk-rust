//! The website's views on the web home (spec 3.5 §7.1 `adk-surface` `website`, §8.2; feature
//! `website`): the web editor, onboarding, the charter cards, the site view and the weekly
//! review, over `adk-website`'s [`WebsiteDesk`].
//!
//! [`router`] serves the desk under `/surface/website`, relative to the `/api` prefix: mount it
//! with `ServerBuilder::add_api_routes`, so the server's auth middleware puts the caller's
//! `RequestContext` in every request; every route needs one (`401` without it), and the desk
//! decides what the caller's roles in the site's charter allow. [`setup_router`] serves the
//! setup code's two routes as root routes, because the person claiming a fresh site has no
//! credentials yet: the code is the credential (spec 3.5 §7.17, A-I20).
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /api/surface/website/setup` (root) | [`SetupView`] |
//! | `POST /api/surface/website/setup/claim` (root) | [`ClaimRequest`] → [`ClaimedView`]: the paired device's token, shown once |
//! | `GET /surface/website` | [`WebsiteHome`]: the site's name, whether it is live, the caller's roles |
//! | `GET`, `POST /surface/website/onboarding` | [`OnboardingView`]; [`OnboardingCommand`] → [`OnboardingOutcome`]. Both need the paired device's token as `Authorization: Bearer` (A-I20) |
//! | `GET`, `POST /surface/website/charter` | [`CharterResponse`]; [`CharterCommand`] → [`CharterResponse`]: signing by a person the approval chain names, from any device |
//! | `GET`, `POST /surface/website/site` | [`SiteViewModel`]; [`SiteCommand`] → [`SiteOutcome`] |
//! | `GET`, `POST /surface/website/review` | [`ReviewPage`] (`?week=`); [`ReviewCommand`] → [`ReviewPage`] |
//! | `GET`, `POST /surface/website/editor` | [`EditorHome`]; [`EditorCommand`] → [`EditorOutcome`] |
//! | `GET /surface/website/editor/drafts/{id}` | [`DraftView`] |
//! | `POST /surface/website/editor/drafts/{id}/media` | [`UploadRequest`] → [`DraftView`]: one file through the media pipeline |
//! | `GET /surface/website/openapi.json` | This API's OpenAPI document |
//!
//! Errors are problem documents with the desk's `website.*` code.

use adk_website::ui::charter::{CharterCommand, CharterReview};
use adk_website::ui::onboarding::OnboardingView;
use adk_website::ui::setup::ClaimRequest;
use adk_website::ui::{
    ClaimedView, DraftView, EditorCommand, EditorHome, EditorOutcome, OnboardingCommand,
    OnboardingOutcome, ReviewCommand, ReviewPage, SetupView, SiteCommand, SiteOutcome,
    SiteViewModel, WebsiteDesk,
};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Context, problem, unauthenticated};

/// The route paths: [`ROOT`](paths::ROOT) and its children as root routes, the rest relative
/// to `/api` (spec 3.5 §8.2).
pub mod paths {
    /// `GET`: the setup screen (root route).
    pub const SETUP: &str = "/api/surface/website/setup";
    /// `POST`: claim the setup code (root route).
    pub const CLAIM: &str = "/api/surface/website/setup/claim";
    /// `GET`: the site and the caller's roles.
    pub const ROOT: &str = "/surface/website";
    /// `GET`, `POST`: onboarding.
    pub const ONBOARDING: &str = "/surface/website/onboarding";
    /// `GET`, `POST`: the charter under review.
    pub const CHARTER: &str = "/surface/website/charter";
    /// `GET`, `POST`: the site view.
    pub const SITE: &str = "/surface/website/site";
    /// `GET`, `POST`: the weekly review.
    pub const REVIEW: &str = "/surface/website/review";
    /// `GET`, `POST`: the web editor.
    pub const EDITOR: &str = "/surface/website/editor";
    /// `GET`: one draft.
    pub const DRAFT: &str = "/surface/website/editor/drafts/{id}";
    /// `POST`: a media file for a draft.
    pub const MEDIA: &str = "/surface/website/editor/drafts/{id}/media";
    /// `GET`: the OpenAPI document.
    pub const OPENAPI: &str = "/surface/website/openapi.json";
}

/// The largest media upload, in bytes, before base64 (spec 3.5 §7.8.2 "Media").
pub const MEDIA_MAX_BYTES: usize = 12 * 1024 * 1024;

/// `GET /surface/website`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebsiteHome {
    /// The site's name.
    pub name: String,
    /// Its public address.
    pub origin: String,
    /// Whether the site is live.
    pub launched: bool,
    /// Whether the site asks for a setup code, and its owner has not claimed it.
    pub needs_setup: bool,
    /// The caller's roles in the charter: `owner`, `publisher`, `editor`, `approver`, `auditor`.
    pub roles: Vec<String>,
}

/// `GET`, `POST /surface/website/charter`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CharterResponse {
    /// The charter under review, if a draft exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter: Option<CharterReview>,
}

/// `POST /surface/website/editor/drafts/{id}/media`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    /// The file's name.
    pub name: String,
    /// The file, base64-encoded.
    pub data_base64: String,
    /// Drafted alt text; a person confirms it before the accessibility check passes.
    #[serde(default)]
    pub alt: Option<String>,
    /// Whether the image was generated; it is then labelled as an illustration.
    #[serde(default)]
    pub generated: bool,
}

/// `GET /surface/website/review` query.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQuery {
    /// The Monday of the week to show; the latest review when absent.
    #[serde(default)]
    pub week: Option<chrono::NaiveDate>,
}

/// The desk's routes, relative to `/api` (spec 3.5 §8.2).
///
/// # Example
///
/// ```rust,no_run
/// # fn example(desk: adk_website::ui::WebsiteDesk, builder: adk_server::ServerBuilder) {
/// let builder = builder
///     .add_api_routes(adk_surface::web::website::router(desk.clone()))
///     .add_root_routes(adk_surface::web::website::setup_router(desk));
/// # let _ = builder;
/// # }
/// ```
pub fn router(desk: WebsiteDesk) -> Router {
    Router::new()
        .route(paths::ROOT, get(home))
        .route(paths::ONBOARDING, get(onboarding).post(onboarding_command))
        .route(paths::CHARTER, get(charter).post(charter_command))
        .route(paths::SITE, get(site).post(site_command))
        .route(paths::REVIEW, get(review).post(review_command))
        .route(paths::EDITOR, get(editor).post(editor_command))
        .route(paths::DRAFT, get(draft))
        .route(
            paths::MEDIA,
            // Base64 is four bytes for every three, plus the JSON around it.
            post(upload).layer(DefaultBodyLimit::max(MEDIA_MAX_BYTES / 3 * 4 + 64 * 1024)),
        )
        .route(paths::OPENAPI, get(openapi_document))
        .with_state(desk)
}

/// The setup code's routes, as root routes: the person claiming a fresh site has no
/// credentials yet (spec 3.5 §7.17, A-I20). Mount with `ServerBuilder::add_root_routes`.
pub fn setup_router(desk: WebsiteDesk) -> Router {
    Router::new().route(paths::SETUP, get(setup)).route(paths::CLAIM, post(claim)).with_state(desk)
}

/// The caller's principal, from the server's authentication.
fn person(context: Context) -> Option<String> {
    match context {
        Some(Extension(Some(context))) => Some(context.user_id),
        _ => None,
    }
}

/// The bearer token the request carries: the paired device's, for onboarding (A-I20).
fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn answer<T: Serialize>(result: adk_core::Result<T>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => problem(&error),
    }
}

macro_rules! caller {
    ($context:expr) => {
        match person($context) {
            Some(person) => person,
            None => return unauthenticated(),
        }
    };
}

async fn setup(State(desk): State<WebsiteDesk>) -> Response {
    Json::<SetupView>(desk.setup_view().await).into_response()
}

async fn claim(State(desk): State<WebsiteDesk>, Json(request): Json<ClaimRequest>) -> Response {
    answer::<ClaimedView>(desk.claim(&request.code, request.device.as_deref()).await)
}

async fn home(State(desk): State<WebsiteDesk>, context: Context) -> Response {
    let person = caller!(context);
    let roles = match desk.roles(&person).await {
        Ok(roles) => roles,
        Err(error) => return problem(&error),
    };
    let setup = desk.setup_view().await;
    let launched = desk.runtime().await.launched().await;
    Json(WebsiteHome {
        name: desk.site_name().await,
        origin: desk.site().origin.clone(),
        launched,
        needs_setup: setup.needed && !setup.paired,
        roles: roles
            .iter()
            .filter_map(|role| serde_json::to_value(role).ok()?.as_str().map(str::to_string))
            .collect(),
    })
    .into_response()
}

async fn onboarding(
    State(desk): State<WebsiteDesk>,
    context: Context,
    headers: HeaderMap,
) -> Response {
    let person = caller!(context);
    answer::<OnboardingView>(desk.onboarding_view(&person, bearer(&headers).as_deref()).await)
}

async fn onboarding_command(
    State(desk): State<WebsiteDesk>,
    context: Context,
    headers: HeaderMap,
    Json(command): Json<OnboardingCommand>,
) -> Response {
    let person = caller!(context);
    answer::<OnboardingOutcome>(
        desk.onboarding_command(&person, bearer(&headers).as_deref(), command).await,
    )
}

async fn charter(State(desk): State<WebsiteDesk>, context: Context) -> Response {
    let person = caller!(context);
    answer(desk.charter_review(&person).await.map(|charter| CharterResponse { charter }))
}

async fn charter_command(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Json(command): Json<CharterCommand>,
) -> Response {
    let person = caller!(context);
    // Changing the charter during onboarding is the paired owner's: through the onboarding
    // route. Here a person the approval chain names reads and signs.
    if !matches!(command, CharterCommand::Sign { .. }) {
        return problem(&adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::Forbidden,
            "website.charter.sign_only",
            "only signing is open here; the owner reviews and changes the charter in onboarding",
        ));
    }
    let result = match desk.charter_command(&person, command).await {
        Ok(_) => desk.charter_review(&person).await,
        Err(error) => Err(error),
    };
    answer(result.map(|charter| CharterResponse { charter }))
}

async fn site(State(desk): State<WebsiteDesk>, context: Context) -> Response {
    let person = caller!(context);
    answer::<SiteViewModel>(desk.site_view(&person).await)
}

async fn site_command(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Json(command): Json<SiteCommand>,
) -> Response {
    let person = caller!(context);
    answer::<SiteOutcome>(desk.site_command(&person, command).await)
}

async fn review(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Query(query): Query<ReviewQuery>,
) -> Response {
    let person = caller!(context);
    answer::<ReviewPage>(desk.review_page(&person, query.week).await)
}

async fn review_command(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Json(command): Json<ReviewCommand>,
) -> Response {
    let person = caller!(context);
    answer::<ReviewPage>(desk.review_command(&person, command).await)
}

async fn editor(State(desk): State<WebsiteDesk>, context: Context) -> Response {
    let person = caller!(context);
    answer::<EditorHome>(desk.editor_home(&person).await)
}

async fn editor_command(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Json(command): Json<EditorCommand>,
) -> Response {
    let person = caller!(context);
    answer::<EditorOutcome>(desk.editor_command(&person, command).await)
}

async fn draft(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Path(id): Path<String>,
) -> Response {
    let person = caller!(context);
    answer::<DraftView>(desk.draft_view(&person, &id).await)
}

async fn upload(
    State(desk): State<WebsiteDesk>,
    context: Context,
    Path(id): Path<String>,
    Json(request): Json<UploadRequest>,
) -> Response {
    let person = caller!(context);
    let bytes =
        match base64::engine::general_purpose::STANDARD.decode(request.data_base64.as_bytes()) {
            Ok(bytes) if bytes.len() <= MEDIA_MAX_BYTES => bytes,
            Ok(_) => {
                return problem(&adk_core::AdkError::new(
                    adk_core::ErrorComponent::Server,
                    adk_core::ErrorCategory::InvalidInput,
                    "website.media.too_large",
                    format!("a file is at most {} MB", MEDIA_MAX_BYTES / 1024 / 1024),
                ));
            }
            Err(error) => {
                return problem(&adk_core::AdkError::new(
                    adk_core::ErrorComponent::Server,
                    adk_core::ErrorCategory::InvalidInput,
                    "website.media.encoding",
                    format!("dataBase64 is not base64: {error}"),
                ));
            }
        };
    answer::<DraftView>(
        desk.upload(&person, &id, &request.name, &bytes, request.alt, request.generated).await,
    )
}

async fn openapi_document() -> Response {
    Json(super::openapi::website::openapi()).into_response()
}

/// The OpenAPI 3.1 document of [`router`] and [`setup_router`], its schemas generated from
/// `adk-website`'s view types (spec 3.5 §8.2).
///
/// # Example
///
/// ```rust
/// let document = adk_surface::web::website::openapi();
/// assert!(document["components"]["schemas"]["SiteViewModel"].is_object());
/// ```
pub fn openapi() -> serde_json::Value {
    super::openapi::website::openapi()
}

/// TypeScript declarations of every schema in [`openapi`], for the web home's website views.
///
/// # Example
///
/// ```rust
/// assert!(adk_surface::web::website::typescript().contains("export interface OnboardingView {"));
/// ```
pub fn typescript() -> String {
    super::openapi::website::typescript()
}
