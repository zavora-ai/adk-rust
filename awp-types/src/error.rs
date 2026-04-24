use crate::AwpVersion;

/// AWP protocol error with HTTP status code mapping.
///
/// Each variant maps to a specific HTTP status code via [`AwpError::status_code()`]
/// and a snake_case error code via [`AwpError::error_code()`].
///
/// # Example
///
/// ```
/// use awp_types::AwpError;
///
/// let err = AwpError::NotFound("resource xyz".to_string());
/// assert_eq!(err.status_code(), 404);
/// assert_eq!(err.error_code(), "not_found");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AwpError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("version mismatch: requested {requested}, current {current}")]
    VersionMismatch { requested: AwpVersion, current: AwpVersion },

    #[error("internal error: {0}")]
    InternalError(String),

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
}

impl AwpError {
    /// Returns the HTTP status code corresponding to this error variant.
    pub fn status_code(&self) -> u16 {
        match self {
            AwpError::InvalidRequest(_) => 400,
            AwpError::Unauthorized(_) => 401,
            AwpError::Forbidden(_) => 403,
            AwpError::NotFound(_) => 404,
            AwpError::RateLimited { .. } => 429,
            AwpError::VersionMismatch { .. } => 406,
            AwpError::InternalError(_) => 500,
            AwpError::ServiceUnavailable(_) => 503,
        }
    }

    /// Returns a snake_case error code string for this variant.
    pub fn error_code(&self) -> &str {
        match self {
            AwpError::InvalidRequest(_) => "invalid_request",
            AwpError::Unauthorized(_) => "unauthorized",
            AwpError::Forbidden(_) => "forbidden",
            AwpError::NotFound(_) => "not_found",
            AwpError::RateLimited { .. } => "rate_limited",
            AwpError::VersionMismatch { .. } => "version_mismatch",
            AwpError::InternalError(_) => "internal_error",
            AwpError::ServiceUnavailable(_) => "service_unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CURRENT_VERSION;

    #[test]
    fn test_status_code_invalid_request() {
        assert_eq!(AwpError::InvalidRequest("bad".into()).status_code(), 400);
    }

    #[test]
    fn test_status_code_unauthorized() {
        assert_eq!(AwpError::Unauthorized("no token".into()).status_code(), 401);
    }

    #[test]
    fn test_status_code_forbidden() {
        assert_eq!(AwpError::Forbidden("denied".into()).status_code(), 403);
    }

    #[test]
    fn test_status_code_not_found() {
        assert_eq!(AwpError::NotFound("missing".into()).status_code(), 404);
    }

    #[test]
    fn test_status_code_rate_limited() {
        assert_eq!(AwpError::RateLimited { retry_after_secs: 30 }.status_code(), 429);
    }

    #[test]
    fn test_status_code_version_mismatch() {
        let err = AwpError::VersionMismatch {
            requested: AwpVersion { major: 2, minor: 0 },
            current: CURRENT_VERSION,
        };
        assert_eq!(err.status_code(), 406);
    }

    #[test]
    fn test_status_code_internal_error() {
        assert_eq!(AwpError::InternalError("oops".into()).status_code(), 500);
    }

    #[test]
    fn test_status_code_service_unavailable() {
        assert_eq!(AwpError::ServiceUnavailable("down".into()).status_code(), 503);
    }

    #[test]
    fn test_display_non_empty() {
        let errors: Vec<AwpError> = vec![
            AwpError::InvalidRequest("bad".into()),
            AwpError::Unauthorized("no token".into()),
            AwpError::Forbidden("denied".into()),
            AwpError::NotFound("missing".into()),
            AwpError::RateLimited { retry_after_secs: 30 },
            AwpError::VersionMismatch {
                requested: AwpVersion { major: 2, minor: 0 },
                current: CURRENT_VERSION,
            },
            AwpError::InternalError("oops".into()),
            AwpError::ServiceUnavailable("down".into()),
        ];
        for err in errors {
            let msg = err.to_string();
            assert!(!msg.is_empty(), "Display for {err:?} should be non-empty");
        }
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(AwpError::InvalidRequest("x".into()).error_code(), "invalid_request");
        assert_eq!(AwpError::Unauthorized("x".into()).error_code(), "unauthorized");
        assert_eq!(AwpError::Forbidden("x".into()).error_code(), "forbidden");
        assert_eq!(AwpError::NotFound("x".into()).error_code(), "not_found");
        assert_eq!(AwpError::RateLimited { retry_after_secs: 1 }.error_code(), "rate_limited");
        assert_eq!(
            AwpError::VersionMismatch { requested: CURRENT_VERSION, current: CURRENT_VERSION }
                .error_code(),
            "version_mismatch"
        );
        assert_eq!(AwpError::InternalError("x".into()).error_code(), "internal_error");
        assert_eq!(AwpError::ServiceUnavailable("x".into()).error_code(), "service_unavailable");
    }
}
