//! How each answer was given (spec 3.3 §8.1 "Decided", §11).
//!
//! `adk_core::ApprovalDecision` records who decided and when, not the surface or the way the
//! answer was given. The engine writes both to the audit record of every answer
//! ([`SURFACE_AUDIT_DECIDE`]) and keeps them in an [`AnswerLog`], so a decision answered
//! elsewhere reads "You said yes in Telegram at 07:31" on every other surface.
//!
//! | Log | Survives a restart | Reads |
//! |---|---|---|
//! | [`InMemoryAnswerLog`] | No | What this process recorded |
//! | [`AuditAnswerLog`] | Yes | The `surface.decide` records of an `AuditLogStore` (the signed audit log), newest first |
//! | [`RecordAnswerLog`] | Yes | A kernel's record over its control plane (`GET /api/kernel/record`), for a surface attached to the kernel, such as a terminal |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditLogStore, AuditRecord};
use adk_core::principal::SubjectRef;
use adk_kernel::control::{ControlClient, RecordLine};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::origin::{Origin, SurfaceKind, Via};
use crate::rules::Answer;

/// The audit action of an answer to a decision given through a surface (spec 3.3 §11).
pub const SURFACE_AUDIT_DECIDE: &str = "surface.decide";

/// One answer given through a surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRecord {
    /// The decision.
    pub approval_id: String,
    /// Who answered.
    pub by: SubjectRef,
    /// Where and how.
    pub via: Via,
    /// The answer.
    pub answer: Answer,
    /// When.
    pub at: DateTime<Utc>,
}

/// Where the engine keeps how each answer was given (spec 3.3 §11).
#[async_trait]
pub trait AnswerLog: Send + Sync {
    /// Records the answer that decided `record.approval_id`.
    async fn record(&self, record: &AnswerRecord) -> Result<()>;

    /// The answer that decided `approval_id`, when it was given through a surface.
    async fn find(&self, approval_id: &str) -> Result<Option<AnswerRecord>>;
}

/// An [`AnswerLog`] in memory, for one process.
///
/// # Example
///
/// ```rust
/// use adk_surface::{AnswerLog, InMemoryAnswerLog};
///
/// # futures::executor::block_on(async {
/// let log = InMemoryAnswerLog::new();
/// assert!(log.find("apr_1").await.unwrap().is_none());
/// # });
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryAnswerLog {
    records: Arc<Mutex<HashMap<String, AnswerRecord>>>,
}

impl InMemoryAnswerLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AnswerLog for InMemoryAnswerLog {
    async fn record(&self, record: &AnswerRecord) -> Result<()> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(record.approval_id.clone(), record.clone());
        Ok(())
    }

    async fn find(&self, approval_id: &str) -> Result<Option<AnswerRecord>> {
        Ok(self.records.lock().unwrap_or_else(|e| e.into_inner()).get(approval_id).cloned())
    }
}

/// An [`AnswerLog`] over the audit log the engine's records go to: `record` writes nothing
/// (the engine's [`AuditTrail`](adk_core::audit_log::AuditTrail) already wrote the
/// `surface.decide` record), and `find` reads it back from the store, newest first, so every
/// process over the same store, and a restarted one, sees how a decision was answered.
#[derive(Clone)]
pub struct AuditAnswerLog {
    store: Arc<dyn AuditLogStore>,
}

impl std::fmt::Debug for AuditAnswerLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditAnswerLog").finish_non_exhaustive()
    }
}

impl AuditAnswerLog {
    /// Reads the `surface.decide` records of `store`.
    pub fn new(store: Arc<dyn AuditLogStore>) -> Self {
        Self { store }
    }
}

/// How many entries one read of the audit log takes.
const AUDIT_PAGE: u64 = 256;

#[async_trait]
impl AnswerLog for AuditAnswerLog {
    async fn record(&self, _record: &AnswerRecord) -> Result<()> {
        Ok(())
    }

    async fn find(&self, approval_id: &str) -> Result<Option<AnswerRecord>> {
        for log in self.store.logs().await? {
            let mut end = self.store.size(&log).await?;
            while end > 0 {
                let start = end.saturating_sub(AUDIT_PAGE);
                let entries = self.store.read(&log, start..end).await?;
                for entry in entries.iter().rev() {
                    let Some(text) = &entry.record else { continue };
                    let Ok(record) = serde_json::from_str::<AuditRecord>(text) else { continue };
                    if let Some(answer) = answer_of(&record, approval_id) {
                        return Ok(Some(answer));
                    }
                }
                end = start;
            }
        }
        Ok(None)
    }
}

