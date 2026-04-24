//! Shared application state for AWP route handlers.

use std::sync::Arc;

use arc_swap::ArcSwap;
use awp_types::BusinessContext;

use crate::consent::ConsentService;
use crate::events::EventSubscriptionService;
use crate::health::HealthStateMachine;
use crate::rate_limit::RateLimiter;
use crate::trust::TrustLevelAssigner;

/// Shared state passed to all AWP route handlers via Axum's state extractor.
#[derive(Clone)]
pub struct AwpState {
    /// Current business context (hot-reloadable).
    pub business_context: Arc<ArcSwap<BusinessContext>>,
    /// Rate limiter for per-trust-level enforcement.
    pub rate_limiter: Arc<dyn RateLimiter>,
    /// Consent management service.
    pub consent_service: Arc<dyn ConsentService>,
    /// Event subscription and delivery service.
    pub event_service: Arc<dyn EventSubscriptionService>,
    /// Health state machine.
    pub health: Arc<HealthStateMachine>,
    /// Trust level assigner.
    pub trust_assigner: Arc<dyn TrustLevelAssigner>,
}
