//! Error types for the adk-anthropic client.
//!
//! This module defines a comprehensive error type system for handling
//! all possible errors that can occur when interacting with the Anthropic API.

use std::error;
use std::fmt;
use std::io;
use std::str::Utf8Error;
use std::sync::Arc;

/// The main error type for the adk-anthropic client.
#[derive(Clone, Debug)]
pub enum Error {
    /// A generic API error occurred.
    Api {
        /// HTTP status code.
        status_code: u16,
        /// Error type string from the API.
        error_type: Option<String>,
        /// Human-readable error message.
        message: String,
        /// Request ID for debugging and support.
        request_id: Option<String>,
    },

    /// Authentication error.
    Authentication {
        /// Human-readable error message.
        message: String,
    },

    /// Authorization/Permission error.
    Permission {
        /// Human-readable error message.
        message: String,
    },

    /// Resource not found.
    NotFound {
        /// Human-readable error message.
        message: String,
        /// Resource type.
        resource_type: Option<String>,
        /// Resource ID.
        resource_id: Option<String>,
    },

    /// Rate limit exceeded.
    RateLimit {
        /// Human-readable error message.
        message: String,
        /// Time to wait before retrying, in seconds.
        retry_after: Option<u64>,
    },

    /// Bad request due to invalid parameters.
    BadRequest {
        /// Human-readable error message.
        message: String,
        /// Parameter that caused the error.
        param: Option<String>,
    },

    /// API timeout error.
    Timeout {
        /// Human-readable error message.
        message: String,
        /// Duration of the timeout in seconds.
        duration: Option<f64>,
    },

    /// Request was aborted by the client.
    Abort {
        /// Human-readable error message.
        message: String,
    },

    /// Connection error.
    Connection {
        /// Human-readable error message.
        message: String,
        /// Underlying cause.
        source: Option<Arc<dyn error::Error + Send + Sync>>,
    },

    /// Server returned a 500 internal error.
    InternalServer {
        /// Human-readable error message.
        message: String,
        /// Request ID for debugging and support.
        request_id: Option<String>,
    },

    /// Server is overloaded or unavailable.
    ServiceUnavailable {
        /// Human-readable error message.
        message: String,
        /// Time to wait before retrying, in seconds.
        retry_after: Option<u64>,
    },

    /// Error during JSON serialization or deserialization.
    Serialization {
        /// Human-readable error message.
        message: String,
        /// The underlying error.
        source: Option<Arc<dyn error::Error + Send + Sync>>,
    },

    /// I/O error.
    Io {
        /// Human-readable error message.
        message: String,
        /// The underlying error.
        source: Arc<io::Error>,
    },

    /// HTTP client error.
    HttpClient {
        /// Human-readable error message.
        message: String,
        /// The underlying error.
        source: Option<Arc<dyn error::Error + Send + Sync>>,
    },

    /// Error during validation of request parameters.
    Validation {
        /// Human-readable error message.
        message: String,
        /// Parameter that failed validation.
        param: Option<String>,
    },

    /// A URL parsing or manipulation error.
    Url {
        /// Human-readable error message.
        message: String,
        /// The underlying error.
        source: Option<url::ParseError>,
    },

    /// A streaming error occurred.
    Streaming {
        /// Human-readable error message.
        message: String,
        /// The underlying error.
        source: Option<Arc<dyn error::Error + Send + Sync>>,
    },

    /// Encoding/decoding error.
    Encoding {
        /// Human-readable error message.
        message: String,
        /// The underlying error.
        source: Option<Arc<dyn error::Error + Send + Sync>>,
    },

    /// Unknown error.
    Unknown {
        /// Human-readable error message.
        message: String,
    },

    /// Unimplemented functionality.
    ToDo {
        /// Human-readable error message.
        message: String,
    },
}

impl Error {
    /// Creates a new API error.
    pub fn api(
        status_code: u16,
        error_type: Option<String>,
        message: String,
        request_id: Option<String>,
    ) -> Self {
        Error::Api { status_code, error_type, message, request_id }
    }

    /// Creates a new authentication error.
    pub fn authentication(message: impl Into<String>) -> Self {
        Error::Authentication { message: message.into() }
    }

    /// Creates a new permission error.
    pub fn permission(message: impl Into<String>) -> Self {
        Error::Permission { message: message.into() }
    }

    /// Creates a new not found error.
    pub fn not_found(
        message: impl Into<String>,
        resource_type: Option<String>,
        resource_id: Option<String>,
    ) -> Self {
        Error::NotFound { message: message.into(), resource_type, resource_id }
    }

    /// Creates a new rate limit error.
    pub fn rate_limit(message: impl Into<String>, retry_after: Option<u64>) -> Self {
        Error::RateLimit { message: message.into(), retry_after }
    }

