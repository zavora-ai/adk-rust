//! The AWP 2.0 conformance kit (feature `verify`; spec 3.5 §7.7 "Conformance kit"; AWP 2.0
//! §16).
//!
//! [`run`] checks a site against its profile's requirements — `static` S1–S9, `static+edge`
//! adds E1–E13, `dynamic` takes the edge rows — through any [`Fetcher`]: HTTPS for a deployed
//! site, [`RouterFetcher`](crate::verify::fetch::RouterFetcher) for an in-process one. Passive
//! checks send only `GET` requests for published documents. Active checks of edge routes —
//! still `GET` only — run when [`ConformanceOptions::active_edge_checks`] is set, which the
//! caller does only for deployments whose control the operator proved (A-I9). Requirements no
//! passive or `GET` check can observe are reported as skipped.
//!
//! [`rules`] holds the document rules the types accept and conformance enforces; [`fixture`]
//! a deterministic signed site; [`vectors`] the protocol's test vectors.

pub mod fixture;
pub mod rules;
pub mod vectors;

use std::sync::Arc;

use awp_types::{
    CapabilityManifestV2, ErrorCode, ErrorEnvelope, KeyScope, KeySet, Profile, SiteContract,
    SiteStatus, TrustTier,
};
use axum::http::{HeaderMap, HeaderValue};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::signing::anchor::DnsTxtResolver;
use crate::signing::jws::verify;
use crate::signing::keys::{Dated, delegated_key, verify_key_set};
use crate::verify::fetch::Fetcher;
use crate::verify::pins::InMemoryPinStore;
use crate::verify::{Verifier, VerifyOptions};

use rules::{DocumentKind, check_document, release_rules};

/// Options of a conformance run.
#[derive(Debug, Clone, Default)]
pub struct ConformanceOptions {
    /// Run the `GET` checks of edge routes; only for a deployment whose control the operator
    /// proved (A-I9).
    pub active_edge_checks: bool,
    /// Files sampled; `0` uses the verifier's default.
    pub samples: usize,
    /// The time live documents are checked at; `None` is now.
    pub now: Option<DateTime<Utc>>,
}

/// The outcome of one requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckStatus {
    /// Met.
    Pass,
    /// Not met.
    Fail,
    /// Not observable by this run.
    Skipped,
}

/// One requirement's result (AWP 2.0 §16.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// The requirement's id, such as `S5` or `E3`.
    pub id: &'static str,
    /// The requirement.
    pub requirement: &'static str,
    /// The outcome.
    pub status: CheckStatus,
    /// Why, for a failure or a skip.
    pub detail: String,
}

/// A conformance run's results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// The domain.
    pub domain: String,
    /// The contract's profile.
    pub profile: Option<Profile>,
    /// One result per requirement of the profile.
    pub results: Vec<CheckResult>,
}

impl ConformanceReport {
    /// Returns `true` when no requirement failed.
    pub fn passed(&self) -> bool {
        self.results.iter().all(|result| result.status != CheckStatus::Fail)
    }

    /// Returns the result of requirement `id`.
    pub fn result(&self, id: &str) -> Option<&CheckResult> {
        self.results.iter().find(|result| result.id == id)
    }
}

const STATIC_REQUIREMENTS: &[(&str, &str)] = &[
    ("S1", "Serves the contract as application/awp+json with profile, keys, release and proof"),
    ("S2", "Serves the key set; the root signs the contract and the delegations"),
    ("S3", "Publishes the _awp-root DNS record for the root key"),
    ("S4", "Serves no file outside its latest release and no server-executable file"),
    ("S5", "Publishes the release manifests, the site log as tiles and a signed checkpoint"),
    ("S6", "Gives every HTML page both twins, linked with rel=\"alternate\""),
    ("S7", "Publishes its public terms with a matching digest"),
    ("S8", "Signs its A2A Agent Card with a card delegation"),
    ("S9", "Uses snapshots and deltas as AWP 2.0 §14.4 defines"),
];

const EDGE_REQUIREMENTS: &[(&str, &str)] = &[
    ("E1", "The contract names identity, status, the manifest and an A2A or MCP surface"),
    ("E2", "Assigns tiers only from verified credentials"),
    ("E3", "Lists only the capabilities visible to the caller's tier"),
    ("E4", "Exposes every capability over each advertised binding through one governed path"),
    ("E5", "Answers held calls with 202, Location and the held-call document"),
    ("E6", "Executes a non_idempotent capability at most once per idempotency key"),
    ("E7", "Refuses consent-bound calls without a live receipt; revocation, export, erasure"),
    ("E8", "Serves a signed status"),
    ("E9", "Delivers events as Standard Webhooks with retries, suspension and replay"),
    ("E10", "Signs commerce receipts; honours open quotes"),
    ("E11", "Answers failures with the error envelope"),
    ("E12", "Negotiates versions with AWP-Version"),
    ("E13", "Exposes no route that changes the site, its content, its policies or its keys"),
];

