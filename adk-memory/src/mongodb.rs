//! MongoDB memory service backed by MongoDB Atlas Vector Search.
//!
//! Provides [`MongoMemoryService`], a [`MemoryService`](crate::MemoryService) implementation
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
        Ok(Self { db, embedding_provider })
    }

    /// Registry collection name for tracking applied migration versions.
    const REGISTRY_COLLECTION: &'static str = "_adk_memory_migrations";

    /// Compiled-in MongoDB migration steps.
    ///
    /// Each entry is `(version, description)`. The actual migration logic is
    /// dispatched by version number in [`run_mongo_memory_step`].
    const MONGO_MEMORY_MIGRATIONS: &'static [(i64, &'static str)] =
        &[(1, "create initial indexes")];

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
            if existing {
                if let Some(&(version, description)) = Self::MONGO_MEMORY_MIGRATIONS.first() {
                    self.record_migration(version, description).await?;
                    max_applied = version;
                }
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

    /// Detect whether the `memory_entries` collection already exists (baseline).
    async fn detect_existing_tables(&self) -> Result<bool> {
        let collections =
            self.db.list_collection_names().await.map_err(|e| {
                adk_core::AdkError::memory(format!("baseline detection failed: {e}"))
            })?;
        Ok(collections.contains(&"memory_entries".to_string()))
    }

    /// Record a successfully applied migration step in the registry.
    async fn record_migration(&self, version: i64, description: &str) -> Result<()> {
        let registry = self.db.collection::<Document>(Self::REGISTRY_COLLECTION);
        let now = BsonDateTime::from_millis(Utc::now().timestamp_millis());
        registry
            .insert_one(doc! {
                "version": version,
                "description": description,
                "applied_at": now,
            })
            .await
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

/// Execute a single MongoDB memory migration step by version number.
///
/// Each step is idempotent — re-running a step that has already been applied
/// completes without error (MongoDB's `create_index` is a no-op if the index
/// already exists with the same specification).
async fn run_mongo_memory_step(db: &Database, version: i64) -> Result<()> {
    match version {
        1 => mongo_memory_v1(db).await,
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

        let mut documents = Vec::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            let content_json = serde_json::to_value(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            let content_bson = mongodb::bson::to_bson(&content_json)
                .map_err(|e| adk_core::AdkError::memory(format!("bson conversion failed: {e}")))?;

            let timestamp = BsonDateTime::from_millis(entry.timestamp.timestamp_millis());

            let mut document = doc! {
                "app_name": app_name,
                "user_id": user_id,
                "session_id": session_id,
                "content": content_bson,
                "content_text": &texts[i],
                "author": &entry.author,
                "timestamp": timestamp,
            };

            if let Some(ref embs) = embeddings {
                let embedding_vec: Vec<mongodb::bson::Bson> =
                    embs[i].iter().map(|&v| mongodb::bson::Bson::Double(v as f64)).collect();
                document.insert("embedding", embedding_vec);
            }

            documents.push(document);
        }

        collection
            .insert_many(documents)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("add_session failed: {e}")))?;

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
                    "$match": {
                        "app_name": &req.app_name,
                        "user_id": &req.user_id,
                    }
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
            let filter = doc! {
                "app_name": &req.app_name,
                "user_id": &req.user_id,
                "$text": { "$search": &req.query },
            };

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

        let memories =
            docs.iter()
                .filter_map(|doc| {
                    let content_bson = doc.get("content")?;
                    let content_json: serde_json::Value =
                        mongodb::bson::from_bson(content_bson.clone()).ok()?;
                    let content: adk_core::Content =
                        serde_json::from_value(content_json).unwrap_or_else(|_| {
                            adk_core::Content { role: "user".to_string(), parts: vec![] }
                        });
                    let author = doc.get_str("author").unwrap_or("unknown").to_string();
                    let timestamp = doc
                        .get_datetime("timestamp")
                        .ok()
                        .map(|dt| {
                            chrono::DateTime::from_timestamp_millis(dt.timestamp_millis())
                                .unwrap_or_default()
                        })
                        .unwrap_or_default();
                    Some(MemoryEntry { content, author, timestamp })
                })
                .collect();

        Ok(SearchResponse { memories })
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

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}
