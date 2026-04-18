//! Neo4j memory service with graph-based contextual retrieval.
//!
//! Provides [`Neo4jMemoryService`], a [`MemoryService`](crate::MemoryService) implementation
//! that stores memory entries as graph nodes with `FOLLOWS` relationships for
//! temporal context enrichment beyond isolated vector matches.
//!
//! # Graph Schema
//!
//! Memory entries are modeled as graph nodes with typed relationships:
//!
//! ```text
//! (:MemorySession {session_id, app_name, user_id})
//!     -[:FROM_SESSION]-> (:MemoryEntry {id, app_name, user_id, session_id, content, author, timestamp, embedding})
//!
//! (:MemoryEntry)-[:FOLLOWS]->(:MemoryEntry)   // temporal ordering
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::Neo4jMemoryService;
//! use neo4rs::Graph;
//!
//! let graph = Graph::new("bolt://localhost:7687", "neo4j", "password").await?;
//! let service = Neo4jMemoryService::new(graph, None)?;
//! service.migrate().await?;
//! ```

use crate::embedding::EmbeddingProvider;
use crate::service::*;
use adk_core::Result;
use async_trait::async_trait;
use chrono::DateTime;
use neo4rs::Graph;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::instrument;

/// Neo4j-backed memory service with graph relationship traversal for richer context.
///
/// When an [`EmbeddingProvider`] is supplied, entries are stored with vector
/// embeddings and searched via Neo4j vector index (`db.index.vector.queryNodes`)
/// for cosine similarity ranking. The search then traverses `FOLLOWS`
/// relationships to include temporally adjacent entries for richer context.
///
/// Without a provider, search falls back to a Neo4j full-text index on the
/// content property, still enriched by `FOLLOWS` traversal.
///
/// # Note
///
/// The [`migrate`](Self::migrate) method creates all required constraints,
/// indexes, and (if an embedding provider is configured) a vector index.
/// All DDL statements use `IF NOT EXISTS` for idempotent execution.
pub struct Neo4jMemoryService {
    graph: Graph,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl Neo4jMemoryService {
    /// Registry node label for tracking applied migration versions.
    const REGISTRY_LABEL: &'static str = "_AdkMemoryMigration";

    /// Compiled-in Neo4j migration steps for memory storage.
    ///
    /// Each entry is `(version, description, &[cypher_statements])`. All Cypher
    /// statements use `IF NOT EXISTS` for idempotent execution.
    ///
    /// The vector index step is handled separately in [`migrate`](Self::migrate)
    /// because it depends on the configured embedding dimensions.
    const NEO4J_MEMORY_MIGRATIONS: &'static [(i64, &'static str, &'static [&'static str])] = &[
        (
            1,
            "create initial constraints and indexes",
            &[
                "CREATE CONSTRAINT memory_entry_unique IF NOT EXISTS \
                 FOR (m:MemoryEntry) REQUIRE (m.id) IS UNIQUE",
                "CREATE INDEX memory_app_user IF NOT EXISTS \
                 FOR (m:MemoryEntry) ON (m.app_name, m.user_id)",
                "CREATE FULLTEXT INDEX memory_content IF NOT EXISTS \
                 FOR (m:MemoryEntry) ON EACH [m.content_text]",
            ],
        ),
        (
            2,
            "add project_id index",
            &["CREATE INDEX memory_project_id IF NOT EXISTS \
                 FOR (m:MemoryEntry) ON (m.project_id)"],
        ),
    ];

    /// Create a Neo4j memory service from an existing graph connection.
    ///
    /// # Arguments
    ///
    /// * `graph` - A connected `neo4rs::Graph` instance
    /// * `embedding_provider` - Optional embedding provider for vector search.
    ///   When provided, [`migrate`](Self::migrate) creates a vector index and
    ///   [`add_session`](crate::MemoryService::add_session) generates embeddings
    ///   for each entry.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_memory::Neo4jMemoryService;
    /// use neo4rs::Graph;
    ///
    /// let graph = Graph::new("bolt://localhost:7687", "neo4j", "password").await?;
    /// let service = Neo4jMemoryService::new(graph, None)?;
    /// ```
    pub fn new(
        graph: Graph,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> adk_core::Result<Self> {
        Ok(Self { graph, embedding_provider })
    }

    /// Returns a reference to the underlying Neo4j graph connection.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Run versioned migrations for Neo4j memory storage.
    ///
    /// The runner:
    /// 1. Creates a uniqueness constraint on registry nodes.
    /// 2. Detects baseline — if `memory_entry_unique` constraint exists but
    ///    registry is empty, records v1 as already applied.
    /// 3. Reads the maximum applied version from the registry.
    /// 4. Returns an error if the database version exceeds the compiled-in max.
    /// 5. Executes each unapplied step idempotently and records it.
    /// 6. Creates the vector index if an embedding provider is configured
    ///    (always idempotent, runs after migration steps).
    pub async fn migrate(&self) -> adk_core::Result<()> {
        // Step 1: Ensure registry has a uniqueness constraint on `version`
        self.graph
            .run(neo4rs::query(&format!(
                "CREATE CONSTRAINT {}_version_unique IF NOT EXISTS \
                 FOR (m:{}) REQUIRE (m.version) IS UNIQUE",
                Self::REGISTRY_LABEL.to_lowercase(),
                Self::REGISTRY_LABEL,
            )))
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!("migration registry creation failed: {e}"))
            })?;

        // Step 2: Read current max applied version
        let mut max_applied = self.read_max_applied_version().await?;

        // Step 3: Baseline detection — if registry is empty but memory_entry_unique
        // constraint already exists, record v1 as applied.
        if max_applied == 0 {
            let existing = self.detect_existing_tables().await?;
            if existing {
                if let Some(&(version, description, _)) = Self::NEO4J_MEMORY_MIGRATIONS.first() {
                    self.record_migration(version, description).await?;
                    max_applied = version;
                }
            }
        }

        // Step 4: Compiled-in max version
        let max_compiled = Self::NEO4J_MEMORY_MIGRATIONS.last().map(|s| s.0).unwrap_or(0);

        // Step 5: Version mismatch check
        if max_applied > max_compiled {
            return Err(adk_core::AdkError::memory(format!(
                "schema version mismatch: database is at v{max_applied} \
                 but code only knows up to v{max_compiled}. \
                 Upgrade your ADK version."
            )));
        }

        // Step 6: Execute unapplied steps idempotently
        for &(version, description, cypher_statements) in Self::NEO4J_MEMORY_MIGRATIONS {
            if version <= max_applied {
                continue;
            }

            for cypher in cypher_statements {
                self.graph.run(neo4rs::query(cypher)).await.map_err(|e| {
                    adk_core::AdkError::memory(format!(
                        "{}",
                        crate::migration::MigrationError {
                            version,
                            description: description.to_string(),
                            cause: e.to_string(),
                        }
                    ))
                })?;
            }

            self.record_migration(version, description).await?;
        }

        // Step 7: Vector index — depends on embedding provider dimensions,
        // so it runs outside the versioned step list. Always idempotent via
        // `IF NOT EXISTS`.
        if let Some(provider) = &self.embedding_provider {
            let dims = provider.dimensions();
            let vector_index_query = format!(
                "CREATE VECTOR INDEX memory_embedding IF NOT EXISTS \
                 FOR (m:MemoryEntry) ON (m.embedding) \
                 OPTIONS {{indexConfig: {{`vector.dimensions`: {dims}, \
                 `vector.similarity_function`: 'cosine'}}}}"
            );
            self.graph.run(neo4rs::query(&vector_index_query)).await.map_err(|e| {
                adk_core::AdkError::memory(format!(
                    "migration failed: vector index creation failed: {e}"
                ))
            })?;
        }

        Ok(())
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    pub async fn schema_version(&self) -> Result<i64> {
        self.read_max_applied_version().await
    }

    /// Read the maximum applied version from the registry nodes.
    async fn read_max_applied_version(&self) -> Result<i64> {
        let query_str =
            format!("OPTIONAL MATCH (m:{}) RETURN max(m.version) AS max_v", Self::REGISTRY_LABEL);
        let mut row_stream = self.graph.execute(neo4rs::query(&query_str)).await.map_err(|e| {
            adk_core::AdkError::memory(format!("migration registry read failed: {e}"))
        })?;

        if let Some(row) = row_stream.next().await.map_err(|e| {
            adk_core::AdkError::memory(format!("migration registry read failed: {e}"))
        })? {
            // max() returns null when no nodes exist; treat as 0
            Ok(row.get::<i64>("max_v").unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    /// Detect whether the `memory_entry_unique` constraint already exists (baseline).
    async fn detect_existing_tables(&self) -> Result<bool> {
        let mut row_stream = self
            .graph
            .execute(neo4rs::query(
                "SHOW CONSTRAINTS YIELD name WHERE name = 'memory_entry_unique' RETURN name",
            ))
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("baseline detection failed: {e}")))?;

        let found = row_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("baseline detection failed: {e}")))?
            .is_some();

        Ok(found)
    }

    /// Record a successfully applied migration step as a registry node.
    async fn record_migration(&self, version: i64, description: &str) -> Result<()> {
        let query_str = format!(
            "CREATE (m:{} {{version: $version, description: $description, applied_at: datetime()}})",
            Self::REGISTRY_LABEL,
        );
        self.graph
            .run(
                neo4rs::query(&query_str)
                    .param("version", version)
                    .param("description", description.to_string()),
            )
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

#[async_trait]
impl MemoryService for Neo4jMemoryService {
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

        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("transaction failed: {e}")))?;

        // MERGE the MemorySession node
        txn.run(
            neo4rs::query(
                "MERGE (:MemorySession {session_id: $session_id, \
                 app_name: $app_name, user_id: $user_id})",
            )
            .param("session_id", session_id.to_string())
            .param("app_name", app_name.to_string())
            .param("user_id", user_id.to_string()),
        )
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("add_session failed: {e}")))?;

        // Create MemoryEntry nodes and FROM_SESSION relationships
        let mut entry_ids: Vec<String> = Vec::with_capacity(entries.len());

        for (i, entry) in entries.iter().enumerate() {
            let entry_id = format!("{session_id}_{i}");
            entry_ids.push(entry_id.clone());

            let content_json = serde_json::to_string(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            let content_text = &texts[i];
            let timestamp_str = entry.timestamp.to_rfc3339();

            if let Some(ref embs) = embeddings {
                // Convert Vec<f32> to Vec<f64> for Neo4j
                let embedding_f64: Vec<f64> = embs[i].iter().map(|&v| v as f64).collect();

                txn.run(
                    neo4rs::query(
                        "MATCH (s:MemorySession {session_id: $session_id, \
                         app_name: $app_name, user_id: $user_id}) \
                         CREATE (s)-[:FROM_SESSION]->(e:MemoryEntry { \
                             id: $id, app_name: $app_name, user_id: $user_id, \
                             session_id: $session_id, content: $content, \
                             content_text: $content_text, author: $author, \
                             timestamp: $timestamp, embedding: $embedding, \
                             project_id: null \
                         })",
                    )
                    .param("session_id", session_id.to_string())
                    .param("app_name", app_name.to_string())
                    .param("user_id", user_id.to_string())
                    .param("id", entry_id)
                    .param("content", content_json)
                    .param("content_text", content_text.clone())
                    .param("author", entry.author.clone())
                    .param("timestamp", timestamp_str)
                    .param("embedding", embedding_f64),
                )
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("add_session failed: {e}")))?;
            } else {
                txn.run(
                    neo4rs::query(
                        "MATCH (s:MemorySession {session_id: $session_id, \
                         app_name: $app_name, user_id: $user_id}) \
                         CREATE (s)-[:FROM_SESSION]->(e:MemoryEntry { \
                             id: $id, app_name: $app_name, user_id: $user_id, \
                             session_id: $session_id, content: $content, \
                             content_text: $content_text, author: $author, \
                             timestamp: $timestamp, \
                             project_id: null \
                         })",
                    )
                    .param("session_id", session_id.to_string())
                    .param("app_name", app_name.to_string())
                    .param("user_id", user_id.to_string())
                    .param("id", entry_id)
                    .param("content", content_json)
                    .param("content_text", content_text.clone())
                    .param("author", entry.author.clone())
                    .param("timestamp", timestamp_str),
                )
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("add_session failed: {e}")))?;
            }
        }

        // Create FOLLOWS relationships between consecutive entries
        for i in 0..entry_ids.len().saturating_sub(1) {
            txn.run(
                neo4rs::query(
                    "MATCH (prev:MemoryEntry {id: $prev_id}) \
                     MATCH (curr:MemoryEntry {id: $curr_id}) \
                     CREATE (prev)-[:FOLLOWS]->(curr)",
                )
                .param("prev_id", entry_ids[i].clone())
                .param("curr_id", entry_ids[i + 1].clone()),
            )
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!("add_session failed: FOLLOWS creation: {e}"))
            })?;
        }

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("commit failed: {e}")))?;

        Ok(())
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

        let mut txn = self
            .graph
            .start_txn()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("transaction failed: {e}")))?;

        // MERGE the MemorySession node
        txn.run(
            neo4rs::query(
                "MERGE (:MemorySession {session_id: $session_id, \
                 app_name: $app_name, user_id: $user_id})",
            )
            .param("session_id", session_id.to_string())
            .param("app_name", app_name.to_string())
            .param("user_id", user_id.to_string()),
        )
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("add_session_to_project failed: {e}")))?;

        // Create MemoryEntry nodes with project_id and FROM_SESSION relationships
        let mut entry_ids: Vec<String> = Vec::with_capacity(entries.len());

        for (i, entry) in entries.iter().enumerate() {
            let entry_id = format!("{session_id}_{i}");
            entry_ids.push(entry_id.clone());

            let content_json = serde_json::to_string(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            let content_text = &texts[i];
            let timestamp_str = entry.timestamp.to_rfc3339();

            if let Some(ref embs) = embeddings {
                let embedding_f64: Vec<f64> = embs[i].iter().map(|&v| v as f64).collect();

                txn.run(
                    neo4rs::query(
                        "MATCH (s:MemorySession {session_id: $session_id, \
                         app_name: $app_name, user_id: $user_id}) \
                         CREATE (s)-[:FROM_SESSION]->(e:MemoryEntry { \
                             id: $id, app_name: $app_name, user_id: $user_id, \
                             session_id: $session_id, content: $content, \
                             content_text: $content_text, author: $author, \
                             timestamp: $timestamp, embedding: $embedding, \
                             project_id: $project_id \
                         })",
                    )
                    .param("session_id", session_id.to_string())
                    .param("app_name", app_name.to_string())
                    .param("user_id", user_id.to_string())
                    .param("id", entry_id)
                    .param("content", content_json)
                    .param("content_text", content_text.clone())
                    .param("author", entry.author.clone())
                    .param("timestamp", timestamp_str)
                    .param("embedding", embedding_f64)
                    .param("project_id", project_id.to_string()),
                )
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!("add_session_to_project failed: {e}"))
                })?;
            } else {
                txn.run(
                    neo4rs::query(
                        "MATCH (s:MemorySession {session_id: $session_id, \
                         app_name: $app_name, user_id: $user_id}) \
                         CREATE (s)-[:FROM_SESSION]->(e:MemoryEntry { \
                             id: $id, app_name: $app_name, user_id: $user_id, \
                             session_id: $session_id, content: $content, \
                             content_text: $content_text, author: $author, \
                             timestamp: $timestamp, \
                             project_id: $project_id \
                         })",
                    )
                    .param("session_id", session_id.to_string())
                    .param("app_name", app_name.to_string())
                    .param("user_id", user_id.to_string())
                    .param("id", entry_id)
                    .param("content", content_json)
                    .param("content_text", content_text.clone())
                    .param("author", entry.author.clone())
                    .param("timestamp", timestamp_str)
                    .param("project_id", project_id.to_string()),
                )
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!("add_session_to_project failed: {e}"))
                })?;
            }
        }

        // Create FOLLOWS relationships between consecutive entries
        for i in 0..entry_ids.len().saturating_sub(1) {
            txn.run(
                neo4rs::query(
                    "MATCH (prev:MemoryEntry {id: $prev_id}) \
                     MATCH (curr:MemoryEntry {id: $curr_id}) \
                     CREATE (prev)-[:FOLLOWS]->(curr)",
                )
                .param("prev_id", entry_ids[i].clone())
                .param("curr_id", entry_ids[i + 1].clone()),
            )
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!(
                    "add_session_to_project failed: FOLLOWS creation: {e}"
                ))
            })?;
        }

        txn.commit()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("commit failed: {e}")))?;

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

        let content_text = crate::text::extract_text(&entry.content);
        let content_json = serde_json::to_string(&entry.content)
            .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
        let timestamp_str = entry.timestamp.to_rfc3339();
        let entry_id = format!("entry_{}", chrono::Utc::now().timestamp_millis());

        if let Some(ref provider) = self.embedding_provider {
            let text_for_embed =
                if content_text.is_empty() { " ".to_string() } else { content_text.clone() };
            let embeddings = provider.embed(&[text_for_embed]).await.map_err(|e| {
                adk_core::AdkError::memory(format!("embedding generation failed: {e}"))
            })?;
            let embedding_f64: Vec<f64> = embeddings[0].iter().map(|&v| v as f64).collect();

            self.graph
                .run(
                    neo4rs::query(
                        "CREATE (e:MemoryEntry { \
                             id: $id, app_name: $app_name, user_id: $user_id, \
                             content: $content, content_text: $content_text, \
                             author: $author, timestamp: $timestamp, \
                             embedding: $embedding, project_id: $project_id \
                         })",
                    )
                    .param("id", entry_id)
                    .param("app_name", app_name.to_string())
                    .param("user_id", user_id.to_string())
                    .param("content", content_json)
                    .param("content_text", content_text)
                    .param("author", entry.author.clone())
                    .param("timestamp", timestamp_str)
                    .param("embedding", embedding_f64)
                    .param("project_id", project_id.to_string()),
                )
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!("add_entry_to_project failed: {e}"))
                })?;
        } else {
            self.graph
                .run(
                    neo4rs::query(
                        "CREATE (e:MemoryEntry { \
                             id: $id, app_name: $app_name, user_id: $user_id, \
                             content: $content, content_text: $content_text, \
                             author: $author, timestamp: $timestamp, \
                             project_id: $project_id \
                         })",
                    )
                    .param("id", entry_id)
                    .param("app_name", app_name.to_string())
                    .param("user_id", user_id.to_string())
                    .param("content", content_json)
                    .param("content_text", content_text)
                    .param("author", entry.author.clone())
                    .param("timestamp", timestamp_str)
                    .param("project_id", project_id.to_string()),
                )
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!("add_entry_to_project failed: {e}"))
                })?;
        }

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10) as i64;

        // Build the project_id filter clause
        let project_filter = match &req.project_id {
            None => "AND node.project_id IS NULL".to_string(),
            Some(_) => "AND (node.project_id IS NULL OR node.project_id = $project_id)".to_string(),
        };

        let results = if let Some(ref provider) = self.embedding_provider {
            // Vector search via db.index.vector.queryNodes
            let query_embedding = provider
                .embed(std::slice::from_ref(&req.query))
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("query embedding failed: {e}")))?;
            let query_vec: Vec<f64> = query_embedding[0].iter().map(|&v| v as f64).collect();

            let cypher = format!(
                "CALL db.index.vector.queryNodes('memory_embedding', $limit, \
                 $query_embedding) \
                 YIELD node, score \
                 WHERE node.app_name = $app_name AND node.user_id = $user_id \
                 {project_filter} \
                 OPTIONAL MATCH (node)-[:FOLLOWS]-(adjacent:MemoryEntry) \
                 RETURN node.id AS id, node.content AS content, \
                        node.author AS author, node.timestamp AS timestamp, \
                        score, \
                        collect(adjacent.id) AS adj_ids, \
                        collect(adjacent.content) AS adj_contents, \
                        collect(adjacent.author) AS adj_authors, \
                        collect(adjacent.timestamp) AS adj_timestamps \
                 ORDER BY score DESC"
            );

            let mut query = neo4rs::query(&cypher)
                .param("limit", limit)
                .param("query_embedding", query_vec)
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone());

            if let Some(ref pid) = req.project_id {
                query = query.param("project_id", pid.clone());
            }

            let mut row_stream = self
                .graph
                .execute(query)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?;

            let mut entries = Vec::new();
            let mut seen_ids: HashSet<String> = HashSet::new();

            while let Some(row) = row_stream
                .next()
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?
            {
                // Add the primary match
                if let Some(entry) = row_to_memory_entry(&row) {
                    let id = row.get::<String>("id").unwrap_or_default();
                    if seen_ids.insert(id) {
                        entries.push(entry);
                    }
                }

                // Add adjacent entries from FOLLOWS traversal
                collect_adjacent_entries(&row, &mut seen_ids, &mut entries);
            }

            entries
        } else {
            // Full-text search fallback
            let cypher = format!(
                "CALL db.index.fulltext.queryNodes('memory_content', $query) \
                 YIELD node, score \
                 WHERE node.app_name = $app_name AND node.user_id = $user_id \
                 {project_filter} \
                 OPTIONAL MATCH (node)-[:FOLLOWS]-(adjacent:MemoryEntry) \
                 RETURN node.id AS id, node.content AS content, \
                        node.author AS author, node.timestamp AS timestamp, \
                        score, \
                        collect(adjacent.id) AS adj_ids, \
                        collect(adjacent.content) AS adj_contents, \
                        collect(adjacent.author) AS adj_authors, \
                        collect(adjacent.timestamp) AS adj_timestamps \
                 ORDER BY score DESC \
                 LIMIT $limit"
            );

            let mut query = neo4rs::query(&cypher)
                .param("query", req.query.clone())
                .param("app_name", req.app_name.clone())
                .param("user_id", req.user_id.clone())
                .param("limit", limit);

            if let Some(ref pid) = req.project_id {
                query = query.param("project_id", pid.clone());
            }

            let mut row_stream = self
                .graph
                .execute(query)
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?;

            let mut entries = Vec::new();
            let mut seen_ids: HashSet<String> = HashSet::new();

            while let Some(row) = row_stream
                .next()
                .await
                .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?
            {
                // Add the primary match
                if let Some(entry) = row_to_memory_entry(&row) {
                    let id = row.get::<String>("id").unwrap_or_default();
                    if seen_ids.insert(id) {
                        entries.push(entry);
                    }
                }

                // Add adjacent entries from FOLLOWS traversal
                collect_adjacent_entries(&row, &mut seen_ids, &mut entries);
            }

            entries
        };

        Ok(SearchResponse { memories: results })
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

        // Use full-text search to find matching entries, then delete those in the project
        let mut row_stream = self
            .graph
            .execute(
                neo4rs::query(
                    "CALL db.index.fulltext.queryNodes('memory_content', $query) \
                     YIELD node \
                     WHERE node.app_name = $app_name AND node.user_id = $user_id \
                     AND node.project_id = $project_id \
                     DETACH DELETE node \
                     RETURN count(node) AS deleted_count",
                )
                .param("query", query.to_string())
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string())
                .param("project_id", project_id.to_string()),
            )
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!("delete_entries_in_project failed: {e}"))
            })?;

        let count = if let Some(row) = row_stream.next().await.map_err(|e| {
            adk_core::AdkError::memory(format!("delete_entries_in_project failed: {e}"))
        })? {
            row.get::<i64>("deleted_count").unwrap_or(0) as u64
        } else {
            0
        };

        Ok(count)
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        validate_project_id(project_id)?;

        let mut row_stream = self
            .graph
            .execute(
                neo4rs::query(
                    "MATCH (e:MemoryEntry {app_name: $app_name, user_id: $user_id, \
                     project_id: $project_id}) \
                     DETACH DELETE e \
                     RETURN count(e) AS deleted_count",
                )
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string())
                .param("project_id", project_id.to_string()),
            )
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_project failed: {e}")))?;

        let count = if let Some(row) = row_stream
            .next()
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_project failed: {e}")))?
        {
            row.get::<i64>("deleted_count").unwrap_or(0) as u64
        } else {
            0
        };

        // Clean up orphaned session nodes
        self.graph
            .run(
                neo4rs::query(
                    "MATCH (s:MemorySession {app_name: $app_name, user_id: $user_id}) \
                     WHERE NOT (s)-[:FROM_SESSION]->() \
                     DELETE s",
                )
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string()),
            )
            .await
            .map_err(|e| {
                adk_core::AdkError::memory(format!("delete_project cleanup failed: {e}"))
            })?;

        Ok(count)
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        self.graph
            .run(
                neo4rs::query(
                    "MATCH (e:MemoryEntry {app_name: $app_name, user_id: $user_id}) \
                     DETACH DELETE e",
                )
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string()),
            )
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_user failed: {e}")))?;

        // Clean up orphaned session nodes
        self.graph
            .run(
                neo4rs::query(
                    "MATCH (s:MemorySession {app_name: $app_name, user_id: $user_id}) \
                     WHERE NOT (s)-[:FROM_SESSION]->() \
                     DELETE s",
                )
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string()),
            )
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_user cleanup failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id))]
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        self.graph
            .run(
                neo4rs::query(
                    "MATCH (e:MemoryEntry {app_name: $app_name, user_id: $user_id, session_id: $session_id}) \
                     DETACH DELETE e",
                )
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string())
                .param("session_id", session_id.to_string()),
            )
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_session failed: {e}")))?;

        // Clean up orphaned session node
        self.graph
            .run(
                neo4rs::query(
                    "MATCH (s:MemorySession {session_id: $session_id, app_name: $app_name, user_id: $user_id}) \
                     WHERE NOT (s)-[:FROM_SESSION]->() \
                     DELETE s",
                )
                .param("app_name", app_name.to_string())
                .param("user_id", user_id.to_string())
                .param("session_id", session_id.to_string()),
            )
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_session cleanup failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        let _ = self
            .graph
            .execute(neo4rs::query("RETURN 1"))
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}

