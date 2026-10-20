//! Writing long-term facts: provenance, redaction, deduplication, supersession and the
//! scope cap (spec 3.1 §7.15.2, §9.2 I41, I43).

use std::collections::HashSet;

use adk_core::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::redact::{find_secret, redact_pii};
use super::runtime::{FactsRuntime, harness_subject};
use super::types::{
    FactFilter, FactKind, FactScope, FactSource, FactStatus, MemoryFact, Provenance, fact_digest,
};
use crate::audit::HarnessAudit;

/// One of the user's own messages, for quote verification.
#[derive(Clone, Debug)]
pub(crate) struct UserMessage {
    pub(crate) text: String,
    pub(crate) at: DateTime<Utc>,
    pub(crate) invocation_id: String,
}

/// A fact proposed by the model or the distiller (spec 3.1 §7.15.2: `{ text, kind,
/// source, quote?, confidence, supersedes? }`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FactCandidate {
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) quote: Option<String>,
    #[serde(default)]
    pub(crate) confidence: Option<f32>,
    #[serde(default)]
    pub(crate) supersedes: Option<String>,
}

/// Where a candidate comes from and what it may be checked against.
pub(crate) struct AdmitContext<'a> {
    pub(crate) app: &'a str,
    pub(crate) user: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) invocation_id: &'a str,
    pub(crate) run_id: Option<String>,
    /// The user's own messages in the source session; empty for a model's proposal.
    pub(crate) user_messages: &'a [UserMessage],
    pub(crate) audit: Option<HarnessAudit>,
}

/// What became of a candidate.
#[derive(Clone, Debug)]
pub(crate) enum Admitted {
    /// Stored as a new fact.
    Stored(MemoryFact),
    /// The same fact was already live; its `uses` was bumped.
    Duplicate(MemoryFact),
    /// Not stored, with the reason.
    Rejected {
        #[cfg_attr(not(feature = "runner"), allow(dead_code))] // recorded by distillation
        digest: String,
        reason: String,
    },
}

/// Least words and characters of a verifiable quote.
const QUOTE_MIN_WORDS: usize = 2;
const QUOTE_MIN_CHARS: usize = 8;
/// Facts superseded by similarity in one admission, at most.
const SIMILAR_SUPERSEDE_MAX: usize = 3;

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn content_words(text: &str) -> HashSet<String> {
    adk_memory::text::extract_words(text)
        .into_iter()
        .filter(|word| word.chars().count() >= 3)
        .collect()
}

/// The user message that verifies `quote` for a fact with `text`: the whitespace-
/// normalised quote appears verbatim in it, has at least two words and eight characters,
/// and shares a word of three or more letters with the fact (spec 3.1 §9.2 I41).
pub(crate) fn verify_quote<'a>(
    quote: &str,
    text: &str,
    messages: &'a [UserMessage],
) -> Option<&'a UserMessage> {
    let quote = collapse_whitespace(quote);
    if quote.split(' ').count() < QUOTE_MIN_WORDS || quote.chars().count() < QUOTE_MIN_CHARS {
        return None;
    }
    if content_words(&quote).is_disjoint(&content_words(text)) {
        return None;
    }
    messages.iter().find(|message| collapse_whitespace(&message.text).contains(&quote))
}

fn rejected(text: &str, reason: impl Into<String>) -> Admitted {
    Admitted::Rejected { digest: fact_digest(text), reason: reason.into() }
}