    /// Creates a new bad request error.
    pub fn bad_request(message: impl Into<String>, param: Option<String>) -> Self {
        Error::BadRequest { message: message.into(), param }
    }

    /// Creates a new timeout error.
    pub fn timeout(message: impl Into<String>, duration: Option<f64>) -> Self {
        Error::Timeout { message: message.into(), duration }
    }

    /// Creates a new abort error.
    pub fn abort(message: impl Into<String>) -> Self {
        Error::Abort { message: message.into() }
    }

    /// Creates a new connection error.
    pub fn connection(
        message: impl Into<String>,
        source: Option<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Error::Connection { message: message.into(), source: source.map(Arc::from) }
    }

    /// Creates a new internal server error.
    pub fn internal_server(message: impl Into<String>, request_id: Option<String>) -> Self {
        Error::InternalServer { message: message.into(), request_id }
    }

    /// Creates a new service unavailable error.
    pub fn service_unavailable(message: impl Into<String>, retry_after: Option<u64>) -> Self {
        Error::ServiceUnavailable { message: message.into(), retry_after }
    }

    /// Creates a new serialization error.
    pub fn serialization(
        message: impl Into<String>,
        source: Option<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Error::Serialization { message: message.into(), source: source.map(Arc::from) }
    }

    /// Creates a new I/O error.
    pub fn io(message: impl Into<String>, source: io::Error) -> Self {
        Error::Io { message: message.into(), source: Arc::new(source) }
    }

    /// Creates a new HTTP client error.
    pub fn http_client(
        message: impl Into<String>,
        source: Option<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Error::HttpClient { message: message.into(), source: source.map(Arc::from) }
    }

    /// Creates a new validation error.
    pub fn validation(message: impl Into<String>, param: Option<String>) -> Self {
        Error::Validation { message: message.into(), param }
    }

    /// Creates a new URL error.
    pub fn url(message: impl Into<String>, source: Option<url::ParseError>) -> Self {
        Error::Url { message: message.into(), source }
    }

    /// Creates a new streaming error.
    pub fn streaming(
        message: impl Into<String>,
        source: Option<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Error::Streaming { message: message.into(), source: source.map(Arc::from) }
    }

    /// Creates a new encoding error.
    pub fn encoding(
        message: impl Into<String>,
        source: Option<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Error::Encoding { message: message.into(), source: source.map(Arc::from) }
    }

    /// Creates a new unknown error.
    pub fn unknown(message: impl Into<String>) -> Self {
        Error::Unknown { message: message.into() }
    }

    /// Creates a new ToDo error for unimplemented functionality.
    pub fn todo(message: impl Into<String>) -> Self {
        Error::ToDo { message: message.into() }
    }

    /// Returns true if this error is related to authentication.
    pub fn is_authentication(&self) -> bool {
        matches!(self, Error::Authentication { .. })
    }

    /// Returns true if this error is related to permissions.
    pub fn is_permission(&self) -> bool {
        matches!(self, Error::Permission { .. })
    }

