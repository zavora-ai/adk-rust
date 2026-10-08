//! RAG pipeline orchestrator.
//!
//! The [`RagPipeline`] coordinates the full ingest-and-query workflow by
//! composing an [`EmbeddingProvider`], a [`VectorStore`], a [`Chunker`],
//! and an optional [`Reranker`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::{RagPipeline, RagConfig, InMemoryVectorStore, FixedSizeChunker};
//!
//! let pipeline = RagPipeline::builder()
//!     .config(RagConfig::default())
//!     .embedding_provider(Arc::new(my_embedder))
//!     .vector_store(Arc::new(InMemoryVectorStore::new()))
//!     .chunker(Arc::new(FixedSizeChunker::new(512, 100)))
//!     .build()?;
//!
//! pipeline.create_collection("docs").await?;
//! pipeline.ingest("docs", &document).await?;
//! let results = pipeline.query("docs", "search query").await?;
//! ```

use std::sync::Arc;

use tracing::{error, info};

use crate::chunking::Chunker;
use crate::config::RagConfig;
use crate::document::{Chunk, Document, SearchResult};
use crate::embedding::EmbeddingProvider;
use crate::error::{RagError, Result};
use crate::reranker::Reranker;
use crate::vectorstore::VectorStore;

/// The RAG pipeline orchestrator.
///
/// Coordinates document ingestion (chunk → embed → store) and query
/// execution (embed → search → rerank → filter). Construct one via
/// [`RagPipeline::builder()`].
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
        let dimensions = self.embedding_provider.dimensions();
        self.vector_store.create_collection(name, dimensions).await.map_err(|e| {
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

    /// Ingest a single document: chunk → embed → store.
    ///
    /// Returns the chunks that were stored (with embeddings attached).
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if embedding or storage fails, or if
    /// the embedding provider returns a different number of vectors than there
    /// are chunks, including the document ID in the error message.
    pub async fn ingest(&self, collection: &str, document: &Document) -> Result<Vec<Chunk>> {
        // 1. Chunk the document
        let mut chunks = self.chunker.chunk(document);
        if chunks.is_empty() {
            info!(document.id = %document.id, chunk_count = 0, "ingested document (empty)");
            return Ok(chunks);
        }

        // 2. Collect chunk texts for batch embedding
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

        // 3. Generate embeddings
        let embeddings = self.embedding_provider.embed_batch(&texts).await.map_err(|e| {
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

        // 5. Upsert into vector store
        self.vector_store.upsert(collection, &chunks).await.map_err(|e| {
            error!(document.id = %document.id, error = %e, "upsert failed during ingestion");
            RagError::PipelineError(format!("upsert failed for document '{}': {e}", document.id))
        })?;

        let chunk_count = chunks.len();
        info!(document.id = %document.id, chunk_count, "ingested document");

        Ok(chunks)
    }

    /// Ingest multiple documents through the chunk → embed → store workflow.
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

    /// Query the pipeline: embed → search → rerank → filter by threshold.
    ///
    /// Returns search results ordered by descending relevance score. Results
    /// below the configured `similarity_threshold` are filtered out.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::PipelineError`] if embedding or search fails.
    pub async fn query(&self, collection: &str, query: &str) -> Result<Vec<SearchResult>> {
        // 1. Embed the query
        let query_embedding = self.embedding_provider.embed(query).await.map_err(|e| {
            error!(error = %e, "embedding failed during query");
            RagError::PipelineError(format!("query embedding failed: {e}"))
        })?;

        // 2. Search the vector store
        let results = self
            .vector_store
            .search(collection, &query_embedding, self.config.top_k)
            .await
            .map_err(|e| {
                error!(collection, error = %e, "vector store search failed");
                RagError::PipelineError(format!("search failed in collection '{collection}': {e}"))
            })?;

        // 3. Rerank if a reranker is configured
        let results = if let Some(reranker) = &self.reranker {
            reranker.rerank(query, results).await.map_err(|e| {
                error!(error = %e, "reranking failed");
                RagError::PipelineError(format!("reranking failed: {e}"))
            })?
        } else {
            results
        };

        // 4. Filter by similarity threshold
        let threshold = self.config.similarity_threshold;
        let filtered: Vec<SearchResult> =
            results.into_iter().filter(|r| r.score >= threshold).collect();

        info!(result_count = filtered.len(), "query completed");

        Ok(filtered)
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
    use std::collections::HashMap;

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

    #[tokio::test]
    async fn ingest_fails_when_embedder_returns_fewer_vectors_than_chunks() {
        let store = Arc::new(InMemoryVectorStore::new());
        let pipeline = RagPipeline::builder()
            .config(RagConfig::default())
            .embedding_provider(Arc::new(ShortEmbedder))
            .vector_store(store.clone())
            .chunker(Arc::new(FixedSizeChunker::try_new(4, 0).expect("valid sizes")))
            .build()
            .expect("build pipeline");
        pipeline.create_collection("docs").await.expect("create collection");

        let document = Document {
            id: "doc".to_string(),
            text: "abcdefgh".to_string(),
            metadata: HashMap::new(),
            source_uri: None,
        };
        let err = pipeline.ingest("docs", &document).await.expect_err("short embedding batch");
        assert!(
            matches!(&err, RagError::PipelineError(message)
                if message == "embedding provider returned 1 vectors for 2 chunks of document 'doc'"),
            "unexpected error: {err}"
        );

        let stored = store.search("docs", &[1.0, 0.0], 10).await.expect("search");
        assert!(stored.is_empty(), "no chunk may be stored without its embedding: {stored:?}");
    }
}
