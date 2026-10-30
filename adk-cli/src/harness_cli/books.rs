//! What `adk-rust up` gives the kernel's control plane for the routes the surfaces need
//! (spec 3.3 §10 K3, K5): the memory book over each area's harness, the record book over the
//! kernel's signed audit log, and the mode store through which an area's mode, changed while
//! the kernel runs, governs its agents from their next step.

use std::sync::Arc;

use adk_core::AdkError;
#[cfg(any(feature = "persist", feature = "postgres"))]
use adk_core::principal::SubjectRef;
#[cfg(any(feature = "persist", feature = "postgres"))]
use adk_core::{ErrorCategory, ErrorComponent, UserId};
#[cfg(any(feature = "persist", feature = "postgres"))]
use adk_harness::{FactSource, FactStatus, FactStore, MemoryFact};
use adk_harness::{HarnessMode, ModeRecord, ModeSource, ModeStore};
#[cfg(any(feature = "persist", feature = "postgres", feature = "audit"))]
use adk_kernel::KernelError;
use adk_kernel::control::ControlStore;
#[cfg(any(feature = "persist", feature = "postgres"))]
use adk_kernel::control::{FactOrigin, MemoryBook, RememberedFact, TidyUp, TidyUpUndo};
use adk_kernel::{PartitionId, PartitionMode};
use async_trait::async_trait;

use super::APP_NAME;

/// The mode an area's agents start in, as the kernel reports it (spec 3.3 §10 K3).
pub(super) fn partition_mode(mode: HarnessMode) -> PartitionMode {
    match mode {
        HarnessMode::ReadOnly => PartitionMode::ReadOnly,
        HarnessMode::Plan => PartitionMode::Plan,
        HarnessMode::Auto => PartitionMode::Auto,
        _ => PartitionMode::Ask,
    }
}

/// A harness's [`ModeStore`] that reads, for a session, the mode its area was last set to
/// while the kernel runs (spec 3.3 §10 K3). Every part of the harness reads the mode through
/// its store on every step, so the change governs the next one.
///
/// | Session | Mode |
/// |---|---|
/// | Its own record is newer than the area's change | Its own |
/// | Its own record is older, set by a person, an approved plan or the harness itself (the loop guard, a follow-up's pin, K-I7) | The stricter of the two: the area tightens it, never loosens it |
/// | No record of its own | The area's |
///
/// The change holds while the area's harness still starts in the mode it replaced, so a later
/// `adk-rust up --mode` or `adk-rust partitions set-mode` takes over; a change to Go ahead
/// is not applied where `adk-rust up` refuses `--mode auto`. The kernel refuses a looser
/// area mode while an agent of the area is working, so a loosening reaches a session from
/// its next turn (spec 3.1 I14).
pub(super) struct AreaModeStore {
    inner: Arc<dyn ModeStore>,
    control: Arc<dyn ControlStore>,
    area: PartitionId,
    /// The mode the area's harness started in.
    start: HarnessMode,
    /// `adk-rust up` refuses Go ahead here: beyond loopback without `--sandbox`.
    auto_refused: bool,
}

impl AreaModeStore {
    pub(super) fn new(
        inner: Arc<dyn ModeStore>,
        control: Arc<dyn ControlStore>,
        area: PartitionId,
        start: HarnessMode,
        auto_refused: bool,
    ) -> Self {
        Self { inner, control, area, start, auto_refused }
    }
}

#[async_trait]
impl ModeStore for AreaModeStore {
    async fn get(
        &self,
        app: &str,
        user: &str,
        session: &str,
    ) -> adk_core::Result<Option<ModeRecord>> {
        let record = self.inner.get(app, user, session).await?;
        // A store that cannot be read fails the read, which the policy takes as read-only.
        let settings = self.control.area_settings(&self.area).await.map_err(AdkError::from)?;
        let change = settings
            .as_ref()
            .and_then(|settings| settings.mode_over(Some(partition_mode(self.start))))
            .filter(|change| !(self.auto_refused && change.value == PartitionMode::Auto));
        let Some(change) = change else {
            return Ok(record);
        };
        let mode = HarnessMode::from(change.value);
        match record {
            Some(record) if record.at >= change.at || !mode.is_stricter_than(record.mode) => {
                Ok(Some(record))
            }
            record => {
                let mut area = ModeRecord::new(mode, ModeSource::User);
                area.at = change.at;
                area.revision = record.map_or(0, |record| record.revision);
                Ok(Some(area))
            }
        }
    }

    async fn set(
        &self,
        app: &str,
        user: &str,
        session: &str,
        record: ModeRecord,
    ) -> adk_core::Result<()> {
        self.inner.set(app, user, session, record).await
    }
}

/// The memory of each area's harness, read and corrected through the harness, so recall,
/// the index and the harness's own audit records follow every correction (spec 3.3 §10 K5).
/// Built over `harness.db`'s fact store (feature `persist`) or the `--store` backend's
/// (feature `postgres`).
#[cfg(any(feature = "persist", feature = "postgres"))]
pub(super) struct HarnessMemoryBook {
    areas: Vec<(PartitionId, Arc<adk_harness::Harness>)>,
    store: Arc<dyn FactStore>,
}

#[cfg(any(feature = "persist", feature = "postgres"))]
impl HarnessMemoryBook {
    /// The book over `areas`' harnesses, whose facts and tidy-ups `store` keeps.
    pub(super) fn new(
        areas: Vec<(PartitionId, Arc<adk_harness::Harness>)>,
        store: Arc<dyn FactStore>,
    ) -> Self {
        Self { areas, store }
    }
}

/// The error a fact no area's harness holds answers with.
#[cfg(any(feature = "persist", feature = "postgres"))]
fn no_fact(id: &str) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::NotFound,
        "harness.memory.fact_not_found",
        format!("no fact {id} of yours is remembered"),
    ))
}

