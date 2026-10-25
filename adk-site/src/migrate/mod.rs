//! Migration from an existing site or online presence (spec 3.5 §7.8 "Sources", "Sanitiser",
//! "Feature mapping", "Freeze", "Probes"; §7.8.1 migration rows; §7.18 stages 1–3; A-I9, A-I10).
//!
//! | Stage | Types | Spec 3.5 |
//! |-------|-------|----------|
//! | Domain claim | [`claim::DomainClaim`], [`claim::ProvenDomain`], [`ClaimScope`] | §7.18 stage 1, A-I9 |
//! | Sources | [`SiteSource`] → [`SiteInventory`]: [`sources::WordPressRest`], [`sources::Wxr`], [`sources::Crawl`], [`sources::Presence`], [`sources::IsolatedReplay`] | §7.8 "Sources", S2, S3 |
//! | Outbound HTTP | [`http::HttpClient`]: every request signed through a [`http::CrawlSigner`] (Web Bot Auth), robots-respecting, rate-limited, egress-checked and recorded | §7.3 "Outbound", §7.8 |
//! | Sanitiser | [`sanitize::Sanitiser`]: HTML to Markdown through an allowlist | §7.8 "Sanitiser" |
//! | Detectors | [`detect`]: hidden-link spam, cloaking, executables and MIME mismatches, obfuscated scripts | §7.8 "Sanitiser", S3 |
//! | Quarantine | [`quarantine::QuarantineBook`]: findings with evidence, released only by a per-item [`ApprovalGate`](crate::pipeline::ApprovalGate) decision | §7.8, §7.8.1 "Quarantine" |
//! | Feature mapping | [`mapping::map_features`]: `CapabilityV2` drafts and owner decisions | §7.8 "Feature mapping", N5 |
//! | Content | [`convert::to_content`]: inventory items as content items that keep their URLs | §7.8.2, S17 |
//! | Freeze | [`freeze::Freeze`]: a crawled snapshot published as a release, forms mapped to edge capabilities | §7.8 "Freeze", S4 |
//! | Parity | [`parity::MigrationParity`], a launch-gate extension: URL coverage, perceptual difference, main-text similarity, metadata | §7.8.1 |
//! | Probes | [`probe::Prober`]: page digests against the release window, status, certificate, DNS, security headers; [`probe::Drift`] for the Guardian | §7.8 "Probes", §7.8.3, S5 |
//! | Archive | [`archive::LegacyArchive`]: the legacy export as an encrypted artifact; no delete | A-I10 |
//!
//! Nothing here contacts a legacy site except through [`http::HttpClient`], and a site whose
//! control the owner has not proved gets only polite public reads (A-I9).
//!
//! # Example
//!
//! Inventory a WordPress export and turn it into content items:
//!
//! ```rust,no_run
//! use adk_site::migrate::{SiteSource, SourceContext, convert, sources::Wxr};
//!
//! # async fn demo(context: &SourceContext) -> adk_site::migrate::Result<()> {
//! let export = std::fs::read("export.xml").unwrap();
//! let inventory = Wxr::from_bytes(export).inventory(context).await?;
//! let converted = convert::to_content(&inventory, &convert::ConvertOptions::new("en".parse().unwrap()))?;
//! assert_eq!(converted.items.len(), inventory.items.len());
//! # Ok(())
//! # }
//! ```

pub mod archive;
mod audit;
pub mod claim;
pub mod convert;
pub mod detect;
mod error;
pub mod freeze;
pub(crate) mod html;
pub mod http;
mod inventory;
pub mod mapping;
pub mod parity;
pub mod probe;
pub mod quarantine;
pub mod robots;
pub mod sanitize;
pub mod serve;
pub mod sources;
mod xml;

use std::sync::Arc;

use adk_core::audit_log::AuditRecorder;
use adk_core::{ActingContext, Clock, SystemClock};
use async_trait::async_trait;

