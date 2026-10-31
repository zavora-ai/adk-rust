//! Conformance cases for [`SubjectDataStore`] adapters.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`export_returns_only_the_subject`] | The export of a seeded subject holds data; that of a subject never seeded is empty |
//! | [`erase_removes_only_the_subject`] | After an erasure no read path returns the subject's data, its export is empty, and another subject's data is intact |
//! | [`erase_is_idempotent`] | A repeat erasure succeeds and deletes nothing |
//! | [`erase_covers_linked_ids`] | Erasing a subject erases the data of the visitor ids linked to it |
//! | [`erase_stays_in_its_applications`] | Erasing a subject in one application leaves its data in another |
//!
//! The harness writes through the store's own API and counts what its read paths return,
//! so the suite checks the store, not the adapter's view of it. An adapter whose
//! [`StoreSupport`](crate::subject::StoreSupport) is `Eventual` runs the suite against a
//! backend where deletion is immediate, or marks it `#[ignore]` for a live service.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::unique_unicode;
use crate::principal::SubjectRef;
use crate::subject::{SubjectDataStore, SubjectScope};

/// Opens a [`SubjectDataStore`] adapter, writes data through the store it adapts, and
/// counts what that store's read paths return.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_core::async_trait;
/// use adk_core::conformance::SubjectDataStoreHarness;
/// use adk_core::subject::SubjectDataStore;
///
/// struct Sessions { service: Arc<MySessionService> }
///
/// #[async_trait]
/// impl SubjectDataStoreHarness for Sessions {
///     async fn open(&self) -> Arc<dyn SubjectDataStore> {
///         Arc::new(SessionSubjectData::new(self.service.clone()))
///     }
///
///     async fn seed(&self, app_name: &str, user_id: &str) {
///         // create a session with an event and `user:` state for the user
///     }
///
///     async fn visible(&self, app_name: &str, user_id: &str) -> usize {
///         // sessions listed + events and `user:` keys read back for the user
///     }
/// }
/// ```
#[async_trait]
pub trait SubjectDataStoreHarness: Send + Sync {
    /// Opens the adapter under test.
    async fn open(&self) -> Arc<dyn SubjectDataStore>;

    /// Writes data for `user_id` in `app_name` through the adapted store's own API.
    async fn seed(&self, app_name: &str, user_id: &str);

    /// Counts the items every read path of the adapted store returns for `user_id` in
    /// `app_name`: 0 when the store holds nothing for it.
    async fn visible(&self, app_name: &str, user_id: &str) -> usize;
}

/// Whether `value` holds no data: `null`, or arrays and objects holding no data.
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().all(is_empty),
        Value::Object(fields) => fields.values().all(is_empty),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn app() -> String {
    unique_unicode("app")
}

fn person() -> SubjectRef {
    SubjectRef::human(unique_unicode("person"))
}

/// The export of a seeded subject holds data, and that of a subject never seeded is
/// empty.
pub async fn export_returns_only_the_subject<H: SubjectDataStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let (app, alice, bob) = (app(), person(), person());
    harness.seed(&app, &alice.id).await;

    let exported = store.export(&SubjectScope::new([&app], alice)).await.expect("export");
    assert!(!is_empty(&exported), "the export holds the subject's data: {exported}");
    let nobody = store.export(&SubjectScope::new([&app], bob)).await.expect("export");
    assert!(is_empty(&nobody), "nothing is exported for a subject never seeded: {nobody}");
}

/// After an erasure no read path returns the subject's data and its export is empty;
/// another subject's data in the same application is intact.
pub async fn erase_removes_only_the_subject<H: SubjectDataStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let (app, alice, bob) = (app(), person(), person());
    harness.seed(&app, &alice.id).await;
    harness.seed(&app, &bob.id).await;
    let bob_before = harness.visible(&app, &bob.id).await;
    assert!(harness.visible(&app, &alice.id).await > 0, "the harness seeded data");

    let scope = SubjectScope::new([&app], alice.clone());
    let erasure = store.erase(&scope).await.expect("erase");
    assert!(erasure.deleted > 0, "the erasure reports what it deleted: {erasure:?}");
    assert_eq!(harness.visible(&app, &alice.id).await, 0, "no read path returns the subject");
    let exported = store.export(&scope).await.expect("export");
    assert!(is_empty(&exported), "nothing is exported after erasure: {exported}");
    assert_eq!(harness.visible(&app, &bob.id).await, bob_before, "another subject is untouched");
}

/// A repeat erasure succeeds and deletes nothing.
pub async fn erase_is_idempotent<H: SubjectDataStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let (app, alice) = (app(), person());
    harness.seed(&app, &alice.id).await;
    let scope = SubjectScope::new([&app], alice.clone());
    store.erase(&scope).await.expect("erase");
    let again = store.erase(&scope).await.expect("repeat");
    assert_eq!(again.deleted, 0, "a repeat deletes nothing: {again:?}");
    assert_eq!(harness.visible(&app, &alice.id).await, 0);
}

/// Erasing a subject erases the data of the visitor ids linked to it.
pub async fn erase_covers_linked_ids<H: SubjectDataStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let (app, alice) = (app(), person());
    let visitor = SubjectRef::visitor(unique_unicode("visitor"));
    harness.seed(&app, &alice.id).await;
    harness.seed(&app, &visitor.id).await;

    let scope = SubjectScope::new([&app], alice.clone()).with_linked(vec![visitor.clone()]);
    let exported = store.export(&SubjectScope::new([&app], visitor.clone())).await.expect("export");
    assert!(!is_empty(&exported), "the visitor id holds data: {exported}");
    store.erase(&scope).await.expect("erase");
    assert_eq!(harness.visible(&app, &alice.id).await, 0, "the subject's data is gone");
    assert_eq!(harness.visible(&app, &visitor.id).await, 0, "the linked id's data is gone");
}

/// Erasing a subject in one application leaves its data in another.
pub async fn erase_stays_in_its_applications<H: SubjectDataStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let (first, second, alice) = (app(), app(), person());
    harness.seed(&first, &alice.id).await;
    harness.seed(&second, &alice.id).await;
    let kept = harness.visible(&second, &alice.id).await;

    store.erase(&SubjectScope::new([&first], alice.clone())).await.expect("erase");
    assert_eq!(harness.visible(&first, &alice.id).await, 0);
    assert_eq!(harness.visible(&second, &alice.id).await, kept, "the other application keeps it");
}
