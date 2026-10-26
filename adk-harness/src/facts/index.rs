//! `FactIndex`: finding candidate facts for recall (spec 3.1 §7.15.2).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use adk_core::{Content, Part, Result};
use adk_memory::{EmbeddingProvider, MemoryEntry, MemoryService, SearchRequest};
use async_trait::async_trait;

use super::store::{FactEmbedding, FactStore};
use super::types::{FACT_AUTHOR_PREFIX, FactFilter, FactScope, MemoryFact};

/// Finds candidate facts for a query (spec 3.1 §7.15.2).
///
/// An index only finds candidates: recall reads each candidate back from the
/// [`FactStore`], which decides status, scope and forgetting, so a forgotten fact an
/// index still holds is never recalled (spec 3.1 §9.2 I44). `search` covers the facts of
/// `scope.project` and the user-wide facts of the same user, and returns `(fact id,
/// score)` pairs best first, scores in `0.0..=1.0`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_harness::{FactIndex, FactKind, FactScope, FactSource, FactStatus, FactStore, InMemoryFactStore, LocalFactIndex, MemoryFact, Provenance};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let store = Arc::new(InMemoryFactStore::new());
/// let index = LocalFactIndex::new(store.clone());
/// let scope = FactScope::new("app", "alice", Some("repo".into()));
/// let fact = MemoryFact::new(scope.clone(), FactKind::ProjectFact, "The tests need --features sqlite",
///     FactStatus::Assumed, Provenance::new(FactSource::AgentInferred, "s", "i"));
/// store.put(fact.clone()).await.unwrap();
/// index.upsert(&fact).await.unwrap();
/// let hits = index.search(&scope, "run the tests", 5).await.unwrap();
/// assert_eq!(hits[0].0, fact.id);
/// # });
/// ```
#[async_trait]
pub trait FactIndex: Send + Sync {
    /// Indexes `fact`, replacing an earlier entry for it.
    async fn upsert(&self, fact: &MemoryFact) -> Result<()>;
    /// Drops `fact` from the index.
    async fn remove(&self, fact: &MemoryFact) -> Result<()>;
    /// The facts of `scope` (its project and the user-wide ones) that match `query`, best
    /// first, at most `limit`.
    async fn search(
        &self,
        scope: &FactScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>>;
}

/// The share of `query_words` that `words` contains, as `InMemoryMemoryService` scores.
pub(crate) fn lexical_score(query_words: &HashSet<String>, words: &HashSet<String>) -> f32 {
    if query_words.is_empty() {
        return 0.0;
    }
    let matched = query_words.iter().filter(|word| words.contains(*word)).count();
    matched as f32 / query_words.len() as f32
}

/// The cosine similarity of `a` and `b`, or `None` when they differ in length or either
/// is zero.
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some((dot / (na.sqrt() * nb.sqrt())) as f32)
}

/// The id a fact's vector is stored under (spec 3.1 §7.32, I73): the embedder's
/// [`EmbeddingProvider::model_id`], or `unnamed/<dimensions>` for an embedder without one.
pub(crate) fn embedder_id(embedder: &dyn EmbeddingProvider) -> String {
    embedder
        .model_id()
        .map_or_else(|| format!("unnamed/{}", embedder.dimensions()), ToString::to_string)
}

fn best_first(mut scored: Vec<(String, f32)>, limit: usize) -> Vec<(String, f32)> {
    scored.retain(|(_, score)| *score > 0.0);
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    scored.truncate(limit);
    scored
}

/// The default [`FactIndex`]: recall computed in process over the [`FactStore`]
/// (spec 3.1 §7.15.2).
///
/// | Embedder | Score |
/// |---|---|
/// | set ([`with_embedder`](Self::with_embedder)) | cosine similarity of the query's embedding and each fact's, stored with the fact through [`FactStore::put_embedding`] |
/// | none | lexical: the share of query words a fact contains, as `InMemoryMemoryService` scores |
///
/// Each vector is stored with its embedder's id (spec 3.1 §7.32, I73). A fact without a
/// vector from this index's embedder — written before an embedder was set, or by another
/// embedder, even one whose vectors share the length — is embedded when a search first
/// needs it, so two models' vectors are never compared. When the embedder fails, the
/// search falls back to lexical scores.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// let index = adk_harness::LocalFactIndex::new(Arc::new(adk_harness::InMemoryFactStore::new()));
/// # let _ = index;
/// ```
pub struct LocalFactIndex {
    store: Arc<dyn FactStore>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
}

