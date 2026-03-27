//! Durable transaction journal, evidence storage, and safe memory indexing.
//!
//! These components preserve compaction-safe transaction continuity across ACP
//! stable `2026-01-30` and AP2 `v0.1-alpha` (`2026-03-22`) sessions while
//! keeping raw evidence out of transcript and semantic memory surfaces.

pub mod evidence_store;
pub mod memory_index;
pub mod session_state;
pub mod store;

pub use evidence_store::ArtifactBackedEvidenceStore;
pub use memory_index::PaymentMemoryIndex;
pub use session_state::{
    ACTIVE_INDEX_KEY, COMPLETED_INDEX_KEY, TRANSACTION_KEY_PREFIX, TransactionLocator,
    build_journal_event, completed_index_state_key, transaction_state_key,
    transaction_state_storage_key,
};
pub use store::SessionBackedTransactionStore;
