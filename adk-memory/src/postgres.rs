//! PostgreSQL memory service implementation with pgvector.
//!
//! Provides [`PostgresMemoryService`], a `MemoryService` backed by PostgreSQL
//! with pgvector cosine similarity search and tsvector keyword fallback.
//!
//! ## High-dimensional embeddings (>2000 dims)
//!
//! pgvector limits both HNSW and IVFFlat indexes to 2000 dimensions on
//! the `vector` type. When the embedding provider reports more than 2000
//! dimensions (e.g. Gemini `embedding-001` at 3072), the service
//! automatically uses `halfvec` expression indexing — storing full-precision
//! `vector(N)` data but indexing and querying via `halfvec(N)` casts.
//! `halfvec` supports HNSW up to 4000 dimensions with negligible recall
//! loss and 50% storage savings on the index.

use crate::embedding::EmbeddingProvider;
use crate::migration::pg_runner;
use crate::service::*;
use adk_core::Result;
use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Duration;
use tracing::instrument;

/// Maximum dimensions for a direct `vector` index in pgvector.
/// Beyond this, we use `halfvec` expression indexing.
const PGVECTOR_MAX_DIRECT_INDEX_DIMS: usize = 2000;

/// Maximum dimensions for a `halfvec` index in pgvector.
const PGVECTOR_MAX_HALFVEC_INDEX_DIMS: usize = 4000;

/// pgvector index algorithm for the embedding column.
///
/// Defaults to [`Hnsw`](VectorIndexType::Hnsw). When the embedding
/// dimension exceeds 2000, the index is automatically built using
/// `halfvec` expression indexing (supports up to 4000 dimensions).
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
    /// Supports up to 2000 dimensions directly, or up to 4000 dimensions
    /// via automatic `halfvec` expression indexing. Recommended default.
    Hnsw {
        /// Maximum number of connections per node (default: 16).
        m: u32,
        /// Size of the dynamic candidate list during construction (default: 64).
        ef_construction: u32,
    },
    /// IVFFlat (Inverted File with Flat compression) index.
    ///
    /// Supports up to 2000 dimensions directly, or up to 4000 dimensions
    /// via automatic `halfvec` expression indexing. Faster index builds
    /// than HNSW but lower recall.
    IvfFlat {
        /// Number of inverted lists (default: 100).
        lists: u32,
    },
    /// Skip vector index creation entirely.
    ///
    /// Queries use exact sequential scan. Fine for small datasets
    /// (<100k rows) or when you manage indexes manually.
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
/// For embeddings with more than 2000 dimensions, the service
/// automatically uses `halfvec` expression indexing and query casts.
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
    /// True when dims > 2000 and we use halfvec expression indexing.
    use_halfvec: bool,
    default_ttl: Option<Duration>,
}

/// Builder for [`PostgresMemoryService`] with configurable vector index.
///
/// # Example
///
/// ```rust,ignore
/// use adk_memory::{PostgresMemoryService, VectorIndexType};
///
/// // HNSW (default) — auto-uses halfvec for >2000 dims
/// let service = PostgresMemoryService::builder("postgres://...", None)
///     .build()
///     .await?;
///
/// // IVFFlat with custom lists
/// let service = PostgresMemoryService::builder("postgres://...", None)
///     .vector_index(VectorIndexType::IvfFlat { lists: 200 })
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
            adk_core::AdkError::memory(format!("memory database connection failed: {e}"))
        })?;
        let use_halfvec = needs_halfvec(&self.embedding_provider);
        Ok(PostgresMemoryService {
            pool,
            embedding_provider: self.embedding_provider,
            vector_index: self.vector_index,
            use_halfvec,
            default_ttl: None,
        })
    }
}

/// Returns true when the provider's dimensions exceed the direct index limit.
fn needs_halfvec(provider: &Option<Arc<dyn EmbeddingProvider>>) -> bool {
    provider.as_ref().is_some_and(|p| p.dimensions() > PGVECTOR_MAX_DIRECT_INDEX_DIMS)
}