impl std::fmt::Debug for LocalFactIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalFactIndex").field("embedder", &self.embedder.is_some()).finish()
    }
}

impl LocalFactIndex {
    /// A lexical index over `store`.
    pub fn new(store: Arc<dyn FactStore>) -> Self {
        Self { store, embedder: None }
    }

    /// Scores by the cosine similarity of `embedder`'s vectors.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    fn lexical(facts: &[MemoryFact], query: &str, limit: usize) -> Vec<(String, f32)> {
        let query_words = adk_memory::text::extract_words(query);
        best_first(
            facts
                .iter()
                .map(|fact| {
                    let words = adk_memory::text::extract_words(&fact.text);
                    (fact.id.clone(), lexical_score(&query_words, &words))
                })
                .collect(),
            limit,
        )
    }

    async fn embedded(
        &self,
        embedder: &dyn EmbeddingProvider,
        scope: &FactScope,
        facts: &[MemoryFact],
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        let id = embedder_id(embedder);
        let query_vector = embedder
            .embed(&[query.to_string()])
            .await?
            .into_iter()
            .next()
            .filter(|vector| !vector.is_empty())
            .ok_or_else(|| adk_core::AdkError::memory("the embedder returned no vector"))?;
        let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
        for scope in [Some(scope.clone()), scope.project.as_ref().map(|_| scope.user_wide())]
            .into_iter()
            .flatten()
        {
            vectors.extend(
                self.store
                    .embeddings(&scope)
                    .await?
                    .into_iter()
                    .filter(|(_, embedding)| embedding.is_from(&id))
                    .map(|(fact, embedding)| (fact, embedding.vector)),
            );
        }
        let missing: Vec<&MemoryFact> = facts
            .iter()
            .filter(|fact| {
                vectors.get(&fact.id).is_none_or(|vector| vector.len() != query_vector.len())
            })
            .collect();
        if !missing.is_empty() {
            let texts: Vec<String> = missing.iter().map(|fact| fact.text.clone()).collect();
            let embedded = embedder.embed(&texts).await?;
            for (fact, vector) in missing.into_iter().zip(embedded) {
                let embedding = FactEmbedding::new(id.clone(), vector.clone());
                if let Err(error) = self.store.put_embedding(&fact.id, embedding).await {
                    tracing::debug!(error = %error, "could not store a fact embedding");
                }
                vectors.insert(fact.id.clone(), vector);
            }
        }
        Ok(best_first(
            facts
                .iter()
                .filter_map(|fact| {
                    let vector = vectors.get(&fact.id)?;
                    Some((fact.id.clone(), cosine(&query_vector, vector)?.max(0.0)))
                })
                .collect(),
            limit,
        ))
    }
}

#[async_trait]
impl FactIndex for LocalFactIndex {
    async fn upsert(&self, fact: &MemoryFact) -> Result<()> {
        let Some(embedder) = &self.embedder else {
            return Ok(());
        };
        let vector = embedder
            .embed(std::slice::from_ref(&fact.text))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| adk_core::AdkError::memory("the embedder returned no vector"))?;
        let embedding = FactEmbedding::new(embedder_id(embedder.as_ref()), vector);
        self.store.put_embedding(&fact.id, embedding).await
    }

    async fn remove(&self, _fact: &MemoryFact) -> Result<()> {
        // The store returns embeddings of live facts only, so nothing remains to drop.
        Ok(())
    }

    async fn search(
        &self,
        scope: &FactScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let facts = self.store.list(FactFilter::recall(scope)).await?;
        if facts.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(embedder) = &self.embedder {
            match self.embedded(embedder.as_ref(), scope, &facts, query, limit).await {
                Ok(hits) => return Ok(hits),
                Err(error) => {
                    tracing::warn!(error = %error, "fact embedding failed; recalling by words instead");
                }
            }
        }
        Ok(Self::lexical(&facts, query, limit))
    }
}

