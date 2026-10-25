//! # adk-site
//!
//! The site pipeline of an AWP 2.0 website (spec 3.5 §7.8): content as files, a static
//! generator with machine views of every page, a launch gate, signed releases in an
//! append-only log, and hosts that switch releases atomically. Nothing here runs when a page
//! is requested: a site is static files plus a log that proves which files are the site.
//!
//! | Module | Contents | Spec 3.5 |
//! |--------|----------|----------|
//! | [`batch`] | [`batch::Batcher`]: windows per kind, flush triggers, size limits; [`batch::check_host`] | §7.8.3, Q15, Q16 |
//! | [`content`] | `site.toml`, items as Markdown with front matter, [`content::ContentStore`] and [`content::ContentHistory`] with conformance suites | §7.8.2 |
//! | [`stream`] | [`stream::StreamLane`]: entries signed by the edge key, served at once, folded into releases | §7.8.3, A-I26 |
//! | [`theme`] | Templates and design tokens, WCAG contrast | §7.8, §7.16 |
//! | [`generator`] | [`generator::SiteGenerator`] and the built-in renderer: pages per language, Markdown and structured twins, `llms.txt`, `robots.txt`, `security.txt`, sitemaps, feeds, JSON-LD, redirects, incremental builds; Zola, Hugo, Astro and Build Awesome as sandboxed external generators (features `gen-*`) | §7.4, §7.8 |
//! | [`feed`] | [`feed::ContentSource`], model-free [`feed::FeedMapping`]s, feed bounds | §7.8.2, §7.8.3, A-I27 |
//! | [`gate`] | [`gate::LaunchGate`] in fast and full tiers | §7.8.1, §7.8.3 |
//! | [`scan`] | Server-code refusal | A-I1 |
//! | [`release`] | [`release::ReleaseBuilder`]: manifests, snapshots and deltas | §7.7, §7.8.3 |
//! | [`signer`] | [`signer::ReleaseSigner`]: the delegated key that signs checkpoints | §7.7, A-I5 |
//! | [`live`] | [`live::LiveStore`]: the live lane the edge serves | §7.8.3, A-I26 |
//! | [`log`] | [`log::SiteLog`]: `{seq, manifest}` entries, C2SP tiles and checkpoints, proofs | §7.7; AWP 2.0 §14.5 |
//! | [`host`] | [`host::SiteHost`], [`host::DirectoryHost`] and the host conformance suite; S3, GCS, Azure, Cloudflare, Netlify, GitHub Pages and rsync adapters (features `host-*`) | §7.8, §7.8.3 |
//! | [`lanes`] | Release, live and stream lanes per field; claims stay in releases | §7.8.3, A-I26 |
//! | [`pipeline`] | Draft, check, preview, approve, release, publish, announce, measure; corrections, withdrawals, schedules, revert, rollback | §7.8.2 |
//! | [`media`] | Uploads sniffed, executables and polyglots refused, location stripped, resized | §7.8.2 |
//! | [`updates`] | [`updates::UpdateEngine`]: lanes, feed bounds, batches, exceptions, scheduled full checks, revert and replay | §7.8.3, A-I24–A-I27 |
//! | [`translation`] | Translation policy as data | §7.8.2, §7.8.3 |
//! | [`redirects`] | Redirects in each host's format | §7.8 |
//! | `migrate` (feature `migrate`) | Sources, domain claim, sanitiser, feature mapping, freeze, parity, probes, archive | §7.8, §7.8.1, §7.18 |
//!
//! # Example
//!
//! Build a site, release it into a signed log and publish it to a directory:
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::audit_log::InMemoryAuditLogStore;
//! use adk_site::content::{SiteConfig, SiteProject};
//! use adk_site::generator::{BuiltinGenerator, SiteGenerator};
//! use adk_site::host::{DirectoryHost, HostTarget, SiteHost};
//! use adk_site::log::SiteLog;
//! use adk_site::release::{ReleaseBuilder, ReleaseOptions};
//! use adk_site::signer::Ed25519ReleaseSigner;
//! use awp_types::ReleaseSource;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_site::Result<()> {
//! let config = SiteConfig::new("Example", "example.com", vec!["en".parse().unwrap()]);
//! let project = SiteProject::new(config, Vec::new(), chrono::Utc::now());
//! let (out, served) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
//! let report = BuiltinGenerator.build(&project, out.path()).await?;
//!
//! let signer = Arc::new(Ed25519ReleaseSigner::generate("example.com")?);
//! let log = SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), "example.com", signer).await?;
//! let options = ReleaseOptions {
//!     source: ReleaseSource::Onboarding,
//!     generator: report.generator.clone(),
//!     charter_version: 1,
//!     redirects: report.redirects.clone(),
//!     written: None,
//!     force_snapshot: false,
//! };
//! let release = ReleaseBuilder::new().release(out.path(), None, options, &log).await?;
//!
//! let host = DirectoryHost::new(served.path());
//! host.publish(&release, HostTarget::Production).await?;
//! assert!(host.fetch("/index.html").await?.is_some());
//! assert!(host.fetch("/.well-known/awp/log/checkpoint").await?.is_some());
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod batch;
pub mod content;
mod error;
pub mod feed;
pub mod gate;
pub mod generator;
pub mod host;
pub mod lanes;
pub mod live;
pub mod log;
pub mod media;
#[cfg(feature = "migrate")]
pub mod migrate;
pub mod pipeline;
pub mod redirects;
pub mod release;
pub mod scan;
pub mod signer;
pub mod stream;
pub mod theme;
pub mod translation;
pub mod updates;

pub use error::{Result, SiteError};
