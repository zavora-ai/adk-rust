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
}

type MemoryStore = HashMap<MemoryKey, HashMap<String, Vec<StoredEntry>>>;

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
                StoredEntry { entry, words }
            })
            .filter(|e| !e.words.is_empty())
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
        let stored = StoredEntry { entry, words };

        let mut store = self.store.write().unwrap();
        let sessions = store.entry(key).or_default();
        sessions.entry("__direct__".to_string()).or_default().push(stored);

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
            entries.retain(|stored| !Self::has_intersection(&stored.words, &query_words));
            removed += (before - entries.len()) as u64;
        }

        Ok(removed)
    }

    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let query_words = crate::text::extract_words(&req.query);
        let limit = req.limit.unwrap_or(10);

        let key = MemoryKey { app_name: req.app_name, user_id: req.user_id };

        let store = self.store.read().unwrap();
        let sessions = match store.get(&key) {
            Some(s) => s,
            None => return Ok(SearchResponse { memories: Vec::new() }),
        };

        let mut memories = Vec::new();
        for stored_entries in sessions.values() {
            for stored in stored_entries {
                if Self::has_intersection(&stored.words, &query_words) {
                    memories.push(stored.entry.clone());
                }
            }
        }

        memories.truncate(limit);

        Ok(SearchResponse { memories })
    }
}
