//! Discovery and protection: stages 2 and 3 of onboarding (spec 3.5 §7.18, §7.8 "Sources",
//! "Sanitiser", "Feature mapping", "Freeze", A-I9, A-I10, A-I18).
//!
//! | Starting point | Default source | Others |
//! |---|---|---|
//! | Existing site | `Crawl` of its URL | `WordPressRest`, `Wxr`, `IsolatedReplay` through [`Discovery::with_source`] |
//! | Online presence | `Presence` of its profiles | — |
//! | Nothing | none | — |
//!
//! Everything discovery finds is `Assumed` and cites its source ([`record_facts`]); only the
//! owner's confirmation makes a fact `Known`. A migration converts, quarantines against the
//! conversion and converts again without the held subjects ([`migrate`]); quarantine findings,
//! feature decisions, the kind, the freeze and the design choice reach the owner as
//! [`OwnerCard`]s.

use std::sync::Arc;

use adk_core::Result;
use adk_harness::{
    FACT_TEXT_MAX_CHARS, FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact,
    Provenance, redact_fact_text,
};
use adk_site::migrate::convert::{ConvertOptions, Converted, to_content};
use adk_site::migrate::detect::{Detectors, Finding};
use adk_site::migrate::mapping::{FeatureMap, OwnerDecision, map_features};
use adk_site::migrate::parity::{ParityConfig, SiteDesign};
use adk_site::migrate::quarantine::{QuarantineBook, QuarantineEntry, QuarantineState};
use adk_site::migrate::sources::{Crawl, Presence};
use adk_site::migrate::{Fact, SiteInventory, SiteSource, SourceContext, SourceKind};
use awp_types::LanguageTag;
use serde::{Deserialize, Serialize};
use url::Url;

use super::StartingPoint;
use crate::kinds::{KindEvidence, KindProposal, detect};
use crate::themes::ThemeId;

/// Pages the cloaking check fetches twice: the home page and the top pages, as many as the
/// launch gate's parity compares (spec 3.5 §7.8.1).
const CLOAKING_PAGES: usize = 20;

fn migrate_error(error: adk_site::migrate::MigrateError) -> adk_core::AdkError {
    adk_core::AdkError::config(error.to_string())
}

/// Whether the owner keeps the current design or moves to a new theme (spec 3.5 §7.18
/// stages 6–7; owner decision of 2026-10-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "design", content = "theme", rename_all = "snake_case")]
pub enum DesignChoice {
    /// Keep the site's current look: the launch is gated on visual parity under 0.05.
    KeepCurrent,
    /// Move to a new theme: the launch is gated on URL coverage, text and metadata; the
    /// owner approves the look from the preview.
    NewTheme(ThemeId),
}

impl DesignChoice {
    /// The design choice the migration's parity check runs with.
    pub fn site_design(self) -> SiteDesign {
        match self {
            Self::KeepCurrent => SiteDesign::Keep,
            Self::NewTheme(_) => SiteDesign::NewTheme,
        }
    }

    /// The parity thresholds the migration's launch gate runs with: a new theme reports the
    /// perceptual difference without gating on it.
    pub fn parity(self) -> ParityConfig {
        ParityConfig::for_design(self.site_design())
    }
}

/// A decision discovery puts to the owner (spec 3.5 §7.18 stages 2–3, §7.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "card", rename_all = "snake_case")]
pub enum OwnerCard {
    /// Confirm the proposed site kind.
    ConfirmKind {
        /// The proposal and its reasons.
        proposal: KindProposal,
    },
    /// Freeze the legacy site: serve a crawled snapshot, take the old server off the public
    /// path.
    Freeze {
        /// The legacy site.
        url: String,
    },
    /// Release or keep holding a quarantined subject.
    Quarantined {
        /// The subject, its findings and evidence.
        entry: QuarantineEntry,
    },
    /// Decide what a dynamic feature becomes, such as comments or a member area.
    Feature {
        /// The question, options and default.
        decision: OwnerDecision,
    },
    /// Keep the current design or move to a new theme.
    Design {
        /// The new themes on offer.
        themes: Vec<ThemeId>,
    },
}

