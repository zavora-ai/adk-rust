//! MongoDB memory service backed by MongoDB Atlas Vector Search.
//!
//! Provides [`MongoMemoryService`], a [`MemoryService`] implementation
//! that stores memory entries in MongoDB with optional Atlas Vector Search for
//! cosine similarity ranking. When no embedding provider is configured, falls back
//! to MongoDB text search on the content field.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::MongoMemoryService;
//!
//! let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
//! let service = MongoMemoryService::new(client, "my_db", None)?;
//! service.migrate().await?;
//! ```

use crate::embedding::EmbeddingProvider;
use crate::service::*;
use adk_core::Result;
use async_trait::async_trait;
use chrono::Utc;
use mongodb::bson::{DateTime as BsonDateTime, Document, doc};
use mongodb::options::IndexOptions;
use mongodb::{Client, Database, IndexModel};
use std::sync::Arc;
use std::time::Duration;
use tracing::instrument;

/// MongoDB-backed memory service with optional Atlas Vector Search support.
///
/// When an [`EmbeddingProvider`] is supplied, entries are stored with vector
/// embeddings and searched via MongoDB Atlas `$vectorSearch` aggregation stage
/// for cosine similarity ranking. Without a provider, search falls back to
/// MongoDB `$text` search using a text index on the content field.
///
/// # Note
///
/// Atlas Vector Search indexes must be created separately via the Atlas UI or
/// API — they cannot be created programmatically through the MongoDB driver.
/// The [`migrate`](Self::migrate) method creates the text index for fallback
/// search and the compound index on `(app_name, user_id)`.
pub struct MongoMemoryService {
    db: Database,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    default_ttl: Option<Duration>,
}