/// The caller as the harness names users.
#[cfg(any(feature = "persist", feature = "postgres"))]
fn user_id(user: &str) -> adk_kernel::Result<UserId> {
    UserId::new(user).map_err(|error| {
        KernelError::Core(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::InvalidInput,
            "harness.memory.invalid_user",
            format!("`{user}` is not a user id: {error}"),
        ))
    })
}

/// Whether `error` says the harness holds no such fact of the user.
#[cfg(any(feature = "persist", feature = "postgres"))]
fn not_held(error: &AdkError) -> bool {
    error.code == "harness.memory.fact_not_found"
}

/// A fact as the home shows it; a guess names `noticed_by`, the agent that noticed it.
#[cfg(any(feature = "persist", feature = "postgres"))]
fn remembered(fact: MemoryFact, area: &PartitionId, noticed_by: Option<String>) -> RememberedFact {
    let origin = match (&fact.status, fact.provenance.source) {
        (FactStatus::Known, FactSource::UserStated) => FactOrigin::YouSaidThis,
        (FactStatus::Known, _) => FactOrigin::YouConfirmedThis,
        _ => FactOrigin::Guess,
    };
    let mut remembered = RememberedFact::new(fact.id, fact.text, origin, fact.valid_from)
        .with_kind(fact.kind.as_str());
    if fact.scope.project.is_some() {
        remembered = remembered.with_area(area.clone());
    }
    if origin == FactOrigin::Guess
        && let Some(agent) = noticed_by
    {
        remembered = remembered.with_noticed_by(agent);
    }
    match fact.provenance.tick {
        Some(tick) => remembered.with_tidy_up(tick),
        None => remembered,
    }
}

/// The agent whose session `session` of `user` is: the stamp its harness writes on the
/// session's first run (spec 3.3 §5.7 "Threads"), or `None` for a session no agent stamped,
/// such as an overnight tidy-up's own.
#[cfg(any(feature = "persist", feature = "postgres"))]
async fn session_agent(
    harness: &adk_harness::Harness,
    user: &str,
    session: &str,
) -> Option<String> {
    let session = harness
        .runner()
        .session_service()
        .get(adk_session::GetRequest {
            app_name: harness.app_name().to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
            num_recent_events: Some(0),
            after: None,
        })
        .await
        .ok()?;
    session.state().get(adk_harness::AGENT_STATE_KEY)?.as_str().map(str::to_string)
}

#[cfg(any(feature = "persist", feature = "postgres"))]
#[async_trait]
impl MemoryBook for HarnessMemoryBook {
    async fn facts(
        &self,
        user: &str,
        area: Option<&PartitionId>,
    ) -> adk_kernel::Result<Vec<RememberedFact>> {
        let id = user_id(user)?;
        let mut facts: Vec<RememberedFact> = Vec::new();
        for (partition, harness) in &self.areas {
            if area.is_some_and(|area| area != partition) {
                continue;
            }
            // The agent each session belongs to, read once per session.
            let mut agents: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            for fact in harness.facts(&id).await.map_err(KernelError::Core)? {
                if fact.status.is_live() && !facts.iter().any(|known| known.id == fact.id) {
                    let session = &fact.provenance.session_id;
                    let noticed_by = if fact.status == FactStatus::Known {
                        None
                    } else if let Some(agent) = agents.get(session) {
                        agent.clone()
                    } else {
                        let agent = session_agent(harness, user, session).await;
                        agents.insert(session.clone(), agent.clone());
                        agent
                    };
                    facts.push(remembered(fact, partition, noticed_by));
                }
            }
        }
        facts.sort_by_key(|fact| std::cmp::Reverse(fact.learned_at));
        Ok(facts)
    }

    async fn confirm(
        &self,
        user: &str,
        id: &str,
        _by: &SubjectRef,
    ) -> adk_kernel::Result<RememberedFact> {
        let user = user_id(user)?;
        for (area, harness) in &self.areas {
            match harness.accept_fact(&user, id).await {
                Err(error) if not_held(&error) => continue,
                result => {
                    return result
                        .map(|fact| remembered(fact, area, None))
                        .map_err(KernelError::Core);
                }
            }
        }
        Err(no_fact(id))
    }

    async fn fix(
        &self,
        user: &str,
        id: &str,
        text: &str,
        _by: &SubjectRef,
    ) -> adk_kernel::Result<RememberedFact> {
        let user = user_id(user)?;
        for (area, harness) in &self.areas {
            match harness.correct_fact(&user, id, text).await {
                Err(error) if not_held(&error) => continue,
                result => {
                    return result
                        .map(|fact| remembered(fact, area, None))
                        .map_err(KernelError::Core);
                }
            }
        }
        Err(no_fact(id))
    }

    async fn forget(&self, user: &str, id: &str, _by: &SubjectRef) -> adk_kernel::Result<()> {
        let user = user_id(user)?;
        for (_, harness) in &self.areas {
            match harness.forget_fact(&user, id).await {
                Err(error) if not_held(&error) => continue,
                result => return result.map_err(KernelError::Core),
            }
        }
        Err(no_fact(id))
    }

    async fn tidy_ups(&self, user: &str) -> adk_kernel::Result<Vec<TidyUp>> {
        let records = self.store.consolidations(APP_NAME, user).await.map_err(KernelError::Core)?;
        Ok(records
            .into_iter()
            .map(|record| {
                let mut tidy_up =
                    TidyUp::new(&record.tick_id, record.partition.as_str(), record.at).with_counts(
                        record.merged.len(),
                        record.superseded().count(),
                        record.forgotten().count(),
                        record.distilled.len(),
                    );
                if let Some(at) = record.finished_at {
                    tidy_up = tidy_up.finished(at);
                }
                if let Some(at) = record.undone_at {
                    tidy_up = tidy_up.undone(at);
                }
                tidy_up
            })
            .collect())
    }

