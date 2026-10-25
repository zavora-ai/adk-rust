//! A deterministic AWP 2.0 site for tests, the conformance kit's self-check and the test
//! vectors (spec 3.5 §7.7, §11).
//!
//! [`FixtureSite::build`] signs a contract, a key set with `release` and `card`/`receipt`
//! delegations, public terms, an Agent Card, a snapshot and a delta release, the site log's
//! tiles and a checkpoint, from fixed seeds and times; [`FixtureSite::router`] serves it.
//! Tests tamper with [`FixtureSite::files`] to reproduce each S13 failure.

use std::collections::BTreeMap;
use std::sync::Arc;

use awp_types::v2::contract::{CHECKPOINT_PATH, release_manifest_path};
use awp_types::{
    ContactLinks, ContractRelease, Digest, FileEntry, GeneratorRef, IdentityMethod, IdentityPolicy,
    KeyScope, KeySet, LogEntry, PolicyRef, Profile, PublicTerms, ReleaseManifest, ReleaseSource,
    RootKey, SiteContract, SiteRef, SurfaceLinks,
};
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::signing::SigningError;
use crate::signing::anchor::{FixedTxtResolver, root_record, root_record_name};
use crate::signing::bundle::StaticBundle;
use crate::signing::card::sign_agent_card;
use crate::signing::jws::{DocumentSigner, Ed25519DocumentSigner, document_digest};
use crate::signing::keys::delegate;
use crate::signing::log::{LOG_PATH, log_origin, signed_checkpoint, tiles};

/// The fixture's root key seed.
pub const ROOT_SEED: [u8; 32] = [0x11; 32];
/// The fixture's `release` key seed.
pub const RELEASE_SEED: [u8; 32] = [0x22; 32];
/// The fixture's `card` and `receipt` key seed.
pub const EDGE_SEED: [u8; 32] = [0x33; 32];

/// The fixture's clock: 2026-10-25T09:00:00Z.
pub fn fixture_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap()
}

/// A file the fixture serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// Its media type.
    pub content_type: String,
    /// Its bytes.
    pub body: Vec<u8>,
}

impl Served {
    fn new(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self { content_type: content_type.to_owned(), body: body.into() }
    }
}

/// A signed AWP 2.0 site built from fixed seeds and times.
#[derive(Debug)]
pub struct FixtureSite {
    /// The domain.
    pub domain: String,
    /// The root key.
    pub root: Arc<Ed25519DocumentSigner>,
    /// The `release` key.
    pub release_key: Arc<Ed25519DocumentSigner>,
    /// The `card` and `receipt` key.
    pub edge_key: Arc<Ed25519DocumentSigner>,
    /// The key set.
    pub keys: KeySet,
    /// The signed contract.
    pub contract: SiteContract,
    /// The release manifests, by sequence.
    pub manifests: Vec<ReleaseManifest>,
    /// The site log's entries.
    pub entries: Vec<LogEntry>,
    /// Every served file, by absolute path.
    pub files: BTreeMap<String, Served>,
    /// A resolver holding the `_awp-root` record.
    pub resolver: FixedTxtResolver,
}

fn file_entry(served: &Served) -> FileEntry {
    FileEntry {
        sha256: Digest::from_sha256_bytes(Sha256::digest(&served.body).into()),
        size: served.body.len() as u64,
        content_type: served.content_type.clone(),
    }
}

fn page(title: &str, body: &str) -> Served {
    Served::new(
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><html><head><title>{title}</title>\
             <link rel=\"alternate\" type=\"application/awp+json\" href=\"index.awp.json\">\
             <link rel=\"alternate\" type=\"text/markdown\" href=\"index.md\">\
             <link rel=\"awp\" href=\"/.well-known/awp.json\"></head>\
             <body><h1>{title}</h1><p>{body}</p></body></html>\n"
        ),
    )
}

fn twins(path: &str, title: &str, body: &str, release_seq: u64) -> Vec<(String, Served)> {
    let base = path.trim_end_matches('/');
    let view = json!({
        "path": path,
        "releaseSeq": release_seq,
        "kind": "page",
        "language": "en",
        "title": title,
        "resource": { "body": body },
        "markdown": format!("{base}/index.md"),
    });
    vec![
        (format!("{base}/index.html"), page(title, body)),
        (
            format!("{base}/index.awp.json"),
            Served::new(
                "application/awp+json",
                serde_json::to_vec_pretty(&view).unwrap_or_default(),
            ),
        ),
        (
            format!("{base}/index.md"),
            Served::new("text/markdown; charset=utf-8", format!("# {title}\n\n{body}\n")),
        ),
    ]
}

