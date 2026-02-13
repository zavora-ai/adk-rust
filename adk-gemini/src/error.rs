use eventsource_stream::EventStreamError;
use reqwest::header::InvalidHeaderValue;
use snafu::Snafu;
#[cfg(feature = "vertex")]
use tonic::Status;
use url::Url;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("failed to parse API key"))]
    InvalidApiKey {
        source: InvalidHeaderValue,
    },

    #[snafu(display("failed to construct URL (probably incorrect model name): {suffix}"))]
    ConstructUrl {
        source: url::ParseError,
        suffix: String,
    },

    PerformRequestNew {
        source: reqwest::Error,
    },

    #[snafu(display("failed to perform request to '{url}'"))]
    PerformRequest {
        source: reqwest::Error,
        url: Url,
    },

    #[snafu(display(
        "bad response from server; code {code}; description: {}",
        description.as_deref().unwrap_or("none")
    ))]
    BadResponse {
        /// HTTP status code
        code: u16,
        /// HTTP error description
        description: Option<String>,
    },

    MissingResponseHeader {
        header: String,
    },

    #[snafu(display("failed to obtain stream SSE part"))]
    BadPart {
        source: EventStreamError<reqwest::Error>,
    },

    #[snafu(display("failed to deserialize JSON response"))]
    Deserialize {
        source: serde_json::Error,
    },

    #[snafu(display("failed to generate content"))]
    DecodeResponse {
        source: reqwest::Error,
    },

    #[snafu(display("failed to parse URL"))]
    UrlParse {
        source: url::ParseError,
    },

    #[cfg(feature = "vertex")]
    #[snafu(display("failed to build google cloud credentials"))]
    GoogleCloudAuth {
        source: google_cloud_auth::build_errors::Error,
    },

    #[cfg(feature = "vertex")]
    #[snafu(display("failed to obtain google cloud auth headers"))]
    GoogleCloudCredentialHeaders {
        source: google_cloud_auth::errors::CredentialsError,
    },

    #[cfg(feature = "vertex")]
    #[snafu(display("google cloud credentials returned NotModified without cached headers"))]
    GoogleCloudCredentialHeadersUnavailable,

    #[cfg(feature = "vertex")]
    #[snafu(display("failed to parse google cloud credentials JSON"))]
    GoogleCloudCredentialParse {
        source: serde_json::Error,
    },

    #[cfg(feature = "vertex")]
    #[snafu(display("failed to build google cloud vertex client"))]
    GoogleCloudClientBuild {
        source: google_cloud_gax::client_builder::Error,
    },

    #[cfg(feature = "vertex")]
    #[snafu(display("failed to send google cloud vertex request"))]
    GoogleCloudRequest {
        source: google_cloud_aiplatform_v1::Error,
    },

    #[cfg(feature = "vertex")]
    #[snafu(display("gRPC status error: {source}"))]
    GrpcStatus {
        source: Status,
    },

    #[snafu(display("failed to serialize google cloud request"))]
    GoogleCloudRequestSerialize {
        source: serde_json::Error,
    },

    #[snafu(display("failed to deserialize google cloud request"))]
    GoogleCloudRequestDeserialize {
        source: serde_json::Error,
    },

    #[snafu(display("failed to serialize google cloud response"))]
    GoogleCloudResponseSerialize {
        source: serde_json::Error,
    },

    #[snafu(display("failed to deserialize google cloud response"))]
    GoogleCloudResponseDeserialize {
        source: serde_json::Error,
    },

    #[snafu(display("google cloud request payload is not an object"))]
    GoogleCloudRequestNotObject,

    #[snafu(display("google cloud configuration is required for this authentication mode"))]
    MissingGoogleCloudConfig,

    #[snafu(display("google cloud authentication is required for this configuration"))]
    MissingGoogleCloudAuth,

    #[snafu(display("service account JSON is missing required field 'project_id'"))]
    MissingGoogleCloudProjectId,

    #[snafu(display("api key is required for this configuration"))]
    MissingApiKey,

    #[snafu(display(
        "operation '{operation}' is not supported with the google cloud sdk backend (PredictionService currently exposes generateContent/embedContent only)"
    ))]
    GoogleCloudUnsupported {
        operation: &'static str,
    },

    #[snafu(display("failed to create tokio runtime for google cloud client"))]
    TokioRuntime {
        source: std::io::Error,
    },

    #[snafu(display("google cloud client initialization thread panicked"))]
    GoogleCloudInitThreadPanicked,

    #[snafu(display("failed to parse service account JSON"))]
    ServiceAccountKeyParse {
        source: serde_json::Error,
    },

    #[snafu(display("failed to sign service account JWT"))]
    ServiceAccountJwt {
        source: jsonwebtoken::errors::Error,
    },

    #[snafu(display("failed to request service account token from '{url}'"))]
    ServiceAccountToken {
        source: reqwest::Error,
        url: String,
    },

    #[snafu(display("failed to deserialize service account token response"))]
    ServiceAccountTokenDeserialize {
        source: serde_json::Error,
    },
    #[snafu(display("I/O error during file operations"))]
    Io {
        source: std::io::Error,
    },

    #[snafu(display("Configuration error: {message}"))]
    Configuration {
        message: String,
    },

    #[snafu(display("Missing expiration"))]
    MissingExpiration,
    #[snafu(display("Validation error: {source}"))]
    Validation {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[snafu(display("Long display name '{display_name}' ({chars} chars > 128)"))]
    LongDisplayName {
        display_name: String,
        chars: usize,
    },
}

#[cfg(feature = "vertex")]
impl From<Status> for Error {
    fn from(s: Status) -> Self {
        Error::GrpcStatus { source: s }
    }
}
