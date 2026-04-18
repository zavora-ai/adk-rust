//! Intra-invocation context compaction trigger logic.
//!
//! This module provides [`IntraInvocationCompactor`], which checks whether the
//! estimated token count of conversation events exceeds a threshold and triggers
//! summarization using the existing [`BaseEventsSummarizer`] trait. The actual
//! summarization logic lives in `adk-agent/src/compaction.rs` (`LlmEventSummarizer`);
//! this module only handles the trigger decision and overlap preservation.

use adk_core::intra_compaction::{IntraCompactionConfig, estimate_tokens};
use adk_core::{BaseEventsSummarizer, Event, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Intra-invocation compactor that monitors token usage during a single
/// invocation and triggers summarization when the context exceeds a threshold.
///
/// The compactor enforces at-most-once compaction per LLM call cycle via an
/// [`AtomicBool`] guard. Call [`reset_cycle`](Self::reset_cycle) at the start
/// of each LLM call to re-arm the guard.
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::IntraInvocationCompactor;
/// use adk_core::IntraCompactionConfig;
///
/// let compactor = IntraInvocationCompactor::new(
///     IntraCompactionConfig::default(),
///     summarizer,
/// );
///
/// // Before each LLM call:
/// compactor.reset_cycle();
/// if let Some(compacted) = compactor.maybe_compact(&events).await? {
///     events = compacted;
/// }
/// ```
pub struct IntraInvocationCompactor {
    config: IntraCompactionConfig,
    /// Reuses the existing `BaseEventsSummarizer` trait from adk-core.
    summarizer: Arc<dyn BaseEventsSummarizer>,
    /// Track whether compaction already ran this LLM call cycle.
    compacted_this_cycle: AtomicBool,
}

impl IntraInvocationCompactor {
    /// Create a new compactor with the given config and summarizer.
    pub fn new(
        config: IntraCompactionConfig,
        summarizer: Arc<dyn BaseEventsSummarizer>,
    ) -> Self {
        Self {
            config,
            summarizer,
            compacted_this_cycle: AtomicBool::new(false),
        }
    }

    /// Check if compaction is needed and perform it if so.
    ///
    /// Returns `Some(compacted_events)` if compaction was triggered, or `None`
    /// if no compaction was needed (below threshold or already compacted this cycle).
    ///
    /// On summarizer error, logs a warning and returns `None` (uncompacted history
    /// is used).
    pub async fn maybe_compact(&self, events: &[Event]) -> Result<Option<Vec<Event>>> {
        // Guard: at most once per cycle
        if self
            .compacted_this_cycle
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(None);
        }

        let estimated = estimate_tokens(events, self.config.chars_per_token);
        if estimated <= self.config.token_threshold {
            // Below threshold — reset the guard so a future call in the same
            // cycle can still trigger if tokens grow.
            self.compacted_this_cycle.store(false, Ordering::SeqCst);
            return Ok(None);
        }

        // Determine which events to summarize vs. preserve
        let overlap = self.config.overlap_event_count.min(events.len());
        let summarize_end = events.len().saturating_sub(overlap);

        if summarize_end == 0 {
            // All events are in the overlap window — nothing to summarize
            self.compacted_this_cycle.store(false, Ordering::SeqCst);
            return Ok(None);
        }

        let events_to_summarize = &events[..summarize_end];
        let overlap_events = &events[summarize_end..];

        // Call the summarizer — on error, log and return uncompacted
        match self.summarizer.summarize_events(events_to_summarize).await {
            Ok(Some(summary_event)) => {
                let mut compacted = Vec::with_capacity(1 + overlap);
                compacted.push(summary_event);
                compacted.extend_from_slice(overlap_events);
                Ok(Some(compacted))
            }
            Ok(None) => {
                // Summarizer returned None (e.g., empty input) — no compaction
                Ok(None)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "intra-invocation compaction failed, continuing with uncompacted history"
                );
                Ok(None)
            }
        }
    }

    /// Reset the per-cycle guard. Call this at the start of each LLM call.
    pub fn reset_cycle(&self) {
        self.compacted_this_cycle.store(false, Ordering::SeqCst);
    }
}
