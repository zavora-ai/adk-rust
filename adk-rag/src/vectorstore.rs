//! Vector store trait for storing and searching vector embeddings.

use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::document::{Chunk, SearchResult};
use crate::error::Result;
use crate::filter::{Filter, validate_field_name};
use crate::scope::Scope;

/// How a collection is created.
///
/// `filterable` declares the metadata keys a [`Filter`] may name. A backend stores each as
/// a column or field of its own where it needs one — LanceDB and Agent Retrieval do — and
/// [`RagPipeline`](crate::RagPipeline) refuses a filter naming any other key with
/// [`RagError::UnsupportedFilter`](crate::RagError::UnsupportedFilter) before any I/O.
/// `document_id`, `source_uri`, and the chunk ID are always filterable.
///
/// # Example
///
/// ```rust
/// use adk_rag::CollectionSpec;
///
/// let spec = CollectionSpec::new(768).with_filterable(["lang", "section"]);
/// assert_eq!(spec.dimensions, 768);
/// assert!(spec.filterable.contains("lang"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectionSpec {
    /// The number of dimensions of every embedding stored in the collection.
    pub dimensions: usize,
    /// The metadata keys a filter may name.
    pub filterable: BTreeSet<String>,
}

impl CollectionSpec {
    /// A collection of `dimensions`-dimensional embeddings with no filterable metadata keys.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions, filterable: BTreeSet::new() }
    }

    /// Declares metadata keys a filter may name, in addition to any declared before.
    ///
    /// A key is 1 to [`MAX_FIELD_LEN`](crate::filter::MAX_FIELD_LEN) ASCII letters, digits,
    /// and underscores, starting with a letter or underscore; `create_collection` rejects
    /// any other key.
    #[must_use]
    pub fn with_filterable<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.filterable.extend(keys.into_iter().map(Into::into));
        self
    }

    /// Checks every filterable key against the key rule.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::UnsupportedFilter`](crate::RagError::UnsupportedFilter) naming the
    /// first key outside the rule.
    pub fn validate(&self) -> Result<()> {
        self.filterable.iter().try_for_each(|key| validate_field_name(key))
    }
}

/// A similarity search within one scope.
///
/// # Example
///
/// ```rust
/// use adk_rag::{Field, Filter, VectorQuery};
///
/// let embedding = [0.1, 0.9];
/// let english = Filter::eq(Field::metadata("lang"), "en");
/// let query = VectorQuery::new(&embedding, 5).with_filter(&english);
/// assert_eq!(query.top_k, 5);
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct VectorQuery<'a> {
    /// The query embedding.
    pub embedding: &'a [f32],
    /// The most results to return.
    pub top_k: usize,
    /// Only chunks the filter matches are candidates.
    pub filter: Option<&'a Filter>,
}

impl<'a> VectorQuery<'a> {
    /// A search for the `top_k` chunks nearest to `embedding`.
    pub fn new(embedding: &'a [f32], top_k: usize) -> Self {
        Self { embedding, top_k, filter: None }
    }

    /// Searches only the chunks `filter` matches; the filter applies before the top `k` are
    /// chosen.
    #[must_use]
    pub fn with_filter(mut self, filter: &'a Filter) -> Self {
        self.filter = Some(filter);
        self
    }
}

/// A storage backend for vector embeddings with similarity search.
///
/// Implementations manage named collections of [`Chunk`]s. Every operation on stored chunks
/// names a [`Scope`], and a stored chunk is identified by its collection, its scope, and its
/// chunk ID together:
///
/// - `upsert` replaces a chunk with the same ID in the same scope and never touches another
///   scope's chunk with that ID.
/// - `search` considers only the scope's chunks, applying the scope inside the backend's
///   search before the top `k` are chosen, so a small scope beside a large one still gets
///   its nearest chunks.
/// - `delete`, `delete_where`, `count`, and `purge_scope` act on the scope's chunks only.
///
/// No operation reads, writes, or deletes across scopes, apart from `delete_collection`.
///
/// `search`, `count`, and `delete_where` take a [`Filter`], which the backend compiles into
/// its own query language with bound parameters or typed expressions and applies with the
/// scope, before the top `k` are chosen. A backend that cannot express a filter returns
/// [`RagError::UnsupportedFilter`](crate::RagError::UnsupportedFilter) before any I/O, and so
/// does every backend for a filter that fails [`Filter::validate`].
///
/// # Example
///
/// ```rust
/// use adk_rag::{Chunk, CollectionSpec, InMemoryVectorStore, Scope, VectorQuery, VectorStore};
///
/// # #[tokio::main]
/// # async fn main() -> adk_rag::Result<()> {
/// let store = InMemoryVectorStore::new();
/// store.create_collection("docs", &CollectionSpec::new(2)).await?;
///
/// let tenant_a = Scope::new("tenant-a")?;
/// let tenant_b = Scope::new("tenant-b")?;
/// let chunk = Chunk::new("doc_0", "doc", "tenant a's text").with_embedding(vec![1.0, 0.0]);
/// store.upsert("docs", &tenant_a, &[chunk]).await?;
///
/// let query = VectorQuery::new(&[1.0, 0.0], 5);
/// assert_eq!(store.search("docs", &tenant_a, &query).await?.len(), 1);
/// assert!(store.search("docs", &tenant_b, &query).await?.is_empty());
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Create a named collection. No-op if it already exists.
    async fn create_collection(&self, name: &str, spec: &CollectionSpec) -> Result<()>;

    /// Delete a named collection and every scope's data in it.
    async fn delete_collection(&self, name: &str) -> Result<()>;

    /// Upsert chunks into a scope of a collection. Chunks must have embeddings set.
    async fn upsert(&self, collection: &str, scope: &Scope, chunks: &[Chunk]) -> Result<()>;

    /// Delete a scope's chunks by their IDs. Unknown IDs are ignored.
    async fn delete(&self, collection: &str, scope: &Scope, ids: &[&str]) -> Result<()>;

    /// Search a scope for the `query.top_k` chunks most similar to `query.embedding` among
    /// those `query.filter` matches.
    ///
    /// Returns results ordered by descending similarity score.
    async fn search(
        &self,
        collection: &str,
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>>;

    /// The number of chunks a scope holds in a collection, or of those `filter` matches.
    async fn count(&self, collection: &str, scope: &Scope, filter: Option<&Filter>) -> Result<u64>;

    /// Delete the scope's chunks that `filter` matches, returning how many were deleted.
    ///
    /// The count is taken as the delete starts on backends whose delete reports no count.
    /// [`purge_scope`](Self::purge_scope) is the only way to delete a whole scope.
    async fn delete_where(&self, collection: &str, scope: &Scope, filter: &Filter) -> Result<u64>;

    /// Delete every chunk of a scope in a collection, returning how many it held.
    ///
    /// The count is taken as the purge starts on backends whose delete reports no count.
    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64>;
}
