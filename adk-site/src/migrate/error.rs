//! Errors of the migration stages.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

use crate::error::SiteError;

/// The result type of [`crate::migrate`].
pub type Result<T> = std::result::Result<T, MigrateError>;

/// An error from a migration stage (spec 3.5 §7.8).
///
/// Converts into [`AdkError`] with component `Deploy` and a `site.migrate.*` code.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MigrateError {
    /// A site pipeline error.
    #[error(transparent)]
    Site(#[from] SiteError),

    /// A request failed before an answer arrived.
    #[error("request to '{url}' failed: {message}")]
    Http {
        /// The URL.
        url: String,
        /// What failed.
        message: String,
    },

    /// A request answered a status the stage cannot use.
    #[error("'{url}' answered HTTP {status}")]
    Status {
        /// The URL.
        url: String,
        /// The status.
        status: u16,
    },

    /// The egress policy refused a request before any connection was made.
    #[error("request to '{url}' refused: {reason}")]
    Egress {
        /// The URL.
        url: String,
        /// Why.
        reason: String,
    },

    /// The site's `robots.txt` disallows the URL for this crawler.
    #[error("robots.txt disallows '{url}'; the crawler does not fetch it")]
    RobotsDisallowed {
        /// The URL.
        url: String,
    },

    /// The step needs a proven domain (spec 3.5 A-I9).
    #[error(
        "{action} on '{domain}' needs proof of control of the domain (A-I9); until a DNS TXT \
         record or the well-known token proves it, only polite public reads run"
    )]
    NotProven {
        /// The domain.
        domain: String,
        /// The step that was refused.
        action: String,
    },

    /// A domain claim could not be proven.
    #[error("domain claim for '{domain}' not proven: {reason}")]
    ClaimFailed {
        /// The domain.
        domain: String,
        /// Why.
        reason: String,
    },

    /// A credential could not be obtained or is broader than the step allows.
    #[error("credential refused: {0}. Issue a read-scoped credential through the broker.")]
    Credential(String),

    /// A document could not be parsed.
    #[error("could not parse {what}: {message}")]
    Parse {
        /// The document.
        what: String,
        /// The parser's message.
        message: String,
    },

    /// The container driver failed.
    #[error("container driver: {0}")]
    Container(String),

    /// A screenshot could not be taken.
    #[error("screenshot failed: {0}")]
    Screenshot(String),

    /// The archive failed.
    #[error("legacy archive: {0}")]
    Archive(String),

    /// A DNS lookup failed.
    #[error("DNS lookup failed: {0}")]
    Dns(String),

    /// A configured limit stopped the stage.
    #[error("limit reached: {0}")]
    Limit(String),

    /// An approval or audit service failed.
    #[error("{0}")]
    Service(String),
}

impl MigrateError {
    pub(crate) fn http(url: impl std::fmt::Display, message: impl std::fmt::Display) -> Self {
        Self::Http { url: url.to_string(), message: message.to_string() }
    }

    pub(crate) fn parse(what: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self::Parse { what: what.into(), message: message.to_string() }
    }

    /// The stable error code, `site.migrate.<kind>`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Site(error) => error.code(),
            Self::Http { .. } => "site.migrate.http",
            Self::Status { .. } => "site.migrate.status",
            Self::Egress { .. } => "site.migrate.egress_refused",
            Self::RobotsDisallowed { .. } => "site.migrate.robots_disallowed",
            Self::NotProven { .. } => "site.migrate.not_proven",
            Self::ClaimFailed { .. } => "site.migrate.claim_failed",
            Self::Credential(_) => "site.migrate.credential",
            Self::Parse { .. } => "site.migrate.parse",
            Self::Container(_) => "site.migrate.container",
            Self::Screenshot(_) => "site.migrate.screenshot",
            Self::Archive(_) => "site.migrate.archive",
            Self::Dns(_) => "site.migrate.dns",
            Self::Limit(_) => "site.migrate.limit",
            Self::Service(_) => "site.migrate.service",
        }
    }

    fn category(&self) -> ErrorCategory {
        match self {
            Self::Site(error) => site_category(error.code()),
            Self::Http { .. }
            | Self::Container(_)
            | Self::Screenshot(_)
            | Self::Dns(_)
            | Self::Service(_) => ErrorCategory::Unavailable,
            Self::Status { .. } | Self::Archive(_) => ErrorCategory::Internal,
            Self::Egress { .. }
            | Self::RobotsDisallowed { .. }
            | Self::NotProven { .. }
            | Self::Credential(_) => ErrorCategory::Forbidden,
            Self::ClaimFailed { .. } => ErrorCategory::Unauthorized,
            Self::Parse { .. } => ErrorCategory::InvalidInput,
            Self::Limit(_) => ErrorCategory::ResourceExhausted,
        }
    }
}

/// The category of a [`SiteError`] code, as `adk-site` reports it.
fn site_category(code: &str) -> ErrorCategory {
    match code {
        "site.io" | "site.host" | "site.release.signing_failed" => ErrorCategory::Unavailable,
        "site.release.server_code" | "site.gate.failed" => ErrorCategory::Forbidden,
        "site.release.not_found" | "site.not_found" => ErrorCategory::NotFound,
        "site.release.sequence" | "site.pipeline" => ErrorCategory::Conflict,
        "site.digest_mismatch" | "site.log" => ErrorCategory::Internal,
        _ => ErrorCategory::InvalidInput,
    }
}

impl From<MigrateError> for AdkError {
    fn from(error: MigrateError) -> Self {
        AdkError::new(ErrorComponent::Deploy, error.category(), error.code(), error.to_string())
    }
}

impl From<MigrateError> for SiteError {
    fn from(error: MigrateError) -> Self {
        match error {
            MigrateError::Site(error) => error,
            other => SiteError::GateFailed(other.to_string()),
        }
    }
}

impl From<AdkError> for MigrateError {
    fn from(error: AdkError) -> Self {
        Self::Service(error.to_string())
    }
}
