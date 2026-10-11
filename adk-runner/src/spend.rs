//! Spend estimate for model calls the spend ledger cannot bound.
//!
//! Model calls reserve their upper-bound cost through
//! [`ModelCallScope`](adk_core::ModelCallScope); [`LlmSpendEstimate`] is what a call to an
//! unpriced model commits when its response reports no cost. The runner writes
//! [`Runner::with_llm_spend_estimate`](crate::Runner::with_llm_spend_estimate) into
//! [`RunConfig::llm_spend_estimate`](adk_core::RunConfig::llm_spend_estimate) for each run.

pub use adk_core::LlmSpendEstimate;