pub use claim::{ClaimScope, ProvenDomain};
pub use error::{MigrateError, Result};
pub use inventory::{
    DetectedFeature, Fact, FactSource, FeatureEvidence, FeatureKind, FormField, InventoryItem,
    InventoryMedia, InventoryUrl, ItemStatus, Menu, MenuEntry, PageMeta, ReviewQuote,
    SiteInventory, SourceKind, Taxonomy, Term,
};

/// What a source needs to read a site (spec 3.5 §7.8).
///
/// `scope` decides what a source may do: until the owner proves control of the domain
/// ([`claim::DomainClaim`]) only polite public reads run (A-I9).
#[derive(Clone)]
pub struct SourceContext {
    /// The site's own acting identity: drafts are read with credentials the broker issues to it.
    pub acting: ActingContext,
    /// Polite public reads, or everything a proven domain allows.
    pub scope: ClaimScope,
    /// The signed, robots-respecting, rate-limited client every request goes through.
    pub http: Arc<http::HttpClient>,
    /// The clock inventories are stamped with.
    pub clock: Arc<dyn Clock>,
    /// Where `site.migration.*` audit records go, when anywhere.
    pub audit: Option<Arc<dyn AuditRecorder>>,
    /// Upper bounds on what a source reads.
    pub limits: SourceLimits,
}

impl std::fmt::Debug for SourceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceContext")
            .field("acting", &self.acting.agent.id)
            .field("scope", &self.scope)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl SourceContext {
    /// A context for `acting` with polite scope, the system clock and default limits.
    pub fn new(acting: ActingContext, http: Arc<http::HttpClient>) -> Self {
        Self {
            acting,
            scope: ClaimScope::Polite,
            http,
            clock: Arc::new(SystemClock),
            audit: None,
            limits: SourceLimits::default(),
        }
    }

    /// Sets the scope, such as [`ClaimScope::Proven`] after a domain claim.
    #[must_use]
    pub fn with_scope(mut self, scope: ClaimScope) -> Self {
        self.scope = scope;
        self
    }

    /// Sets the clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Records `site.migration.*` audit records in `recorder`.
    #[must_use]
    pub fn with_audit(mut self, recorder: Arc<dyn AuditRecorder>) -> Self {
        self.audit = Some(recorder);
        self
    }

    /// Sets the limits.
    #[must_use]
    pub fn with_limits(mut self, limits: SourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The proven domain, or [`MigrateError::NotProven`] naming `action`.
    ///
    /// # Errors
    ///
    /// [`MigrateError::NotProven`] when the scope is polite (A-I9).
    pub fn require_proven(&self, domain: &str, action: &str) -> Result<&ProvenDomain> {
        self.scope.require(domain, action)
    }
}

/// Upper bounds on what a source reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLimits {
    /// Pages fetched by a crawl.
    pub max_pages: usize,
    /// Links followed from the start page.
    pub max_depth: usize,
    /// Media files downloaded.
    pub max_media: usize,
    /// Bytes read from one response.
    pub max_response_bytes: usize,
    /// Items per WordPress REST collection page (at most 100).
    pub per_page: usize,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_pages: 2_000,
            max_depth: 12,
            max_media: 5_000,
            max_response_bytes: 20 * 1024 * 1024,
            per_page: 100,
        }
    }
}

/// Reads an existing site or presence into a [`SiteInventory`] (spec 3.5 §7.8).
///
/// Implementations: [`sources::WordPressRest`], [`sources::Wxr`], [`sources::Crawl`],
/// [`sources::Presence`] and [`sources::IsolatedReplay`]; [`sources::source_conformance`]
/// checks any of them.
#[async_trait]
pub trait SiteSource: Send + Sync {
    /// Reads the site: URLs, items, media, taxonomies, menus and detected dynamic features.
    ///
    /// # Errors
    ///
    /// Returns an error when the site cannot be read at all; pages that fail are recorded in
    /// the inventory's `notes` instead.
    async fn inventory(&self, ctx: &SourceContext) -> Result<SiteInventory>;
}