impl FactsRuntime {
    /// Admits `candidate` as a fact of `ctx`'s user (spec 3.1 §7.15.2).
    ///
    /// `Known` only from a verified quote; a model's or a tool's word is `Assumed` at
    /// most. A secret or an over-long text is rejected; personal data is redacted. A
    /// `Preference` is user-wide; every other kind is the project's.
    ///
    /// # Errors
    ///
    /// Returns the store's or, in Block mode, the audit trail's error.
    pub(crate) async fn admit(
        &self,
        candidate: &FactCandidate,
        ctx: &AdmitContext<'_>,
    ) -> Result<Admitted> {
        let raw = candidate.text.trim();
        if raw.is_empty() {
            return Ok(rejected(raw, "empty"));
        }
        if raw.chars().count() > self.config.max_fact_chars {
            return Ok(rejected(
                raw,
                format!("longer than {} characters", self.config.max_fact_chars),
            ));
        }
        let Ok(kind) = candidate.kind.parse::<FactKind>() else {
            return Ok(rejected(raw, format!("unknown kind '{}'", candidate.kind)));
        };
        for part in std::iter::once(raw).chain(candidate.quote.as_deref()) {
            if let Some(secret) = find_secret(part) {
                return Ok(rejected(raw, format!("holds a secret ({secret})")));
            }
        }

        // Provenance: Known only from the user's own words (I41).
        let verified = candidate
            .quote
            .as_deref()
            .and_then(|quote| verify_quote(quote, raw, ctx.user_messages));
        let (status, source) = match verified {
            Some(_) => (FactStatus::Known, FactSource::UserStated),
            None => (
                FactStatus::Assumed,
                match candidate.source.as_deref() {
                    Some("tool_observed" | "ToolObserved" | "toolObserved") => {
                        FactSource::ToolObserved
                    }
                    _ => FactSource::AgentInferred,
                },
            ),
        };
        let evidence_at = verified.map(|message| message.at);

        // Redaction before storage and indexing (I43).
        let (text, _) = redact_pii(raw);
        let quote = match (verified, candidate.quote.as_deref()) {
            (Some(_), Some(quote)) => Some(redact_pii(&collapse_whitespace(quote)).0),
            _ => None,
        };
        let scope = FactScope::new(
            ctx.app,
            ctx.user,
            (kind != FactKind::Preference).then(|| self.project.clone()),
        );
        let invocation =
            verified.map_or(ctx.invocation_id, |message| message.invocation_id.as_str());
        let mut provenance = Provenance::new(source, ctx.session_id, invocation);
        provenance.run_id = ctx.run_id.clone();
        provenance.quote = quote;
        let mut fact = MemoryFact::new(scope.clone(), kind, text, status, provenance);
        if fact.status == FactStatus::Assumed {
            fact.confidence = candidate.confidence.map(|c| c.clamp(0.0, 1.0));
        }

        // History: the same text forgotten or superseded comes back only from the user's
        // words newer than its end (I44).
        let existing = self.store.list(FactFilter::exactly(&scope)).await?;
        let mut replaces = Vec::new();
        for old in existing.iter().filter(|old| old.digest == fact.digest) {
            if old.status.is_live() {
                if old.status == FactStatus::Assumed && fact.status == FactStatus::Known {
                    replaces.push(old.clone());
                    continue;
                }
                let stored = self.store.put(fact.clone()).await?;
                return Ok(Admitted::Duplicate(stored));
            }
            let newer = evidence_at.zip(old.valid_to).is_some_and(|(said, ended)| said > ended);
            if !newer {
                return Ok(rejected(
                    &fact.text,
                    format!("the same fact was {} earlier", old.status.label()),
                ));
            }
        }

        // An explicitly named fact.
        if let Some(id) = candidate.supersedes.as_deref().filter(|id| !id.trim().is_empty()) {
            let Some(old) = self.store.get(id.trim()).await? else {
                return Ok(rejected(&fact.text, format!("supersedes unknown fact '{id}'")));
            };
            if old.scope.app != ctx.app || old.scope.user != ctx.user || !old.status.is_live() {
                return Ok(rejected(&fact.text, format!("supersedes unknown fact '{id}'")));
            }
            if old.status == FactStatus::Known && fact.status != FactStatus::Known {
                return Ok(rejected(
                    &fact.text,
                    "a known fact changes only by the user's own words or a person's correction",
                ));
            }
            replaces.push(old);
        }

        // A very similar fact of the same kind.
        let similar = self.index.search(&scope, &fact.text, 5).await.unwrap_or_default();
        for (id, score) in similar {
            if score < self.config.supersede_score || replaces.len() >= SIMILAR_SUPERSEDE_MAX {
                continue;
            }
            if replaces.iter().any(|old| old.id == id) {
                continue;
            }
            let Some(old) = self.store.get(&id).await? else {
                continue;
            };
            let allowed = old.status == FactStatus::Assumed || fact.status == FactStatus::Known;
            if old.status.is_live()
                && old.kind == fact.kind
                && old.scope == scope
                && old.digest != fact.digest
                && allowed
            {
                replaces.push(old);
            }
        }

        // The scope cap: the least-used assumed fact goes; known facts never silently.
        let live = existing.iter().filter(|old| old.status.is_live()).count();
        if live.saturating_sub(replaces.len()) >= self.config.max_facts {
            let victim = existing
                .iter()
                .filter(|old| {
                    old.status == FactStatus::Assumed && !replaces.iter().any(|r| r.id == old.id)
                })
                .min_by(|a, b| a.uses.cmp(&b.uses).then_with(|| a.valid_from.cmp(&b.valid_from)));
            match victim {
                Some(victim) => {
                    self.change_status(
                        ctx.audit.as_ref(),
                        victim,
                        FactStatus::Forgotten,
                        &harness_subject(),
                    )
                    .await?;
                }
                None => {
                    return Ok(rejected(
                        &fact.text,
                        format!(
                            "the scope already holds {} known facts; forget some first",
                            self.config.max_facts
                        ),
                    ));
                }
            }
        }

        for old in &replaces {
            self.change_status(
                ctx.audit.as_ref(),
                old,
                FactStatus::Superseded { by: fact.id.clone() },
                &harness_subject(),
            )
            .await?;
        }
        if let Some(audit) = &ctx.audit {
            audit
                .record(
                    "harness.memory.fact.stored",
                    json!({
                        "factId": fact.id,
                        "kind": fact.kind.as_str(),
                        "status": fact.status.label(),
                        "source": fact.provenance.source.as_str(),
                        "project": fact.scope.project,
                        "digest": fact.digest,
                        "sessionId": ctx.session_id,
                    }),
                )
                .await?;
        }
        let stored = self.store.put(fact).await?;
        if let Err(error) = self.index.upsert(&stored).await {
            tracing::warn!(error = %error, "could not index a fact; recall by search may miss it");
        }
        Ok(Admitted::Stored(stored))
    }
}

