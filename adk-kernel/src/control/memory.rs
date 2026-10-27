//! What the agents remember, and the person's corrections (spec 3.3 §10 K5, §5.1 "What your
//! agents remember", §8.1, §8.2).
//!
//! The kernel holds no facts: the harness does (spec 3.1 §7.15.2). A [`MemoryBook`] is what
//! the harness gives the kernel, so the control plane can list the facts with their
//! provenance and apply a person's correction; `adk-rust up` builds one over each area's
//! harness.
//!
//! | Route (under `/api`) | Caller | Does |
//! |---|---|---|
//! | `GET /kernel/memory` | operator, or a device with `read` | The facts of the caller, live ones only, and the overnight tidy-ups ([`MemoryResponse`]) |
//! | `POST /kernel/memory/facts/{id}/confirm` | the person, no device | "That's right": a guess becomes "You said this" |
//! | `POST /kernel/memory/facts/{id}/fix` | the person, no device | "Fix it": the fact is replaced by the person's words ([`FixRequest`]) |
//! | `POST /kernel/memory/facts/{id}/forget` | the person, no device | "Forget it": never recalled again |
//! | `POST /kernel/memory/undo` | the person, no device | "Undo": reverts the latest overnight tidy-up, or the one named ([`UndoRequest`]) |
//!
//! Each change is recorded as `kernel.control` (`memory.confirm`, `memory.fix`,
//! `memory.forget`, `memory.undo`, the fact or tidy-up as its target) before it is applied;
//! the record carries ids, never a fact's text.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::paths;
use super::routes::{Caller, ControlBackend, record};
use super::settings::require_owner;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;

/// Where a remembered fact came from, in the home's words (spec 3.3 §8.1): a known fact
/// reads "You said this", whether the person said it or confirmed or corrected a guess, and
/// an assumed one "A guess".
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::FactOrigin;
///
/// assert_eq!(serde_json::to_value(FactOrigin::YouSaidThis).unwrap(), "youSaidThis");
/// assert_eq!(FactOrigin::YouConfirmedThis.label(), "You said this");
/// assert_eq!(FactOrigin::Guess.label(), "A guess");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum FactOrigin {
    /// The person's own words: a verified quote (`Known`, `UserStated`).
    YouSaidThis,
    /// A person confirmed or corrected it (`Known`, `UserConfirmed`); it reads "You said
    /// this" (spec 3.3 §8.1).
    YouConfirmedThis,
    /// The agents inferred or read it; it can be confirmed, fixed or forgotten (`Assumed`).
    Guess,
}

impl FactOrigin {
    /// The words the home shows (spec 3.3 §8.1): "You said this" for a known fact, "A
    /// guess" for an assumed one.
    pub fn label(self) -> &'static str {
        match self {
            Self::YouSaidThis | Self::YouConfirmedThis => "You said this",
            Self::Guess => "A guess",
        }
    }
}

/// One fact the agents remember (spec 3.3 §5.1 "What your agents remember").
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{FactOrigin, RememberedFact};
///
/// let fact = RememberedFact::new("01JB", "You send invoices on the last working day.", FactOrigin::Guess, chrono::Utc::now())
///     .with_noticed_by("finance");
/// assert_eq!(serde_json::to_value(&fact).unwrap()["noticedBy"], "finance");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RememberedFact {
    /// The fact's id.
    pub id: String,
    /// The fact, redacted as the harness stores it. Escape it before display (S-I9).
    pub text: String,
    /// Where it came from.
    pub origin: FactOrigin,
    /// What it is about: `preference`, `project_fact`, `procedure`, `outcome`, `decision`,
    /// `correction`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The area whose agents remember it; `None` for every area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<PartitionId>,
    /// The agent that noticed a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noticed_by: Option<String>,
    /// When it was learned.
    pub learned_at: DateTime<Utc>,
    /// The overnight tidy-up that wrote it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tidy_up: Option<String>,
}

