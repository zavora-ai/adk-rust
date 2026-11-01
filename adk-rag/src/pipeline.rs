//! RAG pipeline orchestrator.
//!
//! The [`RagPipeline`] coordinates the full ingest-and-query workflow by
//! composing an [`EmbeddingProvider`], a [`VectorStore`], a [`Chunker`],
//! and an optional [`Reranker`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::{RagPipeline, RagConfig, InMemoryVectorStore, FixedSizeChunker, Scope};
//!
//! let pipeline = RagPipeline::builder()
//!     .config(RagConfig::default())
//!     .embedding_provider(Arc::new(my_embedder))
//!     .vector_store(Arc::new(InMemoryVectorStore::new()))
//!     .chunker(Arc::new(FixedSizeChunker::new(512, 100)))
//!     .build()?;
//!
//! pipeline.create_collection("docs").await?;
//! let tenant = pipeline.scoped(Scope::from_parts(["support", "tenant-42"])?);
//! tenant.ingest("docs", &document).await?;
//! let results = tenant.query("docs", "search query").await?;
//! ```

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info};

use crate::chunking::Chunker;
use crate::config::RagConfig;
use crate::document::{Chunk, Document, SearchResult};
use crate::embedding::EmbeddingProvider;
use crate::error::{RagError, Result};
use crate::filter::{Field, Filter};
use crate::reranker::Reranker;
use crate::scope::Scope;
use crate::vectorstore::{CollectionSpec, VectorQuery, VectorStore};

/// The RAG pipeline orchestrator.
///
/// Coordinates document ingestion (chunk → embed → store) and query
/// execution (embed → search → rerank → filter). Construct one via
/// [`RagPipeline::builder()`]. Ingest and query go through [`RagPipeline::scoped`], which
/// binds them to one [`Scope`].
pub struct RagPipeline {
    config: RagConfig,
    embedding_provider: Arc<dyn EmbeddingProvider>,
    vector_store: Arc<dyn VectorStore>,
    chunker: Arc<dyn Chunker>,
    reranker: Option<Arc<dyn Reranker>>,
    /// Filterable metadata keys of each collection this pipeline created.
    filterable: RwLock<HashMap<String, BTreeSet<String>>>,
}

impl RagPipeline {
    /// Create a new [`RagPipelineBuilder`].
    pub fn builder() -> RagPipelineBuilder {
        RagPipelineBuilder::default()
    }

    /// Return a reference to the pipeline configuration.
    pub fn config(&self) -> &RagConfig {
        &self.config
    }

    /// Return a reference to the embedding provider.
    pub fn embedding_provider(&self) -> &Arc<dyn EmbeddingProvider> {
        &self.embedding_provider
    }

    /// Return a reference to the vector store.
    pub fn vector_store(&self) -> &Arc<dyn VectorStore> {
        &self.vector_store
    }

    /// Create a named collection in the vector store.
    ///
    /// The collection is created with the dimensionality reported by the
    /// configured [`EmbeddingProvider`].
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if the vector store operation fails.
    pub async fn create_collection(&self, name: &str) -> Result<()> {
        self.create_collection_with_filterable(name, Vec::<String>::new()).await
    }

    /// Create a named collection whose `filterable` metadata keys a [`Filter`] may name.
    ///
    /// The pipeline remembers the keys, and refuses a filter that names any other metadata
    /// key with [`RagError::UnsupportedFilter`] before any I/O. Call this at startup for every
    /// collection the pipeline filters, including one another process created: creation is
    /// idempotent.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// pipeline.create_collection_with_filterable("docs", ["lang", "section"]).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if the vector store operation fails, including
    /// for a key outside the rule in [`CollectionSpec::with_filterable`].
    pub async fn create_collection_with_filterable<I, S>(
        &self,
        name: &str,
        filterable: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let spec =
            CollectionSpec::new(self.embedding_provider.dimensions()).with_filterable(filterable);
        self.vector_store.create_collection(name, &spec).await.map_err(|e| {
            error!(collection = name, error = %e, "failed to create collection");
            RagError::PipelineError(format!("failed to create collection '{name}': {e}"))
        })?;
        self.filterable.write().await.insert(name.to_string(), spec.filterable);
        Ok(())
    }

    /// Checks `filter`'s shape and that its metadata keys are declared on `collection`.
    async fn check_filter(&self, collection: &str, filter: &Filter) -> Result<()> {
        let filterable = self.filterable.read().await;
        let declared = filterable.get(collection).cloned().unwrap_or_default();
        filter.validate_declared(collection, &declared)
    }

