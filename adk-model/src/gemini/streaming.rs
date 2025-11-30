use adk_core::{Content, LlmResponse, Part};
use futures::Stream;
use std::pin::Pin;

/// Aggregate streaming responses into a single response
pub async fn aggregate_stream(
    mut stream: Pin<Box<dyn Stream<Item = adk_core::Result<LlmResponse>> + Send>>,
) -> adk_core::Result<LlmResponse> {
    use futures::StreamExt;

    let mut aggregated_text = String::new();
    let mut last_response: Option<LlmResponse> = None;

    while let Some(result) = stream.next().await {
        let response = result?;

        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    aggregated_text.push_str(text);
                }
            }
        }

        last_response = Some(response);
    }

    let mut final_response = last_response.ok_or_else(|| {
        adk_core::AdkError::Model("No responses received from stream".to_string())
    })?;

    final_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: aggregated_text }],
    });
    final_response.partial = false;
    final_response.turn_complete = true;

    Ok(final_response)
}
