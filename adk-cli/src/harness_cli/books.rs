//! What `adk-rust up` gives the kernel's control plane for the routes the surfaces need
//! (spec 3.3 §10 K3, K5): the memory book over each area's harness, the record book over the
//! kernel's signed audit log, and the mode store through which an area's mode, changed while
//! the kernel runs, governs its agents from their next step.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, UserId};
use adk_harness::{
    FactSource, FactStatus, FactStore, HarnessMode, MemoryFact, ModeRecord, ModeSource, ModeStore,
};
use adk_kernel::control::{
    ControlStore, FactOrigin, MemoryBook, RememberedFact, TidyUp, TidyUpUndo,
};
use adk_kernel::{KernelError, PartitionId, PartitionMode};
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

/// A harness's [`ModeStore`] that reads, for a session, the mode its area was last set to:
/// the session's own record, or the area's mode the person changed while the kernel runs
/// when that change is newer (spec 3.3 §10 K3). Every part of the harness reads the mode
/// through its store on every step, so the change governs the next one.
pub(super) struct AreaModeStore {
    inner: Arc<dyn ModeStore>,
    control: Arc<dyn ControlStore>,
    area: PartitionId,
}

impl AreaModeStore {
    pub(super) fn new(
        inner: Arc<dyn ModeStore>,
        control: Arc<dyn ControlStore>,
        area: PartitionId,
    ) -> Self {
        Self { inner, control, area }
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
        let Some(change) = settings.and_then(|settings| settings.mode) else {
            return Ok(record);
        };
        if let Some(record) = &record
            && record.at >= change.at
        {
            return Ok(Some(record.clone()));
        }
        let mut area = ModeRecord::new(HarnessMode::from(change.value), ModeSource::User);
        area.at = change.at;
        area.revision = record.map_or(0, |record| record.revision);
        Ok(Some(area))
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
pub(super) struct HarnessMemoryBook {
    areas: Vec<(PartitionId, Arc<adk_harness::Harness>)>,
    store: Arc<dyn FactStore>,
}

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
fn no_fact(id: &str) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::NotFound,
        "harness.memory.fact_not_found",
        format!("no fact {id} of yours is remembered"),
    ))
}

/// The caller as the harness names users.
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
fn not_held(error: &AdkError) -> bool {
    error.code == "harness.memory.fact_not_found"
}

/// A fact as the home shows it.
fn remembered(fact: MemoryFact, area: &PartitionId) -> RememberedFact {
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
    if origin == FactOrigin::Guess && area.as_str() != PartitionId::DEFAULT {
        remembered = remembered.with_noticed_by(area.as_str());
    }
    match fact.provenance.tick {
        Some(tick) => remembered.with_tidy_up(tick),
        None => remembered,
    }
}

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
            for fact in harness.facts(&id).await.map_err(KernelError::Core)? {
                if fact.status.is_live() && !facts.iter().any(|known| known.id == fact.id) {
                    facts.push(remembered(fact, partition));
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
                    return result.map(|fact| remembered(fact, area)).map_err(KernelError::Core);
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
                    return result.map(|fact| remembered(fact, area)).map_err(KernelError::Core);
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
    let subject = record
        .actor
        .on_behalf_of
        .as_ref()
        .map(|subject| subject.id.clone())
        .or_else(|| text(record.ext.get("subject").and_then(|subject| subject.get("id"))));
    if let Some(subject) = subject {
        line = line.with_subject(subject);
    }
    if let Some(agent) = &record.actor.agent {
        line = line.with_agent(agent.to_string());
    }
    let tool = text(record.ext.get("tool")).or_else(|| {
        matches!(record.kind, adk_core::audit_log::AuditRecordKind::ToolCall)
            .then(|| record.action.clone())
    });
    if let Some(tool) = tool {
        line = line.with_tool(tool);
    }
    if let Some(decision) =
        record.decision.and_then(|d| serde_json::to_value(d).ok()).and_then(wire)
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
    if let Some(origin) = record.ext.get("origin")
        && let Some(how) = text(origin.get("kind"))
    {
        let channel = text(origin.get("channel")).or_else(|| text(record.ext.get("device")));
        line = line.with_how(how, channel.as_deref());
    }
    let target = text(record.ext.get("target")).or_else(|| text(record.ext.get("approvalId")));
    if let Some(target) = target {
        line = line.with_target(target);
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
    use adk_harness::{
        ConsolidationRecord, FactKind, FactScope, InMemoryFactStore, InMemoryModeStore,
        InstructionsConfig, Provenance,
    };
    use adk_kernel::control::{AreaSettings, Change, InMemoryControlStore};
    use adk_model::{ScriptedLlm, ScriptedResponse};

    use super::*;

    /// A harness with long-term memory in `store`, over a fresh workspace.
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
        let book = HarnessMemoryBook::new(vec![(PartitionId::new("work"), harness)], store.clone());
        let by = SubjectRef::human("ana");

        let facts = book.facts("ana", None).await.unwrap();
        let origin = |id: &str| facts.iter().find(|fact| fact.id == id).map(|fact| fact.origin);
        assert_eq!(origin(&said.id), Some(FactOrigin::YouSaidThis));
        assert_eq!(origin(&guess.id), Some(FactOrigin::Guess));
        assert_eq!(
            facts.iter().find(|fact| fact.id == guess.id).unwrap().noticed_by.as_deref(),
            Some("work")
        );
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

    /// An area's mode changed after a session's own record governs the session; a session
    /// set after the change keeps its own (spec 3.3 §10 K3).
    #[tokio::test]
    async fn the_newest_of_the_area_mode_and_the_session_mode_governs() {
        let inner = Arc::new(InMemoryModeStore::new());
        let control = Arc::new(InMemoryControlStore::new());
        let store = AreaModeStore::new(inner.clone(), control.clone(), PartitionId::default());
        assert!(
            store.get(APP_NAME, "ana", "s1").await.unwrap().is_none(),
            "no change: the default"
        );

        let mut session = ModeRecord::new(HarnessMode::Auto, ModeSource::User);
        session.revision = 3;
        session.at = chrono::Utc::now() - chrono::Duration::minutes(5);
        inner.set(APP_NAME, "ana", "s1", session.clone()).await.unwrap();
        let change = Change::new(PartitionMode::ReadOnly, "ana", chrono::Utc::now());
        control.put_area_settings(AreaSettings::new("default").with_mode(change)).await.unwrap();
        let read = store.get(APP_NAME, "ana", "s1").await.unwrap().unwrap();
        assert_eq!((read.mode, read.revision), (HarnessMode::ReadOnly, 3));
        assert_eq!(
            store.get(APP_NAME, "ana", "s2").await.unwrap().unwrap().mode,
            HarnessMode::ReadOnly
        );

        let mut later = ModeRecord::new(HarnessMode::Plan, ModeSource::User);
        later.at = chrono::Utc::now() + chrono::Duration::seconds(1);
        store.set(APP_NAME, "ana", "s1", later).await.unwrap();
        assert_eq!(
            store.get(APP_NAME, "ana", "s1").await.unwrap().unwrap().mode,
            HarnessMode::Plan
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
}