/// A [`FactIndex`] over any `adk_memory::MemoryService` backend (spec 3.1 §7.15.2):
/// vector search on PostgreSQL (pgvector), MongoDB and Neo4j, Vertex AI Memory Bank, FTS5
/// keyword search on SQLite, Redis.
///
/// Each fact is a `MemoryEntry` whose `author` is `adk-harness:fact:<id>`, in the fact's
/// project (user-wide facts are global entries), whose first text part is the fact text
/// and whose second is the fact id, so `delete_entries_in_project` with the id removes
/// exactly that entry. Search results map back to facts through the author. The backend
/// returns its hits best first without scores; the index passes its
/// [`min_score`](Self::with_min_score) to the backend and reports scores falling from 1.0
/// by rank, all above 0.5, so recall keeps the backend's order.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// let index = adk_harness::MemoryServiceIndex::new(Arc::new(adk_memory::InMemoryMemoryService::new()));
/// # let _ = index;
/// ```
pub struct MemoryServiceIndex {
    service: Arc<dyn MemoryService>,
    min_score: Option<f32>,
}

impl std::fmt::Debug for MemoryServiceIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryServiceIndex").field("min_score", &self.min_score).finish()
    }
}

impl MemoryServiceIndex {
    /// An index over `service`; the backend should report `supports_project_scoping`.
    pub fn new(service: Arc<dyn MemoryService>) -> Self {
        Self { service, min_score: None }
    }

    /// Passes `min_score` to the backend's search, for backends that score similarity.
    #[must_use]
    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = Some(min_score);
        self
    }

    fn entry(fact: &MemoryFact) -> MemoryEntry {
        MemoryEntry {
            content: Content {
                role: "user".to_string(),
                parts: vec![
                    Part::Text { text: fact.text.clone() },
                    Part::Text { text: fact.id.clone() },
                ],
            },
            author: format!("{FACT_AUTHOR_PREFIX}{}", fact.id),
            timestamp: fact.valid_from,
        }
    }
}

#[async_trait]
impl FactIndex for MemoryServiceIndex {
    async fn upsert(&self, fact: &MemoryFact) -> Result<()> {
        let (app, user) = (&fact.scope.app, &fact.scope.user);
        self.remove(fact).await?;
        match &fact.scope.project {
            Some(project) => {
                self.service.add_entry_to_project(app, user, project, Self::entry(fact)).await
            }
            None => self.service.add_entry(app, user, Self::entry(fact)).await,
        }
    }

    async fn remove(&self, fact: &MemoryFact) -> Result<()> {
        let (app, user) = (&fact.scope.app, &fact.scope.user);
        match &fact.scope.project {
            Some(project) => {
                self.service.delete_entries_in_project(app, user, project, &fact.id).await?
            }
            None => self.service.delete_entries(app, user, &fact.id).await?,
        };
        Ok(())
    }

    async fn search(
        &self,
        scope: &FactScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let response = self
            .service
            .search(SearchRequest {
                query: query.to_string(),
                user_id: scope.user.clone(),
                app_name: scope.app.clone(),
                limit: Some(limit),
                min_score: self.min_score,
                project_id: scope.project.clone(),
            })
            .await?;
        let mut seen = HashSet::new();
        let ids: Vec<String> = response
            .memories
            .into_iter()
            .filter_map(|entry| entry.author.strip_prefix(FACT_AUTHOR_PREFIX).map(str::to_string))
            .filter(|id| seen.insert(id.clone()))
            .take(limit)
            .collect();
        let count = ids.len().max(1) as f32;
        Ok(ids
            .into_iter()
            .enumerate()
            .map(|(rank, id)| (id, 1.0 - 0.5 * rank as f32 / count))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_scores_are_the_share_of_query_words() {
        let words = |text: &str| adk_memory::text::extract_words(text);
        assert_eq!(
            lexical_score(&words("run the tests"), &words("the tests need sqlite")),
            2.0 / 3.0
        );
        assert_eq!(lexical_score(&words(""), &words("x")), 0.0);
    }

    #[test]
    fn cosine_needs_equal_non_zero_vectors() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), Some(0.0));
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), None);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), None);
    }
}
