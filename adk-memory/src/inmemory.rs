use crate::service::*;
use adk_core::Result;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MemoryKey {
    app_name: String,
    user_id: String,
}

#[derive(Clone)]
struct StoredEntry {
    entry: MemoryEntry,
    words: HashSet<String>,
    project_id: Option<String>,
}

type MemoryStore = HashMap<MemoryKey, HashMap<String, Vec<StoredEntry>>>;

/// The session key entries added without a session are kept under.
const DIRECT_SESSION: &str = "__direct__";

/// In-process memory with keyword search, for development and tests.
///
/// Search tokenizes the query and every entry with [`extract_words`](crate::text::extract_words),
/// keeps entries that share at least one word with the query, scores each by the share of
/// query words it contains (the score `min_score` filters on), and returns the best `limit`
/// matches, newest first among equal scores. Entries without words, such as an image, are
/// stored and listed but never match a query.
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let service = InMemoryMemoryService::new();
/// let entry = MemoryEntry {
///     content: Content::new("user").with_text("Hometown: Nairobi."),
///     author: "user".to_string(),
///     timestamp: chrono::Utc::now(),
/// };
/// service.add_session("app", "ada", "s1", vec![entry]).await?;
/// let found = service
///     .search(SearchRequest {
///         query: "nairobi".to_string(),
///         user_id: "ada".to_string(),
///         app_name: "app".to_string(),
///         limit: None,
///         min_score: None,
///         project_id: None,
///     })
///     .await?;
/// assert_eq!(found.memories.len(), 1);
/// # Ok(())
/// # }
/// ```
pub struct InMemoryMemoryService {
    store: Arc<RwLock<MemoryStore>>,
}

impl InMemoryMemoryService {
    pub fn new() -> Self {
        Self { store: Arc::new(RwLock::new(HashMap::new())) }
    }

    fn has_intersection(set1: &HashSet<String>, set2: &HashSet<String>) -> bool {
        if set1.is_empty() || set2.is_empty() {
            return false;
        }
        set1.iter().any(|word| set2.contains(word))
    }
}

