//! `SubjectRights` over the in-memory registry and consent store, with fake data stores:
//! erasure order and outcomes, export, links, revocation, and visitor approvers.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::audit_log::{AuditRecordKind, AuditTrail, InMemoryAuditRecorder};
use adk_core::conformance::SubjectDataStoreHarness;
use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentPurpose, ConsentRecord, ConsentStore, ErasureStatus, ExportOutcome,
    InMemoryConsentStore, InMemorySubjectRegistry, SUBJECT_ERASING_CODE, StoreErasure,
    StoreOutcome, StoreReport, StoreSupport, SubjectDataStore, SubjectRegistry, SubjectRights,
    SubjectScope, SubjectState, VisitorId, unsupported_error,
};
use adk_core::{
    AdkError, ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
    ApprovalStore, ApproverSelector, Clock, InMemoryApprovalStore, ManualClock, Result,
    StoreApprovalService, ToolConfirmationRequest, async_trait,
};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// Items per `(app, user)`.
type Items = BTreeMap<(String, String), Vec<String>>;

/// Items per `(app, user)`, the shape of every store `SubjectRights` reaches by key.
#[derive(Clone, Default)]
struct MapStore {
    items: Arc<Mutex<Items>>,
    support: Option<StoreSupport>,
    failing: Arc<AtomicBool>,
    exports: bool,
}

impl MapStore {
    fn new() -> Self {
        Self { exports: true, ..Self::default() }
    }

    async fn put(&self, app: &str, user: &str, item: &str) {
        self.items.lock().await.entry((app.into(), user.into())).or_default().push(item.into());
    }

    async fn count(&self, app: &str, user: &str) -> usize {
        self.items.lock().await.get(&(app.to_owned(), user.to_owned())).map_or(0, Vec::len)
    }
}

#[async_trait]
impl SubjectDataStore for MapStore {
    fn name(&self) -> &str {
        "map"
    }

    fn support(&self) -> StoreSupport {
        self.support.unwrap_or(StoreSupport::Hard)
    }

    async fn export(&self, scope: &SubjectScope) -> Result<Value> {
        if !self.exports {
            return Err(unsupported_error("map", "export"));
        }
        let items = self.items.lock().await;
        let mut found = Vec::new();
        for app in &scope.app_names {
            for user in scope.user_ids() {
                found.extend(
                    items.get(&(app.clone(), user.to_owned())).into_iter().flatten().cloned(),
                );
            }
        }
        Ok(json!({ "items": found }))
    }

    async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(AdkError::session("map store is down"));
        }
        let mut items = self.items.lock().await;
        let mut deleted = 0;
        for app in &scope.app_names {
            for user in scope.user_ids() {
                deleted +=
                    items.remove(&(app.clone(), user.to_owned())).map_or(0, |v| v.len() as u64);
            }
        }
        Ok(StoreErasure::deleted(deleted))
    }
}

#[derive(Clone)]
struct MapHarness(MapStore);

#[async_trait]
impl SubjectDataStoreHarness for MapHarness {
    async fn open(&self) -> Arc<dyn SubjectDataStore> {
        Arc::new(self.0.clone())
    }

    async fn seed(&self, app_name: &str, user_id: &str) {
        self.0.put(app_name, user_id, "note").await;
    }

    async fn visible(&self, app_name: &str, user_id: &str) -> usize {
        self.0.count(app_name, user_id).await
    }
}

mod map_store_conformance {
    adk_core::subject_data_store_conformance_tests!(
        harness = super::MapHarness(super::MapStore::new())
    );
}

struct Fixture {
    rights: SubjectRights,
    registry: InMemorySubjectRegistry,
    consent: InMemoryConsentStore,
    map: MapStore,
    recorder: Arc<InMemoryAuditRecorder>,
    clock: ManualClock,
}

fn base_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).single().expect("valid instant")
}

