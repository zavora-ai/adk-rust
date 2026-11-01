//! Consent: signed receipts, revocation, export and erasure (spec 3.5 §7.5, A-I8; AWP 2.0
//! §11).
//!
//! | Route | Does |
//! |---|---|
//! | `POST /awp/v2/consent` | Grants `{scope, policyVersion, jurisdiction, channel}`; answers `201` with the receipt signed by the edge's `receipt` key and the subject's session in `AWP-Subject` |
//! | `POST /awp/v2/consent/revoke` | Revokes `{"receipt": id}`, or every receipt of the subject for `{"scope": …}`; answers `202` |
//! | `GET /awp/v2/consent/export` | Every receipt and every item of data the site holds about the subject |
//! | `POST /awp/v2/consent/erase` | Revokes every receipt, erases the subject's data and receipts; answers `202` |
//!
//! These routes authenticate the **subject**, never the requester's tier: by a visitor token
//! the edge issued ([`VisitorTokens`]), in `AWP-Subject`, the `__Host-adk_visitor` cookie, or a
//! link the edge signed and sent to the subject, whose `subject` query parameter carries the
//! same token. A subject is a random [`VisitorId`]; it holds no personal data, and it is the
//! same id the visitor converses as.
//!
//! Export and erasure also reach every store of the edge's [`SubjectRights`]: the public
//! partition's sessions, the beacon's visitor sets, and each store added with
//! [`AwpEdgeBuilder::subject_store`](super::AwpEdgeBuilder::subject_store). The receipts are
//! the framework's consent records ([`ReceiptConsentStore`]), so there is one source of
//! consent.
//!
//! A revocation takes effect at once: pending held calls that reference the receipt are
//! rejected, later calls cannot use it, and `consent.revoked` reaches the requesters whose
//! calls referenced it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_auth::VisitorTokens;
use adk_awp::signing::jws::{DocumentSigner, sign};
use adk_awp::v2::response::awp_json;
use adk_awp::v2::store::{
    ConsentReceiptStore, ConsentRecord, HeldCallStore, HeldCallTransition, LeadStore, StoreError,
};
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentEvidence, ConsentPurpose, ConsentStore, SubjectRights, VisitorId, unsupported_error,
};
use async_trait::async_trait;
use awp_types::{
    Channel, ConsentReceipt, ConsentRevoked, ConsentScope, Digest, ErrorBody, ErrorCode,
    HeldCallState, LegalBasis, Proof,
};
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::events::{ConsentReferences, EdgeEvents};
use super::tiers::EdgeCaller;

/// The consent routes' base path.
pub const CONSENT_PATH: &str = "/awp/v2/consent";
/// The request and response field carrying the subject's session token.
pub const SUBJECT_HEADER: &str = "awp-subject";

/// A policy version receipts may name (spec 3.5 §7.5; AWP 2.0 §11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentPolicy {
    /// The digest of the policy's text.
    pub digest: Digest,
    /// How long a receipt under it lasts; `None` until revoked.
    pub ttl: Option<chrono::Duration>,
}

/// What the edge grants consent for (spec 3.5 §7.5).
#[derive(Clone)]
#[non_exhaustive]
pub struct ConsentConfig {
    /// The scopes the site asks for, from its public terms.
    pub scopes: Vec<ConsentScope>,
    /// Policies by version.
    pub policies: BTreeMap<String, ConsentPolicy>,
    /// The key visitor tokens and signed links are authenticated with.
    pub subject_key: Arc<[u8]>,
    /// How long a subject session lasts.
    pub session_ttl: chrono::Duration,
}

impl std::fmt::Debug for ConsentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentConfig")
            .field("scopes", &self.scopes)
            .field("policies", &self.policies.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ConsentConfig {
    /// Consent for `scopes` under the policy versions in `policies`, sessions keyed by
    /// `subject_key` (at least 32 bytes, kept by the site so sessions survive restarts) and
    /// lasting 30 days.
    pub fn new(
        scopes: Vec<ConsentScope>,
        policies: BTreeMap<String, ConsentPolicy>,
        subject_key: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            scopes,
            policies,
            subject_key: subject_key.into(),
            session_ttl: chrono::Duration::days(30),
        }
    }
}

impl Default for ConsentConfig {
    /// No scopes or policies, and a random subject key, so sessions last the process.
    fn default() -> Self {
        let mut key = Vec::with_capacity(32);
        key.extend_from_slice(Uuid::new_v4().as_bytes());
        key.extend_from_slice(Uuid::new_v4().as_bytes());
        Self::new(Vec::new(), BTreeMap::new(), key)
    }
}

