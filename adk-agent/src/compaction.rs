//! LLM-based event summarizer for context compaction.
//!
//! [`LlmEventSummarizer`] lives in `adk-core`, so the runner's auto-compaction summarizes with
//! it as well; this module keeps its `adk_agent` path.

pub use adk_core::LlmEventSummarizer;
