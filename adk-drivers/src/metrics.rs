//! KPI sources: driver readings a KPI is measured by (spec 3.5 §7.11, §7.12, §7.13).
//!
//! The email outbox's delivery statistics, analytics, search performance and ad spend each
//! implement the kernel KPI tracker's [`KpiSource`], under the names `email`, `analytics`,
//! `search` and `ads`; each also lists its metrics and measures one directly.

pub use adk_kernel::charter::KpiSource;

/// Refuses a metric the source `name` does not measure.
pub(crate) fn unknown_metric(name: &str, metrics: Vec<String>, metric: &str) -> crate::DriverError {
    crate::DriverError::invalid(format!(
        "`{name}` does not measure `{metric}`; it measures {}",
        metrics.join(", ")
    ))
}