/// The data a site holds about a subject beyond its receipts, for export and erasure
/// (spec 3.5 §7.5; AWP 2.0 §11.3).
///
/// `adk-website` implements it over its leads and contact records; [`LeadsData`] exports the
/// leads that reference the subject's receipts.
#[async_trait]
pub trait SubjectData: Send + Sync {
    /// Every item of data held about `subject`, whose receipts are `receipts`.
    ///
    /// # Errors
    ///
    /// Returns an error when the data cannot be read.
    async fn export(&self, subject: &str, receipts: &[ConsentRecord]) -> adk_core::Result<Value>;

    /// Erases every item of data held about `subject`; returns how many.
    ///
    /// # Errors
    ///
    /// Returns an error when the data cannot be erased.
    async fn erase(&self, subject: &str, receipts: &[ConsentRecord]) -> adk_core::Result<usize>;
}

/// The leads that reference a subject's receipts: exported, and deleted on erasure (spec 3.5
/// §7.5; AWP 2.0 §11.3).
pub struct LeadsData {
    leads: Arc<dyn LeadStore>,
}

impl LeadsData {
    /// The data in `leads`.
    pub fn new(leads: Arc<dyn LeadStore>) -> Self {
        Self { leads }
    }

    /// The leads that reference one of `receipts`.
    async fn of(
        &self,
        receipts: &[ConsentRecord],
    ) -> adk_core::Result<Vec<adk_awp::v2::store::Lead>> {
        let ids: BTreeSet<Uuid> = receipts.iter().map(|record| record.receipt.id).collect();
        Ok(self
            .leads
            .since(DateTime::<Utc>::MIN_UTC)
            .await
            .map_err(|error| adk_core::AdkError::session(error.to_string()))?
            .into_iter()
            .filter(|lead| lead.consent.iter().any(|reference| ids.contains(&reference.0)))
            .collect())
    }
}

#[async_trait]
impl SubjectData for LeadsData {
    async fn export(&self, _subject: &str, receipts: &[ConsentRecord]) -> adk_core::Result<Value> {
        Ok(json!({ "leads": self.of(receipts).await? }))
    }

    async fn erase(&self, _subject: &str, receipts: &[ConsentRecord]) -> adk_core::Result<usize> {
        let mut erased = 0;
        for lead in self.of(receipts).await? {
            if self
                .leads
                .delete(&lead.id)
                .await
                .map_err(|error| adk_core::AdkError::session(error.to_string()))?
            {
                erased += 1;
            }
        }
        Ok(erased)
    }
}

/// The visitor token a request presents: the `AWP-Subject` header, a signed link's `subject`
/// query parameter, or the `__Host-adk_visitor` cookie, in that order.
pub(crate) fn presented_token(headers: &HeaderMap, link: Option<&str>) -> Option<String> {
    headers
        .get(SUBJECT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| link.map(str::to_owned))
        .or_else(|| {
            headers
                .get(axum::http::header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(VisitorTokens::from_cookie_header)
                .map(str::to_owned)
        })
}

/// The edge's consent receipts as the framework's [`ConsentStore`], so the consent gates of
/// the public partition read the receipts the consent routes issue: one source of consent.
///
/// A receipt becomes a record of a visitor subject, its scope the purpose of the same name,
/// and the signed receipt its evidence (`awp.receipt`). Consent is granted only through
/// `POST /awp/v2/consent`, which signs the receipt, so [`grant`](ConsentStore::grant) is
/// unsupported.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_awp::v2::store::InMemoryConsentReceiptStore;
/// use adk_core::subject::{ConsentStore, VisitorId};
/// use adk_server::awp::ReceiptConsentStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let consent = ReceiptConsentStore::new(Arc::new(InMemoryConsentReceiptStore::new()));
/// assert!(consent.for_subject(&VisitorId::generate().subject()).await?.is_empty());
/// # Ok(())
/// # }
/// ```
pub struct ReceiptConsentStore {
    receipts: Arc<dyn ConsentReceiptStore>,
}

impl ReceiptConsentStore {
    /// The receipts in `receipts`.
    pub fn new(receipts: Arc<dyn ConsentReceiptStore>) -> Self {
        Self { receipts }
    }
}

impl std::fmt::Debug for ReceiptConsentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiptConsentStore").finish_non_exhaustive()
    }
}

fn receipt_failed(error: StoreError) -> adk_core::AdkError {
    adk_core::AdkError::session(format!("awp consent receipts unavailable: {error}"))
}