    /// Returns true if this error is a "not found" error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::NotFound { .. })
    }

    /// Returns true if this error is related to rate limiting.
    pub fn is_rate_limit(&self) -> bool {
        matches!(self, Error::RateLimit { .. })
    }

    /// Returns true if this error is a bad request.
    pub fn is_bad_request(&self) -> bool {
        matches!(self, Error::BadRequest { .. })
    }

    /// Returns true if this error is a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout { .. })
    }

    /// Returns true if this error is an abort.
    pub fn is_abort(&self) -> bool {
        matches!(self, Error::Abort { .. })
    }

    /// Returns true if this error is a connection error.
    pub fn is_connection(&self) -> bool {
        matches!(self, Error::Connection { .. })
    }

    /// Returns true if this error is a server error.
    pub fn is_server_error(&self) -> bool {
        matches!(self, Error::InternalServer { .. } | Error::ServiceUnavailable { .. })
    }

    /// Returns true if this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Api { status_code, .. } => {
                matches!(status_code, 408 | 409 | 429 | 500..=599)
            }
            Error::Timeout { .. } => true,
            Error::Connection { .. } => true,
            Error::RateLimit { .. } => true,
            Error::ServiceUnavailable { .. } => true,
            Error::InternalServer { .. } => true,
            _ => false,
        }
    }

    /// Returns true if this error is a ToDo error.
    pub fn is_todo(&self) -> bool {
        matches!(self, Error::ToDo { .. })
    }

    /// Returns true if this error is a validation error.
    pub fn is_validation(&self) -> bool {
        matches!(self, Error::Validation { .. })
    }

    /// Returns the request ID associated with this error, if any.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Error::Api { request_id, .. } => request_id.as_deref(),
            Error::InternalServer { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }

    /// Returns the status code associated with this error, if any.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            Error::Api { status_code, .. } => Some(*status_code),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api { message, error_type, request_id, .. } => {
                if let Some(error_type) = error_type {
                    if let Some(request_id) = request_id {
                        write!(f, "{error_type}: {message} (Request ID: {request_id})")
                    } else {
                        write!(f, "{error_type}: {message}")
                    }
                } else if let Some(request_id) = request_id {
                    write!(f, "API error: {message} (Request ID: {request_id})")
                } else {
                    write!(f, "API error: {message}")
                }
            }
            Error::Authentication { message } => {
                write!(f, "Authentication error: {message}")
            }
            Error::Permission { message } => {
                write!(f, "Permission error: {message}")
            }
            Error::NotFound { message, resource_type, resource_id } => {
                let prefix = if let Some(resource_type) = resource_type {
                    format!("Resource not found ({resource_type})")
                } else {
                    "Resource not found".to_string()
                };

                let suffix = if let Some(resource_id) = resource_id {
                    format!(" [ID: {resource_id}]")
                } else {
                    "".to_string()
                };

                write!(f, "{prefix}: {message}{suffix}")
            }
            Error::RateLimit { message, retry_after } => {
                if let Some(retry_after) = retry_after {
                    write!(f, "Rate limit exceeded: {message} (retry after {retry_after} seconds)")
                } else {
                    write!(f, "Rate limit exceeded: {message}")
                }
            }
            Error::BadRequest { message, param } => {
                if let Some(param) = param {
                    write!(f, "Bad request: {message} (parameter: {param})")
                } else {
                    write!(f, "Bad request: {message}")
                }
            }
            Error::Timeout { message, duration } => {
                if let Some(duration) = duration {
                    write!(f, "Timeout error: {message} ({duration} seconds)")
                } else {
                    write!(f, "Timeout error: {message}")
                }
            }
            Error::Abort { message } => {
                write!(f, "Request aborted: {message}")
            }
            Error::Connection { message, .. } => {
                write!(f, "Connection error: {message}")
            }
            Error::InternalServer { message, request_id } => {
                if let Some(request_id) = request_id {
                    write!(f, "Internal server error: {message} (Request ID: {request_id})")
                } else {
                    write!(f, "Internal server error: {message}")
                }
            }
            Error::ServiceUnavailable { message, retry_after } => {
                if let Some(retry_after) = retry_after {
                    write!(f, "Service unavailable: {message} (retry after {retry_after} seconds)")
                } else {
                    write!(f, "Service unavailable: {message}")
                }
            }
            Error::Serialization { message, .. } => {
                write!(f, "Serialization error: {message}")
            }
            Error::Io { message, .. } => {
                write!(f, "I/O error: {message}")
            }
            Error::HttpClient { message, .. } => {
                write!(f, "HTTP client error: {message}")
            }
            Error::Validation { message, param } => {
                if let Some(param) = param {
                    write!(f, "Validation error: {message} (parameter: {param})")
                } else {
                    write!(f, "Validation error: {message}")
                }
            }
            Error::Url { message, .. } => {
                write!(f, "URL error: {message}")
            }
            Error::Streaming { message, .. } => {
                write!(f, "Streaming error: {message}")
            }
            Error::Encoding { message, .. } => {
                write!(f, "Encoding error: {message}")
            }
            Error::Unknown { message } => {
                write!(f, "Unknown error: {message}")
            }
            Error::ToDo { message } => {
                write!(f, "Unimplemented: {message}")
            }
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Connection { source, .. } => {
                source.as_ref().map(|e| e.as_ref() as &(dyn error::Error + 'static))
            }
            Error::Serialization { source, .. } => {
                source.as_ref().map(|e| e.as_ref() as &(dyn error::Error + 'static))
            }
            Error::Io { source, .. } => Some(source),
            Error::HttpClient { source, .. } => {
                source.as_ref().map(|e| e.as_ref() as &(dyn error::Error + 'static))
            }
            Error::Url { source, .. } => {
                source.as_ref().map(|e| e as &(dyn error::Error + 'static))
            }
            Error::Streaming { source, .. } => {
                source.as_ref().map(|e| e.as_ref() as &(dyn error::Error + 'static))
            }
            Error::Encoding { source, .. } => {
                source.as_ref().map(|e| e.as_ref() as &(dyn error::Error + 'static))
            }
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::io(err.to_string(), err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::serialization(format!("JSON error: {err}"), Some(Box::new(err)))
    }
}

impl From<url::ParseError> for Error {
    fn from(err: url::ParseError) -> Self {
        Error::url(format!("URL parse error: {err}"), Some(err))
    }
}

impl From<Utf8Error> for Error {
    fn from(err: Utf8Error) -> Self {
        Error::encoding(format!("UTF-8 error: {err}"), Some(Box::new(err)))
    }
}

/// A specialized Result type for adk-anthropic operations.
pub type Result<T> = std::result::Result<T, Error>;
