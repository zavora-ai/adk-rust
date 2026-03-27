//! Email action node executor (requires `action-email` feature).
//!
//! Validates the email configuration and returns an informative error
//! about the required email drivers. The actual drivers (`lettre` for SMTP,
//! `imap`/`native-tls`/`mailparse` for IMAP) are not yet wired as
//! dependencies — this module serves as a validated placeholder.

use adk_action::{EmailMode, EmailNodeConfig};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute an Email action node.
///
/// Validates the configuration and returns a descriptive error indicating
/// which email driver is needed.
pub async fn execute_email(config: &EmailNodeConfig, _ctx: &NodeContext) -> Result<NodeOutput> {
    let node_id = &config.standard.id;

    // Validate config based on mode
    validate_email_config(config, node_id)?;

    match config.mode {
        EmailMode::Monitor => {
            let imap = config.imap.as_ref().expect("validated above");
            tracing::debug!(
                node = %node_id,
                host = %imap.host,
                port = imap.port,
                username = %imap.username,
                "IMAP monitor node validated (placeholder)"
            );

            Err(GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: "Email monitoring (IMAP) is not yet available. \
                          The 'action-email' feature is reserved for imap/native-tls/mailparse \
                          integration. To enable IMAP support, add these crates as dependencies \
                          and implement the IMAP connection and search logic in this module."
                    .to_string(),
            })
        }
        EmailMode::Send => {
            let smtp = config.smtp.as_ref().expect("validated above");
            let recipients = config.recipients.as_ref().expect("validated above");
            let content = config.content.as_ref().expect("validated above");

            tracing::debug!(
                node = %node_id,
                host = %smtp.host,
                port = smtp.port,
                to_count = recipients.to.len(),
                subject = %content.subject,
                "SMTP send node validated (placeholder)"
            );

            Err(GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: "Email sending (SMTP) is not yet available. \
                          The 'action-email' feature is reserved for lettre integration. \
                          To enable SMTP support, add the lettre crate as a dependency \
                          and implement the message builder and transport in this module."
                    .to_string(),
            })
        }
    }
}

/// Validate the email configuration based on the mode.
fn validate_email_config(config: &EmailNodeConfig, node_id: &str) -> Result<()> {
    match config.mode {
        EmailMode::Monitor => {
            if config.imap.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: "email node in 'monitor' mode requires an 'imap' \
                              configuration block"
                        .to_string(),
                });
            }
        }
        EmailMode::Send => {
            if config.smtp.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: "email node in 'send' mode requires an 'smtp' \
                              configuration block"
                        .to_string(),
                });
            }
            if config.recipients.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: "email node in 'send' mode requires a 'recipients' \
                              configuration block"
                        .to_string(),
                });
            }
            if config.content.is_none() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: "email node in 'send' mode requires a 'content' \
                              configuration block"
                        .to_string(),
                });
            }
        }
    }
    Ok(())
}
