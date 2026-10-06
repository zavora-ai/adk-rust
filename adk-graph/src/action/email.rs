//! Email action node executor (compiled with the `action-email` feature).
//!
//! **Not implemented.** Neither SMTP sending nor IMAP monitoring is integrated
//! under any feature, so this module validates the configuration and then
//! returns an error saying so. [`ActionNodeExecutor`](super::ActionNodeExecutor)
//! rejects an email node when the graph is built, before any node runs.

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
                message: "email action nodes are not implemented: IMAP monitoring is not \
                          integrated, in any feature configuration"
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
                message: "email action nodes are not implemented: SMTP sending is not \
                          integrated, in any feature configuration"
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
