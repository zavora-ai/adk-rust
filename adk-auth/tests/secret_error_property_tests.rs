//! Property tests for Secret Provider error categorization.
//!
//! **Feature: competitive-parity-v070, Property 9: Secret Error Categorization**
//! *For any* secret retrieval error of type authentication, network, or not-found,
//! the `SecretProvider` implementation SHALL return an `AdkError` whose `ErrorCategory`
//! is `Unauthorized`, `Unavailable`, or `NotFound` respectively.
//! **Validates: Requirements 7.5**

use adk_auth::secrets::SecretProvider;
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_trait::async_trait;
use proptest::prelude::*;

/// The kind of error a mock secret provider should return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretErrorKind {
    Authentication,
    Network,
    NotFound,
}

/// A mock `SecretProvider` that always returns an error of the configured kind.
struct FailingSecretProvider {
    error_kind: SecretErrorKind,
    message: String,
}

impl FailingSecretProvider {
    fn new(error_kind: SecretErrorKind, message: String) -> Self {
        Self { error_kind, message }
    }
}

#[async_trait]
impl SecretProvider for FailingSecretProvider {
    async fn get_secret(&self, _name: &str) -> Result<String, AdkError> {
        match self.error_kind {
            SecretErrorKind::Authentication => Err(AdkError::unauthorized(
                ErrorComponent::Auth,
                "auth.secret.unauthorized",
                &self.message,
            )),
            SecretErrorKind::Network => Err(AdkError::unavailable(
                ErrorComponent::Auth,
                "auth.secret.unavailable",
                &self.message,
            )),
            SecretErrorKind::NotFound => Err(AdkError::not_found(
                ErrorComponent::Auth,
                "auth.secret.not_found",
                &self.message,
            )),
        }
    }
}

/// Map `SecretErrorKind` to the expected `ErrorCategory`.
fn expected_category(kind: SecretErrorKind) -> ErrorCategory {
    match kind {
        SecretErrorKind::Authentication => ErrorCategory::Unauthorized,
        SecretErrorKind::Network => ErrorCategory::Unavailable,
        SecretErrorKind::NotFound => ErrorCategory::NotFound,
    }
}

/// Strategy that generates one of the three error kinds.
fn arb_error_kind() -> impl Strategy<Value = SecretErrorKind> {
    prop_oneof![
        Just(SecretErrorKind::Authentication),
        Just(SecretErrorKind::Network),
        Just(SecretErrorKind::NotFound),
    ]
}

/// Strategy that generates a non-empty secret name.
fn arb_secret_name() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_-]{0,30}".prop_map(|s| s)
}

/// Strategy that generates a non-empty error message.
fn arb_error_message() -> impl Strategy<Value = String> {
    "[a-zA-Z ]{1,50}".prop_map(|s| s)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 9: Secret Error Categorization**
    /// *For any* secret retrieval error of type authentication, network, or not-found,
    /// the returned AdkError has the correct ErrorCategory.
    /// **Validates: Requirements 7.5**
    #[test]
    fn prop_secret_error_has_correct_category(
        error_kind in arb_error_kind(),
        secret_name in arb_secret_name(),
        error_message in arb_error_message(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let provider = FailingSecretProvider::new(error_kind, error_message);
            let result = provider.get_secret(&secret_name).await;

            prop_assert!(result.is_err(), "expected an error from the provider");
            let err = result.unwrap_err();
            let expected = expected_category(error_kind);
            prop_assert_eq!(
                err.category, expected,
                "error kind {:?} should map to category {}, got {}",
                error_kind, expected, err.category
            );
            Ok(())
        })?;
    }

    /// For any authentication error, the AdkError should report is_unauthorized() == true.
    #[test]
    fn prop_auth_error_is_unauthorized(
        secret_name in arb_secret_name(),
        error_message in arb_error_message(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let provider = FailingSecretProvider::new(SecretErrorKind::Authentication, error_message);
            let err = provider.get_secret(&secret_name).await.unwrap_err();
            prop_assert!(err.is_unauthorized(), "authentication errors should be unauthorized");
            prop_assert!(!err.is_retryable(), "authentication errors should not be retryable");
            Ok(())
        })?;
    }

    /// For any network error, the AdkError should report is_retryable() == true.
    #[test]
    fn prop_network_error_is_retryable(
        secret_name in arb_secret_name(),
        error_message in arb_error_message(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let provider = FailingSecretProvider::new(SecretErrorKind::Network, error_message);
            let err = provider.get_secret(&secret_name).await.unwrap_err();
            prop_assert!(err.is_retryable(), "network errors should be retryable");
            Ok(())
        })?;
    }

    /// For any not-found error, the AdkError should report is_not_found() == true.
    #[test]
    fn prop_not_found_error_is_not_found(
        secret_name in arb_secret_name(),
        error_message in arb_error_message(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let provider = FailingSecretProvider::new(SecretErrorKind::NotFound, error_message);
            let err = provider.get_secret(&secret_name).await.unwrap_err();
            prop_assert!(err.is_not_found(), "not-found errors should report is_not_found");
            prop_assert!(!err.is_retryable(), "not-found errors should not be retryable");
            Ok(())
        })?;
    }

    /// For any error kind, the AdkError component should be Auth.
    #[test]
    fn prop_secret_error_component_is_auth(
        error_kind in arb_error_kind(),
        secret_name in arb_secret_name(),
        error_message in arb_error_message(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let provider = FailingSecretProvider::new(error_kind, error_message);
            let err = provider.get_secret(&secret_name).await.unwrap_err();
            prop_assert_eq!(
                err.component, ErrorComponent::Auth,
                "secret provider errors should have Auth component"
            );
            Ok(())
        })?;
    }
}