impl MongoMemoryService {
    /// Create a MongoDB memory service from an existing client.
    ///
    /// # Arguments
    ///
    /// * `client` - A connected `mongodb::Client`
    /// * `database_name` - The target MongoDB database name
    /// * `embedding_provider` - Optional embedding provider for vector search
    pub fn new(
        client: Client,
        database_name: &str,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<Self> {
        let db = client.database(database_name);
        Ok(Self { db, embedding_provider, default_ttl: None })
    }

    /// Expires each entry written without an expiry `ttl` after it is written.
    ///
    /// The TTL index that [`migrate`](Self::migrate) creates on `expires_at` lets MongoDB
    /// delete expired entries itself, on its TTL monitor's schedule (every 60 seconds by
    /// default); reads skip them before then.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_memory::MongoMemoryService;
    /// use std::time::Duration;
    ///
    /// let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
    /// let service = MongoMemoryService::new(client, "my_db", None)?
    ///     .with_default_ttl(Duration::from_secs(30 * 24 * 3600));
    /// ```
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// The document an entry is stored as: `scope` (app, user, session and project) plus
    /// the entry, stamped with its id and expiry.
    fn document(
        &self,
        scope: &Document,
        entry: MemoryEntry,
        embedding: Option<&[f32]>,
        now: chrono::DateTime<Utc>,
    ) -> Result<Document> {
        let entry = crate::expiry::stamp(entry, self.default_ttl, now);
        let content_json = serde_json::to_value(&entry.content)
            .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
        let content_bson = mongodb::bson::to_bson(&content_json)
            .map_err(|e| adk_core::AdkError::memory(format!("bson conversion failed: {e}")))?;
        let mut document = scope.clone();
        document.insert("memory_id", entry.id.unwrap_or_default());
        document.insert("content", content_bson);
        document.insert("content_text", crate::text::extract_text(&entry.content));
        document.insert("author", entry.author);
        document.insert("timestamp", BsonDateTime::from_millis(entry.timestamp.timestamp_millis()));
        // A BSON date, so the TTL index deletes the entry once it passes.
        if let Some(expires_at) = entry.expires_at {
            document.insert("expires_at", BsonDateTime::from_millis(expires_at.timestamp_millis()));
        }
        if let Some(embedding) = embedding {
            let embedding: Vec<mongodb::bson::Bson> =
                embedding.iter().map(|&v| mongodb::bson::Bson::Double(v as f64)).collect();
            document.insert("embedding", embedding);
        }
        Ok(document)
    }

    /// Registry collection name for tracking applied migration versions.
    const REGISTRY_COLLECTION: &'static str = "_adk_memory_migrations";

    /// Compiled-in MongoDB migration steps.
    ///
    /// Each entry is `(version, description)`. The actual migration logic is
    /// dispatched by version number in [`run_mongo_memory_step`].
    const MONGO_MEMORY_MIGRATIONS: &'static [(i64, &'static str)] = &[
        (1, "create initial indexes"),
        (2, "add project_id compound index"),
        (3, "add memory_id index and expires_at TTL index"),
    ];

    /// Run versioned migrations for MongoDB memory storage.
    ///
    /// The runner:
    /// 1. Creates the registry collection with a unique index on `version`.
    /// 2. Detects baseline — if `memory_entries` collection exists but registry
    ///    is empty, records v1 as already applied.
    /// 3. Reads the maximum applied version from the registry.
    /// 4. Returns an error if the database version exceeds the compiled-in max.
    /// 5. Executes each unapplied step idempotently and records it.
    ///
    /// Several processes may migrate one database at once: every step creates its
    /// indexes idempotently, and each version is recorded with an upsert, so a version
    /// another process recorded first is not an error.
    ///
    /// **Note:** Atlas Vector Search index on the `embedding` field must be
    /// created separately via the Atlas UI or API. The index should be named
    /// `memory_embedding_index` with cosine similarity on the `embedding` path.
    pub async fn migrate(&self) -> Result<()> {
        // Step 1: Ensure registry collection has a unique index on `version`
        self.db
            .collection::<Document>(Self::REGISTRY_COLLECTION)
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "version": 1 })
                    .options(
                        IndexOptions::builder()
                            .unique(true)
                            .name("idx_migration_version_unique".to_string())
                            .build(),
                    )
                    .build(),
            )
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!("migration registry creation failed: {e}"))
            })?;

        // Step 2: Read current max applied version
        let mut max_applied = self.read_max_applied_version().await?;

        // Step 3: Baseline detection — if registry is empty but memory_entries
        // collection already exists, record v1 as applied.
        if max_applied == 0 {
            let existing = self.detect_existing_tables().await?;
            if existing && let Some(&(version, description)) = Self::MONGO_MEMORY_MIGRATIONS.first()
            {
                self.record_migration(version, description).await?;
                max_applied = version;
            }
        }

        // Step 4: Compiled-in max version
        let max_compiled = Self::MONGO_MEMORY_MIGRATIONS.last().map(|s| s.0).unwrap_or(0);

        // Step 5: Version mismatch check
        if max_applied > max_compiled {
            return Err(adk_core::AdkError::memory(format!(
                "schema version mismatch: database is at v{max_applied} \
                 but code only knows up to v{max_compiled}. \
                 Upgrade your ADK version."
            )));
        }

        // Step 6: Execute unapplied steps idempotently
        for &(version, description) in Self::MONGO_MEMORY_MIGRATIONS {
            if version <= max_applied {
                continue;
            }

            run_mongo_memory_step(&self.db, version).await.map_err(|e| {
                adk_core::AdkError::memory(format!(
                    "{}",
                    crate::migration::MigrationError {
                        version,
                        description: description.to_string(),
                        cause: e.to_string(),
                    }
                ))
            })?;

            self.record_migration(version, description).await?;
        }

        Ok(())
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    pub async fn schema_version(&self) -> Result<i64> {
        // Check if registry collection exists
        let collections =
            self.db.list_collection_names().await.map_err(|e| {
                adk_core::AdkError::memory(format!("schema version query failed: {e}"))
            })?;
        if !collections.contains(&Self::REGISTRY_COLLECTION.to_string()) {
            return Ok(0);
        }

        self.read_max_applied_version().await
    }

    /// Read the maximum applied version from the registry collection.
    async fn read_max_applied_version(&self) -> Result<i64> {
        use mongodb::options::FindOneOptions;

        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let opts = FindOneOptions::builder().sort(doc! { "version": -1 }).build();
        let result = registry.find_one(doc! {}).with_options(opts).await.map_err(|e| {
            adk_core::AdkError::memory(format!("migration registry read failed: {e}"))
        })?;

        match result {
            Some(doc) => {
                let version = doc.get_i64("version").unwrap_or(0);
                Ok(version)
            }
            None => Ok(0),
        }
    }

    /// Whether v1's last index exists (baseline). A first `migrate` running in another
    /// process creates v1's indexes in order, so the collection or an earlier index
    /// existing does not mean v1 is complete.
    async fn detect_existing_tables(&self) -> Result<bool> {
        let failed = |e: mongodb::error::Error| {
            adk_core::AdkError::memory(format!("baseline detection failed: {e}"))
        };
        let collections = self.db.list_collection_names().await.map_err(failed)?;
        if !collections.iter().any(|name| name == "memory_entries") {
            return Ok(false);
        }
        let indexes = self
            .db
            .collection::<Document>("memory_entries")
            .list_index_names()
            .await
            .map_err(failed)?;
        Ok(indexes.iter().any(|name| name == "idx_memory_entries_text"))
    }

    /// Records an applied migration step unless another process recorded it first.
    async fn record_migration(&self, version: i64, description: &str) -> Result<()> {
        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let now = BsonDateTime::from_millis(Utc::now().timestamp_millis());
        let recorded = registry
            .update_one(
                doc! { "version": version },
                doc! { "$setOnInsert": { "description": description, "applied_at": now } },
            )
            .upsert(true)
            .await;
        // The server retries an upsert that loses a race on the unique `version` index;
        // a duplicate key that still surfaces means the version is recorded.
        match recorded {
            Ok(_) => Ok(()),
            Err(e) if is_duplicate_key(&e) => Ok(()),
            Err(e) => Err(e),
        }
        .map_err(|e| {
            adk_core::AdkError::memory(format!(
                "{}",
                crate::migration::MigrationError {
                    version,
                    description: description.to_string(),
                    cause: format!("registry record failed: {e}"),
                }
            ))
        })?;
        Ok(())
    }
}

