//! Long-term facts with provenance (spec 3.1 §7.15.2, feature `memory`).
//!
//! The harness learns facts — preferences, project facts, procedures, outcomes,
//! decisions, corrections — and recalls them on each user turn. Every fact carries its
//! provenance: it is `Known` only when the user's own words verify it or a person promoted
//! it; anything the model, a tool, a file or another agent says is `Assumed` at most
//! (spec 3.1 §9.2 I41).
//!
//! | Part | Role |
//! |---|---|
//! | [`FactStore`] | the system of record: [`InMemoryFactStore`], [`FileFactStore`], `SqliteFactStore` (feature `sqlite`) |
//! | [`FactIndex`] | finds candidates: [`LocalFactIndex`] (embedded or lexical), [`MemoryServiceIndex`] over any `adk_memory::MemoryService` |
//! | recall | a `<memory-facts>` block in the untrusted section on the first model call of each user turn |
//! | writing | `memory_remember` (assumed only), session distillation, verified quotes, human promotion |
//! | consolidation | the spec 3.2 tick: distillation of missed sessions with quote promotion off, two passes, the loss cap, [`undo_consolidation`] |

mod consolidate;
#[cfg(feature = "runner")]
pub(crate) mod distill;
#[cfg(any(feature = "memory-embeddings", feature = "memory-embeddings-local"))]
mod embed;
mod index;
#[cfg(feature = "kernel")]
mod kernel_tick;
pub(crate) mod recall;
mod redact;
pub(crate) mod runtime;
mod store;
mod types;
pub(crate) mod writer;

pub mod conformance;

pub use consolidate::{
    BesideKnown, CONSOLIDATION_CLUSTER_SCORE, ChangeReason, ConsolidationConfig,
    ConsolidationRecord, ConsolidationRequest, ConsolidationUndo, FactChange, SessionDistillation,
    find_consolidation, undo_consolidation,
};
#[cfg(any(feature = "memory-embeddings", feature = "memory-embeddings-local"))]
pub use embed::RagEmbedder;
pub(crate) use index::embedder_id;
pub use index::{FactIndex, LocalFactIndex, MemoryServiceIndex};
#[cfg(feature = "kernel")]
pub use kernel_tick::{HarnessConsolidator, HarnessObservations, SessionStateFilter};
pub use redact::redact_fact_text;
#[cfg(feature = "sqlite")]
pub use store::SqliteFactStore;
pub use store::{FactEmbedding, FactStore, FileFactStore, InMemoryFactStore};
pub use types::{
    FACT_AUTHOR_PREFIX, FACT_TEXT_MAX_CHARS, FactFilter, FactKind, FactProjects, FactScope,
    FactSource, FactStatus, LongTermMemoryConfig, MemoryFact, Provenance, fact_digest,
};