impl Default for InMemoryMemoryService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryService for InMemoryMemoryService {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };

        let stored_entries: Vec<StoredEntry> = entries
            .into_iter()
            .map(|entry| {
                let words = crate::text::extract_words_from_content(&entry.content);
                StoredEntry { entry, words, project_id: None }
            })
            .collect();

        if stored_entries.is_empty() {
            return Ok(());
        }

        let mut store = self.store.write().unwrap();
        let sessions = store.entry(key).or_default();
        sessions.insert(session_id.to_string(), stored_entries);

        Ok(())
    }

    fn supports_project_scoping(&self) -> bool {
        true
    }

    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        validate_project_id(project_id)?;

        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };

        let stored_entries: Vec<StoredEntry> = entries
            .into_iter()
            .map(|entry| {
                let words = crate::text::extract_words_from_content(&entry.content);
                StoredEntry { entry, words, project_id: Some(project_id.to_string()) }
            })
            .collect();

        if stored_entries.is_empty() {
            return Ok(());
        }

        let mut store = self.store.write().unwrap();
        let sessions = store.entry(key).or_default();
        sessions.insert(session_id.to_string(), stored_entries);

        Ok(())
    }

    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()> {
        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };
        let words = crate::text::extract_words_from_content(&entry.content);
        let stored = StoredEntry { entry, words, project_id: None };

        let mut store = self.store.write().unwrap();
        let sessions = store.entry(key).or_default();
        sessions.entry(DIRECT_SESSION.to_string()).or_default().push(stored);

        Ok(())
    }

    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        validate_project_id(project_id)?;

        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };
        let words = crate::text::extract_words_from_content(&entry.content);
        let stored = StoredEntry { entry, words, project_id: Some(project_id.to_string()) };

        let mut store = self.store.write().unwrap();
        let sessions = store.entry(key).or_default();
        sessions.entry(DIRECT_SESSION.to_string()).or_default().push(stored);

        Ok(())
    }

    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64> {
        let query_words = crate::text::extract_words(query);
        if query_words.is_empty() {
            return Ok(0);
        }

        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };

        let mut store = self.store.write().unwrap();
        let sessions = match store.get_mut(&key) {
            Some(s) => s,
            None => return Ok(0),
        };

        let mut removed: u64 = 0;
        for entries in sessions.values_mut() {
            let before = entries.len();
            entries.retain(|stored| {
                // Only delete global entries (project_id is None)
                stored.project_id.is_some() || !Self::has_intersection(&stored.words, &query_words)
            });
            removed += (before - entries.len()) as u64;
        }

        Ok(removed)
    }

    async fn list_recent(
        &self,
        app_name: &str,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };
        let store = self.store.read().unwrap();
        let mut entries: Vec<MemoryEntry> = store
            .get(&key)
            .map(|sessions| {
                sessions.values().flatten().map(|stored| stored.entry.clone()).collect()
            })
            .unwrap_or_default();
        // Newest first. `Reverse` keeps this a key-based sort, which clippy prefers, without
        // flipping the comparison by hand.
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));
        entries.truncate(limit);
        Ok(entries)
    }

    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        let query_words = crate::text::extract_words(query);
        if query_words.is_empty() {
            return Ok(0);
        }

        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };

        let mut store = self.store.write().unwrap();
        let sessions = match store.get_mut(&key) {
            Some(s) => s,
            None => return Ok(0),
        };

        let mut removed: u64 = 0;
        for entries in sessions.values_mut() {
            let before = entries.len();
            entries.retain(|stored| {
                // Only delete entries matching the given project
                stored.project_id.as_deref() != Some(project_id)
                    || !Self::has_intersection(&stored.words, &query_words)
            });
            removed += (before - entries.len()) as u64;
        }

        Ok(removed)
    }

    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };

        let mut store = self.store.write().unwrap();
        let sessions = match store.get_mut(&key) {
            Some(s) => s,
            None => return Ok(0),
        };

        let mut removed: u64 = 0;
        for entries in sessions.values_mut() {
            let before = entries.len();
            entries.retain(|stored| stored.project_id.as_deref() != Some(project_id));
            removed += (before - entries.len()) as u64;
        }

        Ok(removed)
    }

    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };

        let mut store = self.store.write().unwrap();
        store.remove(&key);

        Ok(())
    }

    async fn export_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserMemory>> {
        let key = MemoryKey { app_name: app_name.to_string(), user_id: user_id.to_string() };
        let store = self.store.read().unwrap();
        let mut memories: Vec<UserMemory> = store
            .get(&key)
            .into_iter()
            .flatten()
            .flat_map(|(session_id, entries)| {
                entries.iter().map(move |stored| UserMemory {
                    entry: stored.entry.clone(),
                    session_id: (session_id != DIRECT_SESSION).then(|| session_id.clone()),
                    project_id: stored.project_id.clone(),
                })
            })
            .collect();
        memories.sort_by_key(|memory| memory.entry.timestamp);
        Ok(memories)
    }

    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let query_words = crate::text::extract_words(&req.query);
        let limit = req.limit.unwrap_or(10);
        let min_score = req.min_score.unwrap_or(0.0);

        let key = MemoryKey { app_name: req.app_name, user_id: req.user_id };

        let store = self.store.read().unwrap();
        let sessions = match store.get(&key) {
            Some(s) => s,
            None => return Ok(SearchResponse { memories: Vec::new() }),
        };

        let mut matches = Vec::new();
        for stored in sessions.values().flatten() {
            let in_scope = match (&stored.project_id, &req.project_id) {
                // Global entries are always in scope; project entries only in their project.
                (None, _) => true,
                (Some(entry_project), Some(project)) => entry_project == project,
                (Some(_), None) => false,
            };
            if !in_scope {
                continue;
            }
            let score = crate::text::match_score(&query_words, &stored.words);
            if score > 0.0 && score >= min_score {
                matches.push((score, stored.entry.clone()));
            }
        }

        Ok(SearchResponse { memories: crate::text::rank_matches(matches, limit) })
    }
}