    /// Delete a named collection from the vector store.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if the vector store operation fails.
    pub async fn delete_collection(&self, name: &str) -> Result<()> {
        self.vector_store.delete_collection(name).await.map_err(|e| {
            error!(collection = name, error = %e, "failed to delete collection");
            RagError::PipelineError(format!("failed to delete collection '{name}': {e}"))
        })?;
        self.filterable.write().await.remove(name);
        Ok(())
    }

    /// A view of the pipeline that ingests into, searches, and deletes from one scope.
    ///
    /// Every operation on stored chunks goes through a [`ScopedRag`], so none can read,
    /// write, or delete across scopes. The scope comes from the application, such as the
    /// authenticated tenant, never from a model.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_rag::Scope;
    ///
    /// let tenant = pipeline.scoped(Scope::from_parts(["support", tenant_id])?);
    /// tenant.ingest("docs", &document).await?;
    /// let results = tenant.query("docs", "refund window").await?;
    /// ```
    pub fn scoped(&self, scope: Scope) -> ScopedRag<'_> {
        ScopedRag { pipeline: self, scope }
    }
}

/// Options for [`ScopedRag::query_with`].
///
/// # Example
///
/// ```rust
/// use adk_rag::QueryOptions;
///
/// use adk_rag::{Field, Filter};
///
/// let options = QueryOptions::new()
///     .with_top_k(3)
///     .with_filter(Filter::eq(Field::metadata("lang"), "en"));
/// assert_eq!(options.top_k, Some(3));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct QueryOptions {
    /// The most results to retrieve; the pipeline's configured `top_k` when `None`.
    pub top_k: Option<usize>,
    /// Only chunks the filter matches are candidates.
    pub filter: Option<Filter>,
}

impl QueryOptions {
    /// Options that change nothing about the configured query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieves `top_k` results instead of the configured number.
    #[must_use]
    pub fn with_top_k(mut self, top_k: usize) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// Searches only the chunks `filter` matches; the vector store applies the filter before
    /// the top `k` are chosen.
    #[must_use]
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }
}

/// A [`RagPipeline`] bound to one [`Scope`], from [`RagPipeline::scoped`].
///
/// Ingest writes the scope's chunks, queries search only the scope's chunks, and deletes
/// remove only the scope's chunks.
pub struct ScopedRag<'a> {
    pipeline: &'a RagPipeline,
    scope: Scope,
}