fn consent_record(record: ConsentRecord) -> adk_core::subject::ConsentRecord {
    let receipt = record.receipt;
    let document = serde_json::to_value(&receipt).ok();
    let mut evidence = ConsentEvidence::new("awp.receipt", receipt.id.to_string());
    evidence.document = document;
    adk_core::subject::ConsentRecord {
        id: receipt.id.to_string(),
        subject: SubjectRef::visitor(receipt.subject_ref),
        purpose: ConsentPurpose::from(receipt.scope.as_str()),
        policy_version: receipt.policy_version,
        granted_at: receipt.granted_at,
        expires_at: receipt.expires_at,
        revoked_at: record.revoked_at,
        evidence: Some(evidence),
    }
}

#[async_trait]
impl ConsentStore for ReceiptConsentStore {
    async fn grant(&self, _record: adk_core::subject::ConsentRecord) -> adk_core::Result<()> {
        Err(unsupported_error("awp receipts", "grant consent outside POST /awp/v2/consent"))
    }

    async fn get(&self, id: &str) -> adk_core::Result<Option<adk_core::subject::ConsentRecord>> {
        let Ok(id) = id.parse::<Uuid>() else { return Ok(None) };
        Ok(self.receipts.get(id).await.map_err(receipt_failed)?.map(consent_record))
    }

    async fn for_subject(
        &self,
        subject: &SubjectRef,
    ) -> adk_core::Result<Vec<adk_core::subject::ConsentRecord>> {
        let mut records: Vec<_> = self
            .receipts
            .for_subject(&subject.id)
            .await
            .map_err(receipt_failed)?
            .into_iter()
            .map(consent_record)
            .filter(|record| record.subject == *subject)
            .collect();
        records.sort_by(|a, b| a.granted_at.cmp(&b.granted_at).then_with(|| a.id.cmp(&b.id)));
        Ok(records)
    }

    async fn revoke(
        &self,
        id: &str,
        at: DateTime<Utc>,
    ) -> adk_core::Result<Option<adk_core::subject::ConsentRecord>> {
        let Ok(id) = id.parse::<Uuid>() else { return Ok(None) };
        Ok(self.receipts.revoke(id, at).await.map_err(receipt_failed)?.map(consent_record))
    }

    async fn erase(&self, subject: &SubjectRef) -> adk_core::Result<u64> {
        if subject.kind != adk_core::SubjectKind::Visitor {
            return Ok(0);
        }
        let erased = self.receipts.erase(&subject.id).await.map_err(receipt_failed)?;
        Ok(erased as u64)
    }
}

#[derive(Clone)]
pub(crate) struct ConsentState {
    pub(crate) config: Arc<ConsentConfig>,
    pub(crate) tokens: VisitorTokens,
    pub(crate) rights: SubjectRights,
    pub(crate) store: Arc<dyn ConsentReceiptStore>,
    pub(crate) held_calls: Arc<dyn HeldCallStore>,
    pub(crate) references: ConsentReferences,
    pub(crate) events: EdgeEvents,
    pub(crate) signer: Arc<dyn DocumentSigner>,
    pub(crate) data: Arc<dyn SubjectData>,
    pub(crate) audit: Option<AuditTrail>,
}

impl ConsentState {
    pub(crate) fn router(&self) -> Router {
        Router::new()
            .route(CONSENT_PATH, post(grant))
            .route(&format!("{CONSENT_PATH}/revoke"), post(revoke))
            .route(&format!("{CONSENT_PATH}/export"), get(export))
            .route(&format!("{CONSENT_PATH}/erase"), post(erase))
            .with_state(self.clone())
    }

    /// The authenticated subject: the visitor a presented token names.
    fn subject(&self, headers: &HeaderMap, query: &BTreeMap<String, String>) -> Option<String> {
        presented_token(headers, query.get("subject").map(String::as_str))
            .and_then(|token| self.tokens.verify(&token))
            .map(String::from)
    }