/// A migration's result: the content converted twice around the quarantine, the book of
/// held subjects, and the feature map (spec 3.5 §7.8).
#[derive(Debug, Clone)]
pub struct Migration {
    /// The content, without the held subjects.
    pub converted: Converted,
    /// The quarantined subjects.
    pub quarantine: QuarantineBook,
    /// Capability drafts for the edge and decisions for the owner.
    pub features: FeatureMap,
}

/// Converts `inventory`, quarantines what the detectors flag against the conversion, and
/// converts again without the held subjects (spec 3.5 §7.8 "Sanitiser", §7.18 stage 3).
///
/// # Errors
///
/// Returns the conversion's error.
pub async fn migrate(inventory: &SiteInventory, language: LanguageTag) -> Result<Migration> {
    migrate_with(inventory, language, Vec::new()).await
}

/// As [`migrate`], quarantining `findings` too, such as the cloaking check's
/// ([`Detectors::scan_cloaking`]), which reads the live site.
///
/// # Errors
///
/// Returns the conversion's error.
pub async fn migrate_with(
    inventory: &SiteInventory,
    language: LanguageTag,
    findings: Vec<Finding>,
) -> Result<Migration> {
    let mut findings = findings;
    findings.extend(Detectors::new().scan_inventory(inventory).await);
    let options = ConvertOptions::new(language);
    let first = to_content(inventory, &options).map_err(migrate_error)?;
    let quarantine = QuarantineBook::new(findings, &first);
    let converted =
        to_content(inventory, &options.excluding(quarantine.excluded())).map_err(migrate_error)?;
    Ok(Migration { converted, quarantine, features: map_features(inventory) })
}

/// What discovery found (spec 3.5 §7.18 stage 2).
#[derive(Debug, Clone)]
pub struct Discovered {
    /// The starting point.
    pub start: StartingPoint,
    /// The inventory, when there was something to read.
    pub inventory: Option<SiteInventory>,
    /// The proposed kind, when the evidence points at one.
    pub kind: Option<KindProposal>,
    /// Whether the existing site runs on a legacy server runtime, so the freeze is offered.
    pub legacy_runtime: bool,
    /// The migration of an existing site.
    pub migration: Option<Migration>,
}

impl Discovered {
    /// The facts found, each `Assumed` with its source.
    pub fn facts(&self) -> &[Fact] {
        self.inventory.as_ref().map_or(&[], |inventory| inventory.facts.as_slice())
    }

    /// The owner's cards (spec 3.5 §7.18 stages 2–3, §7.8): the kind to confirm, the freeze
    /// for a legacy site, every quarantined subject still held, every feature decision, and
    /// the design choice for an existing site.
    pub fn cards(&self) -> Vec<OwnerCard> {
        let mut cards = Vec::new();
        if let Some(proposal) = &self.kind {
            cards.push(OwnerCard::ConfirmKind { proposal: proposal.clone() });
        }
        if let (true, StartingPoint::ExistingSite { url }) = (self.legacy_runtime, &self.start) {
            cards.push(OwnerCard::Freeze { url: url.clone() });
        }
        if let Some(migration) = &self.migration {
            for entry in &migration.quarantine.entries {
                if entry.state != QuarantineState::Refused {
                    cards.push(OwnerCard::Quarantined { entry: entry.clone() });
                }
            }
            for decision in &migration.features.decisions {
                cards.push(OwnerCard::Feature { decision: decision.clone() });
            }
            cards.push(OwnerCard::Design { themes: ThemeId::ALL.to_vec() });
        }
        cards
    }
}

/// Discovery for one starting point (spec 3.5 §7.18 stage 2).
pub struct Discovery {
    start: StartingPoint,
    source: Option<Arc<dyn SiteSource>>,
    language: LanguageTag,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovery").field("start", &self.start).finish_non_exhaustive()
    }
}

