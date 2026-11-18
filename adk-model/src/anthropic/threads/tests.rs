use super::*;
use adk_anthropic::{
    CompactionBlock, ContentBlock, MessageRole, Model, SystemPrompt, ThinkingConfig,
};

fn user(text: &str) -> MessageParam {
    MessageParam::new_with_string(text.to_string(), MessageRole::User)
}

fn assistant(text: &str) -> MessageParam {
    MessageParam::new_with_string(text.to_string(), MessageRole::Assistant)
}

fn params(messages: &[MessageParam]) -> MessageCreateParams {
    let mut params = MessageCreateParams::new(
        1_024,
        messages.to_vec(),
        Model::Custom("claude-sonnet-5-5".to_string()),
    );
    params.system = Some(SystemPrompt::from_string("You plan trips.".to_string()));
    params
}

/// A first request, answered with `reply` as response `msg_1`.
fn answered(threads: &Threads, history: &[MessageParam], reply: &MessageParam) {
    let mut first = params(history);
    let plan = threads.prepare(&mut first).expect("threaded");
    assert_eq!(first.thread, Some(ThreadParam::Create));
    assert!(!plan.continues);
    threads.record(&plan, "msg_1", reply.clone());
}

#[test]
fn a_request_that_extends_what_the_thread_holds_sends_only_the_new_messages() {
    let threads = Threads::default();
    let reply = assistant("Lisbon has trams.");
    answered(&threads, &[user("Plan a weekend in Lisbon.")], &reply);

    let history = [user("Plan a weekend in Lisbon."), reply, user("And the food?")];
    let mut next = params(&history);
    let plan = threads.prepare(&mut next).unwrap();

    assert!(plan.continues);
    assert_eq!(next.thread, Some(ThreadParam::continue_from("msg_1")));
    assert_eq!(next.messages, [user("And the food?")]);
    assert_eq!((next.system.is_none(), next.tools.is_none()), (true, true));
    assert!(next.betas.contains(&adk_anthropic::MESSAGE_THREADS_BETA.to_string()));
    assert_eq!(plan.unthreaded(), params(&history), "a refused thread falls back to this");

    // Each response continues once: a retry of the same request creates a new thread.
    let mut retry = params(&history);
    assert!(!threads.prepare(&mut retry).unwrap().continues);
}

#[test]
fn a_rewritten_turn_or_a_changed_fixed_field_starts_a_new_thread() {
    let threads = Threads::default();
    let reply = assistant("Lisbon has trams.");
    answered(&threads, &[user("Plan a weekend in Lisbon.")], &reply);
    // A guardrail rewrote the reply in the session.
    let rewritten = [user("Plan a weekend in Lisbon."), assistant("[redacted]"), user("And?")];
    let mut next = params(&rewritten);
    assert!(!threads.prepare(&mut next).unwrap().continues);
    assert_eq!(next.messages, rewritten);

    answered(&threads, &[user("Plan a weekend in Lisbon.")], &reply);
    let mut thinking = params(&[user("Plan a weekend in Lisbon."), reply, user("And?")]);
    thinking.thinking = Some(ThinkingConfig::adaptive());
    assert!(!threads.prepare(&mut thinking).unwrap().continues);
}

#[test]
fn a_conversation_with_a_compaction_or_one_the_api_refused_is_not_threaded() {
    let threads = Threads::default();
    let block = ContentBlock::Compaction(CompactionBlock::new("Lisbon weekend.", "sig"));
    let mut compacted =
        params(&[MessageParam::new_with_blocks(vec![block], MessageRole::Assistant), user("And?")]);
    assert!(threads.prepare(&mut compacted).is_none());
    assert_eq!(compacted.thread, None);

    let mut refused = params(&[user("Plan a weekend in Lisbon.")]);
    let plan = threads.prepare(&mut refused).unwrap();
    threads.unthread(&plan);
    let mut again = params(&[user("Plan a weekend in Lisbon.")]);
    assert!(threads.prepare(&mut again).is_none());
}