impl PostgresMemoryService {
    /// Connect to PostgreSQL for memory storage.
    ///
    /// Uses HNSW vector indexing by default. Automatically switches to
    /// `halfvec` expression indexing when the embedding provider reports
    /// more than 2000 dimensions.
    pub async fn new(
        database_url: &str,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<Self> {
        let pool = PgPool::connect(database_url).await.map_err(|e| {
            adk_core::AdkError::memory(format!("memory database connection failed: {e}"))
        })?;
        let use_halfvec = needs_halfvec(&embedding_provider);
        Ok(Self {
            pool,
            embedding_provider,
            vector_index: VectorIndexType::default(),
            use_halfvec,
            default_ttl: None,
        })
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
    pub fn from_pool(pool: PgPool, embedding_provider: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        let use_halfvec = needs_halfvec(&embedding_provider);
        Self {
            pool,
            embedding_provider,
            vector_index: VectorIndexType::default(),
            use_halfvec,
            default_ttl: None,
        }
    }

    /// Create a memory service from an existing pool with a specific index type.
    pub fn from_pool_with_index(
        pool: PgPool,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
        vector_index: VectorIndexType,
    ) -> Self {
        let use_halfvec = needs_halfvec(&embedding_provider);
        Self { pool, embedding_provider, vector_index, use_halfvec, default_ttl: None }
    }

    /// Expires each entry written without an expiry `ttl` after it is written.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_memory::PostgresMemoryService;
    /// use std::time::Duration;
    ///
    /// let service = PostgresMemoryService::new("postgres://...", None)
    ///     .await?
    ///     .with_default_ttl(Duration::from_secs(30 * 24 * 3600));
    /// ```
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Embeds `entries` in one batch when a provider is set, then inserts them in one
    /// transaction, each stamped with its id and expiry.
    async fn insert(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: Option<&str>,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let now = Utc::now();
        let texts: Vec<String> =
            entries.iter().map(|e| crate::text::extract_text(&e.content)).collect();
        let mut embeddings = match &self.embedding_provider {
            Some(provider) => {
                let non_empty: Vec<String> = texts
                    .iter()
                    .map(|t| if t.is_empty() { " ".to_string() } else { t.clone() })
                    .collect();
                let embeddings = provider.embed(&non_empty).await.map_err(|e| {
                    adk_core::AdkError::memory(format!("embedding generation failed: {e}"))
                })?;
                if embeddings.len() != entries.len() {
                    return Err(adk_core::AdkError::memory(format!(
                        "embedding provider returned {} embeddings for {} entries",
                        embeddings.len(),
                        entries.len()
                    )));
                }
                embeddings.into_iter().map(|e| Some(pgvector::Vector::from(e))).collect()
            }
            None => vec![None; entries.len()],
        }
        .into_iter();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;
        for (entry, text) in entries.into_iter().zip(&texts) {
            let entry = crate::expiry::stamp(entry, self.default_ttl, now);
            let content_json = serde_json::to_value(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            sqlx::query(
                r#"
                INSERT INTO memory_entries
                    (app_name, user_id, session_id, content, author, timestamp, embedding,
                     search_text, project_id, memory_id, expires_at)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7, to_tsvector('simple', $8), $9, $10, $11)
                "#,
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .bind(&content_json)
            .bind(&entry.author)
            .bind(entry.timestamp)
            .bind(embeddings.next().flatten())
            .bind(text)
            .bind(project_id)
            .bind(&entry.id)
            .bind(entry.expires_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;
        }
        tx.commit().await.map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))
    }

    /// The registry table used to track applied migration versions.
    const REGISTRY_TABLE: &'static str = "_adk_memory_migrations";

    /// V2 migration: add project_id column and composite index.
    const V2_MIGRATION_SQL: &'static str = "\
        ALTER TABLE memory_entries ADD COLUMN IF NOT EXISTS project_id TEXT;\
        CREATE INDEX IF NOT EXISTS idx_memory_app_user_project \
            ON memory_entries(app_name, user_id, project_id);";

    /// V3 migration: memory ids and expiry, with ids backfilled from the row id.
    const V3_MIGRATION_SQL: &'static str = "\
        ALTER TABLE memory_entries ADD COLUMN IF NOT EXISTS memory_id TEXT;\
        ALTER TABLE memory_entries ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;\
        UPDATE memory_entries SET memory_id = id::text WHERE memory_id IS NULL;\
        CREATE INDEX IF NOT EXISTS idx_memory_app_user_memory_id \
            ON memory_entries(app_name, user_id, memory_id);\
        CREATE INDEX IF NOT EXISTS idx_memory_expires_at \
            ON memory_entries(expires_at) WHERE expires_at IS NOT NULL;";

    /// Advisory lock key that [`migrate`](Self::migrate) holds while it runs.
    ///
    /// Concurrent instances take this `pg_advisory_lock` key so only one migrates at a
    /// time. The value is an FNV-1a hash of the registry table name, and `pg_locks` shows
    /// it with the high 32 bits in `classid` and the low 32 bits in `objid`.
    pub const ADVISORY_LOCK_KEY: i64 = {
        let bytes = Self::REGISTRY_TABLE.as_bytes();
        let mut hash: u64 = 0xcbf29ce484222325;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += 1;
        }
        hash as i64
    };

    /// Build the v1 migration SQL dynamically based on embedding dimensions
    /// and vector index configuration.
    ///
    /// The SQL creates the pgvector extension, `memory_entries` table with
    /// vector and tsvector columns, and all required indexes.
    fn build_v1_migration_sql(&self) -> Result<String> {
        let dims = self.embedding_provider.as_ref().map(|p| p.dimensions()).unwrap_or(1536);

        if self.use_halfvec && dims > PGVECTOR_MAX_HALFVEC_INDEX_DIMS {
            return Err(adk_core::AdkError::memory(format!(
                "embedding dimension {dims} exceeds pgvector halfvec index limit of \
                 {PGVECTOR_MAX_HALFVEC_INDEX_DIMS}. Reduce dimensions in your embedding provider \
                 or use VectorIndexType::None for exact search."
            )));
        }

        let mut sql = String::new();

        // pgvector extension
        sql.push_str("CREATE EXTENSION IF NOT EXISTS vector;\n");

        // Main table
        sql.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS memory_entries (\
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                app_name TEXT NOT NULL, \
                user_id TEXT NOT NULL, \
                session_id TEXT NOT NULL, \
                content JSONB NOT NULL, \
                author TEXT NOT NULL, \
                timestamp TIMESTAMPTZ NOT NULL, \
                embedding vector({dims}), \
                search_text TSVECTOR\
            );\n"
        ));