impl FixtureSite {
    /// Builds the fixture for `domain` in `profile`: release 1 is a snapshot, release 2 a
    /// delta that changes the about page.
    ///
    /// # Errors
    ///
    /// [`SigningError`] when a document cannot be signed.
    pub async fn build(domain: &str, profile: Profile) -> Result<Self, SigningError> {
        let root = Arc::new(Ed25519DocumentSigner::from_seed("site-root-2026", ROOT_SEED)?);
        let release_key =
            Arc::new(Ed25519DocumentSigner::from_seed("release-2026-10", RELEASE_SEED)?);
        let edge_key = Arc::new(Ed25519DocumentSigner::from_seed("edge-2026-10", EDGE_SEED)?);
        let not_before = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let not_after = Utc.with_ymd_and_hms(2026, 12, 29, 0, 0, 0).unwrap();
        let keys = KeySet {
            root: RootKey {
                kid: root.kid().to_owned(),
                public_key: root.public_key(),
                endorsement: None,
            },
            delegations: vec![
                delegate(
                    root.as_ref(),
                    release_key.kid(),
                    release_key.public_key(),
                    vec![KeyScope::Release],
                    not_before,
                    not_after,
                )
                .await?,
                delegate(
                    root.as_ref(),
                    edge_key.kid(),
                    edge_key.public_key(),
                    vec![KeyScope::Card, KeyScope::Receipt],
                    not_before,
                    not_after,
                )
                .await?,
            ],
            retired_roots: vec![],
            revocations: vec![],
        };
        let origin = format!("https://{domain}");
        let terms = PublicTerms {
            usage: Some("train-ai=n, search=y".to_owned()),
            policies: vec![PolicyRef {
                name: "privacy".to_owned(),
                url: format!("{origin}/privacy/"),
                version: "2026-10".to_owned(),
                digest: None,
            }],
            consent_scopes: vec![awp_types::ConsentScope::Newsletter],
            contact: ContactLinks {
                security: Some(format!("{origin}/.well-known/security.txt")),
                email: None,
                page: None,
            },
        };
        let mut contract = SiteContract::new(SiteRef {
            id: uuid::Uuid::from_u128(0x018f_2c7e_6a4b_7c8d_9e0f_1a2b_3c4d_5e6f),
            name: "Example Solar".to_owned(),
            domain: domain.to_owned(),
            languages: vec!["en".parse().map_err(|_| SigningError::Key("language".to_owned()))?],
        });
        contract.profile = Some(profile);
        contract.surfaces = Some(SurfaceLinks {
            a2a_card: Some(format!("{origin}/.well-known/agent-card.json")),
            mcp_server_card: None,
            manifest: (profile != Profile::Static).then(|| format!("{origin}/awp/v2/manifest")),
            llms_txt: Some(format!("{origin}/llms.txt")),
            machine_views: true,
        });
        if profile != Profile::Static {
            contract.identity = Some(IdentityPolicy {
                accepts: vec![IdentityMethod::WebBotAuth],
                trust_rules: Some(format!("{origin}/awp/v2/trust")),
            });
            contract.status = Some(awp_types::ContractStatus {
                url: format!("{origin}/awp/v2/status"),
                last_known: Some(awp_types::SiteState::Operational),
            });
        }
        contract.release = Some(ContractRelease {
            seq: 1,
            manifest: format!("{origin}{}", release_manifest_path(1)),
            checkpoint: format!("{origin}{CHECKPOINT_PATH}"),
        });
        contract.contact = Some(terms.contact.clone());
        let bundle = StaticBundle::build(contract, &keys, &terms, root.as_ref()).await?;
        let contract = bundle.contract().clone();

        let card = json!({
            "protocolVersion": "1.0",
            "name": "Example Solar",
            "description": "Answers questions about Example Solar's panels.",
            "url": format!("{origin}/a2a"),
            "version": "1.0.0",
            "capabilities": {},
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["application/json"],
            "skills": [{ "id": "search_site", "name": "search_site", "description": "Search the site", "tags": [] }],
        });
        let card = sign_agent_card(&card, edge_key.as_ref()).await?;

        let mut release_one: BTreeMap<String, Served> = BTreeMap::new();
        for (path, bytes) in bundle.files() {
            release_one.insert(path.clone(), Served::new("application/awp+json", bytes.clone()));
        }
        release_one.insert(
            "/.well-known/agent-card.json".to_owned(),
            Served::new("application/json", serde_json::to_vec_pretty(&card).unwrap_or_default()),
        );
        release_one.insert(
            "/.well-known/security.txt".to_owned(),
            Served::new(
                "text/plain; charset=utf-8",
                format!("Contact: mailto:security@{domain}\nExpires: 2027-10-25T00:00:00Z\n"),
            ),
        );
        release_one.insert(
            "/robots.txt".to_owned(),
            Served::new(
                "text/plain; charset=utf-8",
                "User-agent: *\nContent-Usage: train-ai=n, search=y\nAllow: /\n",
            ),
        );
        release_one.insert(
            "/llms.txt".to_owned(),
            Served::new(
                "text/plain; charset=utf-8",
                format!("# Example Solar\n\n- [Home]({origin}/index.md)\n"),
            ),
        );
        for (path, served) in twins("/", "Example Solar", "Solar power for homes in Nairobi.", 1)
            .into_iter()
            .chain(twins("/about/", "About us", "We install panels.", 1))
        {
            release_one.insert(path, served);
        }

        let generator =
            GeneratorRef { name: "adk-awp-fixture".to_owned(), version: "1".to_owned() };
        let snapshot = ReleaseManifest {
            seq: 1,
            previous: None,
            base: None,
            created_at: fixture_time(),
            files: release_one
                .iter()
                .map(|(path, served)| (path.clone(), file_entry(served)))
                .collect(),
            removed: Default::default(),
            redirects: vec![awp_types::Redirect {
                from: "/old/".to_owned(),
                to: "/about/".to_owned(),
                status: 301,
            }],
            source: ReleaseSource::Onboarding,
            generator: generator.clone(),
            charter_version: 1,
        };
        let mut changed = BTreeMap::new();
        for (path, served) in twins("/about/", "About us", "We install and maintain panels.", 2) {
            changed.insert(path, served);
        }
        let delta = ReleaseManifest {
            seq: 2,
            previous: Some(document_digest(&snapshot)?),
            base: Some(1),
            created_at: fixture_time() + chrono::Duration::hours(1),
            files: changed
                .iter()
                .map(|(path, served)| (path.clone(), file_entry(served)))
                .collect(),
            removed: Default::default(),
            redirects: snapshot.redirects.clone(),
            source: ReleaseSource::Edit { job: "freshness".to_owned(), decision: None },
            generator,
            charter_version: 1,
        };
        let manifests = vec![snapshot, delta];

        let mut files = release_one;
        files.extend(changed);
        let mut site = Self {
            domain: domain.to_owned(),
            root,
            release_key,
            edge_key,
            keys,
            contract,
            manifests,
            entries: Vec::new(),
            files,
            resolver: FixedTxtResolver::default(),
        };
        site.resolver = FixedTxtResolver::default().with(
            &root_record_name(domain),
            &root_record(site.root.kid(), &site.root.public_key()),
        );
        site.publish_log().await?;
        Ok(site)
    }

