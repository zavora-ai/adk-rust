//! PostgreSQL memory service implementation with pgvector.
//!
//! Provides [`PostgresMemoryService`], a `MemoryService` backed by PostgreSQL
//! with pgvector cosine similarity search and tsvector keyword fallback.

use crate::embedding::EmbeddingProvider;
use crate::service::*;
use adk_core::{Part, Result};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use tracing::instrument;

/// pgvector index algorithm for the embedding column.
///
/// Defaults to [`Hnsw`](VectorIndexType::Hnsw), which has no dimension
/// limit and generally provides better recall than IVFFlat.
///
/// # Example
///
/// ```rust,ignore
/// use adk_memory::{PostgresMemoryService, VectorIndexType};
///
/// let service = PostgresMemoryService::builder("postgres://...", None)
///     .vector_index(VectorIndexType::IvfFlat { lists: 100 })
///     .build()
///     .await?;
/// ```
#[derive(Debug, Clone)]
pub enum VectorIndexType {
    /// HNSW (Hierarchical Navigable Small World) index.
    ///
    /// No dimension limit. Recommended for high-dimensional embeddings
    /// such as Gemini's 3072-dimensional vectors.
    Hnsw {
        /// Maximum number of connections per node (default: 16).
        m: u32,
        /// Size of the dynamic candidate list during construction (default: 64).
        ef_construction: u32,
    },
    /// IVFFlat (Inverted File with Flat compression) index.
    ///
    /// Limited to 2000 dimensions by pgvector. Use only when you need
    /// faster index build times on lower-dimensional embeddings.
    IvfFlat {
        /// Number of inverted lists (default: 100).
        lists: u32,
    },
    /// Skip vector index creation entirely.
    ///
    /// Useful when you want to manage indexes manually or when the
    /// table has too few rows for an index to help.
    None,
}

impl Default for VectorIndexType {
    fn default() -> Self {
        Self::Hnsw { m: 16, ef_construction: 64 }
    }
}

/// PostgreSQL-backed memory service with optional vector search.
///
/// When an [`EmbeddingProvider`] is supplied, entries are stored with
/// vector embeddings and searched via pgvector cosine similarity.
/// Without a provider, search falls back to PostgreSQL full-text
/// search (`tsvector`/`tsquery`).
///
/// # Example
///
/// ```rust,ignore
/// use adk_memory::PostgresMemoryService;
///
/// let service = PostgresMemoryService::new(
///     "postgres://user:pass@localhost/mydb",
///     None,
/// ).await?;
/// service.migrate().await?;
/// ```
pub struct PostgresMemoryService {
    pool: PgPool,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    vector_index: VectorIndexType,
}

/// Builder for [`PostgresMemoryService`] with configurable vector index.
///
/// # Example
///
/// ```rust,ignore
/// use adk_memory::{PostgresMemoryService, VectorIndexType};
///
/// // HNSW (default) — works with any dimension count
/// let service = PostgresMemoryService::builder("postgres://...", None)
///     .build()
///     .await?;
///
/// // IVFFlat — only for ≤2000 dimensions
/// let service = PostgresMemoryService::builder("postgres://...", None)
///     .vector_index(VectorIndexType::IvfFlat { lists: 100 })
///     .build()
///     .await?;
/// ```
pub struct PostgresMemoryServiceBuilder {
    database_url: String,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    vector_index: VectorIndexType,
}

impl PostgresMemoryServiceBuilder {
    /// Set the vector index algorithm used during migration.
    pub fn vector_index(mut self, index: VectorIndexType) -> Self {
        self.vector_index = index;
        self
    }

    /// Connect and build the service.
    pub async fn build(self) -> Result<PostgresMemoryService> {
        let pool = PgPool::connect(&self.database_url).await.map_err(|e| {
            adk_core::AdkError::Memory(format!("memory database connection failed: {e}"))
        })?;
        Ok(PostgresMemoryService {
            pool,
            embedding_provider: self.embedding_provider,
            vector_index: self.vector_index,
        })
    }
}