#[cfg(feature = "runner")]
impl FactsRuntime {
    /// The fact `id` when it is `user`'s, in this project or user-wide.
    ///
    /// # Errors
    ///
    /// Returns `harness.memory.fact_not_found` for any other fact.
    pub(crate) async fn owned(&self, app: &str, user: &str, id: &str) -> Result<MemoryFact> {
        self.store
            .get(id.trim())
            .await?
            .filter(|fact| self.scope(app, user).admits(&fact.scope))
            .ok_or_else(|| super::store::not_found(id))
    }

    /// Records a person's correction of `old`: a `Known` fact with source `UserConfirmed`
    /// that supersedes it (spec 3.1 §7.15.2).
    ///
    /// # Errors
    ///
    /// Returns `harness.memory.rejected` for a text that is empty, too long or holds a
    /// secret, or the store's or audit trail's error.
    pub(crate) async fn correct(
        &self,
        by: &adk_core::principal::SubjectRef,
        old: &MemoryFact,
        text: &str,
    ) -> Result<MemoryFact> {
        let text = super::redact::redact_fact_text(text, self.config.max_fact_chars)?;
        let audit = self.audit_as(by.clone(), "");
        if fact_digest(&text) == old.digest {
            if old.status != FactStatus::Known {
                self.change_status(audit.as_ref(), old, FactStatus::Known, by).await?;
            }
            return self.owned(&old.scope.app, &old.scope.user, &old.id).await;
        }
        let fact = MemoryFact::new(
            old.scope.clone(),
            old.kind,
            text,
            FactStatus::Known,
            Provenance::new(FactSource::UserConfirmed, "", ""),
        );
        self.change_status(audit.as_ref(), old, FactStatus::Superseded { by: fact.id.clone() }, by)
            .await?;
        if let Some(audit) = &audit {
            audit
                .record(
                    "harness.memory.fact.stored",
                    json!({
                        "factId": fact.id,
                        "kind": fact.kind.as_str(),
                        "status": fact.status.label(),
                        "source": fact.provenance.source.as_str(),
                        "project": fact.scope.project,
                        "digest": fact.digest,
                        "sessionId": "",
                    }),
                )
                .await?;
        }
        let stored = self.store.put(fact).await?;
        if let Err(error) = self.index.upsert(&stored).await {
            tracing::warn!(error = %error, "could not index a fact; recall by search may miss it");
        }
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::facts::{FactStore, InMemoryFactStore, LocalFactIndex, LongTermMemoryConfig};

    fn runtime(config: LongTermMemoryConfig) -> (FactsRuntime, Arc<InMemoryFactStore>) {
        let store = Arc::new(InMemoryFactStore::new());
        let model =
            Arc::new(adk_model::ScriptedLlm::new("m", Vec::<adk_model::ScriptedResponse>::new()));
        let runtime = FactsRuntime::new(
            config,
            store.clone(),
            Arc::new(LocalFactIndex::new(store.clone())),
            "repo-000000000000".into(),
            "harness".into(),
            false,
            None,
            model,
            None,
            Arc::new(crate::mode::InMemoryModeStore::new()),
            crate::mode::HarnessMode::Ask,
        );
        (runtime, store)
    }

    fn candidate(text: &str, kind: &str) -> FactCandidate {
        FactCandidate { text: text.into(), kind: kind.into(), ..FactCandidate::default() }
    }

    fn quoted(text: &str, kind: &str, quote: &str) -> FactCandidate {
        FactCandidate {
            source: Some("user_stated".into()),
            quote: Some(quote.into()),
            ..candidate(text, kind)
        }
    }

    async fn admit(
        runtime: &FactsRuntime,
        candidate: &FactCandidate,
        said: &[UserMessage],
    ) -> Admitted {
        let context = AdmitContext {
            app: "app",
            user: "alice",
            session_id: "s1",
            invocation_id: "inv-1",
            run_id: None,
            user_messages: said,
            audit: None,
        };
        runtime.admit(candidate, &context).await.unwrap()
    }

    fn stored(admitted: Admitted) -> MemoryFact {
        match admitted {
            Admitted::Stored(fact) => fact,
            other => panic!("not stored: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_same_text_is_one_fact_and_a_quote_promotes_it() {
        let (runtime, store) = runtime(LongTermMemoryConfig::default());
        let first =
            stored(admit(&runtime, &candidate("Use pnpm, not npm", "preference"), &[]).await);
        assert_eq!(first.status, FactStatus::Assumed);
        assert_eq!(first.scope.project, None, "preferences are user-wide");
        let again = admit(&runtime, &candidate("use pnpm, NOT npm.", "preference"), &[]).await;
        assert!(
            matches!(again, Admitted::Duplicate(ref fact) if fact.id == first.id && fact.uses == 1)
        );
        // The user's own words make it known; the assumption is superseded, history kept.
        let said = [said("please use pnpm, not npm here")];
        let known = stored(
            admit(&runtime, &quoted("Use pnpm, not npm", "preference", "use pnpm, not npm"), &said)
                .await,
        );
        assert_eq!(known.status, FactStatus::Known);
        assert_eq!(known.provenance.invocation_id, "inv");
        let old = store.get(&first.id).await.unwrap().unwrap();
        assert_eq!(old.status, FactStatus::Superseded { by: known.id.clone() });
    }

    #[tokio::test]
    async fn a_forgotten_fact_returns_only_by_newer_words_of_the_user() {
        let (runtime, store) = runtime(LongTermMemoryConfig::default());
        let earlier = said("use pnpm, not npm");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let first =
            stored(admit(&runtime, &candidate("Use pnpm, not npm", "preference"), &[]).await);
        store
            .set_status(
                &first.id,
                FactStatus::Forgotten,
                &adk_core::principal::SubjectRef::human("alice"),
            )
            .await
            .unwrap();
        // Re-distilling the old session does not bring it back.
        let candidate = quoted("Use pnpm, not npm", "preference", "use pnpm, not npm");
        let again = admit(&runtime, &candidate, std::slice::from_ref(&earlier)).await;
        assert!(
            matches!(again, Admitted::Rejected { ref reason, .. } if reason.contains("forgotten")),
            "{again:?}"
        );
        // Saying it again later does.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let back = stored(admit(&runtime, &candidate, &[said("use pnpm, not npm")]).await);
        assert_eq!(back.status, FactStatus::Known);
    }

    #[tokio::test]
    async fn a_similar_fact_of_the_same_kind_is_superseded() {
        let (runtime, store) = runtime(LongTermMemoryConfig::default());
        let old = stored(
            admit(&runtime, &candidate("run the tests with the sqlite feature", "procedure"), &[])
                .await,
        );
        let other_kind = stored(
            admit(&runtime, &candidate("run the tests with the sqlite feature on", "outcome"), &[])
                .await,
        );
        let new = stored(
            admit(
                &runtime,
                &candidate("always run the tests with the sqlite feature", "procedure"),
                &[],
            )
            .await,
        );
        assert_eq!(
            store.get(&old.id).await.unwrap().unwrap().status,
            FactStatus::Superseded { by: new.id.clone() }
        );
        assert!(
            store.get(&other_kind.id).await.unwrap().unwrap().status.is_live(),
            "another kind stays"
        );
        // An assumed fact never replaces a known one by similarity.
        let known = stored(
            admit(
                &runtime,
                &quoted(
                    "deploy from the release branch",
                    "decision",
                    "deploy from the release branch",
                ),
                &[said("we deploy from the release branch")],
            )
            .await,
        );
        stored(admit(&runtime, &candidate("deploy from release branch", "decision"), &[]).await);
        assert_eq!(store.get(&known.id).await.unwrap().unwrap().status, FactStatus::Known);
    }

    #[tokio::test]
    async fn a_full_scope_evicts_the_least_used_assumed_fact_never_a_known_one() {
        let config = LongTermMemoryConfig { max_facts: 2, ..LongTermMemoryConfig::default() };
        let (runtime, store) = runtime(config);
        let a = stored(
            admit(&runtime, &candidate("alpha builds with make", "project_fact"), &[]).await,
        );
        let b = stored(
            admit(&runtime, &candidate("beta lints with clippy", "project_fact"), &[]).await,
        );
        admit(&runtime, &candidate("beta lints with clippy", "project_fact"), &[]).await;
        let c = stored(
            admit(&runtime, &candidate("gamma deploys with helm", "project_fact"), &[]).await,
        );
        assert_eq!(
            store.get(&a.id).await.unwrap().unwrap().status,
            FactStatus::Forgotten,
            "least used"
        );
        assert!(store.get(&b.id).await.unwrap().unwrap().status.is_live());
        assert!(store.get(&c.id).await.unwrap().unwrap().status.is_live());
        // Known facts are never evicted: with no assumed fact left, the candidate is refused.
        let by = adk_core::principal::SubjectRef::human("alice");
        store.set_status(&b.id, FactStatus::Known, &by).await.unwrap();
        store.set_status(&c.id, FactStatus::Known, &by).await.unwrap();
        let refused =
            admit(&runtime, &candidate("delta caches with redis", "project_fact"), &[]).await;
        assert!(
            matches!(refused, Admitted::Rejected { ref reason, .. } if reason.contains("known facts")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn secrets_long_text_and_unknown_kinds_are_rejected() {
        let (runtime, store) = runtime(LongTermMemoryConfig::default());
        for (text, kind, expected) in [
            ("the token is ghp_abcdefghijklmnopqrstuvwxyz0123456789", "project_fact", "secret"),
            (&"x".repeat(501)[..], "project_fact", "longer than 500"),
            ("fine", "mood", "unknown kind"),
            ("   ", "project_fact", "empty"),
        ] {
            let admitted = admit(&runtime, &candidate(text, kind), &[]).await;
            assert!(
                matches!(admitted, Admitted::Rejected { ref reason, .. } if reason.contains(expected)),
                "{admitted:?}"
            );
        }
        let quote_secret = quoted(
            "use the staging key",
            "project_fact",
            "use the staging key sk-proj-abcdefghijklmnopqrstuvwxyz",
        );
        let said = [said("use the staging key sk-proj-abcdefghijklmnopqrstuvwxyz")];
        assert!(matches!(admit(&runtime, &quote_secret, &said).await, Admitted::Rejected { .. }));
        assert!(store.list(FactFilter::user("app", "alice")).await.unwrap().is_empty());
    }

    fn said(text: &str) -> UserMessage {
        UserMessage { text: text.to_string(), at: Utc::now(), invocation_id: "inv".into() }
    }

    #[test]
    fn a_quote_verifies_only_against_the_users_own_words() {
        let messages = [said("Please   use pnpm, not npm,\nfor this repo"), said("thanks")];
        let fact = "Use pnpm, not npm";
        assert!(verify_quote("use pnpm, not npm", fact, &messages).is_some());
        assert!(verify_quote("use pnpm,\n  not npm", fact, &messages).is_some());
        // Case, a missing word, or text the user never wrote fail.
        assert!(verify_quote("Use pnpm, not npm", fact, &messages).is_none());
        assert!(verify_quote("use pnpm not npm", fact, &messages).is_none());
        assert!(verify_quote("always use yarn", "Use yarn", &messages).is_none());
        // Too short to mean anything, or unrelated to the fact.
        assert!(verify_quote("pnpm,", fact, &messages).is_none());
        assert!(verify_quote("for this repo", "Deploy on Fridays", &messages).is_none());
        assert!(verify_quote("use pnpm, not npm", fact, &[]).is_none());
    }
}