    /// Writes the release manifests, the log's tiles and a signed checkpoint into
    /// [`Self::files`] for the current [`Self::manifests`].
    ///
    /// # Errors
    ///
    /// [`SigningError`] when a manifest or the checkpoint cannot be signed.
    pub async fn publish_log(&mut self) -> Result<(), SigningError> {
        self.files.retain(|path, _| {
            !path.starts_with("/.well-known/awp/releases/") && !path.starts_with(LOG_PATH)
        });
        self.entries.clear();
        for manifest in &self.manifests {
            let value = serde_json::to_vec_pretty(manifest)
                .map_err(|error| SigningError::Canonicalize(error.to_string()))?;
            self.files.insert(
                release_manifest_path(manifest.seq),
                Served::new("application/awp+json", value),
            );
            self.entries.push(LogEntry { seq: manifest.seq, manifest: document_digest(manifest)? });
        }
        for (path, bytes) in tiles(&self.entries)? {
            self.files.insert(
                format!("{LOG_PATH}{path}"),
                Served::new("application/octet-stream", bytes),
            );
        }
        let signer = Arc::new(self.release_key.note_signer(&log_origin(&self.domain))?);
        let checkpoint = signed_checkpoint(&self.domain, &self.entries, signer).await?;
        self.files.insert(
            CHECKPOINT_PATH.to_owned(),
            Served::new("text/plain; charset=utf-8", checkpoint),
        );
        Ok(())
    }

    /// The JSON of served file `path`.
    pub fn json(&self, path: &str) -> Option<Value> {
        self.files.get(path).and_then(|served| serde_json::from_slice(&served.body).ok())
    }

    /// Replaces served file `path` with `value` as JSON.
    pub fn set_json(&mut self, path: &str, value: &Value) {
        if let Some(served) = self.files.get_mut(path) {
            served.body = serde_json::to_vec_pretty(value).unwrap_or_default();
        }
    }

    /// A router serving the files with HSTS, and a CSP on HTML pages.
    pub fn router(&self) -> Router {
        let files = Arc::new(self.files.clone());
        Router::new().fallback(move |request: Request| {
            let files = Arc::clone(&files);
            async move { serve(&files, request.uri().path()) }
        })
    }
}

fn serve(files: &BTreeMap<String, Served>, path: &str) -> Response {
    let lookup = if path.ends_with('/') { format!("{path}index.html") } else { path.to_owned() };
    let Some(served) = files.get(&lookup) else {
        return (StatusCode::NOT_FOUND, Body::empty()).into_response();
    };
    let mut response = (StatusCode::OK, served.body.clone()).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&served.content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    if served.content_type.starts_with("text/html") {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'; script-src 'none'"),
        );
    }
    response
}
