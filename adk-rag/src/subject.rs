//! A vector store as a [`SubjectDataStore`] for `adk_core::subject::SubjectRights`.

use std::sync::Arc;

use adk_core::subject::{StoreErasure, StoreSupport, SubjectDataStore, SubjectScope};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::filter::{Field, Filter};
use crate::scope::Scope;
use crate::vectorstore::VectorStore;

/// The metadata key that names the data subject a chunk is about. Declare it filterable on
/// the collection ([`CollectionSpec::with_filterable`](crate::CollectionSpec::with_filterable))
/// and set it to the subject's user id on every document ingested about a person.
pub const SUBJECT_METADATA_KEY: &str = "adk_subject";

/// Where a store keeps a subject's chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubjectPartition {
    /// In the listed scopes, tagged with [`SUBJECT_METADATA_KEY`]; erasure deletes the
    /// tagged chunks with [`VectorStore::delete_where`].
    Tagged {
        /// The scopes searched for the subject's chunks.
        scopes: Vec<Scope>,
    },
    /// In a scope of their own, `Scope::from_parts([app, user])`; erasure purges the scope
    /// with [`VectorStore::purge_scope`].
    ScopePerUser,
}

/// Exports and erases a data subject's chunks in the listed collections of a
/// [`VectorStore`].
///
/// | Partition | Finds the subject's chunks | Erases them with |
/// |-----------|----------------------------|------------------|
/// | [`Tagged`](SubjectPartition::Tagged) | `adk_subject` metadata equal to a user id of the scope, in each listed scope | `delete_where` |
/// | [`ScopePerUser`](SubjectPartition::ScopePerUser) | The scope `[app, user]` of each application and user id of the scope | `purge_scope` |
///
/// The vector store trait lists no chunk text, so the export names each collection and
/// scope with the number of the subject's chunks it holds. A backend whose deletes complete
/// later, or keep earlier table versions, declares it with
/// [`with_support`](Self::with_support).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{SubjectDataStore, SubjectScope};
/// use adk_rag::{
///     Chunk, CollectionSpec, InMemoryVectorStore, RagSubjectData, SUBJECT_METADATA_KEY, Scope,
///     SubjectPartition, VectorStore,
/// };
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let store = Arc::new(InMemoryVectorStore::new());
/// let spec = CollectionSpec::new(2).with_filterable([SUBJECT_METADATA_KEY]);
/// store.create_collection("support_notes", &spec).await?;
/// let note = Chunk::new("n1", "ticket-7", "Prefers email.")
///     .with_embedding(vec![1.0, 0.0])
///     .with_metadata(SUBJECT_METADATA_KEY, "alice");
/// store.upsert("support_notes", &Scope::single_tenant(), &[note]).await?;
///
/// let chunks = RagSubjectData::new(
///     store,
///     ["support_notes"],
///     SubjectPartition::Tagged { scopes: vec![Scope::single_tenant()] },
/// );
/// let erased = chunks.erase(&SubjectScope::new(["shop"], SubjectRef::human("alice"))).await?;
/// assert_eq!(erased.deleted, 1);
/// # Ok(())
/// # }
/// ```
pub struct RagSubjectData {
    store: Arc<dyn VectorStore>,
    collections: Vec<String>,
    partition: SubjectPartition,
    support: StoreSupport,
}

impl RagSubjectData {
    /// The subject's chunks in `collections` of `store`, kept as `partition` says.
    pub fn new<I, S>(
        store: Arc<dyn VectorStore>,
        collections: I,
        partition: SubjectPartition,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            store,
            collections: collections.into_iter().map(Into::into).collect(),
            partition,
            support: StoreSupport::Hard,
        }
    }

    /// Declares how the store erases, such as [`StoreSupport::Eventual`] for a managed
    /// service whose deletes complete later.
    #[must_use]
    pub fn with_support(mut self, support: StoreSupport) -> Self {
        self.support = support;
        self
    }

    /// Each `(scope, filter)` that holds the subject's chunks: the filter is `None` for a
    /// scope that is the subject's own.
    fn targets(&self, subject: &SubjectScope) -> adk_core::Result<Vec<(Scope, Option<Filter>)>> {
        match &self.partition {
            SubjectPartition::Tagged { scopes } => {
                let ids: Vec<String> = subject.user_ids().into_iter().map(str::to_owned).collect();
                let tagged = Filter::In(Field::metadata(SUBJECT_METADATA_KEY), ids);
                Ok(scopes.iter().map(|scope| (scope.clone(), Some(tagged.clone()))).collect())
            }
            SubjectPartition::ScopePerUser => {
                let mut targets = Vec::new();
                for app_name in &subject.app_names {
                    for user_id in subject.user_ids() {
                        targets.push((Scope::from_parts([app_name.as_str(), user_id])?, None));
                    }
                }
                Ok(targets)
            }
        }
    }
}

impl std::fmt::Debug for RagSubjectData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RagSubjectData")
            .field("collections", &self.collections)
            .field("partition", &self.partition)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SubjectDataStore for RagSubjectData {
    fn name(&self) -> &str {
        "rag"
    }

    fn support(&self) -> StoreSupport {
        self.support
    }

    async fn export(&self, subject: &SubjectScope) -> adk_core::Result<Value> {
        let mut held = Vec::new();
        for collection in &self.collections {
            for (scope, filter) in self.targets(subject)? {
                let chunks = self.store.count(collection, &scope, filter.as_ref()).await?;
                if chunks > 0 {
                    held.push(json!({
                        "collection": collection,
                        "scope": scope.as_str(),
                        "chunks": chunks,
                    }));
                }
            }
        }
        Ok(json!({ "collections": held }))
    }

    async fn erase(&self, subject: &SubjectScope) -> adk_core::Result<StoreErasure> {
        let mut deleted = 0;
        for collection in &self.collections {
            for (scope, filter) in self.targets(subject)? {
                deleted += match &filter {
                    Some(filter) => self.store.delete_where(collection, &scope, filter).await?,
                    None => self.store.purge_scope(collection, &scope).await?,
                };
            }
        }
        Ok(StoreErasure::deleted(deleted))
    }
}