    async fn audit(&self, action: &str, subject: &str, fields: Value) {
        let Some(trail) = &self.audit else { return };
        let mut draft = AuditRecordDraft::new(AuditRecordKind::Other("awp".to_owned()), action)
            .actor(AuditActor::subject(SubjectRef::visitor(subject)));
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(audit.action = action, error = %error, "awp consent audit record not written");
        }
    }

    /// The effects of revoking `records` (A-I8): pending calls that rely on them are
    /// rejected, and `consent.revoked` reaches the requesters whose calls referenced them.
    pub(crate) async fn revoked(&self, subject: &str, records: &[ConsentRecord]) {
        let ids: BTreeSet<Uuid> = records.iter().map(|record| record.receipt.id).collect();
        let mut audiences: BTreeMap<Uuid, BTreeSet<awp_types::RequesterId>> = BTreeMap::new();
        for id in &ids {
            audiences.insert(*id, self.references.of(*id).await);
        }
        match self.held_calls.pending().await {
            Ok(pending) => {
                for held in pending {
                    let Some(receipt) = held.consent.iter().find(|id| ids.contains(id)) else {
                        continue;
                    };
                    if let Some(requester) = &held.requester {
                        audiences.entry(*receipt).or_default().insert(requester.clone());
                    }
                    let transition = HeldCallTransition {
                        to: HeldCallState::Rejected,
                        result: None,
                        error: Some(ErrorBody::new(
                            ErrorCode::ConsentRequired,
                            "the consent this call relied on was revoked",
                        )),
                    };
                    if let Err(error) = self.held_calls.transition(&held.call.id, transition).await
                    {
                        tracing::warn!(error = %error, "awp held call not stopped after a revocation");
                    }
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "awp pending calls not read after a revocation")
            }
        }
        for record in records {
            let revoked_at = record.revoked_at.unwrap_or_else(Utc::now);
            let audience = audiences.remove(&record.receipt.id).unwrap_or_default();
            if !audience.is_empty() {
                let event = ConsentRevoked {
                    receipt: record.receipt.id,
                    scope: record.receipt.scope.clone(),
                    revoked_at,
                };
                if let Err(error) = self.events.consent_revoked(event, audience).await {
                    tracing::warn!(error = %error, "awp consent.revoked not published");
                }
            }
            self.audit(
                "awp.consent.revoked",
                subject,
                json!({ "receipt": record.receipt.id, "scope": record.receipt.scope }),
            )
            .await;
        }
    }
}

fn refuse(code: ErrorCode, message: &str) -> Response {
    super::envelope(ErrorBody::new(code, message), None)
}

fn store_failed(error: &StoreError) -> Response {
    tracing::error!(error = %error, "awp consent store failed");
    refuse(ErrorCode::InternalError, "consent could not be recorded")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Grant {
    scope: ConsentScope,
    policy_version: String,
    jurisdiction: String,
    #[serde(default)]
    channel: Option<Channel>,
}

async fn grant(
    State(state): State<ConsentState>,
    Query(query): Query<BTreeMap<String, String>>,
    caller: Option<Extension<EdgeCaller>>,
    headers: HeaderMap,
    Json(body): Json<Grant>,
) -> Response {
    if !state.config.scopes.contains(&body.scope) {
        return refuse(
            ErrorCode::InvalidRequest,
            "the site does not ask for consent to this scope",
        );
    }
    let Some(policy) = state.config.policies.get(&body.policy_version) else {
        return refuse(ErrorCode::InvalidRequest, "no such policy version");
    };
    let jurisdiction = body.jurisdiction.trim().to_ascii_uppercase();
    if jurisdiction.len() != 2 || !jurisdiction.bytes().all(|b| b.is_ascii_uppercase()) {
        return refuse(ErrorCode::InvalidRequest, "jurisdiction is an ISO 3166-1 alpha-2 code");
    }
    // The visitor the caller converses as consents; a caller with none gets a new id.
    let subject = state
        .subject(&headers, &query)
        .or_else(|| caller.and_then(|Extension(caller)| caller.visitor).map(String::from))
        .unwrap_or_else(|| VisitorId::generate().to_string());
    if let Err(error) = state.rights.ensure_writable(&SubjectRef::visitor(subject.as_str())).await {
        return refuse(ErrorCode::Forbidden, &error.message);
    }
    let now = Utc::now();
    let mut receipt = ConsentReceipt {
        id: Uuid::new_v4(),
        subject_ref: subject.clone(),
        scope: body.scope,
        policy_version: body.policy_version,
        policy_digest: policy.digest.clone(),
        legal_basis: LegalBasis::ExplicitConsent,
        jurisdiction,
        granted_at: now,
        expires_at: policy.ttl.map(|ttl| now + ttl),
        channel: body.channel.unwrap_or(Channel::Web),
        proof: Proof::eddsa(state.signer.kid(), ""),
    };
    receipt.proof = match sign(&receipt, state.signer.as_ref()).await {
        Ok(proof) => proof,
        Err(error) => {
            tracing::error!(error = %error, "awp consent receipt not signed");
            return refuse(ErrorCode::InternalError, "consent could not be recorded");
        }
    };
    if let Err(error) = state.store.insert(receipt.clone()).await {
        return store_failed(&error);
    }
    state
        .audit(
            "awp.consent.granted",
            &subject,
            json!({ "receipt": receipt.id, "scope": receipt.scope, "policyVersion": receipt.policy_version }),
        )
        .await;
    let mut response = awp_json(StatusCode::CREATED, &receipt);
    if let Ok(visitor) = subject.parse::<VisitorId>()
        && let Ok(token) = HeaderValue::from_str(&state.tokens.issue(&visitor))
    {
        response.headers_mut().insert(SUBJECT_HEADER, token);
    }
    response
}

#[derive(Deserialize)]
struct Revoke {
    #[serde(default)]
    receipt: Option<Uuid>,
    #[serde(default)]
    scope: Option<ConsentScope>,
}

async fn revoke(
    State(state): State<ConsentState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(body): Json<Revoke>,
) -> Response {
    let Some(subject) = state.subject(&headers, &query) else {
        return refuse(
            ErrorCode::Unauthorized,
            "consent routes need the subject's session or signed link",
        );
    };
    let now = Utc::now();
    let revoked = match (body.receipt, body.scope) {
        (Some(id), _) => match state.store.get(id).await {
            Ok(Some(record)) if record.receipt.subject_ref == subject => {
                match state.store.revoke(id, now).await {
                    Ok(record) => record.into_iter().collect::<Vec<_>>(),
                    Err(error) => return store_failed(&error),
                }
            }
            Ok(_) => return refuse(ErrorCode::ResourceNotFound, "no such receipt"),
            Err(error) => return store_failed(&error),
        },
        (None, Some(scope)) => match state.store.revoke_scope(&subject, &scope, now).await {
            Ok(records) => records,
            Err(error) => return store_failed(&error),
        },
        (None, None) => return refuse(ErrorCode::InvalidRequest, "name a receipt or a scope"),
    };
    state.revoked(&subject, &revoked).await;
    let ids: Vec<Uuid> = revoked.iter().map(|record| record.receipt.id).collect();
    awp_json(StatusCode::ACCEPTED, &json!({ "revoked": ids }))
}

async fn export(
    State(state): State<ConsentState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(subject) = state.subject(&headers, &query) else {
        return refuse(
            ErrorCode::Unauthorized,
            "consent routes need the subject's session or signed link",
        );
    };
    let receipts = match state.store.for_subject(&subject).await {
        Ok(receipts) => receipts,
        Err(error) => return store_failed(&error),
    };
    let data = match state.data.export(&subject, &receipts).await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(error = %error, "awp subject data not exported");
            return refuse(ErrorCode::InternalError, "the data could not be exported");
        }
    };
    let stores = match state.rights.export(&SubjectRef::visitor(subject.as_str())).await {
        Ok(export) => export.stores,
        Err(error) => {
            tracing::error!(error = %error, "awp subject stores not exported");
            return refuse(ErrorCode::InternalError, "the data could not be exported");
        }
    };
    awp_json(
        StatusCode::OK,
        &json!({ "subjectRef": subject, "receipts": receipts, "data": data, "stores": stores }),
    )
}