/// The answer a `surface.decide` record holds for `approval_id`, when it decided it.
pub(crate) fn answer_of(record: &AuditRecord, approval_id: &str) -> Option<AnswerRecord> {
    if record.action != SURFACE_AUDIT_DECIDE {
        return None;
    }
    let ext = &record.ext;
    if ext.get("approvalId").and_then(|v| v.as_str()) != Some(approval_id)
        || ext.get("outcome").and_then(|v| v.as_str()) != Some("decided")
    {
        return None;
    }
    let surface: SurfaceKind = serde_json::from_value(ext.get("surface")?.clone()).ok()?;
    let origin: Origin = serde_json::from_value(ext.get("origin")?.clone()).ok()?;
    let answer: Answer = serde_json::from_value(ext.get("answer")?.clone()).ok()?;
    let by = record.actor.on_behalf_of.clone()?;
    Some(AnswerRecord {
        approval_id: approval_id.to_string(),
        by,
        via: Via { surface, origin },
        answer,
        at: record.ts,
    })
}

/// How many of the record's latest actions [`RecordAnswerLog`] reads: the most one
/// `GET /api/kernel/record` answers.
pub const RECORD_LINES: usize = 500;

/// An [`AnswerLog`] over a kernel's record, for a surface attached to the kernel over its
/// control plane (spec 3.3 §5.2, §10 K5, §11): a decision answered in Telegram reads "You said
/// yes in Telegram at 07:31" in the terminal too.
///
/// `find` reads the answers this surface gave first, then the `surface.decide` lines among the
/// record's latest [`RECORD_LINES`] actions (`GET /api/kernel/record`, the operator's or a
/// paired device's with `read`). A record line says how an answer was given, not on which
/// surface: `chat` names its channel, `touch` reads as the phone, `pointer` as the home,
/// `keyboard` as the terminal. An answer older than those lines, or a kernel that shows no
/// record, reads without where it was given.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::control::ControlClient;
/// use adk_surface::{ControlSource, RecordAnswerLog, SurfaceEngine};
///
/// # async fn example(home: &std::path::Path) -> adk_surface::Result<()> {
/// let client = ControlClient::connect_local(home).await?.expect("a kernel is running");
/// let answers = Arc::new(RecordAnswerLog::new(client.clone()));
/// let source = ControlSource::new(client, "adk-rust", "adk-rust");
/// let engine = SurfaceEngine::builder(Arc::new(source)).answers(answers).build();
/// # let _ = engine;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct RecordAnswerLog {
    client: ControlClient,
    given_here: InMemoryAnswerLog,
}

impl std::fmt::Debug for RecordAnswerLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordAnswerLog")
            .field("endpoint", &self.client.endpoint())
            .finish_non_exhaustive()
    }
}

impl RecordAnswerLog {
    /// Reads the record of the kernel `client` reaches.
    pub fn new(client: ControlClient) -> Self {
        Self { client, given_here: InMemoryAnswerLog::new() }
    }
}

#[async_trait]
impl AnswerLog for RecordAnswerLog {
    async fn record(&self, record: &AnswerRecord) -> Result<()> {
        self.given_here.record(record).await
    }

    async fn find(&self, approval_id: &str) -> Result<Option<AnswerRecord>> {
        if let Some(found) = self.given_here.find(approval_id).await? {
            return Ok(Some(found));
        }
        match self.client.record(Some(RECORD_LINES)).await {
            Ok(lines) => Ok(lines.iter().find_map(|line| answer_of_line(line, approval_id))),
            Err(error) => {
                tracing::debug!(error = %error, "the kernel's record could not be read");
                Ok(None)
            }
        }
    }
}