impl PostgresMemoryService {
    /// Connect to PostgreSQL for memory storage.
    ///
    /// Uses HNSW vector indexing by default. For IVFFlat or custom
    /// index settings, use [`builder`](Self::builder) instead.
    pub async fn new(
        database_url: &str,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<Self> {
        let pool = PgPool::connect(database_url).await.map_err(|e| {
            adk_core::AdkError::Memory(format!("memory database connection failed: {e}"))
        })?;
        Ok(Self { pool, embedding_provider, vector_index: VectorIndexType::default() })
    }

    /// Create a builder for fine-grained configuration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let service = PostgresMemoryService::builder("postgres://...", Some(provider))
    ///     .vector_index(VectorIndexType::Hnsw { m: 32, ef_construction: 128 })
    ///     .build()
    ///     .await?;
    /// ```
    pub fn builder(
        database_url: impl Into<String>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> PostgresMemoryServiceBuilder {
        PostgresMemoryServiceBuilder {
            database_url: database_url.into(),
            embedding_provider,
            vector_index: VectorIndexType::default(),
        }
    }

    /// Create a memory service from an existing connection pool.
    ///
    /// Uses HNSW vector indexing by default. To configure the index
    /// type on an existing pool, use [`from_pool_with_index`](Self::from_pool_with_index).
    pub fn from_pool(pool: PgPool, embedding_provider: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        Self { pool, embedding_provider, vector_index: VectorIndexType::default() }
    }

    /// Create a memory service from an existing pool with a specific index type.
    pub fn from_pool_with_index(
        pool: PgPool,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
        vector_index: VectorIndexType,
    ) -> Self {
        Self { pool, embedding_provider, vector_index }
    }

    /// Create the pgvector extension, `memory_entries` table, and indexes.
    ///
    /// The vector column dimension is set to the embedding provider's
    /// `dimensions()` value. If no provider is configured, the column
    /// is created as `vector(1536)` (a common default) but will remain
    /// NULL for all rows.
    ///
    /// The vector index type defaults to HNSW (no dimension limit).
    /// Use [`builder`](Self::builder) to select IVFFlat or skip indexing.
    pub async fn migrate(&self) -> Result<()> {
        let dims = self.embedding_provider.as_ref().map(|p| p.dimensions()).unwrap_or(1536);

        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector").execute(&self.pool).await.map_err(
            |e| adk_core::AdkError::Memory(format!("pgvector extension creation failed: {e}")),
        )?;

        let create_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entries (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                app_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                content JSONB NOT NULL,
                author TEXT NOT NULL,
                timestamp TIMESTAMPTZ NOT NULL,
                embedding vector({dims}),
                search_text TSVECTOR
            )
            "#
        );

        sqlx::query(&create_table)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("migration failed: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_app_user \
             ON memory_entries(app_name, user_id)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("index creation failed: {e}")))?;

        match &self.vector_index {
            VectorIndexType::Hnsw { m, ef_construction } => {
                let idx = format!(
                    "CREATE INDEX IF NOT EXISTS idx_memory_embedding \
                     ON memory_entries USING hnsw (embedding vector_cosine_ops) \
                     WITH (m = {m}, ef_construction = {ef_construction})"
                );
                sqlx::query(&idx).execute(&self.pool).await.map_err(|e| {
                    adk_core::AdkError::Memory(format!("HNSW index creation failed: {e}"))
                })?;
            }
            VectorIndexType::IvfFlat { lists } => {
                let idx = format!(
                    "CREATE INDEX IF NOT EXISTS idx_memory_embedding \
                     ON memory_entries USING ivfflat (embedding vector_cosine_ops) \
                     WITH (lists = {lists})"
                );
                sqlx::query(&idx).execute(&self.pool).await.map_err(|e| {
                    adk_core::AdkError::Memory(format!("IVFFlat index creation failed: {e}"))
                })?;
            }
            VectorIndexType::None => {}
        }

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_search_text \
             ON memory_entries USING gin(search_text)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("index creation failed: {e}")))?;

