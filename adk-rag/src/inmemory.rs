//! In-memory vector store using cosine similarity.
//!
//! This module provides [`InMemoryVectorStore`], a zero-dependency vector store
//! backed by a `HashMap` protected by a `tokio::sync::RwLock`. It is suitable
//! for development, testing, and small-scale use cases.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::filter::Filter;
use crate::scope::Scope;
use crate::vectorstore::{CollectionSpec, VectorQuery, VectorStore};

/// Chunk ID → chunk, for one scope of one collection.
type ScopeChunks = HashMap<String, Chunk>;

/// An in-memory vector store using cosine similarity for search.
///
/// Collections are stored as nested `HashMap`s: collection name → scope → chunk ID → chunk.
/// All operations are async-safe via `tokio::sync::RwLock`.
///
/// # Example
///
/// ```rust
/// use adk_rag::{CollectionSpec, InMemoryVectorStore, Scope, VectorStore};
///
/// # #[tokio::main]
/// # async fn main() -> adk_rag::Result<()> {
/// let store = InMemoryVectorStore::new();
/// store.create_collection("docs", &CollectionSpec::new(384)).await?;
/// assert_eq!(store.count("docs", &Scope::single_tenant(), None).await?, 0);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryVectorStore {
    collections: RwLock<HashMap<String, HashMap<Scope, ScopeChunks>>>,
}

impl InMemoryVectorStore {
    /// Create a new empty in-memory vector store.
    pub fn new() -> Self {
        Self::default()
    }
}

fn missing_collection(collection: &str) -> RagError {
    RagError::VectorStoreError {
        backend: "InMemory".to_string(),
        message: format!("collection '{collection}' does not exist"),
    }
}

/// Compute cosine similarity between two vectors.
///
/// Both vectors are L2-normalized before computing the dot product.
/// Returns 0.0 if either vector has zero magnitude.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    async fn create_collection(&self, name: &str, spec: &CollectionSpec) -> Result<()> {
        spec.validate()?;
        let mut collections = self.collections.write().await;
        collections.entry(name.to_string()).or_default();
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let mut collections = self.collections.write().await;
        collections.remove(name);
        Ok(())
    }

    async fn upsert(&self, collection: &str, scope: &Scope, chunks: &[Chunk]) -> Result<()> {
        let mut collections = self.collections.write().await;
        let scopes =
            collections.get_mut(collection).ok_or_else(|| missing_collection(collection))?;
        let store = scopes.entry(scope.clone()).or_default();
        for chunk in chunks {
            store.insert(chunk.id.clone(), chunk.clone());
        }
        Ok(())
    }

    async fn delete(&self, collection: &str, scope: &Scope, ids: &[&str]) -> Result<()> {
        let mut collections = self.collections.write().await;
        let scopes =
            collections.get_mut(collection).ok_or_else(|| missing_collection(collection))?;
        if let Some(store) = scopes.get_mut(scope) {
            for id in ids {
                store.remove(*id);
            }
        }
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>> {
        if let Some(filter) = query.filter {
            filter.validate()?;
        }
        let collections = self.collections.read().await;
        let scopes = collections.get(collection).ok_or_else(|| missing_collection(collection))?;

        let mut scored: Vec<SearchResult> = scopes
            .get(scope)
            .into_iter()
            .flat_map(HashMap::values)
            .filter(|chunk| query.filter.is_none_or(|filter| filter.matches(chunk)))
            .map(|chunk| {
                let score = cosine_similarity(&chunk.embedding, query.embedding);
                SearchResult { chunk: chunk.clone(), score }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(query.top_k);
        Ok(scored)
    }

    async fn count(&self, collection: &str, scope: &Scope, filter: Option<&Filter>) -> Result<u64> {
        if let Some(filter) = filter {
            filter.validate()?;
        }
        let collections = self.collections.read().await;
        let scopes = collections.get(collection).ok_or_else(|| missing_collection(collection))?;
        let matching = scopes
            .get(scope)
            .into_iter()
            .flat_map(HashMap::values)
            .filter(|chunk| filter.is_none_or(|filter| filter.matches(chunk)))
            .count();
        Ok(matching as u64)
    }

    async fn delete_where(&self, collection: &str, scope: &Scope, filter: &Filter) -> Result<u64> {
        filter.validate()?;
        let mut collections = self.collections.write().await;
        let scopes =
            collections.get_mut(collection).ok_or_else(|| missing_collection(collection))?;
        let Some(store) = scopes.get_mut(scope) else {
            return Ok(0);
        };
        let before = store.len();
        store.retain(|_, chunk| !filter.matches(chunk));
        Ok((before - store.len()) as u64)
    }

    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64> {
        let mut collections = self.collections.write().await;
        let scopes =
            collections.get_mut(collection).ok_or_else(|| missing_collection(collection))?;
        Ok(scopes.remove(scope).map_or(0, |store| store.len() as u64))
    }
}