/// The answer a record line of `surface.decide` holds for `approval_id`, when it decided it.
pub(crate) fn answer_of_line(line: &RecordLine, approval_id: &str) -> Option<AnswerRecord> {
    if line.action != SURFACE_AUDIT_DECIDE
        || line.target.as_deref() != Some(approval_id)
        || line.outcome.as_deref() != Some("decided")
    {
        return None;
    }
    let (surface, origin) = match line.how.as_deref()? {
        "chat" => (
            SurfaceKind::Chat,
            Origin::Chat { channel: line.channel.clone().unwrap_or_else(|| "chat".into()) },
        ),
        "touch" => (SurfaceKind::Phone, Origin::Touch),
        "pointer" => (SurfaceKind::Web, Origin::Pointer),
        "keyboard" => (SurfaceKind::Terminal, Origin::Keyboard),
        "voice" => (SurfaceKind::Voice, Origin::Voice),
        _ => return None,
    };
    let answer = match line.decision.as_deref()? {
        "approved" => Answer::AllowOnce,
        "rejected" => Answer::Deny,
        _ => return None,
    };
    Some(AnswerRecord {
        approval_id: approval_id.to_string(),
        by: SubjectRef::human(line.subject.clone()?),
        via: Via { surface, origin },
        answer,
        at: line.at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::audit_log::{
        AuditActor, AuditAppend, AuditHash, AuditLogEntry, AuditRecordDraft, AuditRecordKind,
        InMemoryAuditLogStore,
    };
    use serde_json::json;

    fn entry(
        seq: u64,
        approval_id: &str,
        outcome: &str,
        origin: serde_json::Value,
    ) -> AuditLogEntry {
        let record = AuditRecordDraft::new(AuditRecordKind::Approval, SURFACE_AUDIT_DECIDE)
            .actor(AuditActor::subject(SubjectRef::human("ana")))
            .ext("approvalId", json!(approval_id))
            .ext("outcome", json!(outcome))
            .ext("answer", json!("allowOnce"))
            .ext("surface", json!("chat"))
            .ext("origin", origin)
            .into_record("replica-1", seq, Utc::now(), None);
        AuditLogEntry {
            seq,
            leaf_hash: AuditHash([seq as u8; 32]),
            record: Some(serde_json::to_string(&record).unwrap()),
        }
    }

    #[tokio::test]
    async fn the_audit_log_says_how_a_decision_was_answered() {
        let store = Arc::new(InMemoryAuditLogStore::new());
        let telegram = json!({ "kind": "chat", "channel": "Telegram" });
        store
            .append(AuditAppend {
                log_id: "replica-1".into(),
                entries: vec![
                    entry(0, "apr_1", "decided", telegram.clone()),
                    entry(1, "apr_1", "already_decided", json!({ "kind": "pointer" })),
                    entry(2, "apr_2", "refused", telegram),
                ],
                nodes: vec![],
            })
            .await
            .unwrap();
        let log = AuditAnswerLog::new(store);
        let found = log.find("apr_1").await.unwrap().unwrap();
        assert_eq!(found.via.origin, Origin::Chat { channel: "Telegram".into() });
        assert_eq!(found.by, SubjectRef::human("ana"));
        assert_eq!(found.answer, Answer::AllowOnce);
        assert!(log.find("apr_2").await.unwrap().is_none(), "a refused answer decided nothing");
    }

    /// A kernel's record line of an answer given in Telegram says where it was given; a line
    /// of another decision, a refused answer or another action says nothing (spec 3.3 §12
    /// phase 1: the terminal shows "You said yes in Telegram").
    #[test]
    fn a_record_line_says_how_a_decision_was_answered() {
        let at = Utc::now();
        let line = |id: &str, outcome: &str| {
            RecordLine::new(7, at, SURFACE_AUDIT_DECIDE)
                .with_subject("ana")
                .with_decision("approved")
                .with_outcome(outcome)
                .with_how("chat", Some("Telegram"))
                .with_target(id)
        };
        let found = answer_of_line(&line("apr_1", "decided"), "apr_1").unwrap();
        assert_eq!(
            found,
            AnswerRecord {
                approval_id: "apr_1".into(),
                by: SubjectRef::human("ana"),
                via: Via {
                    surface: SurfaceKind::Chat,
                    origin: Origin::Chat { channel: "Telegram".into() },
                },
                answer: Answer::AllowOnce,
                at,
            }
        );
        assert_eq!(found.via.phrase(), "in Telegram");
        assert!(answer_of_line(&line("apr_1", "decided"), "apr_2").is_none());
        assert!(answer_of_line(&line("apr_1", "refused"), "apr_1").is_none());
        let control =
            RecordLine::new(8, at, "kernel.control").with_subject("ana").with_target("apr_1");
        assert!(answer_of_line(&control, "apr_1").is_none());
        let touch = RecordLine::new(9, at, SURFACE_AUDIT_DECIDE)
            .with_subject("ana")
            .with_decision("rejected")
            .with_outcome("decided")
            .with_how("touch", None)
            .with_target("apr_3");
        let found = answer_of_line(&touch, "apr_3").unwrap();
        assert_eq!((found.via.phrase(), found.answer), ("on your phone".to_string(), Answer::Deny));
    }
}
