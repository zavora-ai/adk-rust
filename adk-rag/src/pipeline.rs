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

use std::sync::Arc;

use tracing::{error, info};

use crate::chunking::Chunker;
use crate::config::RagConfig;
use crate::document::{Chunk, Document, SearchResult};
use crate::embedding::EmbeddingProvider;
use crate::error::{RagError, Result};
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
        let spec = CollectionSpec::new(self.embedding_provider.dimensions());
        self.vector_store.create_collection(name, &spec).await.map_err(|e| {
            error!(collection = name, error = %e, "failed to create collection");
            RagError::PipelineError(format!("failed to create collection '{name}': {e}"))
        })
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
        })
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
/// let options = QueryOptions::new().with_top_k(3);
/// assert_eq!(options.top_k, Some(3));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct QueryOptions {
    /// The most results to retrieve; the pipeline's configured `top_k` when `None`.
    pub top_k: Option<usize>,
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

    /// Ingest a single document into the scope: chunk → embed → store.
    ///
    /// Returns the chunks that were stored (with embeddings attached).
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
            info!(document.id = %document.id, scope, chunk_count = 0, "ingested document (empty)");
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

        let chunk_count = chunks.len();
        info!(document.id = %document.id, scope, chunk_count, "ingested document");

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
    /// Returns [`RagError::ConfigError`] if `options.top_k` is 0, and
    /// [`RagError::PipelineError`] if embedding, search, or reranking fails.
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

        // 1. Embed the query
        let query_embedding = pipeline.embedding_provider.embed(query).await.map_err(|e| {
            error!(error = %e, "embedding failed during query");
            RagError::PipelineError(format!("query embedding failed: {e}"))
        })?;

        // 2. Search the scope
        let vector_query = VectorQuery::new(&query_embedding, top_k);
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
        self.pipeline.vector_store.count(collection, &self.scope).await.map_err(|e| {
            error!(collection, error = %e, "count failed");
            RagError::PipelineError(format!("count failed in collection '{collection}': {e}"))
        })
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

        let stored = store.count("docs", &Scope::single_tenant()).await.expect("count");
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
}
