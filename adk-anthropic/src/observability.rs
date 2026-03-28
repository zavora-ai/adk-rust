//! No-op observability stubs.
//!
//! Placeholder counters and moments that satisfy call sites without
//! pulling in a metrics crate. ADK uses OpenTelemetry via adk-telemetry instead.

pub(crate) struct NoOpCounter;
pub(crate) struct NoOpMoments;

impl NoOpCounter {
    pub(crate) fn click(&self) {}
    pub(crate) fn count(&self, _n: u64) {}
}

impl NoOpMoments {
    pub(crate) fn add(&self, _v: f64) {}
}

pub(crate) static CLIENT_REQUESTS: NoOpCounter = NoOpCounter;
pub(crate) static CLIENT_REQUEST_ERRORS: NoOpCounter = NoOpCounter;
pub(crate) static CLIENT_REQUEST_RETRIES: NoOpCounter = NoOpCounter;
pub(crate) static CLIENT_REQUEST_DURATION: NoOpMoments = NoOpMoments;
pub(crate) static CLIENT_RETRY_BACKOFF: NoOpMoments = NoOpMoments;

pub(crate) static STREAM_EVENTS: NoOpCounter = NoOpCounter;
pub(crate) static STREAM_ERRORS: NoOpCounter = NoOpCounter;
pub(crate) static STREAM_BYTES: NoOpCounter = NoOpCounter;
pub(crate) static STREAM_TTFB: NoOpMoments = NoOpMoments;
pub(crate) static STREAM_DURATION: NoOpMoments = NoOpMoments;
