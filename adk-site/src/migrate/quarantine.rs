//! The quarantine (spec 3.5 §7.8 "Sanitiser", §7.8.1 "Quarantine"): every detector finding
//! holds its item or file out of the site, with its evidence, until an owner decides on that
//! one item through an [`ApprovalGate`]. Server-executable uploads are never released (A-I1).
//!
//! The flow: convert the inventory, quarantine the findings against that conversion, then
//! convert again leaving out [`QuarantineBook::excluded`]; after a decision releases a
//! subject, convert again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::ActingContext;
use adk_core::audit_log::{AuditDecision, AuditRecorder};
use awp_types::{ContentId, Digest};
use serde::{Deserialize, Serialize};

use super::audit::record;
use super::convert::Converted;
use super::detect::{Finding, Subject};
use super::error::{MigrateError, Result};
use crate::gate::Quarantine;
use crate::pipeline::{ApprovalDecision, ApprovalGate, ApprovalRequest, EntryPoint};
use crate::release::file_digest;

/// The action class quarantine decisions are requested under.
pub const QUARANTINE_ACTION_CLASS: &str = "site.migration.release_quarantined";

/// Where a quarantined subject stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum QuarantineState {
    /// Waiting for a decision.
    Held,
    /// Waiting for people: the decision's ticket.
    Pending {
        /// The pending decision.
        ticket: String,
    },
    /// An owner released it.
    Released {
        /// Who approved it.
        approvers: Vec<String>,
        /// The decision record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },
    /// An owner kept it out of the site.
    Rejected {
        /// Who.
        by: String,
        /// Why.
        reason: String,
    },
    /// Server code (A-I1): never released, whatever anyone decides.
    Refused,
}

impl QuarantineState {
    /// Whether the subject stays out of the site.
    pub fn excluded(&self) -> bool {
        !matches!(self, Self::Released { .. })
    }
}

/// One quarantined subject and every finding about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineEntry {
    /// The subject.
    pub subject: Subject,
    /// The content id a decision names: the item's, or `media:<path>` for a file.
    pub content_id: ContentId,
    /// The inventory item it holds back, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The output path held out of the release, for files and pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The findings.
    pub findings: Vec<Finding>,
    /// The digest of the findings, which a decision binds to.
    pub evidence_digest: Digest,
    /// Where it stands.
    pub state: QuarantineState,
}

/// Every quarantined subject of a migration.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::convert::Converted;
/// use adk_site::migrate::detect::{Evidence, Finding, FindingKind, Subject};
/// use adk_site::migrate::quarantine::QuarantineBook;
///
/// let finding = Finding {
///     kind: FindingKind::Executable,
///     subject: Subject::Media("/wp-content/uploads/cache.php".into()),
///     evidence: vec![Evidence { url: None, detail: "server code".into(), excerpt: String::new(), digest: None }],
/// };
/// let book = QuarantineBook::new(vec![finding], &Converted::default());
/// assert!(book.quarantine().paths.contains("wp-content/uploads/cache.php"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineBook {
    /// The entries, one per subject.
    pub entries: Vec<QuarantineEntry>,
}

impl QuarantineBook {
    /// Quarantines the subject of every finding; `converted` maps item keys to content ids
    /// and page paths.
    pub fn new(findings: Vec<Finding>, converted: &Converted) -> Self {
        let mut grouped: BTreeMap<Subject, Vec<Finding>> = BTreeMap::new();
        for finding in findings {
            grouped.entry(finding.subject.clone()).or_default().push(finding);
        }
        let by_path: BTreeMap<&String, &String> =
            converted.paths.iter().map(|(key, path)| (path, key)).collect();
        let entries = grouped
            .into_iter()
            .map(|(subject, findings)| {
                // An item is held by its content id; a file by its output path.
                let item = match &subject {
                    Subject::Item(key) => Some(key.clone()),
                    Subject::Url(path) => by_path.get(path).map(|key| (*key).clone()),
                    Subject::Media(_) => None,
                };
                let (content_id, path) = match (&subject, &item) {
                    (_, Some(key)) => (
                        converted
                            .ids
                            .get(key)
                            .cloned()
                            .unwrap_or_else(|| ContentId(format!("item:{key}"))),
                        None,
                    ),
                    (Subject::Media(path), None) => (
                        ContentId(format!("media:{path}")),
                        Some(path.trim_start_matches('/').to_string()),
                    ),
                    (Subject::Url(path) | Subject::Item(path), None) => {
                        (ContentId(format!("url:{path}")), Some(page_file(path)))
                    }
                };
                let refused = findings.iter().any(|f| !f.kind.releasable());
                let evidence_digest =
                    file_digest(serde_json::to_string(&findings).unwrap_or_default().as_bytes());
                QuarantineEntry {
                    subject,
                    content_id,
                    item,
                    path,
                    findings,
                    evidence_digest,
                    state: if refused { QuarantineState::Refused } else { QuarantineState::Held },
                }
            })
            .collect();
        Self { entries }
    }