async fn erase(
    State(state): State<ConsentState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(subject) = state.subject(&headers, &query) else {
        return refuse(
            ErrorCode::Unauthorized,
            "consent routes need the subject's session or signed link",
        );
    };
    let now = Utc::now();
    let receipts = match state.store.for_subject(&subject).await {
        Ok(receipts) => receipts,
        Err(error) => return store_failed(&error),
    };
    let mut revoked = Vec::new();
    for record in receipts.iter().filter(|record| record.revoked_at.is_none()) {
        match state.store.revoke(record.receipt.id, now).await {
            Ok(Some(record)) => revoked.push(record),
            Ok(None) => {}
            Err(error) => return store_failed(&error),
        }
    }
    state.revoked(&subject, &revoked).await;
    let data = match state.data.erase(&subject, &receipts).await {
        Ok(count) => count,
        Err(error) => {
            tracing::error!(error = %error, "awp subject data not erased");
            return refuse(ErrorCode::InternalError, "the data could not be erased");
        }
    };
    let erased = match state.store.erase(&subject).await {
        Ok(count) => count,
        Err(error) => return store_failed(&error),
    };
    // Every other store the edge holds the visitor's data in, and the registry, last.
    let report = match state.rights.erase(&SubjectRef::visitor(subject.as_str())).await {
        Ok(report) => report,
        Err(error) => {
            tracing::error!(error = %error, "awp subject erasure did not run");
            return refuse(ErrorCode::InternalError, "the data could not be erased");
        }
    };
    state.audit("awp.consent.erased", &subject, json!({ "receipts": erased, "data": data })).await;
    awp_json(
        StatusCode::ACCEPTED,
        &json!({
            "erased": { "receipts": erased, "data": data },
            "status": report.status,
            "stores": report.stores,
        }),
    )
}