/// Whether `error` is a duplicate-key violation of a unique index.
fn is_duplicate_key(error: &mongodb::error::Error) -> bool {
    use mongodb::error::{ErrorKind, WriteFailure};
    match &*error.kind {
        ErrorKind::Write(WriteFailure::WriteError(write)) => write.code == 11000,
        _ => false,
    }
}

/// Execute a single MongoDB memory migration step by version number.
///
/// Each step is idempotent — re-running a step that has already been applied
/// completes without error (MongoDB's `create_index` is a no-op if the index
/// already exists with the same specification).
async fn run_mongo_memory_step(db: &Database, version: i64) -> Result<()> {
    match version {
        1 => mongo_memory_v1(db).await,
        2 => mongo_memory_v2(db).await,
        3 => mongo_memory_v3(db).await,
        _ => Err(adk_core::AdkError::memory(format!("unknown migration version: {version}"))),
    }
}

/// V1: Create initial indexes on memory_entries collection.
///
/// This matches the original `migrate()` index creation logic:
/// - Compound index on `(app_name, user_id)` for filtered queries
/// - Text index on `content_text` field for fallback text search
async fn mongo_memory_v1(db: &Database) -> Result<()> {
    let collection = db.collection::<Document>("memory_entries");

    // Compound index on (app_name, user_id)
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! { "app_name": 1, "user_id": 1 })
                .options(
                    IndexOptions::builder().name("idx_memory_entries_app_user".to_string()).build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("index creation failed: {e}")))?;

    // Text index on content_text for fallback search
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! { "content_text": "text" })
                .options(
                    IndexOptions::builder().name("idx_memory_entries_text".to_string()).build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("text index creation failed: {e}")))?;

    Ok(())
}

/// V2: Add compound index on `(app_name, user_id, project_id)` for project-scoped queries.
///
/// Existing documents without a `project_id` field are treated as `null` by
/// MongoDB query semantics, which is the correct behavior for global entries.
async fn mongo_memory_v2(db: &Database) -> Result<()> {
    let collection = db.collection::<Document>("memory_entries");

    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! { "app_name": 1, "user_id": 1, "project_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name("idx_memory_entries_app_user_project".to_string())
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|e| {
            adk_core::AdkError::memory(format!("project_id compound index creation failed: {e}"))
        })?;

    Ok(())
}

