//! Agent registry storage backend.
//!
//! Defines the [`AgentRegistryStore`] async trait and provides an
//! [`InMemoryAgentRegistryStore`] default implementation.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::types::AgentCard;

/// Filter criteria for listing agent cards.
///
/// All fields are optional. When multiple fields are set, they are combined
/// with AND semantics — a card must match all specified criteria.
///
/// # Example
///
/// ```rust
/// use adk_server::registry::store::AgentFilter;
///
/// let filter = AgentFilter {
///     name_prefix: Some("my-".to_string()),
///     tag: Some("search".to_string()),
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Default, Clone)]
pub struct AgentFilter {
    /// Match agents whose name starts with this prefix.
    pub name_prefix: Option<String>,
    /// Match agents that contain this tag.
    pub tag: Option<String>,
    /// Match agents whose version falls within this range (reserved for future use).
    pub version_range: Option<String>,
}

/// Async trait for agent registry storage backends.
///
/// Implementations must be `Send + Sync` to support concurrent access
/// from Axum route handlers.
#[async_trait]
pub trait AgentRegistryStore: Send + Sync {
    /// Insert a new agent card into the store.
    ///
    /// Returns an error if an agent with the same name already exists.
    async fn insert(&self, card: AgentCard) -> adk_core::Result<()>;

    /// Retrieve an agent card by name.
    ///
    /// Returns `Ok(None)` if no agent with the given name exists.
    async fn get(&self, name: &str) -> adk_core::Result<Option<AgentCard>>;

    /// List agent cards matching the given filter.
    ///
    /// An empty filter returns all cards.
    async fn list(&self, filter: &AgentFilter) -> adk_core::Result<Vec<AgentCard>>;

    /// Delete an agent card by name.
    ///
    /// Returns `true` if the agent was found and removed, `false` if it did not exist.
    async fn delete(&self, name: &str) -> adk_core::Result<bool>;

    /// Check whether an agent with the given name and version exists.
    async fn exists(&self, name: &str, version: &str) -> adk_core::Result<bool>;
}

/// In-memory implementation of [`AgentRegistryStore`].
///
/// Uses `Arc<RwLock<HashMap<String, AgentCard>>>` for thread-safe concurrent access.
/// Suitable for development and testing. For production use, implement a persistent
/// backend (e.g. PostgreSQL, DynamoDB).
///
/// # Example
///
/// ```rust
/// use adk_server::registry::store::InMemoryAgentRegistryStore;
///
/// let store = InMemoryAgentRegistryStore::new();
/// ```
#[derive(Debug, Clone)]
pub struct InMemoryAgentRegistryStore {
    cards: Arc<RwLock<HashMap<String, AgentCard>>>,
}

impl InMemoryAgentRegistryStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self { cards: Arc::new(RwLock::new(HashMap::new())) }
    }
}

impl Default for InMemoryAgentRegistryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRegistryStore for InMemoryAgentRegistryStore {
    async fn insert(&self, card: AgentCard) -> adk_core::Result<()> {
        let mut store = self.cards.write().await;
        if store.contains_key(&card.name) {
            return Err(adk_core::AdkError::agent(format!(
                "agent '{}' already exists in registry",
                card.name
            )));
        }
        store.insert(card.name.clone(), card);
        Ok(())
    }

    async fn get(&self, name: &str) -> adk_core::Result<Option<AgentCard>> {
        let store = self.cards.read().await;
        Ok(store.get(name).cloned())
    }

    async fn list(&self, filter: &AgentFilter) -> adk_core::Result<Vec<AgentCard>> {
        let store = self.cards.read().await;
        let cards = store.values().filter(|card| matches_filter(card, filter)).cloned().collect();
        Ok(cards)
    }

    async fn delete(&self, name: &str) -> adk_core::Result<bool> {
        let mut store = self.cards.write().await;
        Ok(store.remove(name).is_some())
    }

    async fn exists(&self, name: &str, version: &str) -> adk_core::Result<bool> {
        let store = self.cards.read().await;
        Ok(store.get(name).is_some_and(|card| card.version == version))
    }
}

