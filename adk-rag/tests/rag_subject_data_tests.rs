//! `RagSubjectData` exports and erases a subject's chunks: a scope per user passes the shared
//! subject data suite, and tagged chunks are erased by their `adk_subject` metadata.

use std::sync::Arc;

use adk_core::conformance::SubjectDataStoreHarness;
use adk_core::principal::SubjectRef;
use adk_core::subject::{SubjectDataStore, SubjectScope, VisitorId};
use adk_rag::{
    Chunk, CollectionSpec, Field, Filter, InMemoryVectorStore, RagSubjectData,
    SUBJECT_METADATA_KEY, Scope, SubjectPartition, VectorStore,
};

const COLLECTION: &str = "notes";

fn chunk(user: &str, n: usize) -> Chunk {
    Chunk::new(format!("{user}-{n}"), format!("doc-{user}"), "a note")
        .with_embedding(vec![1.0, 0.0])
        .with_metadata(SUBJECT_METADATA_KEY, user)
}

async fn create(store: &InMemoryVectorStore) {
    let spec = CollectionSpec::new(2).with_filterable([SUBJECT_METADATA_KEY]);
    store.create_collection(COLLECTION, &spec).await.unwrap();
}

/// A scope per user: `[app, user]`.
struct PerUser(Arc<InMemoryVectorStore>);

#[adk_core::async_trait]
impl SubjectDataStoreHarness for PerUser {
    async fn open(&self) -> Arc<dyn SubjectDataStore> {
        Arc::new(RagSubjectData::new(self.0.clone(), [COLLECTION], SubjectPartition::ScopePerUser))
    }

    async fn seed(&self, app_name: &str, user_id: &str) {
        create(&self.0).await;
        let scope = Scope::from_parts([app_name, user_id]).unwrap();
        self.0.upsert(COLLECTION, &scope, &[chunk(user_id, 0), chunk(user_id, 1)]).await.unwrap();
    }

    async fn visible(&self, app_name: &str, user_id: &str) -> usize {
        let scope = Scope::from_parts([app_name, user_id]).unwrap();
        self.0.count(COLLECTION, &scope, None).await.unwrap() as usize
    }
}

mod per_user_scope {
    adk_core::subject_data_store_conformance_tests!(
        harness = super::PerUser(std::sync::Arc::new(adk_rag::InMemoryVectorStore::new()))
    );
}

#[tokio::test]
async fn tagged_chunks_are_exported_and_erased_by_subject() {
    let store = Arc::new(InMemoryVectorStore::new());
    create(&store).await;
    let shared = Scope::single_tenant();
    let visitor = VisitorId::generate();
    for user in ["alice", "bob", visitor.as_str()] {
        store.upsert(COLLECTION, &shared, &[chunk(user, 0), chunk(user, 1)]).await.unwrap();
    }
    let rag = RagSubjectData::new(
        store.clone(),
        [COLLECTION],
        SubjectPartition::Tagged { scopes: vec![shared.clone()] },
    );
    let scope = SubjectScope::new(["shop"], SubjectRef::human("alice"))
        .with_linked(vec![visitor.subject()]);

    let exported = rag.export(&scope).await.unwrap();
    assert_eq!(exported["collections"][0]["chunks"], 4, "{exported}");
    assert_eq!(rag.erase(&scope).await.unwrap().deleted, 4);
    assert_eq!(rag.erase(&scope).await.unwrap().deleted, 0, "a repeat deletes nothing");
    assert_eq!(rag.export(&scope).await.unwrap(), serde_json::json!({ "collections": [] }));
    let bob = Filter::eq(Field::metadata(SUBJECT_METADATA_KEY), "bob");
    assert_eq!(
        store.count(COLLECTION, &shared, Some(&bob)).await.unwrap(),
        2,
        "others are untouched"
    );
}