fn fixture(map: MapStore) -> Fixture {
    let clock = ManualClock::new(base_time());
    let registry = InMemorySubjectRegistry::new().with_clock(Arc::new(clock.clone()));
    let consent = InMemoryConsentStore::new();
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let rights = SubjectRights::new(Arc::new(registry.clone()), Arc::new(consent.clone()))
        .with_app("shop")
        .with_store(Arc::new(map.clone()))
        .with_audit(AuditTrail::new(recorder.clone()))
        .with_clock(Arc::new(clock.clone()));
    Fixture { rights, registry, consent, map, recorder, clock }
}

fn grant(subject: &SubjectRef, purpose: ConsentPurpose) -> ConsentRecord {
    ConsentRecord::new(subject.clone(), purpose, "2026-10").granted_at(base_time())
}

#[tokio::test]
async fn erasure_reaches_every_store_and_linked_ids_and_leaves_others() {
    let fx = fixture(MapStore::new());
    let alice = SubjectRef::human("alice");
    let bob = SubjectRef::human("bob");
    let visitor = VisitorId::generate().subject();
    fx.rights.link(&visitor, &alice).await.unwrap();
    let pseudonym = fx.registry.get(&alice).await.unwrap().unwrap().pseudonym;
    for (user, item) in [(&alice, "order"), (&visitor, "cart"), (&bob, "order")] {
        fx.map.put("shop", &user.id, item).await;
    }
    fx.rights.grant(grant(&alice, ConsentPurpose::PersistentProfile)).await.unwrap();
    fx.rights.grant(grant(&visitor, ConsentPurpose::Analytics)).await.unwrap();
    fx.rights.grant(grant(&bob, ConsentPurpose::Analytics)).await.unwrap();

    let report = fx.rights.erase(&alice).await.unwrap();
    assert_eq!(report.status, ErasureStatus::Completed);
    assert_eq!(report.subjects, 2);
    assert_eq!(
        report.stores,
        vec![
            StoreReport {
                store: "map".into(),
                outcome: StoreOutcome::Erased { deleted: 2, retained: 0 }
            },
            StoreReport {
                store: "consent".into(),
                outcome: StoreOutcome::Erased { deleted: 2, retained: 0 }
            },
        ]
    );
    assert_eq!(fx.map.count("shop", "alice").await, 0);
    assert_eq!(fx.map.count("shop", &visitor.id).await, 0);
    assert_eq!(fx.map.count("shop", "bob").await, 1, "another subject is untouched");
    assert_eq!(fx.consent.for_subject(&bob).await.unwrap().len(), 1);
    for erased in [&alice, &visitor] {
        assert_eq!(fx.registry.get(erased).await.unwrap(), None, "the registry forgets {erased}");
        assert!(fx.consent.for_subject(erased).await.unwrap().is_empty());
    }
    assert_eq!(fx.registry.by_pseudonym(&pseudonym).await.unwrap(), None);

    let records = fx.recorder.records();
    let erasure =
        records.iter().find(|r| r.kind == AuditRecordKind::Erasure).expect("an erasure record");
    assert_eq!(erasure.actor.on_behalf_of, Some(SubjectRef::human(pseudonym)));
    let text =
        serde_json::to_string(&json!({ "ext": erasure.ext, "actor": erasure.actor })).unwrap();
    for id in ["alice", visitor.id.as_str()] {
        assert!(!text.contains(id), "the audit trail never names {id}: {text}");
    }

    let again = fx.rights.erase(&alice).await.unwrap();
    assert_eq!(again.status, ErasureStatus::Completed);
    assert_eq!(again.subjects, 1, "the link went with the first erasure");
    assert_eq!(
        again.stores.iter().map(|line| line.outcome.clone()).collect::<Vec<_>>(),
        vec![StoreOutcome::Erased { deleted: 0, retained: 0 }; 2],
        "a repeat deletes nothing"
    );
    assert_eq!(fx.registry.get(&alice).await.unwrap(), None);
}

