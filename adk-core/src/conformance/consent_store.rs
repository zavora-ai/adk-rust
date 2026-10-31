//! Conformance cases for [`ConsentStore`] backends.
//!
//! | Case | Contract | Backend primitive |
//! |------|----------|-------------------|
//! | [`grant_get_round_trip`] | A record, its evidence included, reads back exactly as granted, through a second handle; a taken id fails with `consent.conflict` | Insert-if-absent |
//! | [`records_are_per_subject`] | `for_subject` returns one subject's records, oldest grant first; kinds are separate subjects | Index by kind and id |
//! | [`revocation_keeps_the_first_time`] | `revoke` stamps the first revocation and keeps it; `None` for an unknown id | Conditional update |
//! | [`revoke_purpose_revokes_live_records`] | `revoke_purpose` revokes the live records of one purpose, or of every purpose, and no others | Filtered update |
//! | [`live_respects_expiry_and_revocation`] | `live` returns the newest record live at the instant asked, or none | Time comparison |
//! | [`erase_removes_one_subject`] | `erase` removes every record of one subject and no other; a repeat erases 0 | Delete by subject |

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Duration;
use serde_json::json;

use super::{base_time, unique_unicode};
use crate::principal::SubjectRef;
use crate::subject::{
    CONSENT_CONFLICT_CODE, ConsentEvidence, ConsentPurpose, ConsentRecord, ConsentStore,
};

/// Opens a [`ConsentStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::async_trait;
/// use adk_core::conformance::ConsentStoreHarness;
/// use adk_core::subject::{ConsentStore, InMemoryConsentStore};
///
/// #[derive(Default)]
/// struct InMemory(InMemoryConsentStore);
///
/// #[async_trait]
/// impl ConsentStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ConsentStore> {
///         Arc::new(self.0.clone())
///     }
/// }
/// ```
#[async_trait]
pub trait ConsentStoreHarness: Send + Sync {
    /// Opens the store. Every call returns a new handle over the same stored data, as a
    /// restarted process would see it. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ConsentStore>;
}

fn visitor() -> SubjectRef {
    SubjectRef::visitor(unique_unicode("visitor"))
}

fn record(subject: &SubjectRef, purpose: ConsentPurpose, offset_secs: i64) -> ConsentRecord {
    ConsentRecord::new(subject.clone(), purpose, "2026-10")
        .with_id(unique_unicode("consent"))
        .granted_at(base_time() + Duration::seconds(offset_secs))
}

/// A record reads back exactly as granted, its evidence and expiry included, through a
/// second handle; a grant with a taken id fails with `consent.conflict` and changes nothing.
pub async fn grant_get_round_trip<H: ConsentStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let subject = visitor();
    let granted = record(&subject, ConsentPurpose::Custom("contact: ü/1".into()), 0)
        .with_expires_at(base_time() + Duration::days(365))
        .with_evidence(
            ConsentEvidence::new("awp.receipt", unique_unicode("receipt"))
                .with_document(json!({ "proof": { "alg": "EdDSA" }, "scope": "contact" })),
        );
    store.grant(granted.clone()).await.expect("grant");
    assert_eq!(harness.open().await.get(&granted.id).await.expect("get"), Some(granted.clone()));

    let duplicate = record(&visitor(), ConsentPurpose::Analytics, 5).with_id(granted.id.clone());
    let error = store.grant(duplicate).await.expect_err("a taken id is refused");
    assert_eq!(error.code, CONSENT_CONFLICT_CODE, "{error}");
    assert_eq!(store.get(&granted.id).await.expect("get"), Some(granted));
    assert_eq!(store.get(&unique_unicode("missing")).await.expect("get"), None);
}

/// `for_subject` returns one subject's records, oldest grant first; a human and a visitor
/// with one id are separate subjects.
pub async fn records_are_per_subject<H: ConsentStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let subject = visitor();
    let same_id = SubjectRef::human(subject.id.clone());
    let later = record(&subject, ConsentPurpose::Analytics, 60);
    let earlier = record(&subject, ConsentPurpose::PersistentProfile, 0);
    let other = record(&same_id, ConsentPurpose::Analytics, 30);
    for each in [&later, &earlier, &other] {
        store.grant(each.clone()).await.expect("grant");
    }
    assert_eq!(store.for_subject(&subject).await.expect("list"), vec![earlier, later]);
    assert_eq!(store.for_subject(&same_id).await.expect("list"), vec![other]);
    assert_eq!(store.for_subject(&visitor()).await.expect("list"), Vec::new());
}

