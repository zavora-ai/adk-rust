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

/// The outcome of a write that only a card's owner may make.
///
/// # Example
///
/// ```rust
/// use adk_server::registry::{AgentRegistryStore, InMemoryAgentRegistryStore, OwnerWrite};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = InMemoryAgentRegistryStore::new();
/// assert_eq!(store.delete_owned("absent", "alice").await?, OwnerWrite::NotFound);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerWrite {
    /// The card belonged to the caller and the write happened.
    Applied,
    /// No card has that name.
    NotFound,
    /// The card belongs to another user or has no owner; nothing changed.
    NotOwner,
}

/// Async trait for agent registry storage backends.
///
/// Implementations must be `Send + Sync` to support concurrent access
/// from Axum route handlers. A backend that persists cards stores
/// [`AgentCard::owner`]; a card stored without one reads back with `owner: None`, which the
/// routes treat as owned by no one.
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

    /// Delete an agent card by name, whoever owns it.
    ///
    /// Returns `true` if the agent was found and removed, `false` if it did not exist.
    async fn delete(&self, name: &str) -> adk_core::Result<bool>;

    /// Replace the card named `card.name` with `card` when the stored card's owner is
    /// `owner`.
    ///
    /// The owner check and the write must be one atomic step, so a card that another user
    /// registered under the same name after the caller read it is never overwritten. A card
    /// without an owner is never replaced.
    async fn replace_owned(&self, card: AgentCard, owner: &str) -> adk_core::Result<OwnerWrite>;

    /// Delete the card named `name` when its owner is `owner`.
    ///
    /// The owner check and the removal must be one atomic step, as in
    /// [`replace_owned`](Self::replace_owned). A card without an owner is never deleted.
    async fn delete_owned(&self, name: &str, owner: &str) -> adk_core::Result<OwnerWrite>;

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

    async fn replace_owned(&self, card: AgentCard, owner: &str) -> adk_core::Result<OwnerWrite> {
        let mut store = self.cards.write().await;
        let Some(stored) = store.get_mut(&card.name) else {
            return Ok(OwnerWrite::NotFound);
        };
        if stored.owner.as_deref() != Some(owner) {
            return Ok(OwnerWrite::NotOwner);
        }
        *stored = card;
        Ok(OwnerWrite::Applied)
    }

    async fn delete_owned(&self, name: &str, owner: &str) -> adk_core::Result<OwnerWrite> {
        let mut store = self.cards.write().await;
        match store.get(name) {
            None => Ok(OwnerWrite::NotFound),
            Some(card) if card.owner.as_deref() == Some(owner) => {
                store.remove(name);
                Ok(OwnerWrite::Applied)
            }
            Some(_) => Ok(OwnerWrite::NotOwner),
        }
    }
}

/// Check whether an agent card matches the given filter criteria.
fn matches_filter(card: &AgentCard, filter: &AgentFilter) -> bool {
    if let Some(prefix) = &filter.name_prefix
        && !card.name.starts_with(prefix.as_str())
    {
        return false;
    }

    if let Some(tag) = &filter.tag
        && !card.tags.contains(tag)
    {
        return false;
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
            owner: None,
        }
    }

    fn owned_card(name: &str, version: &str, owner: Option<&str>) -> AgentCard {
        AgentCard { owner: owner.map(String::from), ..make_card(name, version, vec![]) }
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

    #[tokio::test]
    async fn test_replace_owned_only_by_owner() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(owned_card("agent-a", "1.0.0", Some("alice"))).await.unwrap();

        let by_bob = store.replace_owned(owned_card("agent-a", "6.6.6", Some("bob")), "bob");
        assert_eq!(by_bob.await.unwrap(), OwnerWrite::NotOwner);
        assert_eq!(
            store.get("agent-a").await.unwrap(),
            Some(owned_card("agent-a", "1.0.0", Some("alice")))
        );

        let by_alice = store.replace_owned(owned_card("agent-a", "1.1.0", Some("alice")), "alice");
        assert_eq!(by_alice.await.unwrap(), OwnerWrite::Applied);
        assert_eq!(
            store.get("agent-a").await.unwrap(),
            Some(owned_card("agent-a", "1.1.0", Some("alice")))
        );

        let absent = store.replace_owned(owned_card("absent", "1.0.0", Some("alice")), "alice");
        assert_eq!(absent.await.unwrap(), OwnerWrite::NotFound);
    }

    #[tokio::test]
    async fn test_delete_owned_only_by_owner() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(owned_card("agent-a", "1.0.0", Some("alice"))).await.unwrap();

        assert_eq!(store.delete_owned("agent-a", "bob").await.unwrap(), OwnerWrite::NotOwner);
        assert!(store.get("agent-a").await.unwrap().is_some());
        assert_eq!(store.delete_owned("agent-a", "alice").await.unwrap(), OwnerWrite::Applied);
        assert_eq!(store.get("agent-a").await.unwrap(), None);
        assert_eq!(store.delete_owned("agent-a", "alice").await.unwrap(), OwnerWrite::NotFound);
    }

    #[tokio::test]
    async fn test_card_without_owner_is_changed_by_no_one() {
        let store = InMemoryAgentRegistryStore::new();
        store.insert(owned_card("legacy", "1.0.0", None)).await.unwrap();

        let replaced = store.replace_owned(owned_card("legacy", "2.0.0", Some("alice")), "alice");
        assert_eq!(replaced.await.unwrap(), OwnerWrite::NotOwner);
        assert_eq!(store.delete_owned("legacy", "").await.unwrap(), OwnerWrite::NotOwner);
        assert_eq!(store.get("legacy").await.unwrap(), Some(owned_card("legacy", "1.0.0", None)));
    }
}
