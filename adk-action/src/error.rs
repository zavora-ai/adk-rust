//! Error types for action node execution.

use thiserror::Error;

/// Errors that can occur during action node execution.
#[derive(Debug, Error)]
pub enum ActionError {
    /// HTTP response status did not match validation pattern.
    #[error("HTTP status error: {status} for {url}")]
    HttpStatus { status: u16, url: String },

    /// Operation timed out.
    #[error("Action timed out after {ms}ms: {context}")]
    Timeout { ms: u64, context: String },

    /// No switch condition matched and no default branch configured.
    #[error("No matching branch for switch node '{node_id}'")]
    NoMatchingBranch { node_id: String },

    /// Transform operation failed.
    #[error("Transform failed: {0}")]
    Transform(String),

    /// Code execution failed.
    #[error("Code execution failed: {0}")]
    CodeExecution(String),

    /// Sandbox initialization failed.
    #[error("Sandbox initialization failed: {0}")]
    SandboxInit(String),

    /// Missing credential reference.
    #[error("Missing credential: {0}")]
    MissingCredential(String),

    /// No database connection available.
    #[error("No database connection: {0}")]
    NoDatabase(String),

    /// Invalid timestamp format.
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    /// Webhook wait timed out.
    #[error("Webhook wait timed out after {ms}ms")]
    WebhookTimeout { ms: u64 },

    /// Webhook was cancelled.
    #[error("Webhook cancelled: {0}")]
    WebhookCancelled(String),

    /// Condition polling timed out.
    #[error("Condition timed out after {ms}ms")]
    ConditionTimeout { ms: u64 },

    /// No branch completed in merge node.
    #[error("No branch completed for merge node '{node_id}'")]
    NoBranchCompleted { node_id: String },

    /// Insufficient branches completed for waitN merge.
    #[error("Insufficient branches: got {got}, need {need}")]
    InsufficientBranches { got: u32, need: u32 },

    /// Email authentication failed.
    #[error("Email auth failed: {0}")]
    EmailAuth(String),

    /// Email send failed.
    #[error("Email send failed: {0}")]
    EmailSend(String),

    /// Notification send failed.
    #[error("Notification send failed: {0}")]
    NotificationSend(String),

    /// RSS feed fetch failed.
    #[error("RSS fetch failed: {0}")]
    RssFetch(String),

    /// RSS feed parse failed.
    #[error("RSS parse failed: {0}")]
    RssParse(String),

    /// File read failed.
    #[error("File read failed: {0}")]
    FileRead(String),

    /// File write failed.
    #[error("File write failed: {0}")]
    FileWrite(String),

    /// File delete failed.
    #[error("File delete failed: {0}")]
    FileDelete(String),

    /// File parse failed.
    #[error("File parse failed: {0}")]
    FileParse(String),

    /// Catch-all for other errors.
    #[error("{0}")]
    Other(String),
}

impl From<ActionError> for adk_core::AdkError {
    fn from(err: ActionError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};

        let (category, code) = match &err {
            ActionError::HttpStatus { .. } => (ErrorCategory::Internal, "action.http_status"),
            ActionError::Timeout { .. } => (ErrorCategory::Timeout, "action.timeout"),
            ActionError::NoMatchingBranch { .. } => {
                (ErrorCategory::InvalidInput, "action.no_matching_branch")
            }
            ActionError::Transform(_) => (ErrorCategory::Internal, "action.transform"),
            ActionError::CodeExecution(_) => (ErrorCategory::Internal, "action.code_execution"),
            ActionError::SandboxInit(_) => (ErrorCategory::Internal, "action.sandbox_init"),
            ActionError::MissingCredential(_) => {
                (ErrorCategory::Unauthorized, "action.missing_credential")
            }
            ActionError::NoDatabase(_) => (ErrorCategory::Unavailable, "action.no_database"),
            ActionError::InvalidTimestamp(_) => {
                (ErrorCategory::InvalidInput, "action.invalid_timestamp")
            }
            ActionError::WebhookTimeout { .. } => {
                (ErrorCategory::Timeout, "action.webhook_timeout")
            }
            ActionError::WebhookCancelled(_) => {
                (ErrorCategory::Cancelled, "action.webhook_cancelled")
            }
            ActionError::ConditionTimeout { .. } => {
                (ErrorCategory::Timeout, "action.condition_timeout")
            }
            ActionError::NoBranchCompleted { .. } => {
                (ErrorCategory::Internal, "action.no_branch_completed")
            }
            ActionError::InsufficientBranches { .. } => {
                (ErrorCategory::Internal, "action.insufficient_branches")
            }
            ActionError::EmailAuth(_) => (ErrorCategory::Unauthorized, "action.email_auth"),
            ActionError::EmailSend(_) => (ErrorCategory::Internal, "action.email_send"),
            ActionError::NotificationSend(_) => {
                (ErrorCategory::Internal, "action.notification_send")
            }
            ActionError::RssFetch(_) => (ErrorCategory::Unavailable, "action.rss_fetch"),
            ActionError::RssParse(_) => (ErrorCategory::Internal, "action.rss_parse"),
            ActionError::FileRead(_) => (ErrorCategory::Internal, "action.file_read"),
            ActionError::FileWrite(_) => (ErrorCategory::Internal, "action.file_write"),
            ActionError::FileDelete(_) => (ErrorCategory::Internal, "action.file_delete"),
            ActionError::FileParse(_) => (ErrorCategory::Internal, "action.file_parse"),
            ActionError::Other(_) => (ErrorCategory::Internal, "action.other"),
        };

        adk_core::AdkError::new(ErrorComponent::Graph, category, code, err.to_string())
            .with_source(err)
    }
}
