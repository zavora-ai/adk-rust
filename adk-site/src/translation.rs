//! Translation policy as data (spec 3.5 §7.8.2 "Translations", §7.8.3 "Translations at
//! speed").
//!
//! Each item is drafted in every charter language. The policy says which languages need a
//! person's approval, which languages must exist before an item publishes, whether they
//! publish together, and how far a language may trail its source.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use awp_types::{ContentItem, LanguageTag};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Who approves a translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LanguageApproval {
    /// Published without a person's review.
    None,
    /// A person reviews a sample after publication (most kinds).
    #[default]
    SpotCheck,
    /// A person approves each item in this language before it publishes (public bodies).
    PerLanguage,
}

/// A site kind's translation rules.
///
/// # Example
///
/// ```rust
/// use adk_site::translation::{LanguageApproval, TranslationPolicy};
///
/// let policy = TranslationPolicy::official(vec!["en".parse().unwrap(), "sw".parse().unwrap()]);
/// assert!(policy.simultaneous);
/// assert_eq!(policy.approval_for(&"sw".parse().unwrap()), LanguageApproval::PerLanguage);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslationPolicy {
    /// Languages every item must exist in before it publishes (official languages).
    pub required: Vec<LanguageTag>,
    /// The approval for languages not listed in `approval`.
    pub default_approval: LanguageApproval,
    /// Approval per language.
    pub approval: BTreeMap<LanguageTag, LanguageApproval>,
    /// Whether an item's languages publish in one release.
    pub simultaneous: bool,
    /// How far, in seconds, a language may trail its source; default ten minutes.
    pub lag_seconds: u64,
    /// Items whose translations are approved ahead, such as fixed emergency texts, so they
    /// are never slowed.
    pub pre_approved: BTreeSet<String>,
}

impl Default for TranslationPolicy {
    fn default() -> Self {
        Self {
            required: Vec::new(),
            default_approval: LanguageApproval::SpotCheck,
            approval: BTreeMap::new(),
            simultaneous: false,
            lag_seconds: 600,
            pre_approved: BTreeSet::new(),
        }
    }
}

/// Whether a translation group may publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum TranslationReadiness {
    /// Every required language is present.
    Ready,
    /// Publication waits for these languages.
    Waiting {
        /// The missing languages.
        missing: Vec<LanguageTag>,
    },
}

impl TranslationPolicy {
    /// A public body's policy: every official language required, approved per language and
    /// published together.
    pub fn official(languages: Vec<LanguageTag>) -> Self {
        Self {
            approval: languages
                .iter()
                .map(|l| (l.clone(), LanguageApproval::PerLanguage))
                .collect(),
            required: languages,
            default_approval: LanguageApproval::PerLanguage,
            simultaneous: true,
            ..Self::default()
        }
    }

    /// The allowed lag.
    pub fn lag(&self) -> Duration {
        Duration::from_secs(self.lag_seconds)
    }

    /// Who approves translations into `language`.
    pub fn approval_for(&self, language: &LanguageTag) -> LanguageApproval {
        self.approval.get(language).copied().unwrap_or(self.default_approval)
    }

    /// The required languages missing from `group`: the versions of one item, one per
    /// language.
    pub fn missing_languages<'a>(
        &self,
        group: impl IntoIterator<Item = &'a ContentItem>,
    ) -> Vec<LanguageTag> {
        let present: BTreeSet<String> =
            group.into_iter().map(|item| item.language.as_str().to_ascii_lowercase()).collect();
        self.required
            .iter()
            .filter(|l| !present.contains(&l.as_str().to_ascii_lowercase()))
            .cloned()
            .collect()
    }

    /// Whether `group` may publish: with `simultaneous`, only when every required language is
    /// present; otherwise always.
    pub fn readiness<'a>(
        &self,
        group: impl IntoIterator<Item = &'a ContentItem>,
    ) -> TranslationReadiness {
        let missing = self.missing_languages(group);
        if self.simultaneous && !missing.is_empty() {
            TranslationReadiness::Waiting { missing }
        } else {
            TranslationReadiness::Ready
        }
    }

    /// Whether a translation of a source published at `source_published_at` is overdue at `now`.
    pub fn is_overdue(&self, source_published_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        (now - source_published_at).to_std().is_ok_and(|elapsed| elapsed > self.lag())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use awp_types::{ContentKind, ContentState};

    use super::*;

    fn item(language: &str) -> ContentItem {
        ContentItem {
            id: format!("notices/flood/{language}").as_str().into(),
            kind: ContentKind::Notice,
            slug: "flood".into(),
            language: language.parse().unwrap(),
            fields: BTreeMap::new(),
            volatile: vec![],
            body: None,
            media: vec![],
            sources: vec![],
            schedule: None,
            state: ContentState::Approved,
        }
    }

    #[test]
    fn official_languages_publish_together() {
        let policy =
            TranslationPolicy::official(vec!["en".parse().unwrap(), "sw".parse().unwrap()]);
        let english = item("en");
        assert_eq!(
            policy.readiness([&english]),
            TranslationReadiness::Waiting { missing: vec!["sw".parse().unwrap()] }
        );
        let swahili = item("sw");
        assert_eq!(policy.readiness([&english, &swahili]), TranslationReadiness::Ready);
    }

    #[test]
    fn the_default_policy_never_waits_and_tracks_lag() {
        let policy = TranslationPolicy {
            required: vec!["fr".parse().unwrap()],
            ..TranslationPolicy::default()
        };
        assert_eq!(policy.readiness([&item("en")]), TranslationReadiness::Ready);
        assert_eq!(policy.approval_for(&"fr".parse().unwrap()), LanguageApproval::SpotCheck);
        let at = Utc::now();
        assert!(!policy.is_overdue(at, at + chrono::Duration::minutes(10)));
        assert!(policy.is_overdue(at, at + chrono::Duration::minutes(11)));
    }
}