        // Composite index on app_name + user_id
        sql.push_str(
            "CREATE INDEX IF NOT EXISTS idx_memory_app_user \
             ON memory_entries(app_name, user_id);\n",
        );

        // Vector index (depends on index type and halfvec)
        match &self.vector_index {
            VectorIndexType::Hnsw { m, ef_construction } => {
                if self.use_halfvec {
                    sql.push_str(&format!(
                        "CREATE INDEX IF NOT EXISTS idx_memory_embedding \
                         ON memory_entries USING hnsw ((embedding::halfvec({dims})) halfvec_cosine_ops) \
                         WITH (m = {m}, ef_construction = {ef_construction});\n"
                    ));
                } else {
                    sql.push_str(&format!(
                        "CREATE INDEX IF NOT EXISTS idx_memory_embedding \
                         ON memory_entries USING hnsw (embedding vector_cosine_ops) \
                         WITH (m = {m}, ef_construction = {ef_construction});\n"
                    ));
                }
            }
            VectorIndexType::IvfFlat { lists } => {
                if self.use_halfvec {
                    sql.push_str(&format!(
                        "CREATE INDEX IF NOT EXISTS idx_memory_embedding \
                         ON memory_entries USING ivfflat ((embedding::halfvec({dims})) halfvec_cosine_ops) \
                         WITH (lists = {lists});\n"
                    ));
                } else {
                    sql.push_str(&format!(
                        "CREATE INDEX IF NOT EXISTS idx_memory_embedding \
                         ON memory_entries USING ivfflat (embedding vector_cosine_ops) \
                         WITH (lists = {lists});\n"
                    ));
                }
            }
            VectorIndexType::None => {}
        }

        // GIN index on tsvector for full-text search
        sql.push_str(
            "CREATE INDEX IF NOT EXISTS idx_memory_search_text \
             ON memory_entries USING gin(search_text);\n",
        );