impl RememberedFact {
    /// A fact `id` with `text` from `origin`, learned at `learned_at`.
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        origin: FactOrigin,
        learned_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            origin,
            kind: None,
            area: None,
            noticed_by: None,
            learned_at,
            tidy_up: None,
        }
    }

    /// Sets what it is about.
    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Sets the area.
    #[must_use]
    pub fn with_area(mut self, area: impl Into<PartitionId>) -> Self {
        self.area = Some(area.into());
        self
    }

    /// Sets the agent that noticed it.
    #[must_use]
    pub fn with_noticed_by(mut self, agent: impl Into<String>) -> Self {
        self.noticed_by = Some(agent.into());
        self
    }

    /// Sets the tidy-up that wrote it.
    #[must_use]
    pub fn with_tidy_up(mut self, tick: impl Into<String>) -> Self {
        self.tidy_up = Some(tick.into());
        self
    }
}

/// One overnight tidy-up of the memory: a consolidation tick (spec 3.2 §7.5, spec 3.3 §8.2
/// "Undo").
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::TidyUp;
///
/// let tick = TidyUp::new("01JT", "default", chrono::Utc::now()).with_counts(1, 2, 0, 0);
/// assert_eq!((tick.merged, tick.superseded), (1, 2));
/// assert!(!tick.undoable(), "an unfinished tidy-up cannot be undone");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TidyUp {
    /// The tick's id.
    pub id: String,
    /// The area it tidied.
    pub area: PartitionId,
    /// When it ran.
    pub at: DateTime<Utc>,
    /// When it finished; a tidy-up still running or cut off has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// When it was undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone_at: Option<DateTime<Utc>>,
    /// Facts it wrote by merging others.
    pub merged: usize,
    /// Facts it replaced by a merged one.
    pub superseded: usize,
    /// Facts it forgot.
    pub forgotten: usize,
    /// Facts it learned from sessions nobody had read.
    pub distilled: usize,
}

impl TidyUp {
    /// A tidy-up `id` of `area` at `at`, unfinished, that changed nothing.
    pub fn new(id: impl Into<String>, area: impl Into<PartitionId>, at: DateTime<Utc>) -> Self {
        Self {
            id: id.into(),
            area: area.into(),
            at,
            finished_at: None,
            undone_at: None,
            merged: 0,
            superseded: 0,
            forgotten: 0,
            distilled: 0,
        }
    }

    /// Sets what it changed.
    #[must_use]
    pub fn with_counts(
        mut self,
        merged: usize,
        superseded: usize,
        forgotten: usize,
        distilled: usize,
    ) -> Self {
        (self.merged, self.superseded, self.forgotten, self.distilled) =
            (merged, superseded, forgotten, distilled);
        self
    }

    /// Sets when it finished.
    #[must_use]
    pub fn finished(mut self, at: DateTime<Utc>) -> Self {
        self.finished_at = Some(at);
        self
    }

    /// Sets when it was undone.
    #[must_use]
    pub fn undone(mut self, at: DateTime<Utc>) -> Self {
        self.undone_at = Some(at);
        self
    }

    /// Whether "Undo" applies: it finished, changed something, and was not undone.
    pub fn undoable(&self) -> bool {
        self.finished_at.is_some()
            && self.undone_at.is_none()
            && self.merged + self.superseded + self.forgotten + self.distilled > 0
    }
}

/// What undoing a tidy-up did (spec 3.3 §8.2 "Undo").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TidyUpUndo {
    /// The tidy-up.
    pub id: String,
    /// Facts it had replaced or forgotten, back as guesses.
    pub restored: Vec<String>,
    /// Facts it had written, now forgotten.
    pub forgotten: Vec<String>,
    /// Facts left as they are: a person or a later write changed them since.
    pub kept: Vec<String>,
}

impl TidyUpUndo {
    /// The undo of tidy-up `id`, with the facts it restored, forgot and kept.
    pub fn new(
        id: impl Into<String>,
        restored: Vec<String>,
        forgotten: Vec<String>,
        kept: Vec<String>,
    ) -> Self {
        Self { id: id.into(), restored, forgotten, kept }
    }
}