impl ScopedRag<'_> {
    /// The scope every operation acts on.
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Ingest a single document into the scope: chunk → embed → store, replacing the
    /// document's earlier chunks.
    ///
    /// The new chunks are upserted first, then the document's chunks in the scope that the
    /// new version no longer has are deleted, so a search never finds the document without
    /// chunks and a shorter version leaves no stale chunks. A document with empty text
    /// removes the document's chunks. Returns the chunks that were stored (with embeddings
    /// attached).
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if embedding or storage fails, or if
    /// the embedding provider returns a different number of vectors than there
    /// are chunks, including the document ID in the error message.
    pub async fn ingest(&self, collection: &str, document: &Document) -> Result<Vec<Chunk>> {
        let pipeline = self.pipeline;
        let scope = self.scope.as_str();
        // 1. Chunk the document
        let mut chunks = pipeline.chunker.chunk(document);
        if chunks.is_empty() {
            let removed = self.delete_document(collection, &document.id).await?;
            info!(document.id = %document.id, scope, chunk_count = 0, removed, "ingested document (empty)");
            return Ok(chunks);
        }

        // 2. Collect chunk texts for batch embedding
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

        // 3. Generate embeddings
        let embeddings = pipeline.embedding_provider.embed_batch(&texts).await.map_err(|e| {
            error!(document.id = %document.id, error = %e, "embedding failed during ingestion");
            RagError::PipelineError(format!("embedding failed for document '{}': {e}", document.id))
        })?;

        // 4. Attach embeddings to chunks. `zip` would silently drop the chunks
        // left without a vector, so a count mismatch fails the ingest instead.
        if embeddings.len() != chunks.len() {
            error!(
                document.id = %document.id,
                chunk_count = chunks.len(),
                embedding_count = embeddings.len(),
                "embedding provider returned the wrong number of vectors"
            );
            return Err(RagError::PipelineError(format!(
                "embedding provider returned {} vectors for {} chunks of document '{}'",
                embeddings.len(),
                chunks.len(),
                document.id
            )));
        }
        for (chunk, embedding) in chunks.iter_mut().zip(embeddings) {
            chunk.embedding = embedding;
        }

        // 5. Upsert into the scope
        pipeline.vector_store.upsert(collection, &self.scope, &chunks).await.map_err(|e| {
            error!(document.id = %document.id, error = %e, "upsert failed during ingestion");
            RagError::PipelineError(format!("upsert failed for document '{}': {e}", document.id))
        })?;

        // 6. Remove the document's chunks the new version no longer has
        let stale = Filter::and([
            Filter::eq(Field::DocumentId, document.id.clone()),
            !Filter::is_in(Field::ChunkId, chunks.iter().map(|chunk| chunk.id.clone())),
        ]);
        let removed =
            pipeline.vector_store.delete_where(collection, &self.scope, &stale).await.map_err(
                |e| {
                    error!(document.id = %document.id, error = %e, "stale chunk removal failed");
                    RagError::PipelineError(format!(
                        "removing stale chunks of document '{}' failed: {e}",
                        document.id
                    ))
                },
            )?;

        let chunk_count = chunks.len();
        info!(document.id = %document.id, scope, chunk_count, removed, "ingested document");

        Ok(chunks)
    }

    /// Ingest multiple documents into the scope through the chunk → embed → store workflow.
    ///
    /// Returns all chunks that were stored across all documents.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] on the first document that fails,
    /// including the document ID in the error message.
    pub async fn ingest_batch(
        &self,
        collection: &str,
        documents: &[Document],
    ) -> Result<Vec<Chunk>> {
        let mut all_chunks = Vec::new();
        for document in documents {
            let chunks = self.ingest(collection, document).await?;
            all_chunks.extend(chunks);
        }
        Ok(all_chunks)
    }

    /// Query the scope: embed → search → rerank → filter by threshold.
    ///
    /// Returns search results ordered by descending relevance score. Results
    /// below the configured `similarity_threshold` are filtered out.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if embedding or search fails.
    pub async fn query(&self, collection: &str, query: &str) -> Result<Vec<SearchResult>> {
        self.query_with(collection, query, &QueryOptions::new()).await
    }

    /// Query the scope as [`query`](Self::query) does, with `options` applied.
    ///
    /// The reranker and the similarity threshold apply as they do to [`query`](Self::query).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_rag::QueryOptions;
    ///
    /// let options = QueryOptions::new().with_top_k(3);
    /// let results = pipeline.scoped(scope).query_with("docs", "refund window", &options).await?;
    /// assert!(results.len() <= 3);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RagError::ConfigError`] if `options.top_k` is 0,
    /// [`RagError::UnsupportedFilter`] before any I/O if `options.filter` is malformed or
    /// names a metadata key the collection did not declare, and [`RagError::PipelineError`]
    /// if embedding, search, or reranking fails.
    pub async fn query_with(
        &self,
        collection: &str,
        query: &str,
        options: &QueryOptions,
    ) -> Result<Vec<SearchResult>> {
        let pipeline = self.pipeline;
        let top_k = options.top_k.unwrap_or(pipeline.config.top_k);
        if top_k == 0 {
            return Err(RagError::ConfigError("top_k must be greater than 0".to_string()));
        }
        if let Some(filter) = &options.filter {
            pipeline.check_filter(collection, filter).await?;
        }

        // 1. Embed the query
        let query_embedding = pipeline.embedding_provider.embed(query).await.map_err(|e| {
            error!(error = %e, "embedding failed during query");
            RagError::PipelineError(format!("query embedding failed: {e}"))
        })?;

        // 2. Search the scope
        let mut vector_query = VectorQuery::new(&query_embedding, top_k);
        if let Some(filter) = &options.filter {
            vector_query = vector_query.with_filter(filter);
        }
        let results = pipeline
            .vector_store
            .search(collection, &self.scope, &vector_query)
            .await
            .map_err(|e| {
                error!(collection, error = %e, "vector store search failed");
                RagError::PipelineError(format!("search failed in collection '{collection}': {e}"))
            })?;

        // 3. Rerank if a reranker is configured
        let results = if let Some(reranker) = &pipeline.reranker {
            reranker.rerank(query, results).await.map_err(|e| {
                error!(error = %e, "reranking failed");
                RagError::PipelineError(format!("reranking failed: {e}"))
            })?
        } else {
            results
        };

        // 4. Filter by similarity threshold
        let threshold = pipeline.config.similarity_threshold;
        let filtered: Vec<SearchResult> =
            results.into_iter().filter(|r| r.score >= threshold).collect();

        info!(result_count = filtered.len(), scope = self.scope.as_str(), "query completed");

        Ok(filtered)
    }

    /// The number of chunks the scope holds in `collection`.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if the vector store operation fails.
    pub async fn count(&self, collection: &str) -> Result<u64> {
        self.pipeline.vector_store.count(collection, &self.scope, None).await.map_err(|e| {
            error!(collection, error = %e, "count failed");
            RagError::PipelineError(format!("count failed in collection '{collection}': {e}"))
        })
    }

    /// The number of the scope's chunks in `collection` that `filter` matches.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::UnsupportedFilter`] before any I/O for a malformed filter or an
    /// undeclared metadata key, and [`RagError::PipelineError`] if the vector store fails.
    pub async fn count_where(&self, collection: &str, filter: &Filter) -> Result<u64> {
        self.pipeline.check_filter(collection, filter).await?;
        self.pipeline.vector_store.count(collection, &self.scope, Some(filter)).await.map_err(|e| {
            error!(collection, error = %e, "count failed");
            RagError::PipelineError(format!("count failed in collection '{collection}': {e}"))
        })
    }

    /// Delete the scope's chunks in `collection` that `filter` matches, returning how many
    /// were deleted.
    ///
    /// Other scopes' chunks are untouched. [`purge_scope`](Self::purge_scope) deletes a
    /// whole scope; an empty `And`, `Or`, or `In` is refused rather than read as "all".
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_rag::{Field, Filter};
    ///
    /// let retired = Filter::eq(Field::metadata("status"), "retired");
    /// let deleted = pipeline.scoped(scope).delete_where("docs", &retired).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RagError::UnsupportedFilter`] before any I/O for a malformed filter or an
    /// undeclared metadata key, and [`RagError::PipelineError`] if the vector store fails.
    pub async fn delete_where(&self, collection: &str, filter: &Filter) -> Result<u64> {
        self.pipeline.check_filter(collection, filter).await?;
        let deleted = self
            .pipeline
            .vector_store
            .delete_where(collection, &self.scope, filter)
            .await
            .map_err(|e| {
                error!(collection, error = %e, "delete_where failed");
                RagError::PipelineError(format!(
                    "delete_where failed in collection '{collection}': {e}"
                ))
            })?;
        info!(collection, scope = self.scope.as_str(), deleted, "deleted matching chunks");
        Ok(deleted)
    }

    /// Delete every chunk of `document_id` in the scope, returning how many were deleted.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if the vector store fails.
    pub async fn delete_document(&self, collection: &str, document_id: &str) -> Result<u64> {
        self.delete_where(collection, &Filter::eq(Field::DocumentId, document_id)).await
    }

    /// Delete every chunk the scope holds in `collection`, returning how many it held.
    ///
    /// Other scopes' chunks in the collection are untouched.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if the vector store operation fails.
    pub async fn purge_scope(&self, collection: &str) -> Result<u64> {
        let purged =
            self.pipeline.vector_store.purge_scope(collection, &self.scope).await.map_err(|e| {
                error!(collection, error = %e, "purge failed");
                RagError::PipelineError(format!("purge failed in collection '{collection}': {e}"))
            })?;
        info!(collection, scope = self.scope.as_str(), purged, "purged scope");
        Ok(purged)
    }
}