/// V3: index `memory_id` per user, and expire entries through a TTL index on `expires_at`.
///
/// `expireAfterSeconds: 0` makes MongoDB delete a document once its `expires_at` date has
/// passed; documents without the field never expire.
async fn mongo_memory_v3(db: &Database) -> Result<()> {
    let collection = db.collection::<Document>("memory_entries");
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! { "app_name": 1, "user_id": 1, "memory_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name("idx_memory_entries_memory_id".to_string())
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("memory_id index creation failed: {e}")))?;
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! { "expires_at": 1 })
                .options(
                    IndexOptions::builder()
                        .name("idx_memory_entries_expires_at_ttl".to_string())
                        .expire_after(std::time::Duration::ZERO)
                        .build(),
                )
                .build(),
        )
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("TTL index creation failed: {e}")))?;
    Ok(())
}

/// The filter that keeps entries live at `now`: no `expires_at`, or one after `now`.
///
/// Mirrors `expiry::is_expired`; `$not` also matches documents without the field.
fn live_filter(now: chrono::DateTime<Utc>) -> Document {
    doc! { "expires_at": { "$not": { "$lte": BsonDateTime::from_millis(now.timestamp_millis()) } } }
}

/// A stored document as a memory entry, or `None` when its content is malformed.
fn document_entry(doc: &Document) -> Option<MemoryEntry> {
    let content_json: serde_json::Value =
        mongodb::bson::from_bson(doc.get("content")?.clone()).ok()?;
    let content: adk_core::Content = serde_json::from_value(content_json).ok()?;
    let author = doc.get_str("author").ok()?.to_string();
    let millis = |field: &str| {
        doc.get_datetime(field)
            .ok()
            .and_then(|dt| chrono::DateTime::from_timestamp_millis(dt.timestamp_millis()))
    };
    let mut entry = MemoryEntry::new(content, author).with_timestamp(millis("timestamp")?);
    // Entries written before 3.0 have no memory id; their document id stands in.
    entry.id = doc
        .get_str("memory_id")
        .ok()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| doc.get_object_id("_id").ok().map(|id| id.to_hex()));
    entry.expires_at = millis("expires_at");
    Some(entry)
}

#[async_trait]
impl MemoryService for MongoMemoryService {
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

        let collection = self.db.collection::<Document>("memory_entries");

        // Collect texts for batch embedding
        let texts: Vec<String> =
            entries.iter().map(|e| crate::text::extract_text(&e.content)).collect();

        let embeddings = if let Some(provider) = &self.embedding_provider {
            let non_empty_texts: Vec<String> = texts
                .iter()
                .map(|t| if t.is_empty() { " ".to_string() } else { t.clone() })
                .collect();
            Some(provider.embed(&non_empty_texts).await.map_err(|e| {
                adk_core::AdkError::memory(format!("embedding generation failed: {e}"))
            })?)
        } else {
            None
        };

        let now = Utc::now();
        let scope = doc! {
            "app_name": app_name,
            "user_id": user_id,
            "session_id": session_id,
            "project_id": mongodb::bson::Bson::Null,
        };
        let mut documents = Vec::with_capacity(entries.len());
        for (i, entry) in entries.into_iter().enumerate() {
            let embedding = embeddings.as_ref().map(|embs| embs[i].as_slice());
            documents.push(self.document(&scope, entry, embedding, now)?);
        }