        Ok(())
    }

    /// Extract plain text from a `Content` value for full-text search indexing.
    fn extract_text(content: &adk_core::Content) -> String {
        content
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[async_trait]
impl MemoryService for PostgresMemoryService {
    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, entry_count = entries.len()))]
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Collect texts for batch embedding
        let texts: Vec<String> = entries.iter().map(|e| Self::extract_text(&e.content)).collect();

        let embeddings = if let Some(provider) = &self.embedding_provider {
            let non_empty_texts: Vec<String> = texts
                .iter()
                .map(|t| if t.is_empty() { " ".to_string() } else { t.clone() })
                .collect();
            Some(provider.embed(&non_empty_texts).await.map_err(|e| {
                adk_core::AdkError::Memory(format!("embedding generation failed: {e}"))
            })?)
        } else {
            None
        };

        for (i, entry) in entries.iter().enumerate() {
            let content_json = serde_json::to_value(&entry.content)
                .map_err(|e| adk_core::AdkError::Memory(format!("serialization failed: {e}")))?;
            let text = &texts[i];

            if let Some(ref embs) = embeddings {
                let embedding = pgvector::Vector::from(embs[i].clone());
                sqlx::query(
                    r#"
                    INSERT INTO memory_entries
                        (app_name, user_id, session_id, content, author, timestamp, embedding, search_text)
                    VALUES
                        ($1, $2, $3, $4, $5, $6, $7, to_tsvector('english', $8))
                    "#,
                )
                .bind(app_name)
                .bind(user_id)
                .bind(session_id)
                .bind(&content_json)
                .bind(&entry.author)
                .bind(entry.timestamp)
                .bind(embedding)
                .bind(text)
                .execute(&self.pool)
                .await
                .map_err(|e| {
                    adk_core::AdkError::Memory(format!("insert failed: {e}"))
                })?;
            } else {
                sqlx::query(
                    r#"
                    INSERT INTO memory_entries
                        (app_name, user_id, session_id, content, author, timestamp, search_text)
                    VALUES
                        ($1, $2, $3, $4, $5, $6, to_tsvector('english', $7))
                    "#,
                )
                .bind(app_name)
                .bind(user_id)
                .bind(session_id)
                .bind(&content_json)
                .bind(&entry.author)
                .bind(entry.timestamp)
                .bind(text)
                .execute(&self.pool)
                .await
                .map_err(|e| adk_core::AdkError::Memory(format!("insert failed: {e}")))?;
            }
        }

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10) as i64;

        let rows = if let Some(ref provider) = self.embedding_provider {
            // Vector cosine similarity search
            let query_embedding = provider
                .embed(std::slice::from_ref(&req.query))
                .await
                .map_err(|e| adk_core::AdkError::Memory(format!("query embedding failed: {e}")))?;
            let query_vec =
                pgvector::Vector::from(query_embedding.into_iter().next().ok_or_else(|| {
                    adk_core::AdkError::Memory(
                        "embedding provider returned empty result".to_string(),
                    )
                })?);

            sqlx::query(
                r#"
                SELECT content, author, timestamp, (embedding <=> $3) AS distance
                FROM memory_entries
                WHERE app_name = $1 AND user_id = $2 AND embedding IS NOT NULL
                ORDER BY embedding <=> $3
                LIMIT $4
                "#,
            )
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(query_vec)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("search failed: {e}")))?
        } else {
            // Full-text search fallback
            sqlx::query(
                r#"
                SELECT content, author, timestamp, ts_rank(search_text, plainto_tsquery('english', $3)) AS rank_score
                FROM memory_entries
                WHERE app_name = $1 AND user_id = $2
                  AND search_text @@ plainto_tsquery('english', $3)
                ORDER BY rank_score DESC
                LIMIT $4
                "#,
            )
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&req.query)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("search failed: {e}")))?
        };

        let min_score = req.min_score;
        let memories =
            rows.iter()
                .filter(|row| {
                    if let Some(threshold) = min_score {
                        // For vector search, distance is lower = better; convert to similarity
                        // For text search, rank_score is higher = better
                        let score: f32 = row
                            .try_get::<f32, _>("distance")
                            .map(|d| 1.0 - d)
                            .or_else(|_| row.try_get::<f32, _>("rank_score"))
                            .unwrap_or(0.0);
                        score >= threshold
                    } else {
                        true
                    }
                })
                .map(|row| {
                    let content_json: serde_json::Value = row.get("content");
                    let content: adk_core::Content =
                        serde_json::from_value(content_json).unwrap_or_else(|_| {
                            adk_core::Content { role: "user".to_string(), parts: vec![] }
                        });
                    let author: String = row.get("author");
                    let timestamp: chrono::DateTime<chrono::Utc> = row.get("timestamp");
                    MemoryEntry { content, author, timestamp }
                })
                .collect();

        Ok(SearchResponse { memories })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM memory_entries WHERE app_name = $1 AND user_id = $2")
            .bind(app_name)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("delete_user failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id))]
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM memory_entries WHERE app_name = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("delete_session failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}