        Ok(sql)
    }

    /// Create the pgvector extension, `memory_entries` table, and indexes.
    ///
    /// The vector column uses the embedding provider's `dimensions()` value.
    /// If no provider is configured, defaults to `vector(1536)`.
    ///
    /// When dimensions exceed 2000, the index is built using `halfvec`
    /// expression indexing (`(embedding::halfvec(N))`) which supports
    /// up to 4000 dimensions.
    ///
    /// Migrations are protected by a PostgreSQL advisory lock to prevent
    /// concurrent migration races from multiple application instances. The
    /// lock, every migration statement, and the unlock run on one connection,
    /// which is closed afterwards instead of returning to the pool.
    pub async fn migrate(&self) -> Result<()> {
        // Build the v1 SQL dynamically (parameterized by dims + index type)
        let v1_sql = self.build_v1_migration_sql()?;
        let v2_sql = Self::V2_MIGRATION_SQL;

        let steps: &[(i64, &str, &str)] = &[
            (1, "create memory_entries table with vector and tsvector columns", &v1_sql),
            (2, "add project_id column and composite index", v2_sql),
            (3, "add memory_id and expires_at columns", Self::V3_MIGRATION_SQL),
        ];

        let mut conn =
            self.pool.acquire().await.map_err(|e| {
                adk_core::AdkError::memory(format!("database connection failed: {e}"))
            })?;
        // A session-level advisory lock belongs to this connection. Closing it on
        // drop releases the lock even when this future is cancelled, rather than
        // leaving it held by an idle pooled connection.
        conn.close_on_drop();

        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(Self::ADVISORY_LOCK_KEY)
            .execute(&mut *conn)
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!("advisory lock acquisition failed: {e}"))
            })?;

        let result = pg_runner::run_sql_migrations_on_connection(
            &mut conn,
            Self::REGISTRY_TABLE,
            steps,
            |conn| {
                Box::pin(async move {
                    let row = sqlx::query(
                        "SELECT EXISTS(\
                             SELECT 1 FROM information_schema.tables \
                             WHERE table_schema = current_schema() \
                               AND table_name = 'memory_entries'\
                         ) AS exists_flag",
                    )
                    .fetch_one(conn)
                    .await
                    .map_err(|e| {
                        adk_core::AdkError::memory(format!("baseline detection failed: {e}"))
                    })?;
                    let exists: bool = row.try_get("exists_flag").unwrap_or(false);
                    Ok(exists)
                })
            },
        )
        .await;

        // Released explicitly so the next instance proceeds without waiting for
        // the connection to close; closing the connection releases it regardless.
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(Self::ADVISORY_LOCK_KEY)
            .execute(&mut *conn)
            .await;

        result
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot be read for any reason other
    /// than the table being absent, such as a lost connection or a missing
    /// privilege.
    pub async fn schema_version(&self) -> Result<i64> {
        pg_runner::sql_schema_version(&self.pool, Self::REGISTRY_TABLE).await
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
        self.insert(app_name, user_id, session_id, None, entries).await
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10) as i64;
        // Mirrors `expiry::is_expired`: an entry is live while `expires_at > now`.
        let now = Utc::now();

        let rows = if let Some(ref provider) = self.embedding_provider {
            // Vector cosine similarity search
            let query_embedding = provider
                .embed(std::slice::from_ref(&req.query))
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("query embedding failed: {e}")))?;
            let query_vec =
                pgvector::Vector::from(query_embedding.into_iter().next().ok_or_else(|| {
                    adk_core::AdkError::memory(
                        "embedding provider returned empty result".to_string(),
                    )
                })?);

            if self.use_halfvec {
                // Cast both sides to halfvec so the expression index is used
                let dims = provider.dimensions();
                match &req.project_id {
                    None => {
                        let sql = format!(
                            r#"
                            SELECT memory_id, content, author, timestamp, expires_at,
                                   (embedding::halfvec({dims}) <=> $3::halfvec({dims})) AS distance
                            FROM memory_entries
                            WHERE app_name = $1 AND user_id = $2 AND embedding IS NOT NULL
                              AND project_id IS NULL
                              AND (expires_at IS NULL OR expires_at > $5)
                            ORDER BY embedding::halfvec({dims}) <=> $3::halfvec({dims})
                            LIMIT $4
                            "#
                        );
                        // Only the validated numeric embedding dimension changes SQL structure.
                        sqlx::query(sqlx::AssertSqlSafe(sql))
                            .bind(&req.app_name)
                            .bind(&req.user_id)
                            .bind(&query_vec)
                            .bind(limit)
                            .bind(now)
                            .fetch_all(&self.pool)
                            .await
                            .map_err(|e| {
                                adk_core::AdkError::memory(format!("search failed: {e}"))
                            })?
                    }
                    Some(pid) => {
                        let sql = format!(
                            r#"
                            SELECT memory_id, content, author, timestamp, expires_at,
                                   (embedding::halfvec({dims}) <=> $3::halfvec({dims})) AS distance
                            FROM memory_entries
                            WHERE app_name = $1 AND user_id = $2 AND embedding IS NOT NULL
                              AND (project_id IS NULL OR project_id = $5)
                              AND (expires_at IS NULL OR expires_at > $6)
                            ORDER BY embedding::halfvec({dims}) <=> $3::halfvec({dims})
                            LIMIT $4
                            "#
                        );
                        // Only the validated numeric embedding dimension changes SQL structure.
                        sqlx::query(sqlx::AssertSqlSafe(sql))
                            .bind(&req.app_name)
                            .bind(&req.user_id)
                            .bind(&query_vec)
                            .bind(limit)
                            .bind(pid)
                            .bind(now)
                            .fetch_all(&self.pool)
                            .await
                            .map_err(|e| {
                                adk_core::AdkError::memory(format!("search failed: {e}"))
                            })?
                    }
                }
            } else {
                match &req.project_id {
                    None => sqlx::query(
                        r#"
                            SELECT memory_id, content, author, timestamp, expires_at, (embedding <=> $3) AS distance
                            FROM memory_entries
                            WHERE app_name = $1 AND user_id = $2 AND embedding IS NOT NULL
                              AND project_id IS NULL
                              AND (expires_at IS NULL OR expires_at > $5)
                            ORDER BY embedding <=> $3
                            LIMIT $4
                            "#,
                    )
                    .bind(&req.app_name)
                    .bind(&req.user_id)
                    .bind(&query_vec)
                    .bind(limit)
                    .bind(now)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?,
                    Some(pid) => sqlx::query(
                        r#"
                            SELECT memory_id, content, author, timestamp, expires_at, (embedding <=> $3) AS distance
                            FROM memory_entries
                            WHERE app_name = $1 AND user_id = $2 AND embedding IS NOT NULL
                              AND (project_id IS NULL OR project_id = $5)
                              AND (expires_at IS NULL OR expires_at > $6)
                            ORDER BY embedding <=> $3
                            LIMIT $4
                            "#,
                    )
                    .bind(&req.app_name)
                    .bind(&req.user_id)
                    .bind(query_vec)
                    .bind(limit)
                    .bind(pid)
                    .bind(now)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?,
                }
            }
        } else {
            // Full-text search fallback
            match &req.project_id {
                None => {
                    sqlx::query(
                        r#"
                        SELECT memory_id, content, author, timestamp, expires_at, ts_rank(search_text, plainto_tsquery('simple', $3)) AS rank_score
                        FROM memory_entries
                        WHERE app_name = $1 AND user_id = $2
                          AND search_text @@ plainto_tsquery('simple', $3)
                          AND project_id IS NULL
                              AND (expires_at IS NULL OR expires_at > $5)
                        ORDER BY rank_score DESC
                        LIMIT $4
                        "#,
                    )
                    .bind(&req.app_name)
                    .bind(&req.user_id)
                    .bind(&req.query)
                    .bind(limit)
                    .bind(now)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?
                }
                Some(pid) => {
                    sqlx::query(
                        r#"
                        SELECT memory_id, content, author, timestamp, expires_at, ts_rank(search_text, plainto_tsquery('simple', $3)) AS rank_score
                        FROM memory_entries
                        WHERE app_name = $1 AND user_id = $2
                          AND search_text @@ plainto_tsquery('simple', $3)
                          AND (project_id IS NULL OR project_id = $5)
                              AND (expires_at IS NULL OR expires_at > $6)
                        ORDER BY rank_score DESC
                        LIMIT $4
                        "#,
                    )
                    .bind(&req.app_name)
                    .bind(&req.user_id)
                    .bind(&req.query)
                    .bind(limit)
                    .bind(pid)
                    .bind(now)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?
                }
            }
        };

        let min_score = req.min_score;
        let memories =
            rows.iter()
                .filter(|row| {
                    if let Some(threshold) = min_score {
                        // pgvector's distance operators return `float8` and `ts_rank`
                        // returns `float4`; sqlx refuses to read either as the other.
                        let score = row
                            .try_get::<f64, _>("distance")
                            .map(|distance| 1.0 - distance)
                            .or_else(|_| row.try_get::<f32, _>("rank_score").map(f64::from))
                            .unwrap_or(0.0);
                        score >= f64::from(threshold)
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
                    let mut entry =
                        MemoryEntry::new(content, author).with_timestamp(row.get("timestamp"));
                    entry.id = row.get("memory_id");
                    entry.expires_at = row.get("expires_at");
                    entry
                })
                .collect();

        Ok(SearchResponse { memories })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, limit = %limit))]
    async fn list_recent(
        &self,
        app_name: &str,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT memory_id, content, author, timestamp, expires_at
            FROM memory_entries
            WHERE app_name = $1 AND user_id = $2
              AND (expires_at IS NULL OR expires_at > $4)
            ORDER BY timestamp DESC
            LIMIT $3
            "#,
        )
        .bind(app_name)
        .bind(user_id)
        .bind(limit as i64)
        .bind(Utc::now())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("list_recent failed: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let content_json: serde_json::Value = row.get("content");
                let content: adk_core::Content = serde_json::from_value(content_json)
                    .unwrap_or_else(|_| adk_core::Content {
                        role: "user".to_string(),
                        parts: vec![],
                    });
                let mut entry = MemoryEntry::new(content, row.get::<String, _>("author"))
                    .with_timestamp(row.get("timestamp"));
                entry.id = row.get("memory_id");
                entry.expires_at = row.get("expires_at");
                entry
            })
            .collect())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn export_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserMemory>> {
        let rows = sqlx::query(
            "SELECT session_id, project_id, memory_id, content, author, timestamp, expires_at \
             FROM memory_entries WHERE app_name = $1 AND user_id = $2 ORDER BY timestamp, id",
        )
        .bind(app_name)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("export_user failed: {e}")))?;
        rows.into_iter()
            .map(|row| {
                let content: serde_json::Value = row.get("content");
                // Entries added without a session are stored with an empty session id.
                let session_id: String = row.get("session_id");
                let content = serde_json::from_value(content).map_err(|e| {
                    adk_core::AdkError::memory(format!("stored content is corrupt: {e}"))
                })?;
                let mut entry = MemoryEntry::new(content, row.get::<String, _>("author"))
                    .with_timestamp(row.get("timestamp"));
                entry.id = row.get("memory_id");
                entry.expires_at = row.get("expires_at");
                Ok(UserMemory {
                    entry,
                    session_id: (!session_id.is_empty()).then_some(session_id),
                    project_id: row.get("project_id"),
                })
            })
            .collect()
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM memory_entries WHERE app_name = $1 AND user_id = $2")
            .bind(app_name)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_user failed: {e}")))?;
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
        .map_err(|e| adk_core::AdkError::memory(format!("delete_session failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, query = %query))]
    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM memory_entries
            WHERE app_name = $1 AND user_id = $2
              AND search_text @@ plainto_tsquery('simple', $3)
              AND project_id IS NULL
            "#,
        )
        .bind(app_name)
        .bind(user_id)
        .bind(query)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete_entries failed: {e}")))?;
        Ok(result.rows_affected())
    }

    fn supports_project_scoping(&self) -> bool {
        true
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, project_id = %project_id))]
    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        validate_project_id(project_id)?;
        self.insert(app_name, user_id, session_id, Some(project_id), entries).await
    }

    /// Add a single global entry (`project_id = NULL`).
    ///
    /// Without this override the trait default returns "not implemented",
    /// making direct global writes impossible on the Postgres backend even
    /// though project-scoped writes work.
    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()> {
        // Entries added without a session are stored with an empty session id.
        self.insert(app_name, user_id, "", None, vec![entry]).await
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        validate_project_id(project_id)?;
        self.insert(app_name, user_id, "", Some(project_id), vec![entry]).await
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id, query = %query))]
    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        validate_project_id(project_id)?;

        let result = sqlx::query(
            r#"
            DELETE FROM memory_entries
            WHERE app_name = $1 AND user_id = $2
              AND search_text @@ plainto_tsquery('simple', $3)
              AND project_id = $4
            "#,
        )
        .bind(app_name)
        .bind(user_id)
        .bind(query)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            adk_core::AdkError::memory(format!("delete_entries_in_project failed: {e}"))
        })?;
        Ok(result.rows_affected())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        validate_project_id(project_id)?;

        let result = sqlx::query(
            "DELETE FROM memory_entries WHERE app_name = $1 AND user_id = $2 AND project_id = $3",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete_project failed: {e}")))?;
        Ok(result.rows_affected())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_memory(&self, app_name: &str, user_id: &str, memory_id: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM memory_entries WHERE app_name = $1 AND user_id = $2 AND memory_id = $3",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(memory_id)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete_memory failed: {e}")))?;
        Ok(result.rows_affected())
    }

    #[instrument(skip_all)]
    async fn purge_expired(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM memory_entries WHERE expires_at <= $1")
            .bind(Utc::now())
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("purge_expired failed: {e}")))?;
        Ok(result.rows_affected())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}