/// The requirement a verifier failure breaks, and the order in which the verifier reaches it.
fn requirement_of(failure: &str) -> (&'static str, usize) {
    match failure {
        "contract.unavailable" | "contract.invalid" => ("S1", 0),
        "keys.unavailable" | "keys.invalid" => ("S2", 1),
        "root.unanchored" | "root.changed" => ("S3", 2),
        "contract.signature" | "delegation.signature" => ("S2", 3),
        "card.signature" => ("S8", 4),
        "charter.digest" => ("S7", 5),
        "release.executable" | "page.digest" => ("S4", 7),
        _ => ("S5", 6),
    }
}

fn first_reached(id: &str) -> usize {
    match id {
        "S1" => 0,
        "S2" => 1,
        "S3" => 2,
        "S8" => 4,
        "S7" => 5,
        "S5" | "S9" => 6,
        "S4" | "S6" => 7,
        _ => 8,
    }
}

fn result(
    id: &'static str,
    requirement: &'static str,
    status: CheckStatus,
    detail: impl Into<String>,
) -> CheckResult {
    CheckResult { id, requirement, status, detail: detail.into() }
}

/// Runs the conformance suite against `domain` (AWP 2.0 §16).
///
/// # Example
///
/// ```
/// use std::sync::Arc;
///
/// use adk_awp::conformance::fixture::{FixtureSite, fixture_time};
/// use adk_awp::conformance::{ConformanceOptions, run};
/// use adk_awp::verify::fetch::RouterFetcher;
/// use awp_types::Profile;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let site = FixtureSite::build("example.com", Profile::Static).await.unwrap();
/// let options = ConformanceOptions { now: Some(fixture_time()), ..Default::default() };
/// let report = run(
///     Arc::new(RouterFetcher::new(site.router())),
///     Arc::new(site.resolver.clone()),
///     "example.com",
///     &options,
/// )
/// .await;
/// assert!(report.passed(), "{report:#?}");
/// # }
/// ```
pub async fn run(
    fetcher: Arc<dyn Fetcher>,
    resolver: Arc<dyn DnsTxtResolver>,
    domain: &str,
    options: &ConformanceOptions,
) -> ConformanceReport {
    let now = options.now.unwrap_or_else(Utc::now);
    let mut verify_options = VerifyOptions { now: Some(now), ..Default::default() };
    if options.samples > 0 {
        verify_options.samples = options.samples;
    }
    let verifier =
        Verifier::new(Arc::clone(&fetcher), resolver, Arc::new(InMemoryPinStore::default()))
            .with_options(verify_options);
    let verified = verifier.verify(domain).await;
    let origin = format!("https://{}", verified.domain);

    let contract_value =
        fetch_json(fetcher.as_ref(), &format!("{origin}/.well-known/awp.json"), &HeaderMap::new())
            .await;
    let contract: Option<SiteContract> =
        contract_value.as_ref().and_then(|value| serde_json::from_value(value.clone()).ok());
    let profile = contract.as_ref().and_then(|contract| contract.profile);
    let contract_violations = contract_value
        .as_ref()
        .map(|value| check_document(DocumentKind::SiteContract, value))
        .unwrap_or_default();

    let failed_at =
        verified.failure.as_ref().map(|failure| (requirement_of(failure.name), failure));
    let mut results = Vec::new();
    for &(id, requirement) in STATIC_REQUIREMENTS {
        if profile == Some(Profile::Dynamic) && matches!(id, "S4" | "S5" | "S8" | "S9") {
            continue;
        }
        let verdict = match failed_at {
            Some(((broken, _), failure)) if broken == id => {
                result(id, requirement, CheckStatus::Fail, failure.to_string())
            }
            Some(((_, order), failure)) if first_reached(id) > order => result(
                id,
                requirement,
                CheckStatus::Skipped,
                format!("not reached: verification stopped at {}", failure.name),
            ),
            _ => result(id, requirement, CheckStatus::Pass, ""),
        };
        results.push(verdict);
    }

    // S1 member rules.
    let s1: Vec<String> = contract_violations
        .iter()
        .filter(|v| {
            matches!(
                v.rule,
                "contract.proof"
                    | "contract.keys"
                    | "contract.profile"
                    | "contract.release"
                    | "document.schema"
            )
        })
        .map(|v| format!("{}: {}", v.rule, v.detail))
        .collect();
    downgrade(&mut results, "S1", &s1);

    // S9 release rules on the latest manifest.
    if let Some(seq) = verified.release_seq {
        let url = format!("{origin}/.well-known/awp/releases/{seq}.json");
        let s9: Vec<String> = match fetch_json(fetcher.as_ref(), &url, &HeaderMap::new()).await {
            Some(value) => match serde_json::from_value(value) {
                Ok(manifest) => release_rules(&manifest)
                    .into_iter()
                    .map(|v| format!("{}: {}", v.rule, v.detail))
                    .collect(),
                Err(error) => vec![format!("document.schema: {error}")],
            },
            None => vec![format!("{url} is unavailable")],
        };
        downgrade(&mut results, "S9", &s9);
    }

    // S6 machine views of the sampled HTML pages.
    if contract.as_ref().and_then(|c| c.surfaces.as_ref()).is_some_and(|s| s.machine_views) {
        let mut s6 = Vec::new();
        for path in verified.sampled.iter().filter(|path| path.ends_with("/index.html")) {
            let base = path.trim_end_matches("index.html");
            for twin in ["index.awp.json", "index.md"] {
                let url = format!("{origin}{base}{twin}");
                match fetcher.get(&url, &HeaderMap::new()).await {
                    Ok(fetched) if fetched.status == 200 => {}
                    _ => s6.push(format!("{base}{twin} is missing")),
                }
            }
            if let Ok(page) = fetcher.get(&format!("{origin}{path}"), &HeaderMap::new()).await {
                let html = String::from_utf8_lossy(&page.body);
                if !html.contains("type=\"application/awp+json\"")
                    || !html.contains("type=\"text/markdown\"")
                {
                    s6.push(format!("{path} does not link its twins"));
                }
            }
        }
        downgrade(&mut results, "S6", &s6);
    }

    if matches!(profile, Some(Profile::StaticEdge | Profile::Dynamic)) {
        let keys: Option<KeySet> = match contract.as_ref().and_then(|c| c.keys.clone()) {
            Some(url) => fetch_json(fetcher.as_ref(), &url, &HeaderMap::new())
                .await
                .and_then(|value| serde_json::from_value(value).ok())
                .filter(|keys| verify_key_set(keys).is_ok()),
            None => None,
        };
        edge_checks(
            &mut results,
            fetcher.as_ref(),
            contract.as_ref(),
            keys.as_ref(),
            &contract_violations,
            options,
            now,
        )
        .await;
    }
    ConformanceReport { domain: verified.domain, profile, results }
}

