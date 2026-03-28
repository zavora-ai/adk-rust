use adk_core::{LlmResponse, LlmResponseStream, Part};
use anyhow::Result;
use futures::StreamExt;

pub async fn collect_llm_responses(mut stream: LlmResponseStream) -> Result<Vec<LlmResponse>> {
    let mut responses = Vec::new();

    while let Some(item) = stream.next().await {
        responses.push(item?);
    }

    Ok(responses)
}

pub fn print_llm_responses(responses: &[LlmResponse]) {
    for (index, response) in responses.iter().enumerate() {
        println!(
            "chunk[{index}]: partial={} turn_complete={} finish_reason={:?}",
            response.partial, response.turn_complete, response.finish_reason
        );

        if let Some(content) = response.content.as_ref() {
            for part in &content.parts {
                match part {
                    Part::Text { text } => println!("  text: {}", trim_for_display(text)),
                    Part::Thinking { thinking, .. } => {
                        println!("  thinking: {}", trim_for_display(thinking));
                    }
                    Part::FunctionCall { name, args, id, .. } => {
                        println!(
                            "  function_call: id={} name={} args={}",
                            id.as_deref().unwrap_or("<none>"),
                            name,
                            trim_for_display(&args.to_string())
                        );
                    }
                    Part::FunctionResponse { function_response, id } => {
                        println!(
                            "  function_response: id={} name={} body={}",
                            id.as_deref().unwrap_or("<none>"),
                            function_response.name,
                            trim_for_display(&function_response.response.to_string())
                        );
                    }
                    Part::InlineData { mime_type, data } => {
                        println!("  inline_data: mime_type={mime_type} bytes={}", data.len());
                    }
                    Part::FileData { mime_type, file_uri } => {
                        println!("  file_data: mime_type={mime_type} uri={file_uri}");
                    }
                    Part::ServerToolCall { server_tool_call } => {
                        println!(
                            "  [server-tool-call]: {}",
                            trim_for_display(&server_tool_call.to_string())
                        );
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        println!(
                            "  [server-tool-response]: {}",
                            trim_for_display(&server_tool_response.to_string())
                        );
                    }
                }
            }
        }

        if let Some(usage) = response.usage_metadata.as_ref() {
            println!(
                "  usage: prompt={} completion={} total={} cost={:?}",
                usage.prompt_token_count,
                usage.candidates_token_count,
                usage.total_token_count,
                usage.cost
            );
        }
    }
}

fn trim_for_display(text: &str) -> String {
    const MAX_LEN: usize = 200;

    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= MAX_LEN { compact } else { format!("{}...", &compact[..MAX_LEN]) }
}
