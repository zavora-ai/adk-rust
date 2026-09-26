use adk_core::{Content, Llm, LlmRequest};
use adk_model::opencode::{OpenCodeClient, OpenCodeConfig, OpenCodeService};
use futures::TryStreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    adk_core::ensure_crypto_provider();
    let model = OpenCodeClient::new(
        OpenCodeConfig::new(OpenCodeService::Go, std::env::var("OPENCODE_API_KEY")?, "deepseek-v4.1-flash")
            .with_user_agent("adk-opencode-example/1.0")
            .with_session_id("example-conversation-1"),
    )?;
    let request = LlmRequest::new(
        model.name(),
        vec![Content::new("user").with_text("Explain how to test a Rust function that reads a file")],
    );
    let mut stream = model.generate_content(request, true).await?;
    while let Some(response) = stream.try_next().await? {
        if let Some(content) = response.content {
            println!("{content:?}");
        }
    }
    Ok(())
}
