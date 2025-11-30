// Re-export Event and EventActions from adk_core for unified type
pub use adk_core::{Event, EventActions};

/// Trait for accessing events in a session.
pub trait Events: Send + Sync {
    fn all(&self) -> Vec<Event>;
    fn len(&self) -> usize;
    fn at(&self, index: usize) -> Option<&Event>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