/// Check whether an agent card matches the given filter criteria.
fn matches_filter(card: &AgentCard, filter: &AgentFilter) -> bool {
    if let Some(prefix) = &filter.name_prefix {
        if !card.name.starts_with(prefix.as_str()) {
            return false;
        }
    }

    if let Some(tag) = &filter.tag {
        if !card.tags.contains(tag) {
            return false;
        }
    }

    // version_range is reserved for future use — currently a no-op
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_card(name: &str, version: &str, tags: Vec<&str>) -> AgentCard {
        AgentCard {
            name: name.to_string(),
            version: version.to_string(),
            description: None,
            tags: tags.into_iter().map(String::from).collect(),
            endpoint_url: None,
            capabilities: vec![],
            input_modes: vec![],
            output_modes: vec![],
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn test_insert_and_get() {
        let store = InMemoryAgentRegistryStore::new();
        let card = make_card("agent-a", "1.0.0", vec!["search"]);

        store.insert(card.clone()).await.unwrap();
        let retrieved = store.get("agent-a").await.unwrap();
        assert_eq!(retrieved, Some(card));
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let store = InMemoryAgentRegistryStore::new();
        let result = store.get("nonexistent").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_insert_duplicate_returns_error() {
        let store = InMemoryAgentRegistryStore::new();
        let card = make_card("agent-a", "1.0.0", vec![]);

        store.insert(card.clone()).await.unwrap();
        let result = store.insert(card).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_existing() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(make_card("agent-a", "1.0.0", vec![])).await.unwrap();

        let deleted = store.delete("agent-a").await.unwrap();
        assert!(deleted);
        assert_eq!(store.get("agent-a").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let store = InMemoryAgentRegistryStore::new();
        let deleted = store.delete("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_exists() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(make_card("agent-a", "1.0.0", vec![])).await.unwrap();

        assert!(store.exists("agent-a", "1.0.0").await.unwrap());
        assert!(!store.exists("agent-a", "2.0.0").await.unwrap());
        assert!(!store.exists("nonexistent", "1.0.0").await.unwrap());
    }

    #[tokio::test]
    async fn test_list_no_filter() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(make_card("agent-a", "1.0.0", vec!["search"])).await.unwrap();
        store.insert(make_card("agent-b", "2.0.0", vec!["qa"])).await.unwrap();

        let all = store.list(&AgentFilter::default()).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_list_filter_by_name_prefix() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(make_card("search-agent", "1.0.0", vec![])).await.unwrap();
        store.insert(make_card("search-bot", "1.0.0", vec![])).await.unwrap();
        store.insert(make_card("qa-agent", "1.0.0", vec![])).await.unwrap();

        let filter = AgentFilter { name_prefix: Some("search-".to_string()), ..Default::default() };
        let results = store.list(&filter).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|c| c.name.starts_with("search-")));
    }

    #[tokio::test]
    async fn test_list_filter_by_tag() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(make_card("agent-a", "1.0.0", vec!["search", "qa"])).await.unwrap();
        store.insert(make_card("agent-b", "1.0.0", vec!["qa"])).await.unwrap();
        store.insert(make_card("agent-c", "1.0.0", vec!["chat"])).await.unwrap();

        let filter = AgentFilter { tag: Some("qa".to_string()), ..Default::default() };
        let results = store.list(&filter).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|c| c.tags.contains(&"qa".to_string())));
    }

    #[tokio::test]
    async fn test_list_filter_combined() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(make_card("search-agent", "1.0.0", vec!["search", "qa"])).await.unwrap();
        store.insert(make_card("search-bot", "1.0.0", vec!["search"])).await.unwrap();
        store.insert(make_card("qa-agent", "1.0.0", vec!["qa"])).await.unwrap();

        let filter = AgentFilter {
            name_prefix: Some("search-".to_string()),
            tag: Some("qa".to_string()),
            ..Default::default()
        };
        let results = store.list(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "search-agent");
    }
}