        collection
            .insert_many(documents)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("add_session failed: {e}")))?;

        Ok(())
    }

    fn supports_project_scoping(&self) -> bool {
        true
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, project_id = %project_id, entry_count = entries.len()))]
    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        validate_project_id(project_id)?;

        if entries.is_empty() {
            return Ok(());
        }

        let collection = self.db.collection::<Document>("memory_entries");

        // Collect texts for batch embedding
        let texts: Vec<String> =
            entries.iter().map(|e| crate::text::extract_text(&e.content)).collect();

        let embeddings = if let Some(provider) = &self.embedding_provider {
            let non_empty_texts: Vec<String> = texts
                .iter()
                .map(|t| if t.is_empty() { " ".to_string() } else { t.clone() })
                .collect();
            Some(provider.embed(&non_empty_texts).await.map_err(|e| {
                adk_core::AdkError::memory(format!("embedding generation failed: {e}"))
            })?)
        } else {
            None
        };

        let now = Utc::now();
        let scope = doc! {
            "app_name": app_name,
            "user_id": user_id,
            "session_id": session_id,
            "project_id": project_id,
        };
        let mut documents = Vec::with_capacity(entries.len());
        for (i, entry) in entries.into_iter().enumerate() {
            let embedding = embeddings.as_ref().map(|embs| embs[i].as_slice());
            documents.push(self.document(&scope, entry, embedding, now)?);
        }

        collection.insert_many(documents).await.map_err(|e| {
            adk_core::AdkError::memory(format!("add_session_to_project failed: {e}"))
        })?;

        Ok(())
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

        let collection = self.db.collection::<Document>("memory_entries");

        let content_text = crate::text::extract_text(&entry.content);
        let embedding = match &self.embedding_provider {
            Some(provider) => {
                let text_for_embed =
                    if content_text.is_empty() { " ".to_string() } else { content_text.clone() };
                let mut embeddings = provider.embed(&[text_for_embed]).await.map_err(|e| {
                    adk_core::AdkError::memory(format!("embedding generation failed: {e}"))
                })?;
                Some(embeddings.swap_remove(0))
            }
            None => None,
        };
        let scope = doc! { "app_name": app_name, "user_id": user_id, "project_id": project_id };
        let document = self.document(&scope, entry, embedding.as_deref(), Utc::now())?;

        collection
            .insert_one(document)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("add_entry_to_project failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let collection = self.db.collection::<Document>("memory_entries");
        let limit = req.limit.unwrap_or(10) as i64;

        let docs = if let Some(ref provider) = self.embedding_provider {
            // Vector search via $vectorSearch aggregation stage
            let query_embedding = provider
                .embed(std::slice::from_ref(&req.query))
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("query embedding failed: {e}")))?;
            let query_vec: Vec<mongodb::bson::Bson> =
                query_embedding[0].iter().map(|&v| mongodb::bson::Bson::Double(v as f64)).collect();

            let mut match_filter = doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
            };
            match &req.project_id {
                None => {
                    match_filter.insert("project_id", mongodb::bson::Bson::Null);
                }
                Some(id) => {
                    match_filter.insert(
                        "$or",
                        vec![
                            doc! { "project_id": mongodb::bson::Bson::Null },
                            doc! { "project_id": id },
                        ],
                    );
                }
            }

            let match_filter = doc! { "$and": [match_filter, live_filter(Utc::now())] };
            let pipeline = vec![
                doc! {
                    "$vectorSearch": {
                        "index": "memory_embedding_index",
                        "path": "embedding",
                        "queryVector": &query_vec,
                        "numCandidates": 100,
                        "limit": limit,
                    }
                },
                doc! {
                    "$match": match_filter,
                },
            ];

            let mut cursor = collection.aggregate(pipeline).await.map_err(|e| {
                let msg = e.to_string();
                if msg.contains("PlanExecutor") || msg.contains("$vectorSearch") {
                    adk_core::AdkError::memory(
                        "vector search index not available: Atlas Vector Search index \
                         'memory_embedding_index' must be created via Atlas UI/API"
                            .to_string(),
                    )
                } else {
                    adk_core::AdkError::memory(format!("search failed: {e}"))
                }
            })?;

            let mut results = Vec::new();
            while cursor
                .advance()
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search cursor failed: {e}")))?
            {
                let doc = cursor.deserialize_current().map_err(|e| {
                    adk_core::AdkError::memory(format!("search deserialization failed: {e}"))
                })?;
                results.push(doc);
            }
            results
        } else {
            // Text search fallback
            let mut filter = doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "$text": { "$search": &req.query },
            };
            match &req.project_id {
                None => {
                    filter.insert("project_id", mongodb::bson::Bson::Null);
                }
                Some(id) => {
                    filter.insert(
                        "$or",
                        vec![
                            doc! { "project_id": mongodb::bson::Bson::Null },
                            doc! { "project_id": id.as_str() },
                        ],
                    );
                }
            }

            filter.insert(
                "expires_at",
                live_filter(Utc::now()).get("expires_at").cloned().unwrap_or_default(),
            );
            let mut cursor = collection
                .find(filter)
                .sort(doc! { "score": { "$meta": "textScore" } })
                .limit(limit)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?;

            let mut results = Vec::new();
            while cursor
                .advance()
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search cursor failed: {e}")))?
            {
                let doc = cursor.deserialize_current().map_err(|e| {
                    adk_core::AdkError::memory(format!("search deserialization failed: {e}"))
                })?;
                results.push(doc);
            }
            results
        };

        let memories = docs.iter().filter_map(document_entry).collect();

        Ok(SearchResponse { memories })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        validate_project_id(project_id)?;

        let collection = self.db.collection::<Document>("memory_entries");
        let filter = doc! {
            "app_name": app_name,
            "user_id": user_id,
            "project_id": project_id,
            "$text": { "$search": query },
        };
        let result = collection.delete_many(filter).await.map_err(|e| {
            adk_core::AdkError::memory(format!("delete_entries_in_project failed: {e}"))
        })?;
        Ok(result.deleted_count)
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        validate_project_id(project_id)?;

        let collection = self.db.collection::<Document>("memory_entries");
        let filter = doc! {
            "app_name": app_name,
            "user_id": user_id,
            "project_id": project_id,
        };
        let result = collection
            .delete_many(filter)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_project failed: {e}")))?;
        Ok(result.deleted_count)
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn export_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserMemory>> {
        let failed = |e: mongodb::error::Error| {
            adk_core::AdkError::memory(format!("export_user failed: {e}"))
        };
        let mut cursor = self
            .db
            .collection::<Document>("memory_entries")
            .find(doc! { "app_name": app_name, "user_id": user_id })
            .sort(doc! { "timestamp": 1, "_id": 1 })
            .await
            .map_err(failed)?;
        let mut documents = Vec::new();
        while cursor.advance().await.map_err(failed)? {
            documents.push(cursor.deserialize_current().map_err(failed)?);
        }
        documents
            .iter()
            .map(|document| {
                let corrupt =
                    |field: &str| adk_core::AdkError::memory(format!("stored {field} is corrupt"));
                Ok(UserMemory {
                    entry: document_entry(document).ok_or_else(|| corrupt("entry"))?,
                    session_id: document.get_str("session_id").ok().map(str::to_string),
                    project_id: document.get_str("project_id").ok().map(str::to_string),
                })
            })
            .collect()
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let collection = self.db.collection::<Document>("memory_entries");
        collection
            .delete_many(doc! { "app_name": app_name, "user_id": user_id })
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_user failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id))]
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        let collection = self.db.collection::<Document>("memory_entries");
        collection
            .delete_many(doc! {
                "app_name": app_name,
                "user_id": user_id,
                "session_id": session_id,
            })
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_session failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_memory(&self, app_name: &str, user_id: &str, memory_id: &str) -> Result<u64> {
        let collection = self.db.collection::<Document>("memory_entries");
        let mut filter = doc! { "app_name": app_name, "user_id": user_id, "memory_id": memory_id };
        // An entry written before 3.0 is addressed by its document id.
        if let Ok(object_id) = mongodb::bson::oid::ObjectId::parse_str(memory_id) {
            filter = doc! {
                "app_name": app_name,
                "user_id": user_id,
                "$or": [{ "memory_id": memory_id }, { "_id": object_id, "memory_id": { "$exists": false } }],
            };
        }
        let result = collection
            .delete_many(filter)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_memory failed: {e}")))?;
        Ok(result.deleted_count)
    }

    #[instrument(skip_all)]
    async fn purge_expired(&self) -> Result<u64> {
        let now = BsonDateTime::from_millis(Utc::now().timestamp_millis());
        let result = self
            .db
            .collection::<Document>("memory_entries")
            .delete_many(doc! { "expires_at": { "$lte": now } })
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("purge_expired failed: {e}")))?;
        Ok(result.deleted_count)
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}