/// What the harness gives the kernel so the control plane can show and correct the agents'
/// memory (spec 3.3 §10 K5). `user` is the caller; a book answers only that person's facts,
/// as `fact_not_found` for anyone else's.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::control::{FactOrigin, MemoryBook, RememberedFact, TidyUp, TidyUpUndo};
///
/// struct Nothing;
///
/// #[async_trait::async_trait]
/// impl MemoryBook for Nothing {
///     async fn facts(&self, _: &str, _: Option<&adk_kernel::PartitionId>) -> adk_kernel::Result<Vec<RememberedFact>> { Ok(vec![]) }
///     async fn confirm(&self, _: &str, id: &str, _: &SubjectRef) -> adk_kernel::Result<RememberedFact> { Err(adk_kernel::KernelError::NotFound { pid: id.into() }) }
///     async fn fix(&self, _: &str, id: &str, _: &str, _: &SubjectRef) -> adk_kernel::Result<RememberedFact> { Err(adk_kernel::KernelError::NotFound { pid: id.into() }) }
///     async fn forget(&self, _: &str, id: &str, _: &SubjectRef) -> adk_kernel::Result<()> { Err(adk_kernel::KernelError::NotFound { pid: id.into() }) }
///     async fn tidy_ups(&self, _: &str) -> adk_kernel::Result<Vec<TidyUp>> { Ok(vec![]) }
///     async fn undo_tidy_up(&self, _: &str, id: &str, _: &SubjectRef) -> adk_kernel::Result<TidyUpUndo> { Err(adk_kernel::KernelError::NotFound { pid: id.into() }) }
/// }
/// ```
#[async_trait]
pub trait MemoryBook: Send + Sync {
    /// The live facts of `user`, of `area` when one is named, newest first.
    ///
    /// # Errors
    ///
    /// The book's error.
    async fn facts(&self, user: &str, area: Option<&PartitionId>) -> Result<Vec<RememberedFact>>;

    /// "That's right": `user`'s fact `id` becomes "You said this".
    ///
    /// # Errors
    ///
    /// A `NotFound` error for a fact that is not `user`'s or not live, or the book's error.
    async fn confirm(&self, user: &str, id: &str, by: &SubjectRef) -> Result<RememberedFact>;

    /// "Fix it": `user`'s fact `id` is replaced by `text`, which reads "You said this"; the
    /// old one is kept in the history.
    ///
    /// # Errors
    ///
    /// As [`confirm`](Self::confirm), and an `InvalidInput` error for a text the harness
    /// refuses (empty, too long, a secret).
    async fn fix(
        &self,
        user: &str,
        id: &str,
        text: &str,
        by: &SubjectRef,
    ) -> Result<RememberedFact>;

    /// "Forget it": `user`'s fact `id` is never recalled again.
    ///
    /// # Errors
    ///
    /// As [`confirm`](Self::confirm).
    async fn forget(&self, user: &str, id: &str, by: &SubjectRef) -> Result<()>;

    /// The tidy-ups of `user`'s memory, newest first.
    ///
    /// # Errors
    ///
    /// The book's error.
    async fn tidy_ups(&self, user: &str) -> Result<Vec<TidyUp>>;

    /// "Undo": reverts tidy-up `id` of `user`'s memory.
    ///
    /// # Errors
    ///
    /// A `NotFound` error for an unknown tidy-up, a `Conflict` for one unfinished or
    /// already undone, or the book's error.
    async fn undo_tidy_up(&self, user: &str, id: &str, by: &SubjectRef) -> Result<TidyUpUndo>;
}

/// `GET /kernel/memory` query (spec 3.3 §10 K5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryQuery {
    /// Only facts of this area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<PartitionId>,
}

/// `GET /kernel/memory` answer (spec 3.3 §10 K5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryResponse {
    /// The live facts, newest first.
    pub facts: Vec<RememberedFact>,
    /// The tidy-ups, newest first.
    pub tidy_ups: Vec<TidyUp>,
}

/// The answer of "That's right" and "Fix it": the fact after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactResponse {
    /// The fact.
    pub fact: RememberedFact,
}

/// `POST /kernel/memory/facts/{id}/fix` body (spec 3.3 §8.2 "Fix it").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixRequest {
    /// The person's words.
    pub text: String,
}