fn downgrade(results: &mut [CheckResult], id: &str, problems: &[String]) {
    if problems.is_empty() {
        return;
    }
    if let Some(result) =
        results.iter_mut().find(|result| result.id == id && result.status != CheckStatus::Fail)
    {
        result.status = CheckStatus::Fail;
        result.detail = problems.join("; ");
    }
}

async fn fetch_json(fetcher: &dyn Fetcher, url: &str, headers: &HeaderMap) -> Option<Value> {
    let fetched = fetcher.get(url, headers).await.ok()?;
    (fetched.status == 200).then(|| serde_json::from_slice(&fetched.body).ok()).flatten()
}

async fn edge_checks(
    results: &mut Vec<CheckResult>,
    fetcher: &dyn Fetcher,
    contract: Option<&SiteContract>,
    keys: Option<&KeySet>,
    contract_violations: &[rules::RuleViolation],
    options: &ConformanceOptions,
    now: DateTime<Utc>,
) {
    let e1: Vec<String> = contract_violations
        .iter()
        .filter(|v| {
            matches!(
                v.rule,
                "contract.identity" | "contract.status" | "contract.manifest" | "contract.surfaces"
            )
        })
        .map(|v| format!("{}: {}", v.rule, v.detail))
        .collect();
    for &(id, requirement) in EDGE_REQUIREMENTS {
        let skipped = |why: &str| result(id, requirement, CheckStatus::Skipped, why);
        let outcome = match id {
            "E1" if e1.is_empty() => result(id, requirement, CheckStatus::Pass, ""),
            "E1" => result(id, requirement, CheckStatus::Fail, e1.join("; ")),
            _ if !options.active_edge_checks => skipped(
                "active edge checks run only against a deployment whose control the operator proved (A-I9)",
            ),
            "E3" => check_manifest(fetcher, contract).await,
            "E8" => check_status(fetcher, contract, keys, now).await,
            "E11" => check_error_envelope(fetcher, contract).await,
            "E12" => check_negotiation(fetcher, contract).await,
            _ => skipped(
                "needs capability calls, credentials or deliveries; a GET-only kit cannot observe it",
            ),
        };
        let outcome = CheckResult { id, requirement, ..outcome };
        results.push(outcome);
    }
}