    async fn undo_tidy_up(
        &self,
        user: &str,
        id: &str,
        by: &SubjectRef,
    ) -> adk_kernel::Result<TidyUpUndo> {
        let undone = adk_harness::undo_consolidation(self.store.as_ref(), APP_NAME, user, id, by)
            .await
            .map_err(KernelError::Core)?;
        Ok(TidyUpUndo::new(undone.tick_id, undone.restored, undone.forgotten, undone.kept))
    }
}

/// The record of the kernel's signed audit log, checked as `adk-rust audit verify` checks an
/// export of it (spec 3.3 §10 K5).
#[cfg(feature = "audit")]
pub(super) struct SignedRecordBook {
    logs: Arc<dyn adk_core::audit_log::AuditLogStore>,
    log_id: String,
    verifier: adk_auth::signed_audit::NoteVerifier,
}

#[cfg(feature = "audit")]
impl SignedRecordBook {
    pub(super) fn new(
        logs: Arc<dyn adk_core::audit_log::AuditLogStore>,
        log_id: String,
        verifier: adk_auth::signed_audit::NoteVerifier,
    ) -> Self {
        Self { logs, log_id, verifier }
    }
}

/// An action on record as structured fields.
#[cfg(feature = "audit")]
fn record_line(record: adk_core::audit_log::AuditRecord) -> adk_kernel::control::RecordLine {
    use serde_json::Value;

    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
    let wire = |value: Value| value.as_str().map(str::to_string);
    let mut line =
        adk_kernel::control::RecordLine::new(record.seq, record.ts, record.action.clone());
    // A kernel record names what it did in its `action` field.
    if record.action.starts_with("kernel.")
        && let Some(action) = text(record.ext.get("action"))
    {
        line = line.with_kernel_action(action);
    }
    let subject = record
        .actor
        .on_behalf_of
        .as_ref()
        .map(|subject| subject.id.clone())
        .or_else(|| text(record.ext.get("subject").and_then(|subject| subject.get("id"))));
    if let Some(subject) = subject {
        line = line.with_subject(subject);
    }
    // The agent that acted, or for a person's answer the agent whose call it decided.
    let agent = record
        .actor
        .agent
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| text(record.ext.get("actingAgent")));
    // A drawn form a person sent names the agent that drew it and the form (spec 3.3 §7.4
    // rule 5): "You sent the invoice form Finance drew".
    let (agent, record_target) = match record.action.as_str() {
        "surface.drawn.submitted" => {
            (agent.or_else(|| text(record.ext.get("drawnBy"))), text(record.ext.get("surfaceId")))
        }
        _ => (agent, None),
    };
    if let Some(agent) = agent {
        line = line.with_agent(agent);
    }
    // A folder rule's line says what the rule covers in plain words, "file writes in notes/":
    // its own pattern matches any tool name, and its tools stay in the record's data
    // (spec 3.3 §8.2).
    let tool =
        text(record.ext.get("covers")).or_else(|| text(record.ext.get("tool"))).or_else(|| {
            matches!(record.kind, adk_core::audit_log::AuditRecordKind::ToolCall)
                .then(|| record.action.clone())
        });
    if let Some(tool) = tool {
        line = line.with_tool(tool);
    }
    // A kernel record of an answer carries it as `answer`: a device's action, or a chat
    // thread's `allowOnce` or `deny`.
    let answered = text(record.ext.get("answer")).and_then(|answer| match answer.as_str() {
        "approve" | "allowOnce" => Some("approved".to_string()),
        "reject" | "deny" => Some("rejected".to_string()),
        "edit" => Some("edited".to_string()),
        "respond" => Some("responded".to_string()),
        _ => None,
    });
    // A standing rule's record carries the rule's own decision, to allow or to refuse.
    let ruled =
        record.ext.get("decision").and_then(|decision| text(decision.get("kind"))).and_then(
            |kind| match kind.as_str() {
                "allow" => Some("allowed".to_string()),
                "deny" => Some("denied".to_string()),
                _ => None,
            },
        );
    if let Some(decision) = record
        .decision
        .and_then(|d| serde_json::to_value(d).ok())
        .and_then(wire)
        .or(answered)
        .or(ruled)
    {
        line = line.with_decision(decision);
    }
    let outcome = record
        .outcome
        .and_then(|outcome| serde_json::to_value(outcome).ok())
        .and_then(wire)
        .or_else(|| text(record.ext.get("outcome")));
    if let Some(outcome) = outcome {
        line = line.with_outcome(outcome);
    }
    if let Some(surface) = text(record.ext.get("surface")) {
        line = line.with_surface(surface);
    }
    // The device's name for an answer a paired device gave; its id names nothing a person
    // reads (spec 3.3 §8).
    let device = text(record.ext.get("deviceName"));
    match record.ext.get("origin").and_then(|origin| Some((origin, text(origin.get("kind"))?))) {
        Some((origin, how)) => {
            let channel = text(origin.get("channel")).or(device);
            line = line.with_how(how, channel.as_deref());
        }
        None => {
            if let Some(device) = device {
                line = line.with_channel(device);
            }
        }
    }
    let target = text(record.ext.get("target"))
        .or_else(|| text(record.ext.get("approvalId")))
        .or(record_target);
    if let Some(target) = target {
        line = line.with_target(target);
    }
    // The item a screen action, or an answer to one, acted on, as the person knows it (§8).
    if let Some(title) = text(record.ext.get("title")) {
        line = line.with_title(title);
    }
    // A call's publisher and what decided it, so a surface words a screen action's call by
    // its own records, never by the tool's name (§7.1, §8).
    if let Some(source) = text(record.ext.get("toolSource")) {
        line = line.with_call(source, record.policy_ref.as_deref());
    }
    // A screen action (spec 3.3 §2 item 7): the action as its tool, the item's title as what
    // it acted on, and for an agent's, where the person said yes.
    if record.action == "surface.verb" {
        if let Some(verb) = text(record.ext.get("verb")) {
            line = line.with_tool(verb);
        }
        // The item as the person knows it, never its kernel id (spec 3.3 §8).
        line.target = text(record.ext.get("title"));
        if let Some(answered) = record.ext.get("answered") {
            line = line.with_decision("approved");
            if let Some(surface) = text(answered.get("surface")) {
                line = line.with_surface(surface);
            }
            if let Some(origin) = answered.get("origin")
                && let Some(how) = text(origin.get("kind"))
            {
                line = line.with_how(how, text(origin.get("channel")).as_deref());
            }
        }
    }
    line
}