    /// Records `site.migration.finding` for every entry.
    ///
    /// # Errors
    ///
    /// The recorder's error.
    pub async fn record_findings(
        &self,
        recorder: &Arc<dyn AuditRecorder>,
        acting: &ActingContext,
    ) -> Result<()> {
        for entry in &self.entries {
            let value = serde_json::json!({
                "subject": entry.subject,
                "kinds": entry.findings.iter().map(|f| f.kind).collect::<Vec<_>>(),
                "evidenceDigest": entry.evidence_digest,
                "state": entry.state,
            });
            record(recorder, acting, "site.migration.finding", None, value).await?;
        }
        Ok(())
    }

    /// The items and files the launch gate must not see in the output.
    pub fn quarantine(&self) -> Quarantine {
        let mut quarantine = Quarantine::default();
        for entry in self.entries.iter().filter(|e| e.state.excluded()) {
            if !entry.content_id.0.contains(':') {
                quarantine.items.insert(entry.content_id.clone());
            }
            if let Some(path) = &entry.path {
                quarantine.paths.insert(path.clone());
            }
        }
        quarantine
    }

    /// The item keys and media paths to leave out of a conversion.
    pub fn excluded(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .filter(|e| e.state.excluded())
            .flat_map(|e| {
                let path = match &e.subject {
                    Subject::Media(path) | Subject::Url(path) => Some(path.clone()),
                    Subject::Item(_) => None,
                };
                e.item.clone().into_iter().chain(path)
            })
            .collect()
    }

    /// Asks `gate` to decide on one entry: its item, its evidence digest, the proposer.
    /// A refused entry (A-I1) is not asked about and stays refused.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Service`] when no entry has `content_id` or the gate fails; the
    /// recorder's error.
    pub async fn decide(
        &mut self,
        content_id: &ContentId,
        gate: &dyn ApprovalGate,
        proposer: &str,
        entry_point: EntryPoint,
        audit: Option<(&Arc<dyn AuditRecorder>, &ActingContext)>,
    ) -> Result<QuarantineState> {
        let entry =
            self.entries.iter_mut().find(|e| &e.content_id == content_id).ok_or_else(|| {
                MigrateError::Service(format!("nothing quarantined as '{content_id}'"))
            })?;
        if entry.state == QuarantineState::Refused {
            return Ok(QuarantineState::Refused);
        }
        let request = ApprovalRequest {
            items: vec![entry.content_id.clone()],
            action_class: QUARANTINE_ACTION_CLASS.into(),
            proposer: proposer.to_string(),
            entry: entry_point,
            preview_digest: entry.evidence_digest.clone(),
            change: entry.evidence_digest.clone(),
            diff: Vec::new(),
        };
        let decision =
            gate.decide(&request).await.map_err(|e| MigrateError::Service(e.to_string()))?;
        entry.state = match decision {
            ApprovalDecision::Approved { approvers, decision }
                if !approvers.is_empty() && approvers.iter().all(|a| a != proposer) =>
            {
                QuarantineState::Released { approvers, decision }
            }
            ApprovalDecision::Approved { .. } => QuarantineState::Held,
            ApprovalDecision::Rejected { by, reason } => QuarantineState::Rejected { by, reason },
            ApprovalDecision::Pending { ticket } => QuarantineState::Pending { ticket },
        };
        tracing::info!(content.id = %entry.content_id, state = ?entry.state, "quarantine decision");
        if let Some((recorder, acting)) = audit {
            let value = serde_json::json!({
                "subject": entry.subject,
                "evidenceDigest": entry.evidence_digest,
                "state": entry.state,
            });
            let outcome = Some(match entry.state {
                QuarantineState::Released { .. } => AuditDecision::Approved,
                QuarantineState::Rejected { .. } => AuditDecision::Rejected,
                _ => AuditDecision::ApprovalRequired,
            });
            record(recorder, acting, "site.migration.decided", outcome, value).await?;
        }
        Ok(entry.state.clone())
    }
}

fn page_file(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() || trimmed.ends_with('/') {
        format!("{trimmed}index.html")
    } else {
        trimmed.to_string()
    }
}