/// The answer of "Forget it".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetResponse {
    /// The forgotten fact's id.
    pub forgotten: String,
}

/// `POST /kernel/memory/undo` body (spec 3.3 §8.2 "Undo").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoRequest {
    /// The tidy-up to undo; the latest undoable one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tidy_up: Option<String>,
}

type Backend = State<Arc<dyn ControlBackend>>;

/// The routes, relative to `/api`, over the backend in their state.
pub(super) fn routes() -> Router<Arc<dyn ControlBackend>> {
    Router::new()
        .route(paths::MEMORY, get(list))
        .route(paths::FACT_CONFIRM, post(confirm))
        .route(paths::FACT_FIX, post(fix))
        .route(paths::FACT_FORGET, post(forget))
        .route(paths::MEMORY_UNDO, post(undo))
}

/// The memory book of `backend`, or a `Config` error naming why there is none.
fn book_of(backend: &dyn ControlBackend) -> Result<Arc<dyn MemoryBook>> {
    backend.kernel().and_then(|kernel| kernel.config().control.memory.clone()).ok_or_else(|| {
        KernelError::Config {
            reason: "this kernel shows no memory: set ControlPlaneConfig::with_memory, as \
                     `adk-rust up` does with long-term memory on"
                .into(),
        }
    })
}

async fn list(
    State(backend): Backend,
    caller: Caller,
    Query(query): Query<MemoryQuery>,
) -> Result<Json<MemoryResponse>> {
    caller.require_operator("read what the agents remember")?;
    let book = book_of(backend.as_ref())?;
    let user = &caller.context.user_id;
    let facts = book.facts(user, query.area.as_ref()).await?;
    let tidy_ups = book.tidy_ups(user).await?;
    Ok(Json(MemoryResponse { facts, tidy_ups }))
}

async fn confirm(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<FactResponse>> {
    require_owner(&caller, "confirm what the agents remember")?;
    let book = book_of(backend.as_ref())?;
    record(backend.as_ref(), &caller, "memory.confirm", Some(&id), &[]).await?;
    let fact = book.confirm(&caller.context.user_id, &id, &caller.subject()).await?;
    Ok(Json(FactResponse { fact }))
}

async fn fix(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
    Json(request): Json<FixRequest>,
) -> Result<Json<FactResponse>> {
    require_owner(&caller, "fix what the agents remember")?;
    let book = book_of(backend.as_ref())?;
    record(backend.as_ref(), &caller, "memory.fix", Some(&id), &[]).await?;
    let fact = book.fix(&caller.context.user_id, &id, &request.text, &caller.subject()).await?;
    Ok(Json(FactResponse { fact }))
}

async fn forget(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ForgetResponse>> {
    require_owner(&caller, "make the agents forget")?;
    let book = book_of(backend.as_ref())?;
    record(backend.as_ref(), &caller, "memory.forget", Some(&id), &[]).await?;
    book.forget(&caller.context.user_id, &id, &caller.subject()).await?;
    Ok(Json(ForgetResponse { forgotten: id }))
}

async fn undo(
    State(backend): Backend,
    caller: Caller,
    Json(request): Json<UndoRequest>,
) -> Result<Json<TidyUpUndo>> {
    require_owner(&caller, "undo a tidy-up of the memory")?;
    let book = book_of(backend.as_ref())?;
    let user = &caller.context.user_id;
    let id = match request.tidy_up {
        Some(id) => id,
        None => book
            .tidy_ups(user)
            .await?
            .into_iter()
            .find(TidyUp::undoable)
            .map(|tidy_up| tidy_up.id)
            .ok_or_else(|| {
                KernelError::Core(adk_core::AdkError::new(
                    adk_core::ErrorComponent::Server,
                    adk_core::ErrorCategory::NotFound,
                    crate::error::NOT_FOUND_CODE,
                    "no overnight tidy-up of the memory is left to undo",
                ))
            })?,
    };
    record(backend.as_ref(), &caller, "memory.undo", Some(&id), &[]).await?;
    Ok(Json(book.undo_tidy_up(user, &id, &caller.subject()).await?))
}
