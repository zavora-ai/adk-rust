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
//! These routes authenticate the **subject**, never the requester's tier: by a session token
//! the edge issued, in `AWP-Subject`, or a link the edge signed and sent to the subject, whose
//! `subject` query parameter carries the same token. A subject is a pseudonymous
//! `subjectRef`; it holds no personal data.
//!
//! A revocation takes effect at once: pending held calls that reference the receipt are
//! rejected, later calls cannot use it, and `consent.revoked` reaches the requesters whose
//! calls referenced it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_awp::signing::jws::{DocumentSigner, sign};
use adk_awp::v2::response::awp_json;
use adk_awp::v2::store::{
    ConsentReceiptStore, ConsentRecord, HeldCallStore, HeldCallTransition, LeadStore, StoreError,
};
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use awp_types::{
    Channel, ConsentReceipt, ConsentRevoked, ConsentScope, Digest, ErrorBody, ErrorCode,
    HeldCallState, LegalBasis, Proof,
};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use super::events::{ConsentReferences, EdgeEvents};

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
    /// The key subject sessions and signed links are authenticated with.
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

/// Issues and checks subject session tokens: `v1.{subject}.{expiry}.{HMAC-SHA256}`.
#[derive(Clone)]
pub(crate) struct SubjectTokens {
    key: Arc<[u8]>,
    ttl: chrono::Duration,
}

impl SubjectTokens {
    fn mac(&self, payload: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC takes any key length");
        mac.update(payload.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    pub(crate) fn issue(&self, subject: &str) -> String {
        let expiry = (Utc::now() + self.ttl).timestamp();
        let payload = format!("v1.{}.{expiry}", URL_SAFE_NO_PAD.encode(subject));
        let mac = self.mac(&payload);
        format!("{payload}.{mac}")
    }

    pub(crate) fn subject_of(&self, token: &str) -> Option<String> {
        let (payload, mac) = token.rsplit_once('.')?;
        let expected = self.mac(payload);
        let equal = expected.len() == mac.len()
            && expected.bytes().zip(mac.bytes()).fold(0_u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        if !equal {
            return None;
        }
        let mut parts = payload.split('.');
        let (Some("v1"), Some(subject), Some(expiry), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if expiry.parse::<i64>().ok()? < Utc::now().timestamp() {
            return None;
        }
        String::from_utf8(URL_SAFE_NO_PAD.decode(subject).ok()?).ok()
    }
}

#[derive(Clone)]
pub(crate) struct ConsentState {
    pub(crate) config: Arc<ConsentConfig>,
    pub(crate) tokens: SubjectTokens,
    pub(crate) store: Arc<dyn ConsentReceiptStore>,
    pub(crate) held_calls: Arc<dyn HeldCallStore>,
    pub(crate) references: ConsentReferences,
    pub(crate) events: EdgeEvents,
    pub(crate) signer: Arc<dyn DocumentSigner>,
    pub(crate) data: Arc<dyn SubjectData>,
    pub(crate) audit: Option<AuditTrail>,
}

impl SubjectTokens {
    pub(crate) fn for_config(config: &ConsentConfig) -> Self {
        Self { key: Arc::clone(&config.subject_key), ttl: config.session_ttl }
    }
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

    /// The authenticated subject: the `AWP-Subject` session, or the signed link's `subject`.
    fn subject(&self, headers: &HeaderMap, query: &BTreeMap<String, String>) -> Option<String> {
        let token = headers
            .get(SUBJECT_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| query.get("subject").cloned())?;
        self.tokens.subject_of(token.trim())
    }

    async fn audit(&self, action: &str, subject: &str, fields: Value) {
        let Some(trail) = &self.audit else { return };
        let mut draft = AuditRecordDraft::new(AuditRecordKind::Other("awp".to_owned()), action)
            .actor(AuditActor::subject(SubjectRef::service(format!("subject:{subject}"))));
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
    let subject = state
        .subject(&headers, &query)
        .unwrap_or_else(|| format!("subj_{}", Uuid::new_v4().simple()));
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
    if let Ok(token) = HeaderValue::from_str(&state.tokens.issue(&subject)) {
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
    awp_json(StatusCode::OK, &json!({ "subjectRef": subject, "receipts": receipts, "data": data }))
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
    state.audit("awp.consent.erased", &subject, json!({ "receipts": erased, "data": data })).await;
    awp_json(StatusCode::ACCEPTED, &json!({ "erased": { "receipts": erased, "data": data } }))
}
