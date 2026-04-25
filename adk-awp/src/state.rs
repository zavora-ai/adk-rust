//! Shared application state for AWP route handlers.

use std::sync::Arc;

use arc_swap::ArcSwap;
use awp_types::BusinessContext;

use crate::consent::{ConsentService, InMemoryConsentService};
use crate::events::{EventSubscriptionService, InMemoryEventSubscriptionService};
use crate::health::HealthStateMachine;
use crate::rate_limit::{InMemoryRateLimiter, RateLimiter};
use crate::trust::{DefaultTrustAssigner, TrustLevelAssigner};

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

/// Builder for [`AwpState`] with sensible defaults.
///
/// Only `business_context` is required. All services default to their
/// in-memory implementations. The builder wires `HealthStateMachine` to
/// the event service automatically.
///
/// # Example
///
/// ```rust,ignore
/// use adk_awp::AwpStateBuilder;
/// use adk_awp::BusinessContextLoader;
///
/// let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
/// let state = AwpStateBuilder::new(loader.context_ref()).build();
/// ```
pub struct AwpStateBuilder {
    business_context: Arc<ArcSwap<BusinessContext>>,
    rate_limiter: Option<Arc<dyn RateLimiter>>,
    consent_service: Option<Arc<dyn ConsentService>>,
    event_service: Option<Arc<dyn EventSubscriptionService>>,
    trust_assigner: Option<Arc<dyn TrustLevelAssigner>>,
}

impl AwpStateBuilder {
    /// Create a builder with the given business context.
    pub fn new(business_context: Arc<ArcSwap<BusinessContext>>) -> Self {
        Self {
            business_context,
            rate_limiter: None,
            consent_service: None,
            event_service: None,
            trust_assigner: None,
        }
    }

    /// Set a custom rate limiter. Defaults to [`InMemoryRateLimiter`].
    pub fn rate_limiter(mut self, limiter: Arc<dyn RateLimiter>) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Set a custom consent service. Defaults to [`InMemoryConsentService`].
    pub fn consent_service(mut self, service: Arc<dyn ConsentService>) -> Self {
        self.consent_service = Some(service);
        self
    }

    /// Set a custom event subscription service. Defaults to [`InMemoryEventSubscriptionService`].
    ///
    /// The health state machine is automatically wired to this service.
    pub fn event_service(mut self, service: Arc<dyn EventSubscriptionService>) -> Self {
        self.event_service = Some(service);
        self
    }

    /// Set a custom trust level assigner. Defaults to [`DefaultTrustAssigner`].
    pub fn trust_assigner(mut self, assigner: Arc<dyn TrustLevelAssigner>) -> Self {
        self.trust_assigner = Some(assigner);
        self
    }

    /// Build the [`AwpState`], wiring the health state machine to the event service.
    pub fn build(self) -> AwpState {
        let event_service =
            self.event_service.unwrap_or_else(|| Arc::new(InMemoryEventSubscriptionService::new()));

        AwpState {
            business_context: self.business_context,
            rate_limiter: self.rate_limiter.unwrap_or_else(|| Arc::new(InMemoryRateLimiter::new())),
            consent_service: self
                .consent_service
                .unwrap_or_else(|| Arc::new(InMemoryConsentService::new())),
            health: Arc::new(HealthStateMachine::new(event_service.clone())),
            event_service,
            trust_assigner: self.trust_assigner.unwrap_or_else(|| Arc::new(DefaultTrustAssigner)),
        }
    }
}

impl AwpState {
    /// Create a builder for [`AwpState`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_awp::AwpState;
    /// use arc_swap::ArcSwap;
    /// use std::sync::Arc;
    ///
    /// let ctx = Arc::new(ArcSwap::from_pointee(my_business_context));
    /// let state = AwpState::builder(ctx).build();
    /// ```
    pub fn builder(business_context: Arc<ArcSwap<BusinessContext>>) -> AwpStateBuilder {
        AwpStateBuilder::new(business_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::BusinessContext;

    fn test_context() -> Arc<ArcSwap<BusinessContext>> {
        Arc::new(ArcSwap::from_pointee(BusinessContext::core("Test", "Test site", "example.com")))
    }

    #[test]
    fn test_builder_defaults() {
        let state = AwpState::builder(test_context()).build();
        assert_eq!(state.business_context.load().site_name, "Test");
    }

    #[test]
    fn test_builder_custom_rate_limiter() {
        let limiter = Arc::new(InMemoryRateLimiter::new());
        let state = AwpState::builder(test_context()).rate_limiter(limiter).build();
        assert_eq!(state.business_context.load().site_name, "Test");
    }

    #[test]
    fn test_builder_custom_consent() {
        let consent = Arc::new(InMemoryConsentService::new());
        let state = AwpState::builder(test_context()).consent_service(consent).build();
        assert_eq!(state.business_context.load().site_name, "Test");
    }

    #[test]
    fn test_builder_custom_events() {
        let events = Arc::new(InMemoryEventSubscriptionService::new());
        let state = AwpState::builder(test_context()).event_service(events).build();
        assert_eq!(state.business_context.load().site_name, "Test");
    }

    #[test]
    fn test_builder_custom_trust() {
        let trust = Arc::new(DefaultTrustAssigner);
        let state = AwpState::builder(test_context()).trust_assigner(trust).build();
        assert_eq!(state.business_context.load().site_name, "Test");
    }

    #[tokio::test]
    async fn test_builder_health_wired_to_events() {
        let state = AwpState::builder(test_context()).build();
        // Health should start as Healthy
        let snap = state.health.snapshot().await;
        assert_eq!(snap.state, crate::health::HealthState::Healthy);
    }

    #[test]
    fn test_builder_all_custom() {
        let state = AwpState::builder(test_context())
            .rate_limiter(Arc::new(InMemoryRateLimiter::new()))
            .consent_service(Arc::new(InMemoryConsentService::new()))
            .event_service(Arc::new(InMemoryEventSubscriptionService::new()))
            .trust_assigner(Arc::new(DefaultTrustAssigner))
            .build();
        assert_eq!(state.business_context.load().site_name, "Test");
    }
}
