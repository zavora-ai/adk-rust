use super::*;
use adk_anthropic::{SystemPrompt, ThinkingBlock, ToolParam};
use serde_json::json;

fn tool(name: &str, description: &str) -> ToolUnionParam {
    ToolUnionParam::CustomTool(
        ToolParam::new(name.to_string(), json!({"type": "object", "properties": {}}))
            .with_description(description.to_string()),
    )
}

fn user(text: &str) -> MessageParam {
    MessageParam::new_with_string(text.to_string(), MessageRole::User)
}

fn assistant(text: &str) -> MessageParam {
    MessageParam::new_with_string(text.to_string(), MessageRole::Assistant)
}

fn params(messages: &[MessageParam], tools: &[ToolUnionParam]) -> MessageCreateParams {
    let mut params = MessageCreateParams::new(
        1_024,
        messages.to_vec(),
        Model::Custom("claude-sonnet-5-5".to_string()),
    );
    params.system = Some(SystemPrompt::from_string("You plan trips.".to_string()));
    params.tools = Some(tools.to_vec());
    params
}

fn names(params: &MessageCreateParams) -> Vec<String> {
    params
        .tools
        .iter()
        .flatten()
        .filter_map(|tool| tool_name(&serde_json::to_value(tool).unwrap()))
        .collect()
}

/// The tool changes `params` carries: the index of each system message and its blocks as JSON.
fn changes(params: &MessageCreateParams) -> Vec<(usize, Value)> {
    params
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == MessageRole::System)
        .map(|(index, message)| (index, serde_json::to_value(&message.content).unwrap()))
        .collect()
}

fn addition(tool: &ToolUnionParam) -> Value {
    json!({"type": "tool_addition", "tool": {"type": "tool_definition", "definition": serde_json::to_value(tool).unwrap()}})
}

#[test]
fn a_tool_added_mid_conversation_arrives_as_a_change_after_the_last_user_turn() {
    let ledgers = ToolLedgers::default();
    let (weather, forecast) =
        (tool("get_weather", "Weather now."), tool("get_forecast", "Five days."));
    let mut history = vec![user("Plan a weekend in Lisbon.")];

    let mut first = params(&history, std::slice::from_ref(&weather));
    ledgers.apply(&mut first);
    assert_eq!(
        first,
        params(&history, std::slice::from_ref(&weather)),
        "the first request is sent as it is"
    );

    history.extend([assistant("Checking the weather."), user("Also the forecast, please.")]);
    let mut second = params(&history, &[weather.clone(), forecast.clone()]);
    ledgers.apply(&mut second);
    assert_eq!(names(&second), ["get_weather"], "the tools array stays as first sent");
    assert_eq!(changes(&second), vec![(3, json!([addition(&forecast)]))]);
    assert!(second.requires_inline_tools_beta());

    // Later requests keep the change where it was sent.
    history.extend([assistant("Sunny, then rain."), user("Thanks.")]);
    let mut third = params(&history, &[weather.clone(), forecast.clone()]);
    ledgers.apply(&mut third);
    assert_eq!(names(&third), ["get_weather"]);
    assert_eq!(changes(&third), vec![(3, json!([addition(&forecast)]))]);
    assert_eq!(third.messages.len(), history.len() + 1);
}

#[test]
fn removed_and_redefined_tools_are_changes_too() {
    let ledgers = ToolLedgers::default();
    let (weather, forecast) =
        (tool("get_weather", "Weather now."), tool("get_forecast", "Five days."));
    let mut history = vec![user("Plan a weekend in Lisbon.")];
    ledgers.apply(&mut params(&history, &[weather.clone(), forecast.clone()]));

    history.extend([assistant("Checking."), user("Skip the forecast.")]);
    let mut removal = params(&history, std::slice::from_ref(&weather));
    ledgers.apply(&mut removal);
    assert_eq!(names(&removal), ["get_weather", "get_forecast"]);
    assert_eq!(
        changes(&removal),
        vec![(
            3,
            json!([{"type": "tool_removal", "tool": {"type": "tool_reference", "name": "get_forecast"}}])
        )]
    );

    history.extend([assistant("Done."), user("Use hourly weather.")]);
    let hourly = tool("get_weather", "Hourly weather.");
    let mut redefinition = params(&history, std::slice::from_ref(&hourly));
    ledgers.apply(&mut redefinition);
    assert_eq!(changes(&redefinition)[1], (6, json!([addition(&hourly)])));
}