/// `revoke` stamps the first revocation time and keeps it on a repeat, and returns `None`
/// for an unknown id.
pub async fn revocation_keeps_the_first_time<H: ConsentStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let granted = record(&visitor(), ConsentPurpose::Newsletter, 0);
    store.grant(granted.clone()).await.expect("grant");

    let first = base_time() + Duration::minutes(5);
    let revoked = store.revoke(&granted.id, first).await.expect("revoke").expect("known id");
    let expected = ConsentRecord { revoked_at: Some(first), ..granted.clone() };
    assert_eq!(revoked, expected);
    let again = store
        .revoke(&granted.id, first + Duration::minutes(5))
        .await
        .expect("revoke again")
        .expect("known id");
    assert_eq!(again, expected, "the first revocation time stays");
    assert_eq!(harness.open().await.get(&granted.id).await.expect("get"), Some(expected));
    assert_eq!(store.revoke(&unique_unicode("missing"), first).await.expect("revoke"), None);
}

/// `revoke_purpose` revokes the records of one purpose live at the instant, or of every
/// purpose, and leaves expired, revoked, and other subjects' records alone.
pub async fn revoke_purpose_revokes_live_records<H: ConsentStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let subject = visitor();
    let neighbour = visitor();
    let analytics = record(&subject, ConsentPurpose::Analytics, 0);
    let expired = record(&subject, ConsentPurpose::Analytics, 1)
        .with_expires_at(base_time() + Duration::seconds(30));
    let profile = record(&subject, ConsentPurpose::PersistentProfile, 2);
    let theirs = record(&neighbour, ConsentPurpose::Analytics, 3);
    for each in [&analytics, &expired, &profile, &theirs] {
        store.grant(each.clone()).await.expect("grant");
    }

    let at = base_time() + Duration::minutes(1);
    let revoked = store
        .revoke_purpose(&subject, Some(&ConsentPurpose::Analytics), at)
        .await
        .expect("revoke purpose");
    assert_eq!(revoked, vec![ConsentRecord { revoked_at: Some(at), ..analytics }]);
    assert_eq!(store.get(&expired.id).await.expect("get"), Some(expired), "expired stays as is");
    assert_eq!(store.get(&theirs.id).await.expect("get"), Some(theirs), "others untouched");

    let all = store.revoke_purpose(&subject, None, at).await.expect("revoke every purpose");
    assert_eq!(all, vec![ConsentRecord { revoked_at: Some(at), ..profile }]);
    assert_eq!(store.revoke_purpose(&subject, None, at).await.expect("repeat"), Vec::new());
}

/// `live` returns the newest record live at the instant asked: not before its grant, not
/// at or after its expiry or revocation.
pub async fn live_respects_expiry_and_revocation<H: ConsentStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let subject = visitor();
    let purpose = ConsentPurpose::SessionContinuity;
    let first =
        record(&subject, purpose.clone(), 0).with_expires_at(base_time() + Duration::hours(1));
    let second = record(&subject, purpose.clone(), 600);
    store.grant(first.clone()).await.expect("grant");
    store.grant(second.clone()).await.expect("grant");

    let live = |offset: Duration| {
        let store = Arc::clone(&store);
        let (subject, purpose) = (subject.clone(), purpose.clone());
        async move { store.live(&subject, &purpose, base_time() + offset).await.expect("live") }
    };
    assert_eq!(live(Duration::seconds(-1)).await, None, "before any grant");
    assert_eq!(live(Duration::seconds(0)).await, Some(first.clone()));
    assert_eq!(live(Duration::minutes(30)).await, Some(second.clone()), "the newest live record");
    assert_eq!(
        store.live(&subject, &ConsentPurpose::Analytics, base_time()).await.expect("live"),
        None,
        "another purpose"
    );

    let revoked_at = base_time() + Duration::minutes(40);
    store.revoke(&second.id, revoked_at).await.expect("revoke");
    assert_eq!(live(Duration::minutes(45)).await, Some(first), "the older record is still live");
    assert_eq!(live(Duration::hours(2)).await, None, "expired and revoked");
}

/// `erase` removes every record of one subject, returns how many, and leaves other
/// subjects' records; a repeat erases 0.
pub async fn erase_removes_one_subject<H: ConsentStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let subject = visitor();
    let neighbour = visitor();
    let mine = [
        record(&subject, ConsentPurpose::Analytics, 0),
        record(&subject, ConsentPurpose::Newsletter, 1),
    ];
    let theirs = record(&neighbour, ConsentPurpose::Analytics, 2);
    for each in mine.iter().chain([&theirs]) {
        store.grant(each.clone()).await.expect("grant");
    }
    assert_eq!(store.erase(&subject).await.expect("erase"), 2);
    let reopened = harness.open().await;
    assert_eq!(reopened.for_subject(&subject).await.expect("list"), Vec::new());
    assert_eq!(reopened.get(&mine[0].id).await.expect("get"), None);
    assert_eq!(reopened.for_subject(&neighbour).await.expect("list"), vec![theirs]);
    assert_eq!(reopened.erase(&subject).await.expect("repeat"), 0);
}
