//! Typed identity primitives for ADK-Rust.
//!
//! This module provides strongly-typed wrappers for the identity values used
//! throughout the ADK runtime: application names, user identifiers, session
//! identifiers, and invocation identifiers.
//!
//! ## Identity Layers
//!
//! ADK distinguishes three identity concerns:
//!
//! - **Auth identity** ([`RequestContext`](crate::RequestContext)): who is
//!   authenticated and what scopes they hold.
//! - **Session identity** ([`AdkIdentity`]): the stable `(app, user, session)`
//!   triple that addresses a conversation session.
//! - **Execution identity** ([`ExecutionIdentity`]): session identity plus the
//!   per-invocation `invocation_id`, `branch`, and `agent_name`.
//!
//! ## Validation Rules
//!
//! All leaf identifiers ([`AppName`], [`UserId`], [`SessionId`],
//! [`InvocationId`]) enforce the same validation:
//!
//! - Must not be empty.
//! - Must not contain null bytes (`\0`).
//! - Must not exceed [`MAX_ID_LEN`] bytes (512).
//! - Characters like `:`, `|`, `/`, and `@` are allowed — validation does not
//!   couple to any backend's internal key encoding.
//!
//! ## Examples
//!
//! ```
//! use adk_core::identity::{AdkIdentity, AppName, ExecutionIdentity, InvocationId, SessionId, UserId};
//!
//! let identity = AdkIdentity::new(
//!     AppName::try_from("weather-app").unwrap(),
//!     UserId::try_from("tenant:alice@example.com").unwrap(),
//!     SessionId::generate(),
//! );
//!
//! let exec = ExecutionIdentity {
//!     adk: identity,
//!     invocation_id: InvocationId::generate(),
//!     branch: String::new(),
//!     agent_name: "planner".to_string(),
//! };
//! ```

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

/// Maximum allowed length for any identity value, in bytes.
pub const MAX_ID_LEN: usize = 512;

// ---------------------------------------------------------------------------
// IdentityError
// ---------------------------------------------------------------------------

