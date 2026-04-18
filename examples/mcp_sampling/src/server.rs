//! MCP server with a sampling-powered tool.
//!
//! Exposes a single tool that uses `sampling/createMessage` to ask the
//! connected client's LLM to generate a response. The server itself has
//! no LLM — it relies entirely on the client to provide inference.
//!
//! Spawned as a subprocess by the client. Communicates over stdio.

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CreateMessageRequestParams, SamplingMessage, ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    service::{RequestContext, RoleServer},
};
use serde::Deserialize;

// -- Tool parameter types -----------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SummarizeParams {
    /// The text to summarize via the client's LLM.
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TranslateParams {
    /// The text to translate.
    text: String,
    /// Target language for translation.
    language: String,
}

// -- Server -------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SamplingServer {
    tool_router: ToolRouter<Self>,
}

impl SamplingServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl SamplingServer {
    /// Summarize text using the client's LLM via MCP sampling.
    ///
    /// This tool does NOT have its own LLM. Instead, it sends a
    /// `sampling/createMessage` request back to the client, which
    /// routes it through the client's configured LLM provider.
    #[tool(description = "Summarize the given text. Uses the client's LLM via MCP sampling to generate the summary.")]
    async fn summarize(
        &self,
        Parameters(SummarizeParams { text }): Parameters<SummarizeParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let peer = context.peer;

        // Build a sampling request asking the client's LLM to summarize
        let messages = vec![SamplingMessage::user_text(format!(
            "Please provide a concise summary of the following text:\n\n{text}"
        ))];

        let params = CreateMessageRequestParams::new(messages, 1024);

        match peer.create_message(params).await {
            Ok(result) => {
                // Extract the text from the sampling response
                let response_text = result
                    .message
                    .content
                    .first()
                    .and_then(|c| match c {
                        rmcp::model::SamplingMessageContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "(no text in response)".to_string());

                format!(
                    "Summary (generated via MCP sampling by model '{}'):\n{}",
                    result.model, response_text
                )
            }
            Err(e) => format!("Sampling request failed: {e}"),
        }
    }

    /// Translate text using the client's LLM via MCP sampling.
    ///
    /// Sends a `sampling/createMessage` request with a system prompt
    /// instructing the LLM to act as a translator.
    #[tool(description = "Translate text to a target language. Uses the client's LLM via MCP sampling.")]
    async fn translate(
        &self,
        Parameters(TranslateParams { text, language }): Parameters<TranslateParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let peer = context.peer;

        let messages = vec![SamplingMessage::user_text(format!(
            "Translate the following text to {language}:\n\n{text}"
        ))];

        let mut params = CreateMessageRequestParams::new(messages, 1024);
        params.system_prompt = Some(format!(
            "You are a professional translator. Translate the user's text to {language}. \
             Output only the translation, nothing else."
        ));

        match peer.create_message(params).await {
            Ok(result) => {
                let response_text = result
                    .message
                    .content
                    .first()
                    .and_then(|c| match c {
                        rmcp::model::SamplingMessageContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "(no text in response)".to_string());

                format!(
                    "Translation to {language} (via MCP sampling, model '{}'):\n{response_text}",
                    result.model
                )
            }
            Err(e) => format!("Sampling request failed: {e}"),
        }
    }
}

#[tool_handler]
impl ServerHandler for SamplingServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "MCP server with sampling-powered tools. Tools use the client's LLM \
                 via sampling/createMessage to generate responses.",
            )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = SamplingServer::new();
    let service = rmcp::ServiceExt::serve(server, rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
