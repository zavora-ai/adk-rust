//! MCP server with elicitation-powered tools.
//!
//! Exposes two tools that use elicitation to collect user input at runtime:
//!
//! - `create_user` — collects name + email via form elicitation
//! - `deploy_app` — asks for deployment confirmation via form elicitation
//!
//! Spawned as a subprocess by the client. Communicates over stdio.

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    service::{RequestContext, RoleServer},
};
use serde::{Deserialize, Serialize};

// -- Elicitation schemas (what the server asks the client for) ----------------

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct UserProfile {
    /// Full name of the user
    name: String,
    /// Email address
    email: String,
}
rmcp::elicit_safe!(UserProfile);

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct DeployConfirmation {
    /// Type "yes" to confirm deployment
    confirm: String,
    /// Optional reason for deploying
    reason: Option<String>,
}
rmcp::elicit_safe!(DeployConfirmation);

// -- Tool parameter types -----------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeployParams {
    /// Name of the application to deploy
    app_name: String,
}

// -- Server -------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ElicitationServer {
    tool_router: ToolRouter<Self>,
}

impl ElicitationServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ElicitationServer {
    /// Create a new user account. Collects name and email via elicitation.
    #[tool(description = "Create a new user account. The server will ask for user details via elicitation.")]
    async fn create_user(
        &self,
        context: RequestContext<RoleServer>,
    ) -> String {
        let peer = context.peer;

        if peer.supported_elicitation_modes().is_empty() {
            return "Client does not support elicitation. Cannot collect user details.".into();
        }

        match peer
            .elicit::<UserProfile>("Please provide the new user's name and email address.")
            .await
        {
            Ok(Some(profile)) => {
                format!(
                    "User created successfully!\n  Name: {}\n  Email: {}",
                    profile.name, profile.email
                )
            }
            Ok(None) => "No user data provided. User creation cancelled.".into(),
            Err(rmcp::service::ElicitationError::UserDeclined) => {
                "User declined to provide information. No account created.".into()
            }
            Err(rmcp::service::ElicitationError::UserCancelled) => {
                "User cancelled the request.".into()
            }
            Err(e) => format!("Elicitation failed: {e}"),
        }
    }

    /// Deploy an application to production. Requires confirmation via elicitation.
    #[tool(description = "Deploy the application to production. Requires user confirmation via elicitation.")]
    async fn deploy_app(
        &self,
        Parameters(DeployParams { app_name }): Parameters<DeployParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let peer = context.peer;

        if peer.supported_elicitation_modes().is_empty() {
            return "Client does not support elicitation. Cannot confirm deployment.".into();
        }

        match peer
            .elicit::<DeployConfirmation>(&format!(
                "You are about to deploy '{app_name}' to PRODUCTION. Type 'yes' to confirm."
            ))
            .await
        {
            Ok(Some(confirmation)) => {
                if confirmation.confirm.to_lowercase() == "yes" {
                    let reason = confirmation.reason.unwrap_or_else(|| "No reason given".into());
                    format!(
                        "Deploying '{app_name}' to production!\n  Reason: {reason}\n  Status: Success"
                    )
                } else {
                    format!(
                        "Deployment of '{app_name}' aborted. Confirmation was '{}'.",
                        confirmation.confirm
                    )
                }
            }
            Ok(None) => "No confirmation provided. Deployment aborted.".into(),
            Err(rmcp::service::ElicitationError::UserDeclined) => {
                "Deployment declined by user.".into()
            }
            Err(rmcp::service::ElicitationError::UserCancelled) => {
                "Deployment cancelled by user.".into()
            }
            Err(e) => format!("Elicitation failed: {e}"),
        }
    }
}

#[tool_handler]
impl ServerHandler for ElicitationServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("MCP server with elicitation-powered tools. Tools will ask the client for user input at runtime.")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = ElicitationServer::new();
    let service = rmcp::ServiceExt::serve(server, rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