/// Convert a Neo4j row to a `MemoryEntry` from the primary node columns.
fn row_to_memory_entry(row: &neo4rs::Row) -> Option<MemoryEntry> {
    let content_str = row.get::<String>("content").ok()?;
    let content: adk_core::Content = serde_json::from_str(&content_str)
        .unwrap_or_else(|_| adk_core::Content { role: "user".to_string(), parts: vec![] });
    let author = row.get::<String>("author").unwrap_or_else(|_| "unknown".to_string());
    let timestamp_str = row.get::<String>("timestamp").unwrap_or_default();
    let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_default();

    Some(MemoryEntry { content, author, timestamp })
}

/// Collect adjacent entries from FOLLOWS traversal, deduplicating by ID.
fn collect_adjacent_entries(
    row: &neo4rs::Row,
    seen_ids: &mut HashSet<String>,
    entries: &mut Vec<MemoryEntry>,
) {
    let adj_ids: Vec<String> = row.get("adj_ids").unwrap_or_default();
    let adj_contents: Vec<String> = row.get("adj_contents").unwrap_or_default();
    let adj_authors: Vec<String> = row.get("adj_authors").unwrap_or_default();
    let adj_timestamps: Vec<String> = row.get("adj_timestamps").unwrap_or_default();

    for (i, adj_id) in adj_ids.iter().enumerate() {
        if !seen_ids.insert(adj_id.clone()) {
            continue;
        }

        let content_str = adj_contents.get(i).cloned().unwrap_or_default();
        let content: adk_core::Content = serde_json::from_str(&content_str)
            .unwrap_or_else(|_| adk_core::Content { role: "user".to_string(), parts: vec![] });
        let author = adj_authors.get(i).cloned().unwrap_or_else(|| "unknown".to_string());
        let timestamp_str = adj_timestamps.get(i).cloned().unwrap_or_default();
        let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_default();

        entries.push(MemoryEntry { content, author, timestamp });
    }
}
