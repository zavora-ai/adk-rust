/*!
# URL Context Tool Example

This example demonstrates how to use the URL Context tool to analyze and extract
information from web pages. The URL Context tool allows the Gemini model to read
and understand content from web URLs, making it useful for:

- Summarizing articles and blog posts
- Extracting information from documentation pages
- Fact-checking claims against web sources
- Comparing content from multiple URLs
- Research and synthesis from web content

## Usage

```bash
GEMINI_API_KEY="your-api-key" cargo run --example url_context
```

## What This Example Shows

1. **Basic URL Analysis** - Summarizing content from a news article
2. **Documentation Extraction** - Extracting key features from technical docs
3. **Content Comparison** - Comparing information from multiple sources
4. **Fact Checking** - Verifying claims against authoritative sources
5. **Research Synthesis** - Analyzing academic papers and research content

## API Structure

The URL Context tool uses an empty configuration similar to Google Search:

```rust
let url_context_tool = Tool::url_context();
```

The tool automatically fetches and processes content from URLs mentioned in the user's message,
making the content available to the model for analysis and response generation.

## Features Demonstrated

- Simple URL content analysis
- Multi-URL comparison
- Fact-checking workflows
- Research assistance
- Documentation summarization

## See Also

- [`google_search.rs`](google_search.rs) - Google Search tool integration
*/

use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, Tool};
use std::env;
use std::process::ExitCode;
use tracing::info;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    match do_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let error_chain = DisplayErrorChain::new(e.as_ref());
            tracing::error!(error.debug = ?e, error.chained = %error_chain, "execution failed");
            ExitCode::FAILURE
        }
    }
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment variable
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");

    // Create client
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    info!("starting url context tool example");

    // Create a URL Context tool
    let url_context_tool = Tool::url_context();

    // Example 1: Analyze a news article
    info!("example 1: analyzing news article content");
    let response1 = client
        .generate_content()
        .with_user_message("Please summarize the main points from this article: https://blog.google/technology/ai/google-gemini-ai/")
        .with_tool(url_context_tool.clone())
        .execute()
        .await?;

    info!(
        response = response1.text(),
        "news article analysis completed"
    );

    // Example 2: Extract information from documentation
    info!("example 2: extracting documentation information");
    let response2 = client
        .generate_content()
        .with_user_message(
            "What are the key features mentioned on this page: https://docs.rs/tokio/latest/tokio/",
        )
        .with_tool(url_context_tool.clone())
        .execute()
        .await?;

    info!(
        response = response2.text(),
        "documentation analysis completed"
    );

    // Example 3: Compare content from multiple URLs
    info!("example 3: comparing content from multiple sources");
    let response3 = client
        .generate_content()
        .with_user_message(
            "Compare the features described on these two pages: \
             https://www.rust-lang.org/learn and https://go.dev/learn. \
             What are the similarities and differences in their learning approaches?",
        )
        .with_tool(url_context_tool.clone())
        .execute()
        .await?;

    info!(response = response3.text(), "content comparison completed");

    // Example 4: Fact checking with URL context
    info!("example 4: fact checking with url context");
    let response4 = client
        .generate_content()
        .with_user_message(
            "Based on the information from https://www.who.int/news-room/fact-sheets, \
             can you verify claims about global health statistics and provide accurate data?",
        )
        .with_tool(url_context_tool.clone())
        .execute()
        .await?;

    info!(response = response4.text(), "fact checking completed");

    // Example 5: Research synthesis
    info!("example 5: research synthesis from academic source");
    let response5 = client
        .generate_content()
        .with_user_message(
            "Please read this research paper abstract and methodology section: \
             https://arxiv.org/abs/2103.00020 and explain the key findings in simple terms.",
        )
        .with_tool(url_context_tool)
        .execute()
        .await?;

    info!(response = response5.text(), "research synthesis completed");

    Ok(())
}
