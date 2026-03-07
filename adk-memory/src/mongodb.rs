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
use adk_core::{Part, Result};
use async_trait::async_trait;
use mongodb::bson::{DateTime as BsonDateTime, Document, doc};
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

    /// Create the `memory_entries` collection with required indexes.
    ///
    /// Creates:
    /// - Compound index on `(app_name, user_id)` for filtered queries
    /// - Text index on `content_text` field for fallback text search
    ///
    /// **Note:** Atlas Vector Search index on the `embedding` field must be
    /// created separately via the Atlas UI or API. The index should be named
    /// `memory_embedding_index` with cosine similarity on the `embedding` path.
    pub async fn migrate(&self) -> Result<()> {
        let collection = self.db.collection::<Document>("memory_entries");

        // Compound index on (app_name, user_id)
        let app_user_index =
            IndexModel::builder().keys(doc! { "app_name": 1, "user_id": 1 }).build();

        collection.create_index(app_user_index).await.map_err(|e| {
            adk_core::AdkError::Memory(format!("migration failed: index creation failed: {e}"))
        })?;

        // Text index on content_text for fallback search
        let text_index = IndexModel::builder().keys(doc! { "content_text": "text" }).build();

        collection.create_index(text_index).await.map_err(|e| {
            adk_core::AdkError::Memory(format!("migration failed: text index creation failed: {e}"))
        })?;

        Ok(())
    }

    /// Extract plain text from a `Content` value for text search indexing.
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

        let mut documents = Vec::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            let content_json = serde_json::to_value(&entry.content)
                .map_err(|e| adk_core::AdkError::Memory(format!("serialization failed: {e}")))?;
            let content_bson = mongodb::bson::to_bson(&content_json)
                .map_err(|e| adk_core::AdkError::Memory(format!("bson conversion failed: {e}")))?;

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
            .map_err(|e| adk_core::AdkError::Memory(format!("add_session failed: {e}")))?;

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
                .map_err(|e| adk_core::AdkError::Memory(format!("query embedding failed: {e}")))?;
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
                    adk_core::AdkError::Memory(
                        "vector search index not available: Atlas Vector Search index \
                         'memory_embedding_index' must be created via Atlas UI/API"
                            .to_string(),
                    )
                } else {
                    adk_core::AdkError::Memory(format!("search failed: {e}"))
                }
            })?;

            let mut results = Vec::new();
            while cursor
                .advance()
                .await
                .map_err(|e| adk_core::AdkError::Memory(format!("search cursor failed: {e}")))?
            {
                let doc = cursor.deserialize_current().map_err(|e| {
                    adk_core::AdkError::Memory(format!("search deserialization failed: {e}"))
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
                .map_err(|e| adk_core::AdkError::Memory(format!("search failed: {e}")))?;

            let mut results = Vec::new();
            while cursor
                .advance()
                .await
                .map_err(|e| adk_core::AdkError::Memory(format!("search cursor failed: {e}")))?
            {
                let doc = cursor.deserialize_current().map_err(|e| {
                    adk_core::AdkError::Memory(format!("search deserialization failed: {e}"))
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
            .map_err(|e| adk_core::AdkError::Memory(format!("delete_user failed: {e}")))?;
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
            .map_err(|e| adk_core::AdkError::Memory(format!("delete_session failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}
