use adk_model::openrouter::{
    OpenRouterChatMessage, OpenRouterChatMessageContent, OpenRouterChatResponse,
};

pub fn print_chat_response(response: &OpenRouterChatResponse) {
    println!("model: {}", response.model.as_deref().unwrap_or("<unknown>"));

    if let Some(choice) = response.choices.first() {
        if let Some(message) = choice.message.as_ref() {
            if let Some(reasoning) = message.reasoning.as_deref() {
                println!("reasoning: {}", trim_for_display(reasoning));
            }

            if let Some(text) = chat_message_text(message) {
                println!("text: {}", trim_for_display(&text));
            }

            if let Some(tool_calls) = message.tool_calls.as_ref() {
                for tool_call in tool_calls {
                    let function_name = tool_call
                        .function
                        .as_ref()
                        .and_then(|function| function.name.as_deref())
                        .unwrap_or("<unnamed>");
                    let arguments = tool_call
                        .function
                        .as_ref()
                        .and_then(|function| function.arguments.as_deref())
                        .unwrap_or("{}");
                    println!(
                        "tool_call: id={} name={} args={}",
                        tool_call.id.as_deref().unwrap_or("<none>"),
                        function_name,
                        trim_for_display(arguments)
                    );
                }
            }
        }

        if let Some(finish_reason) = choice.finish_reason.as_deref() {
            println!("finish_reason: {finish_reason}");
        }
    }
}

pub fn chat_message_text(message: &OpenRouterChatMessage) -> Option<String> {
    match message.content.as_ref()? {
        OpenRouterChatMessageContent::Text(text) => Some(text.clone()),
        OpenRouterChatMessageContent::Parts(parts) => {
            let text = parts
                .iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
    }
}

fn trim_for_display(text: &str) -> String {
    const MAX_LEN: usize = 200;

    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= MAX_LEN { compact } else { format!("{}...", &compact[..MAX_LEN]) }
}