#[test]
fn a_conversation_whose_history_changed_starts_over_with_its_tools_as_they_are() {
    let ledgers = ToolLedgers::default();
    let (weather, forecast) =
        (tool("get_weather", "Weather now."), tool("get_forecast", "Five days."));
    let mut history = vec![user("Plan a weekend in Lisbon.")];
    ledgers.apply(&mut params(&history, std::slice::from_ref(&weather)));
    history.extend([assistant("Checking."), user("Also the forecast.")]);
    ledgers.apply(&mut params(&history, &[weather.clone(), forecast.clone()]));

    // A message before the change was rewritten.
    history[1] = assistant("Checking the sky.");
    history.extend([assistant("Sunny."), user("Thanks.")]);
    let mut edited = params(&history, &[weather.clone(), forecast.clone()]);
    ledgers.apply(&mut edited);
    assert_eq!(edited, params(&history, &[weather, forecast]));
}

#[test]
fn a_change_that_cannot_follow_a_user_turn_starts_the_conversation_over() {
    let ledgers = ToolLedgers::default();
    let (weather, forecast) =
        (tool("get_weather", "Weather now."), tool("get_forecast", "Five days."));
    let history = vec![user("Plan a weekend."), assistant("On it.")];
    ledgers.apply(&mut params(&history[..1], std::slice::from_ref(&weather)));

    let mut after_assistant = params(&history, &[weather.clone(), forecast.clone()]);
    ledgers.apply(&mut after_assistant);
    assert_eq!(after_assistant, params(&history, &[weather, forecast]));
}

#[test]
fn a_compaction_request_carries_the_changes_before_it_and_records_none() {
    let ledgers = ToolLedgers::default();
    let (weather, forecast, flights) = (
        tool("get_weather", "Weather now."),
        tool("get_forecast", "Five days."),
        tool("find_flights", "Flights."),
    );
    let mut history = vec![user("Plan a weekend in Lisbon.")];
    ledgers.apply(&mut params(&history, std::slice::from_ref(&weather)));
    history.extend([assistant("Checking."), user("Also the forecast.")]);
    ledgers.apply(&mut params(&history, &[weather.clone(), forecast.clone()]));
    history.extend([assistant("Sunny."), user("Book it.")]);

    let mut compaction = params(&history, &[weather.clone(), forecast.clone(), flights.clone()]);
    compaction.compaction = Some(adk_anthropic::CompactionParam::summarize());
    let tools = ledgers.compaction_tools(&compaction).expect("a tracked conversation");
    ledgers.apply(&mut compaction);
    assert_eq!(names(&compaction), ["get_weather"]);
    assert_eq!(
        changes(&compaction),
        vec![(3, json!([addition(&forecast)]))],
        "no change for flights"
    );

    // The conversation that opens with the summary keeps the same tools array and offers what
    // the summary records, so only the tool added since is a change.
    let block = CompactionBlock::new("Lisbon weekend planned.", "sig");
    ledgers.continue_after(&compaction.model, &compaction, &block, tools);
    let next = vec![
        MessageParam::new_with_blocks(
            vec![ContentBlock::Compaction(block)],
            MessageRole::Assistant,
        ),
        user("Now find flights."),
    ];
    let mut continued = params(&next, &[weather, forecast, flights.clone()]);
    ledgers.apply(&mut continued);
    assert_eq!(names(&continued), ["get_weather"]);
    assert_eq!(changes(&continued), vec![(2, json!([addition(&flights)]))]);
}

#[test]
fn kept_thinking_is_left_out_when_a_change_after_the_summary_is_gone() {
    let ledgers = ToolLedgers::default();
    let (weather, forecast) =
        (tool("get_weather", "Weather now."), tool("get_forecast", "Five days."));
    let mut history = vec![user("Plan a weekend in Lisbon.")];
    ledgers.apply(&mut params(&history, std::slice::from_ref(&weather)));
    history.extend([assistant("Checking."), user("Also the forecast.")]);
    ledgers.apply(&mut params(&history, &[weather.clone(), forecast.clone()]));

    // Only the first message is summarized; the change after it is not recorded in the summary.
    let mut compaction = params(&history[..1], &[weather.clone(), forecast.clone()]);
    compaction.compaction = Some(adk_anthropic::CompactionParam::summarize());
    let tools = ledgers.compaction_tools(&compaction).unwrap();
    let block = CompactionBlock::new("Lisbon.", "sig");
    ledgers.continue_after(&compaction.model, &compaction, &block, tools);
    let thinking = ContentBlock::Thinking(ThinkingBlock::new("", "think-1"));
    let next = vec![
        MessageParam::new_with_blocks(
            vec![
                ContentBlock::Compaction(block),
                thinking,
                ContentBlock::Text(adk_anthropic::TextBlock::new("Checking.")),
            ],
            MessageRole::Assistant,
        ),
        user("Also the forecast."),
    ];
    let mut continued = params(&next, &[weather, forecast]);
    ledgers.apply(&mut continued);
    let MessageParamContent::Array(blocks) = &continued.messages[0].content else { panic!() };
    assert!(!blocks.iter().any(is_thinking), "kept thinking bound to the lost change is left out");
}
