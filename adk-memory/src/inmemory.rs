use crate::service::*;
use adk_core::{Part, Result};
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

pub struct InMemoryMemoryService {
    store: Arc<RwLock<HashMap<MemoryKey, HashMap<String, Vec<StoredEntry>>>>>,
}

impl InMemoryMemoryService {
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn extract_words(text: &str) -> HashSet<String> {
        text.split_whitespace()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect()
    }

    fn extract_words_from_content(content: &adk_core::Content) -> HashSet<String> {
        let mut words = HashSet::new();
        for part in &content.parts {
            if let Part::Text { text } = part {
                words.extend(Self::extract_words(text));
            }
        }
        words
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
    async fn add_session(&self, app_name: &str, user_id: &str, session_id: &str, entries: Vec<MemoryEntry>) -> Result<()> {
        let key = MemoryKey {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
        };

        let stored_entries: Vec<StoredEntry> = entries
            .into_iter()
            .map(|entry| {
                let words = Self::extract_words_from_content(&entry.content);
                StoredEntry { entry, words }
            })
            .filter(|e| !e.words.is_empty())
            .collect();

        if stored_entries.is_empty() {
            return Ok(());
        }

        let mut store = self.store.write().unwrap();
        let sessions = store.entry(key).or_insert_with(HashMap::new);
        sessions.insert(session_id.to_string(), stored_entries);

        Ok(())
    }

    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let query_words = Self::extract_words(&req.query);
        
        let key = MemoryKey {
            app_name: req.app_name,
            user_id: req.user_id,
        };

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

        Ok(SearchResponse { memories })
    }
}
