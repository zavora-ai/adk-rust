//! The in-memory [`AgendaStore`] (spec 3.2 §7.5, §13 Q8).

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::agenda::{AgendaFilter, AgendaItem, AgendaStateKind, AgendaStore};
use crate::error::Result;

/// An [`AgendaStore`] in process memory, for tests and a kernel with no durable store
/// (spec 3.2 §7.5). Its items end with the process.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::{AgendaFilter, AgendaItem, AgendaOrigin, AgendaStore, InMemoryAgendaStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryAgendaStore::new();
/// let now = chrono::Utc::now();
/// store.put(AgendaItem::new("b", SubjectRef::human("alice"), "t", "r", AgendaOrigin::User, now)).await?;
/// store.put(AgendaItem::new("a", SubjectRef::human("alice"), "t", "r", AgendaOrigin::User, now)).await?;
/// let ids: Vec<_> = store.list(&AgendaFilter::default()).await?.into_iter().map(|item| item.id).collect();
/// assert_eq!(ids, ["a", "b"]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryAgendaStore {
    items: RwLock<BTreeMap<String, AgendaItem>>,
}

impl InMemoryAgendaStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AgendaStore for InMemoryAgendaStore {
    async fn put(&self, item: AgendaItem) -> Result<()> {
        self.items.write().await.insert(item.id.clone(), item);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<AgendaItem>> {
        Ok(self.items.read().await.get(id).cloned())
    }

    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>> {
        let items = self.items.read().await;
        let mut matched: Vec<AgendaItem> =
            items.values().filter(|item| filter.matches(item)).cloned().collect();
        matched.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        if let Some(limit) = filter.limit {
            matched.truncate(limit);
        }
        Ok(matched)
    }

    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool> {
        let mut items = self.items.write().await;
        match items.get_mut(&item.id) {
            Some(stored) if stored.state.kind() == from => {
                *stored = item;
                Ok(true)
            }
            Some(_) | None => Ok(false),
        }
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        Ok(self.items.write().await.remove(id).is_some())
    }
}