impl Discovery {
    /// Discovery from `start` with its default source, converting into `language`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a URL that does not parse.
    pub fn new(start: StartingPoint, language: LanguageTag) -> Result<Self> {
        let parse = |text: &str| {
            Url::parse(text)
                .map_err(|e| adk_core::AdkError::config(format!("`{text}` is not a URL: {e}")))
        };
        let source: Option<Arc<dyn SiteSource>> = match &start {
            StartingPoint::ExistingSite { url } => Some(Arc::new(Crawl::new(parse(url)?))),
            StartingPoint::Presence { profiles } => Some(Arc::new(Presence::new(
                profiles.iter().map(|p| parse(p)).collect::<Result<Vec<_>>>()?,
            ))),
            StartingPoint::Nothing => None,
        };
        Ok(Self { start, source, language })
    }

    /// Reads the existing site with `source` instead: `WordPressRest`, `Wxr` or
    /// `IsolatedReplay`.
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn SiteSource>) -> Self {
        self.source = Some(source);
        self
    }

    /// Runs discovery with `ctx` (spec 3.5 §7.18 stage 2): inventories the source, proposes
    /// the kind, flags a legacy runtime, and migrates an existing site, with the cloaking
    /// check when the owner proved the domain (A-I9).
    ///
    /// # Errors
    ///
    /// Returns the source's or the conversion's error.
    #[tracing::instrument(skip_all, name = "website.onboarding.discover")]
    pub async fn run(&self, ctx: &SourceContext) -> Result<Discovered> {
        let Some(source) = &self.source else {
            return Ok(Discovered {
                start: self.start.clone(),
                inventory: None,
                kind: None,
                legacy_runtime: false,
                migration: None,
            });
        };
        let inventory = source.inventory(ctx).await.map_err(migrate_error)?;
        let kind = detect(&KindEvidence::from_inventory(&inventory));
        let legacy_runtime = matches!(
            inventory.source,
            SourceKind::WordPressRest | SourceKind::Wxr | SourceKind::IsolatedReplay
        ) || inventory
            .urls
            .iter()
            .any(|url| url.path.ends_with(".php") || url.path.contains("/wp-"));
        let migration = match &self.start {
            StartingPoint::ExistingSite { .. } => {
                // The dual fetch reads the live site: it runs once the owner proved the
                // domain (A-I9), and the scan notes why when it did not.
                let cloaking =
                    Detectors::new().scan_cloaking(ctx, &inventory, CLOAKING_PAGES).await;
                for note in &cloaking.notes {
                    tracing::info!(note = %note, "website.onboarding.discover");
                }
                Some(migrate_with(&inventory, self.language.clone(), cloaking.findings).await?)
            }
            _ => None,
        };
        Ok(Discovered {
            start: self.start.clone(),
            inventory: Some(inventory),
            kind,
            legacy_runtime,
            migration,
        })
    }
}

/// Records each discovered fact in the site's long-term facts as `Assumed`, citing the page
/// it came from (spec 3.5 §7.18 stage 2, A-I18, S2). Returns the facts' ids.
///
/// # Errors
///
/// Returns the fact store's error, or a refusal for a fact that holds a secret.
pub async fn record_facts(
    facts: &[Fact],
    store: &dyn FactStore,
    scope: &FactScope,
    session: &str,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for fact in facts {
        let text = redact_fact_text(&format!("{}: {}", fact.key, fact.value), FACT_TEXT_MAX_CHARS)?;
        let mut provenance = Provenance::new(FactSource::ToolObserved, session, "discovery");
        provenance.source_ids = vec![fact.source.url.clone()];
        let stored = store
            .put(MemoryFact::new(
                scope.clone(),
                FactKind::ProjectFact,
                text,
                FactStatus::Assumed,
                provenance,
            ))
            .await?;
        ids.push(stored.id);
    }
    Ok(ids)
}