/// Builder for constructing a [`RagPipeline`].
///
/// All fields except `reranker` are required. Call [`build()`](RagPipelineBuilder::build)
/// to validate and produce the pipeline.
///
/// # Example
///
/// ```rust,ignore
/// let pipeline = RagPipeline::builder()
///     .config(RagConfig::default())
///     .embedding_provider(Arc::new(embedder))
///     .vector_store(Arc::new(store))
///     .chunker(Arc::new(chunker))
///     .reranker(Arc::new(reranker))  // optional
///     .build()?;
/// ```
#[derive(Default)]
pub struct RagPipelineBuilder {
    config: Option<RagConfig>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    vector_store: Option<Arc<dyn VectorStore>>,
    chunker: Option<Arc<dyn Chunker>>,
    reranker: Option<Arc<dyn Reranker>>,
}

impl RagPipelineBuilder {
    /// Set the pipeline configuration.
    pub fn config(mut self, config: RagConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the embedding provider.
    pub fn embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Set the vector store backend.
    pub fn vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }

    /// Set the document chunker.
    pub fn chunker(mut self, chunker: Arc<dyn Chunker>) -> Self {
        self.chunker = Some(chunker);
        self
    }

    /// Set an optional reranker for post-search result reordering.
    pub fn reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Build the [`RagPipeline`], validating that all required fields are set.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::ConfigError`] if any required field is missing.
    pub fn build(self) -> Result<RagPipeline> {
        let config =
            self.config.ok_or_else(|| RagError::ConfigError("config is required".to_string()))?;
        let embedding_provider = self
            .embedding_provider
            .ok_or_else(|| RagError::ConfigError("embedding_provider is required".to_string()))?;
        let vector_store = self
            .vector_store
            .ok_or_else(|| RagError::ConfigError("vector_store is required".to_string()))?;
        let chunker =
            self.chunker.ok_or_else(|| RagError::ConfigError("chunker is required".to_string()))?;

        Ok(RagPipeline {
            config,
            embedding_provider,
            vector_store,
            chunker,
            reranker: self.reranker,
            filterable: RwLock::new(HashMap::new()),
        })
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::chunking::FixedSizeChunker;
    use crate::inmemory::InMemoryVectorStore;

    /// Returns one vector fewer than it was asked for.
    struct ShortEmbedder;

    #[async_trait]
    impl EmbeddingProvider for ShortEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0])
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0, 0.0]; texts.len().saturating_sub(1)])
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    /// Embeds a text as its byte length and 1.
    struct LengthEmbedder;

    #[async_trait]
    impl EmbeddingProvider for LengthEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            Ok(vec![text.len() as f32, 1.0])
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    fn pipeline(
        embedder: Arc<dyn EmbeddingProvider>,
        store: Arc<InMemoryVectorStore>,
    ) -> RagPipeline {
        RagPipeline::builder()
            .config(RagConfig::default())
            .embedding_provider(embedder)
            .vector_store(store)
            .chunker(Arc::new(FixedSizeChunker::try_new(4, 0).expect("valid sizes")))
            .build()
            .expect("build pipeline")
    }

    #[tokio::test]
    async fn ingest_fails_when_embedder_returns_fewer_vectors_than_chunks() {
        let store = Arc::new(InMemoryVectorStore::new());
        let pipeline = pipeline(Arc::new(ShortEmbedder), store.clone());
        pipeline.create_collection("docs").await.expect("create collection");

        let document = Document::new("doc", "abcdefgh");
        let scoped = pipeline.scoped(Scope::single_tenant());
        let err = scoped.ingest("docs", &document).await.expect_err("short embedding batch");
        assert!(
            matches!(&err, RagError::PipelineError(message)
                if message == "embedding provider returned 1 vectors for 2 chunks of document 'doc'"),
            "unexpected error: {err}"
        );

        let stored = store.count("docs", &Scope::single_tenant(), None).await.expect("count");
        assert_eq!(stored, 0, "no chunk may be stored without its embedding");
    }

    #[tokio::test]
    async fn query_with_rejects_a_zero_top_k() {
        let pipeline = pipeline(Arc::new(ShortEmbedder), Arc::new(InMemoryVectorStore::new()));
        pipeline.create_collection("docs").await.expect("create collection");

        let options = QueryOptions::new().with_top_k(0);
        let err = pipeline
            .scoped(Scope::single_tenant())
            .query_with("docs", "query", &options)
            .await
            .expect_err("zero top_k");
        assert!(matches!(err, RagError::ConfigError(_)), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn scopes_share_a_collection_without_sharing_chunks() {
        let pipeline = pipeline(Arc::new(LengthEmbedder), Arc::new(InMemoryVectorStore::new()));
        pipeline.create_collection("docs").await.expect("create collection");
        let tenant_a = pipeline.scoped(Scope::from_parts(["app", "a"]).expect("scope"));
        let tenant_b = pipeline.scoped(Scope::from_parts(["app", "b"]).expect("scope"));

        // The same document ID in two scopes keeps two sets of chunks.
        tenant_a.ingest("docs", &Document::new("policy", "aaaa")).await.expect("ingest a");
        tenant_b.ingest("docs", &Document::new("policy", "bbbbbbbb")).await.expect("ingest b");

        let texts = |results: Vec<SearchResult>| -> Vec<String> {
            results.into_iter().map(|r| r.chunk.text).collect()
        };
        assert_eq!(texts(tenant_a.query("docs", "q").await.expect("query a")), ["aaaa"]);
        assert_eq!(texts(tenant_b.query("docs", "q").await.expect("query b")), ["bbbb", "bbbb"]);
        assert_eq!(tenant_a.count("docs").await.expect("count a"), 1);
        assert_eq!(tenant_b.count("docs").await.expect("count b"), 2);

        assert_eq!(tenant_b.purge_scope("docs").await.expect("purge b"), 2);
        assert_eq!(tenant_b.count("docs").await.expect("count b"), 0);
        assert_eq!(tenant_a.count("docs").await.expect("count a"), 1, "a is untouched");
    }

    /// Counts embed calls, to show a refused filter costs no embedding.
    #[derive(Default)]
    struct CountingEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![text.len() as f32, 1.0])
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    #[tokio::test]
    async fn a_filter_on_an_undeclared_key_fails_before_any_io() {
        let embedder = Arc::new(CountingEmbedder::default());
        let pipeline = pipeline(embedder.clone(), Arc::new(InMemoryVectorStore::new()));
        pipeline.create_collection_with_filterable("docs", ["lang"]).await.expect("create");
        let rag = pipeline.scoped(Scope::single_tenant());

        let undeclared = Filter::eq(Field::metadata("tier"), "gold");
        let options = QueryOptions::new().with_filter(undeclared.clone());
        let err = rag.query_with("docs", "q", &options).await.expect_err("undeclared key");
        assert!(
            matches!(&err, RagError::UnsupportedFilter(message) if message.contains("`tier`")),
            "unexpected error: {err}"
        );
        assert!(matches!(
            rag.delete_where("docs", &undeclared).await,
            Err(RagError::UnsupportedFilter(_))
        ));
        assert!(matches!(
            rag.count_where("unknown_collection", &Filter::eq(Field::metadata("lang"), "en")).await,
            Err(RagError::UnsupportedFilter(_))
        ));
        assert_eq!(embedder.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let declared = QueryOptions::new().with_filter(Filter::eq(Field::metadata("lang"), "en"));
        rag.query_with("docs", "q", &declared).await.expect("a declared key is accepted");
        let built_in = QueryOptions::new().with_filter(Filter::eq(Field::DocumentId, "doc"));
        rag.query_with("docs", "q", &built_in).await.expect("built-in fields need no declaration");
    }

    #[tokio::test]
    async fn ingest_replaces_the_documents_chunks_in_its_scope() {
        let pipeline = pipeline(Arc::new(LengthEmbedder), Arc::new(InMemoryVectorStore::new()));
        pipeline.create_collection("docs").await.expect("create collection");
        let rag = pipeline.scoped(Scope::new("tenant").expect("scope"));
        let other = pipeline.scoped(Scope::new("other").expect("scope"));

        rag.ingest("docs", &Document::new("policy", "aaaabbbbcccc")).await.expect("ingest v1");
        other.ingest("docs", &Document::new("policy", "aaaabbbbcccc")).await.expect("ingest");
        rag.ingest("docs", &Document::new("other_doc", "zzzz")).await.expect("ingest");
        assert_eq!(rag.count("docs").await.expect("count"), 4);

        // A shorter version leaves no stale chunks.
        let chunks = rag.ingest("docs", &Document::new("policy", "dddd")).await.expect("ingest v2");
        assert_eq!(chunks.len(), 1);
        let policy = Filter::eq(Field::DocumentId, "policy");
        assert_eq!(rag.count_where("docs", &policy).await.expect("count"), 1);
        let results = rag
            .query_with("docs", "q", &QueryOptions::new().with_filter(policy.clone()))
            .await
            .expect("query");
        assert_eq!(results.iter().map(|r| r.chunk.text.as_str()).collect::<Vec<_>>(), ["dddd"]);

        // An empty version removes the document; another document and scope are untouched.
        rag.ingest("docs", &Document::new("policy", "")).await.expect("ingest empty");
        assert_eq!(rag.count_where("docs", &policy).await.expect("count"), 0);
        assert_eq!(rag.count("docs").await.expect("count"), 1);
        assert_eq!(other.count_where("docs", &policy).await.expect("count"), 3);

        assert_eq!(other.delete_document("docs", "policy").await.expect("delete"), 3);
        assert_eq!(other.count("docs").await.expect("count"), 0);
        assert_eq!(rag.count("docs").await.expect("count"), 1);
    }
}
