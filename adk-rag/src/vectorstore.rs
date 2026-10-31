//! Vector store trait for storing and searching vector embeddings.

use async_trait::async_trait;

use crate::document::{Chunk, SearchResult};
use crate::error::Result;
use crate::scope::Scope;

/// How a collection is created.
///
/// # Example
///
/// ```rust
/// use adk_rag::CollectionSpec;
///
/// let spec = CollectionSpec::new(768);
/// assert_eq!(spec.dimensions, 768);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectionSpec {
    /// The number of dimensions of every embedding stored in the collection.
    pub dimensions: usize,
}

impl CollectionSpec {
    /// A collection of `dimensions`-dimensional embeddings.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

/// A similarity search within one scope.
///
/// # Example
///
/// ```rust
/// use adk_rag::VectorQuery;
///
/// let embedding = [0.1, 0.9];
/// let query = VectorQuery::new(&embedding, 5);
/// assert_eq!(query.top_k, 5);
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct VectorQuery<'a> {
    /// The query embedding.
    pub embedding: &'a [f32],
    /// The most results to return.
    pub top_k: usize,
}

impl<'a> VectorQuery<'a> {
    /// A search for the `top_k` chunks nearest to `embedding`.
    pub fn new(embedding: &'a [f32], top_k: usize) -> Self {
        Self { embedding, top_k }
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
/// - `delete`, `count`, and `purge_scope` act on the scope's chunks only.
///
/// No operation reads, writes, or deletes across scopes, apart from `delete_collection`.
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

    /// Search a scope for the `query.top_k` chunks most similar to `query.embedding`.
    ///
    /// Returns results ordered by descending similarity score.
    async fn search(
        &self,
        collection: &str,
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>>;

    /// The number of chunks a scope holds in a collection.
    async fn count(&self, collection: &str, scope: &Scope) -> Result<u64>;

    /// Delete every chunk of a scope in a collection, returning how many it held.
    ///
    /// The count is taken as the purge starts on backends whose delete reports no count.
    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64>;
}