#[tokio::test]
async fn a_failed_store_keeps_the_subject_erasing_until_a_repeat_finishes() {
    let map = MapStore::new();
    map.failing.store(true, Ordering::SeqCst);
    let fx = fixture(map);
    let visitor = VisitorId::generate().subject();
    fx.map.put("shop", &visitor.id, "cart").await;
    fx.rights.grant(grant(&visitor, ConsentPurpose::PersistentProfile)).await.unwrap();

    let report = fx.rights.erase(&visitor).await.unwrap();
    assert_eq!(report.status, ErasureStatus::Partial);
    assert!(report.failed());
    assert!(matches!(report.stores[0].outcome, StoreOutcome::Failed { .. }));
    assert_eq!(
        fx.registry.get(&visitor).await.unwrap().map(|record| record.state),
        Some(SubjectState::Erasing)
    );
    let refused = fx.rights.grant(grant(&visitor, ConsentPurpose::Analytics)).await.unwrap_err();
    assert_eq!(refused.code, SUBJECT_ERASING_CODE);
    assert_eq!(fx.rights.ensure_writable(&visitor).await.unwrap_err().code, SUBJECT_ERASING_CODE);
    let alice = SubjectRef::human("alice");
    assert_eq!(fx.rights.link(&visitor, &alice).await.unwrap_err().code, SUBJECT_ERASING_CODE);

    fx.map.failing.store(false, Ordering::SeqCst);
    let finished = fx.rights.erase(&visitor).await.unwrap();
    assert_eq!(finished.status, ErasureStatus::Completed);
    assert_eq!(finished.stores[0].outcome, StoreOutcome::Erased { deleted: 1, retained: 0 });
    assert_eq!(fx.registry.get(&visitor).await.unwrap(), None);
    fx.rights.ensure_writable(&visitor).await.unwrap();
}

#[tokio::test]
async fn unsupported_and_unreachable_stores_are_reported_never_skipped() {
    let fx = fixture(MapStore::new());
    let provider = MapStore { support: Some(StoreSupport::Unreachable), ..MapStore::new() };
    let legacy =
        MapStore { support: Some(StoreSupport::Unsupported), exports: false, ..MapStore::new() };
    let rights = fx.rights.clone().with_store(Arc::new(provider)).with_store(Arc::new(legacy));
    let visitor = VisitorId::generate().subject();

    let report = rights.erase(&visitor).await.unwrap();
    assert_eq!(report.status, ErasureStatus::Partial);
    assert!(!report.failed(), "nothing to retry: the remaining stores cannot erase");
    let outcomes: Vec<_> = report.stores.iter().map(|line| line.outcome.clone()).collect();
    assert_eq!(
        outcomes,
        vec![
            StoreOutcome::Erased { deleted: 0, retained: 0 },
            StoreOutcome::Unreachable,
            StoreOutcome::Unsupported,
            StoreOutcome::Erased { deleted: 0, retained: 0 },
        ]
    );
    assert_eq!(fx.registry.get(&visitor).await.unwrap(), None);

    let export = rights.export(&visitor).await.unwrap();
    assert!(!export.is_complete());
    let outcomes: Vec<_> = export.stores.iter().map(|entry| entry.outcome.clone()).collect();
    assert_eq!(
        outcomes,
        vec![
            ExportOutcome::Exported { data: json!({ "items": [] }) },
            ExportOutcome::Unreachable,
            ExportOutcome::Unsupported,
        ]
    );
}

#[tokio::test]
async fn eventual_stores_report_when_deletion_completes() {
    let map = MapStore {
        support: Some(StoreSupport::Eventual { within: Duration::from_secs(3600) }),
        ..MapStore::new()
    };
    let fx = fixture(map);
    let visitor = VisitorId::generate().subject();
    fx.map.put("shop", &visitor.id, "cart").await;

    let report = fx.rights.erase(&visitor).await.unwrap();
    let due = base_time() + chrono::Duration::hours(1);
    assert_eq!(report.status, ErasureStatus::Completed);
    assert_eq!(
        report.stores[0].outcome,
        StoreOutcome::Scheduled { deleted: 1, retained: 0, completes_by: due }
    );
    assert_eq!(report.completes_by, Some(due));
    assert_eq!(report.erased_at, fx.clock.now());
}

