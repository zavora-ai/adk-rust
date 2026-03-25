use adk_model::openrouter::{OpenRouterResponse, OpenRouterResponseOutputItem};

pub fn print_responses_response(response: &OpenRouterResponse) {
    println!("id: {}", response.id.as_deref().unwrap_or("<none>"));
    println!("status: {}", response.status.as_deref().unwrap_or("<unknown>"));
    println!("model: {}", response.model.as_deref().unwrap_or("<unknown>"));

    if let Some(output_text) = response.output_text.as_deref().filter(|text| !text.is_empty()) {
        println!("output_text: {}", trim_for_display(output_text));
    }

    for item in &response.output {
        println!(
            "output_item: type={} id={} status={}",
            item.kind,
            item.id.as_deref().unwrap_or("<none>"),
            item.status.as_deref().unwrap_or("<none>")
        );

        if let Some(text) = response_output_text(item) {
            println!("  text: {}", trim_for_display(&text));
        }

        if let (Some(name), Some(arguments)) = (item.name.as_deref(), item.arguments.as_deref()) {
            println!("  function_call: {name} {}", trim_for_display(arguments));
        }
    }

    if let Some(usage) = response.usage.as_ref() {
        println!(
            "usage: input={:?} output={:?} total={:?} cost={:?}",
            usage.input_tokens, usage.output_tokens, usage.total_tokens, usage.cost
        );
    }
}

fn response_output_text(item: &OpenRouterResponseOutputItem) -> Option<String> {
    let content = item.content.as_ref()?.as_array()?;
    let text = content
        .iter()
        .filter(|part| part.get("type").and_then(|value| value.as_str()) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

fn trim_for_display(text: &str) -> String {
    const MAX_LEN: usize = 200;

    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= MAX_LEN { compact } else { format!("{}...", &compact[..MAX_LEN]) }
}