/// Error returned when a raw string cannot be converted into a typed identifier.
///
/// # Examples
///
/// ```
/// use adk_core::identity::{AppName, IdentityError};
///
/// let err = AppName::try_from("").unwrap_err();
/// assert!(matches!(err, IdentityError::Empty { .. }));
/// ```
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The input string was empty.
    #[error("{kind} must not be empty")]
    Empty {
        /// Human-readable name of the identifier kind (e.g. `"AppName"`).
        kind: &'static str,
    },

    /// The input string exceeded [`MAX_ID_LEN`].
    #[error("{kind} exceeds maximum length of {max} bytes")]
    TooLong {
        /// Human-readable name of the identifier kind.
        kind: &'static str,
        /// The maximum allowed length.
        max: usize,
    },

    /// The input string contained a null byte.
    #[error("{kind} must not contain null bytes")]
    ContainsNull {
        /// Human-readable name of the identifier kind.
        kind: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Shared validation
// ---------------------------------------------------------------------------

fn validate(value: &str, kind: &'static str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::Empty { kind });
    }
    if value.len() > MAX_ID_LEN {
        return Err(IdentityError::TooLong { kind, max: MAX_ID_LEN });
    }
    if value.contains('\0') {
        return Err(IdentityError::ContainsNull { kind });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Macro for leaf identifier newtypes
// ---------------------------------------------------------------------------

macro_rules! define_id {
    (
        $(#[$meta:meta])*
        $name:ident, $kind:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Returns the inner string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Creates a typed identifier from a trusted string without
            /// validation.
            ///
            /// Use this only for values that originate from internal runtime
            /// paths (e.g. session service, runner) where the string is
            /// already known to be valid. Prefer [`TryFrom`] or [`FromStr`]
            /// at trust boundaries.
            pub fn new_unchecked(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdentityError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                validate(s, $kind)?;
                Ok(Self(s.to_owned()))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentityError;

            fn try_from(s: &str) -> Result<Self, Self::Error> {
                validate(s, $kind)?;
                Ok(Self(s.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentityError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                validate(&s, $kind)?;
                Ok(Self(s))
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Leaf identifier types
// ---------------------------------------------------------------------------

define_id! {
    /// A typed wrapper around the logical application name used in session
    /// addressing.
    ///
    /// # Examples
    ///
    /// ```
    /// use adk_core::identity::AppName;
    ///
    /// let app: AppName = "my-app".parse().unwrap();
    /// assert_eq!(app.as_ref(), "my-app");
    ///
    /// // Empty values are rejected
    /// assert!(AppName::try_from("").is_err());
    /// ```
    AppName, "AppName"
}

define_id! {
    /// A typed wrapper around a logical user identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// use adk_core::identity::UserId;
    ///
    /// let uid: UserId = "tenant:alice@example.com".parse().unwrap();
    /// assert_eq!(uid.as_ref(), "tenant:alice@example.com");
    /// ```
    UserId, "UserId"
}

define_id! {
    /// A typed wrapper around a logical session identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// use adk_core::identity::SessionId;
    ///
    /// // Parse from a known value
    /// let sid = SessionId::try_from("session-abc-123").unwrap();
    /// assert_eq!(sid.as_ref(), "session-abc-123");
    ///
    /// // Or generate a new UUID-based session ID
    /// let generated = SessionId::generate();
    /// assert!(!generated.as_ref().is_empty());
    /// ```
    SessionId, "SessionId"
}

define_id! {
    /// A typed wrapper around a single execution or turn identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// use adk_core::identity::InvocationId;
    ///
    /// // Parse from a known value
    /// let iid = InvocationId::try_from("inv-001").unwrap();
    /// assert_eq!(iid.as_ref(), "inv-001");
    ///
    /// // Or generate a new UUID-based invocation ID
    /// let generated = InvocationId::generate();
    /// assert!(!generated.as_ref().is_empty());
    /// ```
    InvocationId, "InvocationId"
}

// ---------------------------------------------------------------------------
// Generation helpers
// ---------------------------------------------------------------------------

impl SessionId {
    /// Generates a new random session identifier using UUID v4.
    ///
    /// # Examples
    ///
    /// ```
    /// use adk_core::identity::SessionId;
    ///
    /// let a = SessionId::generate();
    /// let b = SessionId::generate();
    /// assert_ne!(a, b);
    /// ```
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl InvocationId {
    /// Generates a new random invocation identifier using UUID v4.
    ///
    /// # Examples
    ///
    /// ```
    /// use adk_core::identity::InvocationId;
    ///
    /// let a = InvocationId::generate();
    /// let b = InvocationId::generate();
    /// assert_ne!(a, b);
    /// ```
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

// ---------------------------------------------------------------------------
// AdkIdentity
// ---------------------------------------------------------------------------

/// The stable session-scoped identity triple: application name, user, and
/// session.
///
/// This is the natural addressing key for session-scoped operations. Passing
/// an `AdkIdentity` instead of three separate strings eliminates parameter
/// ordering bugs and makes the addressing model explicit.
///
/// # Display
///
/// The [`Display`](fmt::Display) implementation is diagnostic only and must
/// not be parsed or used as a storage key.
///
/// # Examples
///
/// ```
/// use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
///
/// let identity = AdkIdentity::new(
///     AppName::try_from("weather-app").unwrap(),
///     UserId::try_from("alice").unwrap(),
///     SessionId::try_from("sess-1").unwrap(),
/// );
///
/// assert_eq!(identity.app_name.as_ref(), "weather-app");
/// assert_eq!(identity.user_id.as_ref(), "alice");
/// assert_eq!(identity.session_id.as_ref(), "sess-1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdkIdentity {
    /// The application name.
    pub app_name: AppName,
    /// The user identifier.
    pub user_id: UserId,
    /// The session identifier.
    pub session_id: SessionId,
}

impl AdkIdentity {
    /// Creates a new `AdkIdentity` from its constituent parts.
    pub fn new(app_name: AppName, user_id: UserId, session_id: SessionId) -> Self {
        Self { app_name, user_id, session_id }
    }
}

impl fmt::Display for AdkIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AdkIdentity(app=\"{}\", user=\"{}\", session=\"{}\")",
            self.app_name, self.user_id, self.session_id
        )
    }
}

// ---------------------------------------------------------------------------
// ExecutionIdentity
// ---------------------------------------------------------------------------

/// The per-invocation execution identity, built from a stable
/// [`AdkIdentity`] plus invocation-scoped metadata.
///
/// This is the runtime's internal identity capsule. It carries everything
/// needed for event creation, telemetry correlation, and agent transfers
/// without re-parsing raw strings.
///
/// # Examples
///
/// ```
/// use adk_core::identity::{
///     AdkIdentity, AppName, ExecutionIdentity, InvocationId, SessionId, UserId,
/// };
///
/// let exec = ExecutionIdentity {
///     adk: AdkIdentity::new(
///         AppName::try_from("my-app").unwrap(),
///         UserId::try_from("user-1").unwrap(),
///         SessionId::try_from("sess-1").unwrap(),
///     ),
///     invocation_id: InvocationId::generate(),
///     branch: String::new(),
///     agent_name: "root".to_string(),
/// };
///
/// assert_eq!(exec.adk.app_name.as_ref(), "my-app");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    /// The stable session-scoped identity.
    pub adk: AdkIdentity,
    /// The invocation identifier for this execution turn.
    pub invocation_id: InvocationId,
    /// The branch name. Defaults to an empty string in phase 1.
    pub branch: String,
    /// The name of the currently executing agent.
    pub agent_name: String,
}

// ---------------------------------------------------------------------------
// IdentityError -> AdkError conversion
// ---------------------------------------------------------------------------

impl From<IdentityError> for crate::AdkError {
    fn from(err: IdentityError) -> Self {
        crate::AdkError::Config(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_name_valid() {
        let app = AppName::try_from("my-app").unwrap();
        assert_eq!(app.as_ref(), "my-app");
        assert_eq!(app.to_string(), "my-app");
    }

    #[test]
    fn test_app_name_with_special_chars() {
        // Colons, slashes, and @ are allowed
        let app = AppName::try_from("org:team/app@v2").unwrap();
        assert_eq!(app.as_ref(), "org:team/app@v2");
    }

    #[test]
    fn test_empty_rejected() {
        let err = AppName::try_from("").unwrap_err();
        assert_eq!(err, IdentityError::Empty { kind: "AppName" });
    }

    #[test]
    fn test_null_byte_rejected() {
        let err = UserId::try_from("user\0id").unwrap_err();
        assert_eq!(err, IdentityError::ContainsNull { kind: "UserId" });
    }

    #[test]
    fn test_too_long_rejected() {
        let long = "x".repeat(MAX_ID_LEN + 1);
        let err = SessionId::try_from(long.as_str()).unwrap_err();
        assert_eq!(err, IdentityError::TooLong { kind: "SessionId", max: MAX_ID_LEN });
    }

    #[test]
    fn test_max_length_accepted() {
        let exact = "a".repeat(MAX_ID_LEN);
        assert!(SessionId::try_from(exact.as_str()).is_ok());
    }

    #[test]
    fn test_from_str() {
        let app: AppName = "hello".parse().unwrap();
        assert_eq!(app.as_ref(), "hello");
    }

    #[test]
    fn test_try_from_string() {
        let s = String::from("owned-value");
        let uid = UserId::try_from(s).unwrap();
        assert_eq!(uid.as_ref(), "owned-value");
    }

    #[test]
    fn test_borrow_str() {
        let sid = SessionId::try_from("sess-1").unwrap();
        let borrowed: &str = sid.borrow();
        assert_eq!(borrowed, "sess-1");
    }

    #[test]
    fn test_ord() {
        let a = AppName::try_from("aaa").unwrap();
        let b = AppName::try_from("bbb").unwrap();
        assert!(a < b);
    }

    #[test]
    fn test_session_id_generate() {
        let a = SessionId::generate();
        let b = SessionId::generate();
        assert_ne!(a, b);
        assert!(!a.as_ref().is_empty());
    }

    #[test]
    fn test_invocation_id_generate() {
        let a = InvocationId::generate();
        let b = InvocationId::generate();
        assert_ne!(a, b);
        assert!(!a.as_ref().is_empty());
    }

    #[test]
    fn test_adk_identity_new() {
        let identity = AdkIdentity::new(
            AppName::try_from("app").unwrap(),
            UserId::try_from("user").unwrap(),
            SessionId::try_from("sess").unwrap(),
        );
        assert_eq!(identity.app_name.as_ref(), "app");
        assert_eq!(identity.user_id.as_ref(), "user");
        assert_eq!(identity.session_id.as_ref(), "sess");
    }

    #[test]
    fn test_adk_identity_display() {
        let identity = AdkIdentity::new(
            AppName::try_from("weather-app").unwrap(),
            UserId::try_from("alice").unwrap(),
            SessionId::try_from("abc-123").unwrap(),
        );
        let display = identity.to_string();
        assert!(display.contains("weather-app"));
        assert!(display.contains("alice"));
        assert!(display.contains("abc-123"));
        assert!(display.starts_with("AdkIdentity("));
    }

    #[test]
    fn test_adk_identity_equality() {
        let a = AdkIdentity::new(
            AppName::try_from("app").unwrap(),
            UserId::try_from("user").unwrap(),
            SessionId::try_from("sess").unwrap(),
        );
        let b = a.clone();
        assert_eq!(a, b);

        let c = AdkIdentity::new(
            AppName::try_from("app").unwrap(),
            UserId::try_from("other-user").unwrap(),
            SessionId::try_from("sess").unwrap(),
        );
        assert_ne!(a, c);
    }

    #[test]
    fn test_adk_identity_hash() {
        use std::collections::HashSet;
        let a = AdkIdentity::new(
            AppName::try_from("app").unwrap(),
            UserId::try_from("user").unwrap(),
            SessionId::try_from("sess").unwrap(),
        );
        let b = a.clone();
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_execution_identity() {
        let exec = ExecutionIdentity {
            adk: AdkIdentity::new(
                AppName::try_from("app").unwrap(),
                UserId::try_from("user").unwrap(),
                SessionId::try_from("sess").unwrap(),
            ),
            invocation_id: InvocationId::try_from("inv-1").unwrap(),
            branch: String::new(),
            agent_name: "root".to_string(),
        };
        assert_eq!(exec.adk.app_name.as_ref(), "app");
        assert_eq!(exec.invocation_id.as_ref(), "inv-1");
        assert_eq!(exec.branch, "");
        assert_eq!(exec.agent_name, "root");
    }

    #[test]
    fn test_serde_round_trip_leaf() {
        let app = AppName::try_from("my-app").unwrap();
        let json = serde_json::to_string(&app).unwrap();
        assert_eq!(json, "\"my-app\"");
        let deserialized: AppName = serde_json::from_str(&json).unwrap();
        assert_eq!(app, deserialized);
    }

    #[test]
    fn test_serde_round_trip_adk_identity() {
        let identity = AdkIdentity::new(
            AppName::try_from("app").unwrap(),
            UserId::try_from("user").unwrap(),
            SessionId::try_from("sess").unwrap(),
        );
        let json = serde_json::to_string(&identity).unwrap();
        let deserialized: AdkIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(identity, deserialized);
    }

    #[test]
    fn test_serde_round_trip_execution_identity() {
        let exec = ExecutionIdentity {
            adk: AdkIdentity::new(
                AppName::try_from("app").unwrap(),
                UserId::try_from("user").unwrap(),
                SessionId::try_from("sess").unwrap(),
            ),
            invocation_id: InvocationId::try_from("inv-1").unwrap(),
            branch: "main".to_string(),
            agent_name: "agent".to_string(),
        };
        let json = serde_json::to_string(&exec).unwrap();
        let deserialized: ExecutionIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(exec, deserialized);
    }

    #[test]
    fn test_identity_error_display() {
        let err = IdentityError::Empty { kind: "AppName" };
        assert_eq!(err.to_string(), "AppName must not be empty");

        let err = IdentityError::TooLong { kind: "UserId", max: 512 };
        assert_eq!(err.to_string(), "UserId exceeds maximum length of 512 bytes");

        let err = IdentityError::ContainsNull { kind: "SessionId" };
        assert_eq!(err.to_string(), "SessionId must not contain null bytes");
    }

    #[test]
    fn test_identity_error_to_adk_error() {
        let err = IdentityError::Empty { kind: "AppName" };
        let adk_err: crate::AdkError = err.into();
        assert!(matches!(adk_err, crate::AdkError::Config(_)));
        assert!(adk_err.to_string().contains("AppName must not be empty"));
    }
}