#[cfg(feature = "audit")]
#[async_trait]
impl adk_kernel::control::RecordBook for SignedRecordBook {
    async fn recent(
        &self,
        limit: usize,
    ) -> adk_kernel::Result<Vec<adk_kernel::control::RecordLine>> {
        let size = self.logs.size(&self.log_id).await.map_err(KernelError::Core)?;
        let start = size.saturating_sub(u64::try_from(limit).unwrap_or(u64::MAX));
        let entries = self.logs.read(&self.log_id, start..size).await.map_err(KernelError::Core)?;
        Ok(entries
            .into_iter()
            .rev()
            .filter_map(|entry| entry.record)
            .filter_map(|text| serde_json::from_str(&text).ok())
            .map(record_line)
            .collect())
    }

    async fn check(&self) -> adk_kernel::Result<adk_kernel::control::RecordCheck> {
        let export =
            adk_auth::signed_audit::AuditExport::from_store(self.logs.as_ref(), &self.log_id)
                .await
                .map_err(KernelError::Core)?;
        let report = export.verify(std::slice::from_ref(&self.verifier));
        let mut check = adk_kernel::control::RecordCheck::new(
            report.is_valid(),
            report.records,
            chrono::Utc::now(),
        )
        .with_report(serde_json::to_value(&report)?);
        if let Some(failure) = report.failures.first() {
            check = check.with_problem(failure.to_string());
        }
        Ok(check)
    }
}

#[cfg(test)]
mod tests {
    use adk_harness::InMemoryModeStore;
    #[cfg(feature = "persist")]
    use adk_harness::{
        ConsolidationRecord, FactKind, FactScope, InMemoryFactStore, InstructionsConfig, Provenance,
    };
    use adk_kernel::control::{AreaSettings, Change, InMemoryControlStore};
    #[cfg(feature = "persist")]
    use adk_model::{ScriptedLlm, ScriptedResponse};

    use super::*;