fn verdict(problem: Option<String>) -> CheckResult {
    match problem {
        None => result("", "", CheckStatus::Pass, ""),
        Some(problem) => result("", "", CheckStatus::Fail, problem),
    }
}

fn manifest_url(contract: Option<&SiteContract>) -> Option<String> {
    contract.and_then(|c| c.surfaces.as_ref()).and_then(|s| s.manifest.clone())
}

async fn check_manifest(fetcher: &dyn Fetcher, contract: Option<&SiteContract>) -> CheckResult {
    let Some(url) = manifest_url(contract) else {
        return verdict(Some("no manifest URL".to_owned()));
    };
    let Some(value) = fetch_json(fetcher, &url, &HeaderMap::new()).await else {
        return verdict(Some(format!("{url} is unavailable")));
    };
    let mut problems: Vec<String> = check_document(DocumentKind::CapabilityManifest, &value)
        .into_iter()
        .map(|v| v.detail)
        .collect();
    if let Ok(manifest) = serde_json::from_value::<CapabilityManifestV2>(value) {
        if manifest.tier != Some(TrustTier::Public) {
            problems.push("an unauthenticated caller is not listed as public".to_owned());
        }
        problems.extend(
            manifest
                .capabilities
                .iter()
                .filter(|c| c.min_trust > TrustTier::Public)
                .map(|c| format!("`{}` needs {} but is listed to public", c.name, c.min_trust)),
        );
    }
    verdict((!problems.is_empty()).then(|| problems.join("; ")))
}

async fn check_status(
    fetcher: &dyn Fetcher,
    contract: Option<&SiteContract>,
    keys: Option<&KeySet>,
    now: DateTime<Utc>,
) -> CheckResult {
    let Some(url) = contract.and_then(|c| c.status.as_ref()).map(|s| s.url.clone()) else {
        return verdict(Some("no status URL".to_owned()));
    };
    let Some(value) = fetch_json(fetcher, &url, &HeaderMap::new()).await else {
        return verdict(Some(format!("{url} is unavailable")));
    };
    let Ok(status) = serde_json::from_value::<SiteStatus>(value.clone()) else {
        return verdict(Some("the status is not a signed status document".to_owned()));
    };
    let Some(keys) = keys else { return verdict(Some("the key set does not verify".to_owned())) };
    let problem = match delegated_key(keys, &status.proof.kid, KeyScope::Receipt, Dated::Live(now))
    {
        Ok(key) => verify(&value, &status.proof, key).err().map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    };
    verdict(problem)
}

async fn check_error_envelope(
    fetcher: &dyn Fetcher,
    contract: Option<&SiteContract>,
) -> CheckResult {
    let Some(base) = manifest_url(contract)
        .and_then(|url| url.strip_suffix("/awp/v2/manifest").map(str::to_owned))
    else {
        return verdict(Some("no edge origin".to_owned()));
    };
    let url = format!("{base}/awp/v2/calls/conformance-{}", uuid::Uuid::new_v4().simple());
    let problem = match fetcher.get(&url, &HeaderMap::new()).await {
        Ok(fetched) if fetched.status == 404 => {
            match serde_json::from_slice::<ErrorEnvelope>(&fetched.body) {
                Ok(envelope)
                    if envelope.awp.major == 2
                        && envelope.error.code == ErrorCode::ResourceNotFound =>
                {
                    None
                }
                Ok(envelope) => Some(format!("an unknown call answered {}", envelope.error.code)),
                Err(error) => Some(format!("the 404 is not an error envelope: {error}")),
            }
        }
        Ok(fetched) => Some(format!("an unknown call answered {}", fetched.status)),
        Err(error) => Some(error.to_string()),
    };
    verdict(problem)
}

async fn check_negotiation(fetcher: &dyn Fetcher, contract: Option<&SiteContract>) -> CheckResult {
    let Some(url) = manifest_url(contract) else {
        return verdict(Some("no manifest URL".to_owned()));
    };
    let mut headers = HeaderMap::new();
    headers.insert("AWP-Version", HeaderValue::from_static("9.0"));
    let problem = match fetcher.get(&url, &headers).await {
        Ok(fetched) if fetched.status == 406 => {
            match serde_json::from_slice::<ErrorEnvelope>(&fetched.body) {
                Ok(envelope) if envelope.error.code == ErrorCode::VersionNotSupported => {
                    (fetched.header("awp-version").is_none())
                        .then(|| "406 names no supported versions".to_owned())
                }
                _ => Some("406 is not a version_not_supported envelope".to_owned()),
            }
        }
        Ok(fetched) => Some(format!("AWP-Version: 9.0 answered {}", fetched.status)),
        Err(error) => Some(error.to_string()),
    };
    verdict(problem)
}