#[tokio::test]
async fn export_covers_the_registry_consent_and_linked_ids() {
    let fx = fixture(MapStore::new());
    let alice = SubjectRef::human("alice");
    let visitor = VisitorId::generate().subject();
    fx.rights.link(&visitor, &alice).await.unwrap();
    fx.map.put("shop", "alice", "order").await;
    fx.map.put("shop", &visitor.id, "cart").await;
    fx.map.put("other-app", "alice", "elsewhere").await;
    let consent = grant(&visitor, ConsentPurpose::SessionContinuity);
    fx.rights.grant(consent.clone()).await.unwrap();

    let export = fx.rights.export(&alice).await.unwrap();
    assert!(export.is_complete());
    assert_eq!(export.linked, vec![visitor.clone()]);
    assert_eq!(
        export.registry.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec![alice, visitor]
    );
    assert_eq!(export.consent, vec![consent]);
    assert_eq!(
        export.stores[0].outcome,
        ExportOutcome::Exported { data: json!({ "items": ["order", "cart"] }) },
        "only the covered applications are searched"
    );
}

#[tokio::test]
async fn links_are_audited_by_pseudonym() {
    let fx = fixture(MapStore::new());
    let visitor = VisitorId::generate().subject();
    let alice = SubjectRef::human("alice");
    let record = fx.rights.link(&visitor, &alice).await.unwrap();
    let alice_pseudonym = fx.registry.get(&alice).await.unwrap().unwrap().pseudonym;

    let records = fx.recorder.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action, "subject.link");
    assert_eq!(records[0].actor.on_behalf_of, Some(SubjectRef::human(alice_pseudonym)));
    assert_eq!(records[0].ext.get("visitor"), Some(&Value::String(record.pseudonym)));
}

#[tokio::test]
async fn revoking_stops_consent_and_erases_only_when_configured() {
    let fx = fixture(MapStore::new());
    let visitor = VisitorId::generate().subject();
    fx.map.put("shop", &visitor.id, "cart").await;
    fx.rights.grant(grant(&visitor, ConsentPurpose::Analytics)).await.unwrap();
    fx.rights.grant(grant(&visitor, ConsentPurpose::PersistentProfile)).await.unwrap();

    let revocation = fx.rights.revoke(&visitor, Some(&ConsentPurpose::Analytics)).await.unwrap();
    assert_eq!(revocation.revoked.len(), 1);
    assert_eq!(revocation.erasure, None);
    assert_eq!(fx.map.count("shop", &visitor.id).await, 1, "revoking erases nothing by default");

    let rights = fx.rights.clone().with_erase_on_revoke(ConsentPurpose::PersistentProfile);
    let revocation = rights.revoke(&visitor, None).await.unwrap();
    assert_eq!(revocation.revoked.len(), 1);
    assert_eq!(revocation.erasure.map(|report| report.status), Some(ErasureStatus::Completed));
    assert_eq!(fx.map.count("shop", &visitor.id).await, 0);
}

#[tokio::test]
async fn a_visitor_cannot_decide_an_approval() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let service = StoreApprovalService::new(store.clone());
    let scope = ApprovalScope::new("shop", "alice", "s1");
    let call = ToolConfirmationRequest {
        tool_name: "refund".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "amount": 5 }),
        acting: None,
        origin: None,
    };
    store.record_pending(&scope, &call).await.unwrap();
    let policy = ApprovalPolicy::new([ApproverSelector::group("support")]);
    let request = service.submit(ApprovalRequest::new("r-1", scope, &call, policy)).await.unwrap();

    let visitor = VisitorId::generate().subject();
    let error = service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(visitor).with_groups(["support"]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "approval.visitor_not_allowed");
}