    /// A harness with long-term memory in `store`, over a fresh workspace.
    #[cfg(feature = "persist")]
    fn harness(store: Arc<InMemoryFactStore>) -> (tempfile::TempDir, Arc<adk_harness::Harness>) {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedLlm::new("m", Vec::<ScriptedResponse>::new()));
        let agent = Arc::new(
            adk_harness::HarnessAgent::builder("harness")
                .model(model)
                .workspace(adk_devtools::Workspace::new(dir.path()))
                .project_instructions(InstructionsConfig {
                    user_file: None,
                    require_trust: false,
                    ..InstructionsConfig::default()
                })
                .without_compaction()
                .long_term_memory(adk_harness::LongTermMemoryConfig::default())
                .fact_store(store)
                .build()
                .unwrap(),
        );
        let runner = adk_runner::Runner::builder()
            .app_name(APP_NAME)
            .agent(agent.agent())
            .session_service(Arc::new(adk_session::InMemorySessionService::new()))
            .build()
            .unwrap();
        (dir, Arc::new(adk_harness::Harness::with_runner(agent, runner).unwrap()))
    }

    #[cfg(feature = "persist")]
    fn fact(
        dir: &tempfile::TempDir,
        text: &str,
        status: FactStatus,
        source: FactSource,
    ) -> MemoryFact {
        let scope = FactScope::new(APP_NAME, "ana", Some(adk_harness::project_slug(dir.path())));
        MemoryFact::new(
            scope,
            FactKind::Preference,
            text,
            status,
            Provenance::new(source, "s1", "inv-1"),
        )
    }

    /// The book lists the person's live facts in the home's words, and confirms, fixes and
    /// forgets them through the harness that holds them (spec 3.3 §10 K5).
    #[cfg(feature = "persist")]
    #[tokio::test]
    async fn the_memory_book_shows_provenance_and_corrects_through_the_harness() {
        let store = Arc::new(InMemoryFactStore::new());
        let (dir, harness) = harness(store.clone());
        let said = store
            .put(fact(&dir, "Use pnpm, not npm.", FactStatus::Known, FactSource::UserStated))
            .await
            .unwrap();
        let guess = store
            .put(fact(
                &dir,
                "You send invoices on the last working day.",
                FactStatus::Assumed,
                FactSource::AgentInferred,
            ))
            .await
            .unwrap();
        let old = store
            .put(fact(
                &dir,
                "Your accountant is Bo.",
                FactStatus::Assumed,
                FactSource::ToolObserved,
            ))
            .await
            .unwrap();
        // The session the guesses were learned in is Finance's thread, stamped on its first run.
        harness
            .runner()
            .session_service()
            .create(adk_session::CreateRequest {
                app_name: APP_NAME.into(),
                user_id: "ana".into(),
                session_id: Some("s1".into()),
                state: [(adk_harness::AGENT_STATE_KEY.to_string(), serde_json::json!("finance"))]
                    .into_iter()
                    .collect(),
            })
            .await
            .unwrap();
        // On a kernel without areas too: the noticer is the agent, not the area.
        let single =
            HarnessMemoryBook::new(vec![(PartitionId::default(), harness.clone())], store.clone());
        let listed = single.facts("ana", None).await.unwrap();
        assert_eq!(
            listed.iter().find(|fact| fact.id == guess.id).unwrap().noticed_by.as_deref(),
            Some("finance")
        );
        let book = HarnessMemoryBook::new(vec![(PartitionId::new("work"), harness)], store.clone());
        let by = SubjectRef::human("ana");

        let facts = book.facts("ana", None).await.unwrap();
        let origin = |id: &str| facts.iter().find(|fact| fact.id == id).map(|fact| fact.origin);
        assert_eq!(origin(&said.id), Some(FactOrigin::YouSaidThis));
        assert_eq!(origin(&guess.id), Some(FactOrigin::Guess));
        // A guess names the agent that noticed it, never the area it was noticed in.
        let noticed =
            |id: &str| facts.iter().find(|fact| fact.id == id).unwrap().noticed_by.clone();
        assert_eq!(noticed(&guess.id).as_deref(), Some("finance"));
        assert_eq!(noticed(&said.id), None, "what the person said names no noticer");
        assert!(book.facts("bo", None).await.unwrap().is_empty(), "facts are the caller's own");

        assert_eq!(
            book.confirm("ana", &guess.id, &by).await.unwrap().origin,
            FactOrigin::YouConfirmedThis
        );
        let fixed = book.fix("ana", &old.id, "Your accountant is Cy.", &by).await.unwrap();
        assert_eq!(
            (fixed.text.as_str(), fixed.origin),
            ("Your accountant is Cy.", FactOrigin::YouConfirmedThis)
        );
        book.forget("ana", &said.id, &by).await.unwrap();
        let left: Vec<String> =
            book.facts("ana", None).await.unwrap().into_iter().map(|fact| fact.text).collect();
        assert_eq!(left.len(), 2);
        assert!(
            left.contains(&"Your accountant is Cy.".to_string())
                && !left.contains(&"Use pnpm, not npm.".to_string())
        );
        let error = AdkError::from(book.forget("bo", &guess.id, &by).await.unwrap_err());
        assert_eq!(error.category, ErrorCategory::NotFound, "another person's fact is not theirs");
    }

    /// Tidy-ups come from the fact store's consolidation records, and undo reverts one
    /// (spec 3.2 §7.5, spec 3.3 §8.2 "Undo").
    #[cfg(feature = "persist")]
    #[tokio::test]
    async fn the_memory_book_lists_and_undoes_tidy_ups() {
        let store = Arc::new(InMemoryFactStore::new());
        let (dir, harness) = harness(store.clone());
        let merged = store
            .put(fact(
                &dir,
                "Your accountant is Cy.",
                FactStatus::Assumed,
                FactSource::AgentInferred,
            ))
            .await
            .unwrap();
        let now = chrono::Utc::now();
        let mut record = ConsolidationRecord::new("01JTICK", APP_NAME, "ana", "default", now);
        record.merged = vec![merged.id.clone()];
        record.finished_at = Some(now);
        store.put_consolidation(&record).await.unwrap();
        let book = HarnessMemoryBook::new(vec![(PartitionId::default(), harness)], store.clone());

        let tidy_ups = book.tidy_ups("ana").await.unwrap();
        assert_eq!(tidy_ups.len(), 1);
        assert!(tidy_ups[0].undoable());
        let undone = book.undo_tidy_up("ana", "01JTICK", &SubjectRef::human("ana")).await.unwrap();
        assert_eq!(undone.forgotten, std::slice::from_ref(&merged.id));
        assert!(!book.tidy_ups("ana").await.unwrap()[0].undoable(), "a tidy-up is undone once");
    }

    /// An area's mode governs a session with no mode of its own, and tightens one whose own
    /// mode is older, never loosens it, whoever set it; a session set after the change keeps
    /// its own (spec 3.3 §10 K3).
    #[tokio::test]
    async fn the_area_mode_tightens_a_session_and_never_loosens_it() {
        let inner = Arc::new(InMemoryModeStore::new());
        let control = Arc::new(InMemoryControlStore::new());
        let store = AreaModeStore::new(
            inner.clone(),
            control.clone(),
            PartitionId::default(),
            HarnessMode::Ask,
            false,
        );
        assert!(
            store.get(APP_NAME, "ana", "s1").await.unwrap().is_none(),
            "no change: the default"
        );
        let earlier = chrono::Utc::now() - chrono::Duration::minutes(5);
        let mut session = ModeRecord::new(HarnessMode::Auto, ModeSource::User);
        (session.revision, session.at) = (3, earlier);
        inner.set(APP_NAME, "ana", "s1", session).await.unwrap();
        // The loop guard froze s2 before the area was changed (spec 3.1 §7.4).
        let mut frozen = ModeRecord::new(HarnessMode::ReadOnly, ModeSource::SystemTighten);
        frozen.at = earlier;
        inner.set(APP_NAME, "ana", "s2", frozen).await.unwrap();

        let set = |mode| {
            let control = control.clone();
            async move {
                let change = Change::new(mode, "ana", chrono::Utc::now());
                let settings = AreaSettings::new("default").with_mode(change);
                control.put_area_settings(settings).await.unwrap();
            }
        };
        set(PartitionMode::Plan).await;
        let read = store.get(APP_NAME, "ana", "s1").await.unwrap().unwrap();
        assert_eq!((read.mode, read.revision), (HarnessMode::Plan, 3), "tightened");
        let read = store.get(APP_NAME, "ana", "s2").await.unwrap().unwrap();
        assert_eq!(
            (read.mode, read.set_by),
            (HarnessMode::ReadOnly, ModeSource::SystemTighten),
            "the loop guard's tightening holds"
        );
        let fresh = store.get(APP_NAME, "ana", "s3").await.unwrap().unwrap();
        assert_eq!(fresh.mode, HarnessMode::Plan, "a session without a mode takes the area's");

        set(PartitionMode::Auto).await;
        assert_eq!(
            store.get(APP_NAME, "ana", "s2").await.unwrap().unwrap().mode,
            HarnessMode::ReadOnly,
            "an area set to Go ahead never loosens a frozen session"
        );
        assert_eq!(
            store.get(APP_NAME, "ana", "s3").await.unwrap().unwrap().mode,
            HarnessMode::Auto
        );

        let mut later = ModeRecord::new(HarnessMode::Ask, ModeSource::User);
        later.at = chrono::Utc::now() + chrono::Duration::seconds(1);
        store.set(APP_NAME, "ana", "s2", later).await.unwrap();
        assert_eq!(
            store.get(APP_NAME, "ana", "s2").await.unwrap().unwrap().mode,
            HarnessMode::Ask,
            "a session set after the change keeps its own"
        );
    }

    /// A change made while the area started in another mode gives way to the mode it starts
    /// in now, and a change to Go ahead is not applied where `adk-rust up` refuses it (spec
    /// 3.3 §10 K3).
    #[tokio::test]
    async fn a_new_start_mode_and_a_refused_go_ahead_take_no_change() {
        let inner = Arc::new(InMemoryModeStore::new());
        let control = Arc::new(InMemoryControlStore::new());
        let started_in = |mode| adk_kernel::control::AreaStart::new(Vec::new(), None, Some(mode));
        let change = |mode| Change::new(mode, "ana", chrono::Utc::now());
        let settings = AreaSettings::new("default")
            .with_mode(change(PartitionMode::Auto))
            .with_start(started_in(PartitionMode::Ask));
        control.put_area_settings(settings).await.unwrap();
        let modes = |start, auto_refused| {
            AreaModeStore::new(
                inner.clone(),
                control.clone(),
                PartitionId::default(),
                start,
                auto_refused,
            )
        };
        let same = modes(HarnessMode::Ask, false).get(APP_NAME, "ana", "s1").await.unwrap();
        assert_eq!(same.map(|record| record.mode), Some(HarnessMode::Auto), "the change holds");
        let restarted = modes(HarnessMode::ReadOnly, false).get(APP_NAME, "ana", "s1").await;
        assert!(restarted.unwrap().is_none(), "`up --mode read-only` takes over");
        let refused = modes(HarnessMode::Ask, true).get(APP_NAME, "ana", "s1").await;
        assert!(refused.unwrap().is_none(), "Go ahead beyond loopback without a sandbox");
    }

    /// A device's answer reads as what it did, the answer and the device it came from, and a
    /// kernel record names its own action (spec 3.3 §11).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn a_record_line_says_what_a_kernel_record_did_and_where_an_answer_came_from() {
        use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
        use adk_core::audit_log::{
            AuditActor, AuditLogStore, AuditRecordDraft, AuditRecordKind, InMemoryAuditLogStore,
        };
        use adk_core::principal::SubjectRef;
        use adk_kernel::control::RecordBook;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Other("kernel".into()), "kernel.control")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .ext("action", serde_json::json!("approvals.decide"))
                .ext("target", serde_json::json!("apr_1"))
                .ext("surface", serde_json::json!("device"))
                .ext("device", serde_json::json!("e2a1"))
                .ext("deviceName", serde_json::json!("Ana's phone"))
                .ext("answer", serde_json::json!("approve")),
        )
        .await
        .unwrap();
        let book = SignedRecordBook::new(logs, "kernel-test".into(), verifier);
        let line = book.recent(1).await.unwrap().remove(0);
        assert_eq!(line.kernel_action.as_deref(), Some("approvals.decide"));
        assert_eq!(line.decision.as_deref(), Some("approved"));
        assert_eq!(line.channel.as_deref(), Some("Ana's phone"));
        assert_eq!(line.target.as_deref(), Some("apr_1"));
    }

    /// An answer names the agent whose call it decided and the surface it came from, never a
    /// device's id, and a rule's record says whether the rule allows or refuses (spec 3.3 §8,
    /// §11).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn a_record_line_names_the_agent_the_surface_and_a_rules_decision() {
        use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
        use adk_core::audit_log::{
            AuditActor, AuditDecision, AuditLogStore, AuditRecordDraft, AuditRecordKind,
            InMemoryAuditLogStore,
        };
        use adk_kernel::control::RecordBook;
        use serde_json::json;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        // The engine's record of a paired phone's answer and of the tray's (`audit_decide`).
        let answer = |surface: &str, how: &str, decision: AuditDecision| {
            AuditRecordDraft::new(AuditRecordKind::Approval, "surface.decide")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .decision(decision)
                .ext("surface", json!(surface))
                .ext("origin", json!({ "kind": how }))
                .ext("approvalId", json!("apr_1"))
                .ext("tool", json!("send_email"))
                .ext("outcome", json!("decided"))
                .ext("actingAgent", json!("urn:adk:agent:adk-rust/finance"))
        };
        log.append(
            answer("phone", "touch", AuditDecision::Approved)
                .ext("device", json!("9f3a2b1c0d4e5f67")),
        )
        .await
        .unwrap();
        log.append(answer("tray", "pointer", AuditDecision::Rejected)).await.unwrap();
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Other("harness".into()), "harness.rule.created")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .ext("tool", json!("web_fetch"))
                .ext("decision", json!({ "kind": "deny", "reason": "no browsing" })),
        )
        .await
        .unwrap();
        let book = SignedRecordBook::new(logs, "kernel-test".into(), verifier);
        let lines = book.recent(3).await.unwrap();
        let (rule, tray, phone) = (&lines[0], &lines[1], &lines[2]);
        assert_eq!(
            (phone.how.as_deref(), phone.surface.as_deref(), phone.channel.as_deref()),
            (Some("touch"), Some("phone"), None),
            "a phone's answer names no device id"
        );
        assert_eq!(phone.agent.as_deref(), Some("urn:adk:agent:adk-rust/finance"));
        assert_eq!(
            (tray.how.as_deref(), tray.surface.as_deref(), tray.decision.as_deref()),
            (Some("pointer"), Some("tray"), Some("rejected"))
        );
        assert_eq!(
            (rule.tool.as_deref(), rule.decision.as_deref()),
            (Some("web_fetch"), Some("denied"))
        );
    }

    /// A denied screen action's answer names its item as the person knows it, and keeps the
    /// decision's id as what it acted on (spec 3.3 §8, §11).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn a_denied_screen_actions_line_names_its_item() {
        use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
        use adk_core::audit_log::{
            AuditActor, AuditDecision, AuditLogStore, AuditRecordDraft, AuditRecordKind,
            InMemoryAuditLogStore,
        };
        use adk_kernel::control::RecordBook;
        use serde_json::json;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        // The engine's record of a denial in the home (`audit_decide`).
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Approval, "surface.decide")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .decision(AuditDecision::Rejected)
                .ext("surface", json!("web"))
                .ext("origin", json!({ "kind": "pointer" }))
                .ext("approvalId", json!("apr_1"))
                .ext("tool", json!("verb_snooze"))
                .ext("answer", json!("deny"))
                .ext("outcome", json!("decided"))
                .ext("actingAgent", json!("urn:adk:agent:adk-rust/finance"))
                .ext("title", json!("News digest"))
                .ext("summary", json!("Move News digest to 19:00")),
        )
        .await
        .unwrap();
        let book = SignedRecordBook::new(logs, "kernel-test".into(), verifier);
        let line = book.recent(1).await.unwrap().remove(0);
        let at = line.at;
        assert_eq!(
            line,
            adk_kernel::control::RecordLine::new(0, at, "surface.decide")
                .with_subject("ana")
                .with_agent("urn:adk:agent:adk-rust/finance")
                .with_tool("verb_snooze")
                .with_decision("rejected")
                .with_outcome("decided")
                .with_surface("web")
                .with_how("pointer", None)
                .with_target("apr_1")
                .with_title("News digest")
        );
    }

    /// The governed path's record of a call names the tool's publisher and what decided it, so
    /// a person's no to a screen action is told from a refusal no one was asked about (spec
    /// 3.3 §7.1, §11).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn a_calls_line_names_its_publisher_and_what_decided_it() {
        use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
        use adk_core::audit_log::{
            AuditActor, AuditDecision, AuditLogStore, AuditRecordDraft, AuditRecordKind,
            AuditRecordOutcome, InMemoryAuditLogStore,
        };
        use adk_core::principal::SubjectKind;
        use adk_core::{ActingContext, ToolOrigin};
        use adk_kernel::control::{RecordBook, RecordLine};
        use serde_json::json;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        let finance = ActingContext::derived("adk-rust", "finance", "ana", SubjectKind::Human);
        let origin = ToolOrigin::new("verbs:adk-rust", &json!({ "name": "verb_snooze" }), None);
        // The governed path's record of a call refused by `policy_ref` (`AuditedCall::decision`).
        let refused = |policy_ref: &str| {
            AuditRecordDraft::new(AuditRecordKind::ToolCall, "verb_snooze")
                .actor(AuditActor::new(&finance, "inv-1"))
                .decision(AuditDecision::Denied)
                .outcome(AuditRecordOutcome::NotRun)
                .policy_ref(policy_ref)
                .ext("toolSource", json!(origin.source))
                .ext("declarationDigest", json!(origin.declaration_digest))
        };
        log.append(refused("confirmation")).await.unwrap();
        log.append(refused("tool_policy:adk-surface/verbs")).await.unwrap();
        let book = SignedRecordBook::new(logs, "kernel-test".into(), verifier);
        let lines = book.recent(2).await.unwrap();
        let line = |seq: u64, at, policy_ref: &str| {
            RecordLine::new(seq, at, "verb_snooze")
                .with_subject("ana")
                .with_agent("urn:adk:agent:adk-rust/finance")
                .with_tool("verb_snooze")
                .with_decision("denied")
                .with_outcome("not_run")
                .with_call("verbs:adk-rust", Some(policy_ref))
        };
        assert_eq!(
            lines,
            [
                line(1, lines[0].at, "tool_policy:adk-surface/verbs"),
                line(0, lines[1].at, "confirmation")
            ]
        );
    }

    /// A folder rule's line says what it covers in plain words, never the tools its record
    /// keeps as data: "You allowed file writes in notes/ without asking." (spec 3.3 §8.2).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn a_folder_rules_line_says_what_it_covers_in_plain_words() {
        use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
        use adk_core::audit_log::{
            AuditActor, AuditLogStore, AuditRecordDraft, AuditRecordKind, InMemoryAuditLogStore,
        };
        use adk_kernel::control::RecordBook;
        use serde_json::json;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        // The fields the harness records for "Always allow for this folder" in `notes/`.
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Other("harness".into()), "harness.rule.created")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .ext("tool", json!("*"))
                .ext("tools", json!(["apply_patch", "edit_file", "write_file"]))
                .ext("covers", json!("file writes in notes/"))
                .ext("decision", json!({ "kind": "allow" }))
                .ext("surface", json!("web")),
        )
        .await
        .unwrap();
        let book = SignedRecordBook::new(logs, "kernel-test".into(), verifier);
        let lines = book.recent(1).await.unwrap();
        assert_eq!(
            (lines[0].tool.as_deref(), lines[0].decision.as_deref()),
            (Some("file writes in notes/"), Some("allowed"))
        );
    }

    /// The record book answers the check `adk-rust audit verify` gives an export of the same
    /// log with the same key, and lists the newest actions first (spec 3.3 §10 K5).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn the_record_book_checks_as_audit_verify_does() {
        use adk_auth::signed_audit::{
            AuditExport, AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier,
        };
        use adk_core::audit_log::{
            AuditActor, AuditDecision, AuditLogStore, AuditRecordDraft, AuditRecordKind,
            InMemoryAuditLogStore,
        };
        use adk_core::principal::SubjectRef;
        use adk_kernel::control::RecordBook;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Approval, "surface.decide")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .decision(AuditDecision::Approved)
                .ext("tool", serde_json::json!("send_email"))
                .ext("origin", serde_json::json!({ "kind": "chat", "channel": "Telegram" })),
        )
        .await
        .unwrap();
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Other("kernel".into()), "kernel.control")
                .ext("target", serde_json::json!("default")),
        )
        .await
        .unwrap();
        log.checkpoint().await.unwrap();

        let book = SignedRecordBook::new(logs.clone(), "kernel-test".into(), verifier.clone());
        let check = book.check().await.unwrap();
        let export = AuditExport::from_store(logs.as_ref(), "kernel-test").await.unwrap();
        assert_eq!(check.valid, export.verify(std::slice::from_ref(&verifier)).is_valid());
        assert!(check.valid && check.problem.is_none());
        assert_eq!(check.entries, 2);

        let lines = book.recent(5).await.unwrap();
        assert_eq!(lines.iter().map(|line| line.seq).collect::<Vec<_>>(), [1, 0]);
        let answer = &lines[1];
        assert_eq!(
            (answer.subject.as_deref(), answer.tool.as_deref(), answer.decision.as_deref()),
            (Some("ana"), Some("send_email"), Some("approved"))
        );
        assert_eq!(
            (answer.how.as_deref(), answer.channel.as_deref()),
            (Some("chat"), Some("Telegram"))
        );
        assert_eq!(lines[0].target.as_deref(), Some("default"));

        let other: NoteVerifier =
            Ed25519Signer::generate("someone-else").unwrap().verifier_key().parse().unwrap();
        let untrusted =
            SignedRecordBook::new(logs, "kernel-test".into(), other).check().await.unwrap();
        assert!(!untrusted.valid, "a log signed by another key does not verify");
        assert!(untrusted.problem.is_some());
    }

    /// A screen action reads as the action, the item's title and where it was taken; an
    /// agent's, as the agent's request and where the person said yes (spec 3.3 §2 item 7,
    /// §11).
    #[cfg(feature = "audit")]
    #[tokio::test]
    async fn a_record_line_says_what_a_screen_action_did() {
        use adk_auth::signed_audit::{AuditLog, AuditSigner as _, Ed25519Signer, NoteVerifier};
        use adk_core::audit_log::{
            AuditActor, AuditLogStore, AuditRecordDraft, AuditRecordKind, InMemoryAuditLogStore,
        };
        use adk_core::principal::SubjectRef;
        use adk_core::{ActingContext, SubjectKind};
        use adk_kernel::control::{RecordBook, RecordLine};
        use serde_json::json;

        let logs: Arc<dyn AuditLogStore> = Arc::new(InMemoryAuditLogStore::new());
        let signer = Ed25519Signer::generate("kernel-test").unwrap();
        let verifier: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log =
            AuditLog::builder(logs.clone(), "kernel-test", Arc::new(signer)).open().await.unwrap();
        let verb = |actor: AuditActor| {
            AuditRecordDraft::new(AuditRecordKind::Other("surface".into()), "surface.verb")
                .actor(actor)
                .ext("verb", json!("snooze"))
                .ext("target", json!("agenda:news-digest"))
                .ext("title", json!("News digest"))
                .ext("outcome", json!("done"))
        };
        log.append(
            verb(AuditActor::subject(SubjectRef::human("ana")))
                .ext("surface", json!("voice"))
                .ext("origin", json!({ "kind": "voice" })),
        )
        .await
        .unwrap();
        let finance = ActingContext::derived("adk-rust", "finance", "ana", SubjectKind::Human);
        log.append(
            verb(AuditActor::new(&finance, "inv-1"))
                .ext("origin", json!({ "kind": "agent", "agent": "finance" }))
                .ext("approvalId", json!("apr_1"))
                .ext("answered", json!({ "surface": "phone", "origin": { "kind": "touch" }, "answer": "allowOnce" })),
        )
        .await
        .unwrap();
        // A refusal of an item that is gone has no title: its kernel id is never the target.
        log.append(
            AuditRecordDraft::new(AuditRecordKind::Other("surface".into()), "surface.verb")
                .actor(AuditActor::new(&finance, "inv-2"))
                .ext("verb", json!("snooze"))
                .ext("target", json!("agenda:agenda-01JBX"))
                .ext("outcome", json!("refused"))
                .ext("origin", json!({ "kind": "agent", "agent": "finance" })),
        )
        .await
        .unwrap();
        let book = SignedRecordBook::new(logs, "kernel-test".into(), verifier);
        let lines = book.recent(3).await.unwrap();
        let (refused, agent, person) = (&lines[0], &lines[1], &lines[2]);
        assert_eq!(
            refused,
            &RecordLine::new(refused.seq, refused.at, "surface.verb")
                .with_subject("ana")
                .with_agent("urn:adk:agent:adk-rust/finance")
                .with_tool("snooze")
                .with_outcome("refused")
                .with_how("agent", None)
        );
        assert_eq!(
            person,
            &RecordLine::new(person.seq, person.at, "surface.verb")
                .with_subject("ana")
                .with_tool("snooze")
                .with_outcome("done")
                .with_surface("voice")
                .with_how("voice", None)
                .with_target("News digest")
                .with_title("News digest")
        );
        assert_eq!(
            agent,
            &RecordLine::new(agent.seq, agent.at, "surface.verb")
                .with_subject("ana")
                .with_agent("urn:adk:agent:adk-rust/finance")
                .with_tool("snooze")
                .with_outcome("done")
                .with_target("News digest")
                .with_title("News digest")
                .with_decision("approved")
                .with_surface("phone")
                .with_how("touch", None)
        );
    }
}
