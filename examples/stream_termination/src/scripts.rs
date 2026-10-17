//! Wire scripts for each fault, in the three stream formats the providers use: Chat
//! Completions SSE (OpenAI-compatible, Groq, OpenRouter Chat), Ollama NDJSON, and
//! Responses SSE (OpenRouter Responses).

use crate::upstream::{Reply, Script, split_inside};
use serde_json::{Value, json};
use std::sync::Arc;

/// The complete answer of a text turn.
pub const ANSWER: &str = "The capital of France is Paris.";
/// The answer split across two deltas.
const ANSWER_DELTAS: [&str; 2] = ["The capital of France ", "is Paris."];
/// The same answer cut off mid-word.
const CUT_DELTAS: [&str; 2] = ["The capital of France ", "is Par"];
/// An answer whose multi-byte characters straddle a network write.
pub const MULTIBYTE: &str = "Café ☕ in Zürich, then 東京 🙂.";
/// The bytes `split_inside` cuts in half.
const SPLIT_AT: &str = "🙂";
/// The answer after the weather tool ran.
pub const TOOL_ANSWER: &str = "It is sunny in Paris.";

/// The upstream behaviour a scenario scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A complete text answer.
    Complete,
    /// The stream ends mid-answer, before any finish reason or terminal event.
    CutMidAnswer,
    /// The stream ends while tool-call arguments are still arriving.
    CutMidToolCall,
    /// The provider reports an error frame mid-answer, then ends the stream normally.
    ErrorFrame,
    /// A Chat chunk finishes with `finish_reason: "error"` and no error object.
    ErrorFinishReason,
    /// Every SSE field is written `data:` with no space after the colon.
    DataWithoutSpace,
    /// A multi-byte character is split across two network writes.
    SplitMultibyte,
    /// A complete tool call, then the final answer once the tool result comes back.
    ToolCall,
    /// A tool call whose finish chunk reports `null` token counts.
    ToolCallNullUsage,
}

impl Fault {
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete-answer",
            Self::CutMidAnswer => "cut-mid-answer",
            Self::CutMidToolCall => "cut-mid-tool-call",
            Self::ErrorFrame => "error-frame",
            Self::ErrorFinishReason => "finish-reason-error",
            Self::DataWithoutSpace => "data-without-space",
            Self::SplitMultibyte => "split-multibyte",
            Self::ToolCall => "tool-call",
            Self::ToolCallNullUsage => "tool-call-null-usage",
        }
    }
}

/// Whether the request already carries the weather tool's result.
fn has_tool_result(body: &str) -> bool {
    body.contains(r#""role":"tool""#) || body.contains("function_call_output")
}

// ── Chat Completions SSE ───────────────────────────────────────────────

fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion.chunk",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    })
}

fn usage() -> Value {
    json!({"prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19})
}

fn finish(reason: &str) -> Value {
    let mut value = chunk(json!({}), Some(reason));
    value["usage"] = usage();
    value
}

fn text_chunks(deltas: &[&str]) -> Vec<Value> {
    let mut chunks = vec![chunk(json!({"role": "assistant", "content": ""}), None)];
    chunks.extend(deltas.iter().map(|delta| chunk(json!({"content": delta}), None)));
    chunks
}

fn tool_call_chunks(arguments: &[&str]) -> Vec<Value> {
    let mut chunks = vec![chunk(
        json!({"role": "assistant", "tool_calls": [{
            "index": 0,
            "id": "call_weather",
            "type": "function",
            "function": {"name": "get_weather", "arguments": ""},
        }]}),
        None,
    )];
    chunks.extend(arguments.iter().map(|fragment| {
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": fragment}}]}), None)
    }));
    chunks
}

fn sse(values: &[Value], separator: &str) -> Vec<Vec<u8>> {
    values.iter().map(|value| format!("data:{separator}{value}\n\n").into_bytes()).collect()
}

fn done(separator: &str) -> Vec<u8> {
    format!("data:{separator}[DONE]\n\n").into_bytes()
}

/// The OpenAI-shaped error object a compatible provider sends mid-stream.
fn openai_error() -> Value {
    json!({"error": {
        "message": "The server had an error while processing your request.",
        "type": "server_error",
        "code": "server_error",
    }})
}

/// The mid-stream error chunk OpenRouter documents: an error object beside a choice
/// that finishes with `error`.
fn openrouter_error() -> Value {
    let mut value = chunk(json!({"content": ""}), Some("error"));
    value["error"] = json!({"code": 502, "message": "Upstream provider disconnected"});
    value
}

fn chat_text(deltas: &[&str], separator: &str) -> Vec<Vec<u8>> {
    let mut chunks = text_chunks(deltas);
    chunks.push(finish("stop"));
    let mut writes = sse(&chunks, separator);
    writes.push(done(separator));
    writes
}

/// A Chat Completions script. `openrouter` selects OpenRouter's error-frame shape.
pub fn chat(fault: Fault, openrouter: bool) -> Script {
    Arc::new(move |body: &str| {
        let writes = match fault {
            Fault::Complete => chat_text(&ANSWER_DELTAS, " "),
            Fault::CutMidAnswer => sse(&text_chunks(&CUT_DELTAS), " "),
            // Once a tool result comes back, the turn is answered: on a client that runs the
            // truncated call, the run then looks like a success.
            Fault::CutMidToolCall if has_tool_result(body) => chat_text(&[TOOL_ANSWER], " "),
            Fault::CutMidToolCall => sse(&tool_call_chunks(&[r#"{"city":"Par"#]), " "),
            Fault::ErrorFrame => {
                let mut chunks = text_chunks(&CUT_DELTAS[..1]);
                chunks.push(if openrouter { openrouter_error() } else { openai_error() });
                let mut writes = sse(&chunks, " ");
                writes.push(done(" "));
                writes
            }
            Fault::ErrorFinishReason => {
                let mut chunks = text_chunks(&CUT_DELTAS[..1]);
                chunks.push(finish("error"));
                let mut writes = sse(&chunks, " ");
                writes.push(done(" "));
                writes
            }
            Fault::DataWithoutSpace => chat_text(&ANSWER_DELTAS, ""),
            Fault::SplitMultibyte => chat_text(&[MULTIBYTE], " ")
                .into_iter()
                .flat_map(|write| split_inside(write, SPLIT_AT))
                .collect(),
            Fault::ToolCall | Fault::ToolCallNullUsage if has_tool_result(body) => {
                chat_text(&[TOOL_ANSWER], " ")
            }
            Fault::ToolCall | Fault::ToolCallNullUsage => {
                let mut chunks = tool_call_chunks(&[r#"{"city":"#, r#""Paris"}"#]);
                let mut terminal = finish("tool_calls");
                if fault == Fault::ToolCallNullUsage {
                    terminal["usage"] = json!({"prompt_tokens": 12, "completion_tokens": null, "total_tokens": null});
                }
                chunks.push(terminal);
                let mut writes = sse(&chunks, " ");
                writes.push(done(" "));
                writes
            }
        };
        Reply::sse(writes)
    })
}

// ── Ollama NDJSON ──────────────────────────────────────────────────────

fn ollama_line(message: Value, done: bool) -> Vec<u8> {
    let mut line = json!({
        "model": "qwen3.5",
        "created_at": "2026-10-16T00:00:00Z",
        "message": message,
        "done": done,
    });
    if done {
        let fields = line.as_object_mut().expect("object");
        for (key, value) in [
            ("done_reason", json!("stop")),
            ("total_duration", json!(1_000)),
            ("load_duration", json!(10)),
            ("prompt_eval_count", json!(12)),
            ("prompt_eval_duration", json!(100)),
            ("eval_count", json!(7)),
            ("eval_duration", json!(100)),
        ] {
            fields.insert(key.into(), value);
        }
    }
    format!("{line}\n").into_bytes()
}

fn ollama_text(text: &str) -> Vec<u8> {
    ollama_line(json!({"role": "assistant", "content": text}), false)
}

fn ollama_done() -> Vec<u8> {
    ollama_line(json!({"role": "assistant", "content": ""}), true)
}

fn ollama_tool_call() -> Vec<u8> {
    ollama_line(
        json!({"role": "assistant", "content": "", "tool_calls": [{
            "function": {"name": "get_weather", "arguments": {"city": "Paris"}},
        }]}),
        false,
    )
}

fn ollama_answer(deltas: &[&str]) -> Vec<Vec<u8>> {
    let mut writes: Vec<Vec<u8>> = deltas.iter().map(|delta| ollama_text(delta)).collect();
    writes.push(ollama_done());
    writes
}

/// An Ollama `/api/chat` NDJSON script.
pub fn ollama(fault: Fault) -> Script {
    Arc::new(move |body: &str| {
        let writes = match fault {
            Fault::Complete => ollama_answer(&ANSWER_DELTAS),
            Fault::CutMidAnswer => CUT_DELTAS.iter().map(|delta| ollama_text(delta)).collect(),
            Fault::CutMidToolCall if has_tool_result(body) => ollama_answer(&[TOOL_ANSWER]),
            // Ollama sends each tool call whole, so the turn is cut after the call and
            // before the `done` line.
            Fault::CutMidToolCall => vec![ollama_tool_call()],
            Fault::ErrorFrame => vec![
                ollama_text(CUT_DELTAS[0]),
                br#"{"error":"an error was encountered while running the model: unexpected EOF"}"#
                    .iter()
                    .copied()
                    .chain(*b"\n")
                    .collect(),
            ],
            Fault::SplitMultibyte => ollama_answer(&[MULTIBYTE])
                .into_iter()
                .flat_map(|write| split_inside(write, SPLIT_AT))
                .collect(),
            Fault::ToolCall if has_tool_result(body) => ollama_answer(&[TOOL_ANSWER]),
            Fault::ToolCall => vec![ollama_tool_call(), ollama_done()],
            Fault::ErrorFinishReason | Fault::DataWithoutSpace | Fault::ToolCallNullUsage => {
                unreachable!("not an Ollama scenario")
            }
        };
        Reply::ndjson(writes)
    })
}

// ── Responses SSE ──────────────────────────────────────────────────────

fn event(kind: &str, mut fields: Value) -> Value {
    fields["type"] = json!(kind);
    fields
}

fn message_item(text: &str) -> Value {
    json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

fn call_item(arguments: &str, status: &str) -> Value {
    json!({
        "id": "fc_1",
        "type": "function_call",
        "call_id": "call_weather",
        "name": "get_weather",
        "arguments": arguments,
        "status": status,
    })
}

fn response(status: &str, output: Vec<Value>) -> Value {
    json!({
        "id": "resp_scripted",
        "object": "response",
        "created_at": 1_760_000_000,
        "model": "openai/gpt-4.1-mini",
        "status": status,
        "output": output,
        "usage": {"input_tokens": 12, "output_tokens": 7, "total_tokens": 19},
    })
}

fn responses_text_events(deltas: &[&str]) -> Vec<Value> {
    let mut events = vec![
        event("response.created", json!({"response": response("in_progress", vec![])})),
        event(
            "response.output_item.added",
            json!({"output_index": 0, "item": {"id": "msg_1", "type": "message", "role": "assistant", "status": "in_progress", "content": []}}),
        ),
    ];
    events.extend(deltas.iter().map(|delta| {
        event(
            "response.output_text.delta",
            json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": delta}),
        )
    }));
    events
}

fn responses_text(deltas: &[&str], separator: &str) -> Vec<Vec<u8>> {
    let text = deltas.concat();
    let mut events = responses_text_events(deltas);
    events.push(event(
        "response.output_text.done",
        json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "text": text}),
    ));
    events.push(event(
        "response.output_item.done",
        json!({"output_index": 0, "item": message_item(&text)}),
    ));
    events.push(event(
        "response.completed",
        json!({"response": response("completed", vec![message_item(&text)])}),
    ));
    let mut writes = sse(&events, separator);
    writes.push(done(separator));
    writes
}

/// The failure body of a `response.failed` event or a failed non-streaming response.
fn failed_response() -> Value {
    let mut failed = response("failed", vec![]);
    failed["error"] =
        json!({"code": "server_error", "message": "The model failed to generate a response."});
    failed
}

/// An OpenRouter Responses SSE script.
pub fn responses(fault: Fault) -> Script {
    Arc::new(move |body: &str| {
        let writes = match fault {
            Fault::Complete => responses_text(&ANSWER_DELTAS, " "),
            Fault::CutMidAnswer => sse(&responses_text_events(&CUT_DELTAS), " "),
            Fault::CutMidToolCall if has_tool_result(body) => responses_text(&[TOOL_ANSWER], " "),
            Fault::CutMidToolCall => sse(
                &[
                    event("response.created", json!({"response": response("in_progress", vec![])})),
                    event(
                        "response.output_item.added",
                        json!({"output_index": 0, "item": call_item("", "in_progress")}),
                    ),
                    event(
                        "response.function_call_arguments.delta",
                        json!({"item_id": "fc_1", "output_index": 0, "delta": r#"{"city":"Par"#}),
                    ),
                ],
                " ",
            ),
            Fault::ErrorFrame => {
                let mut events = responses_text_events(&CUT_DELTAS[..1]);
                events.push(event("response.failed", json!({"response": failed_response()})));
                let mut writes = sse(&events, " ");
                writes.push(done(" "));
                writes
            }
            Fault::DataWithoutSpace => responses_text(&ANSWER_DELTAS, ""),
            Fault::SplitMultibyte => responses_text(&[MULTIBYTE], " ")
                .into_iter()
                .flat_map(|write| split_inside(write, SPLIT_AT))
                .collect(),
            Fault::ToolCall if has_tool_result(body) => responses_text(&[TOOL_ANSWER], " "),
            Fault::ToolCall => {
                let arguments = r#"{"city":"Paris"}"#;
                let events = [
                    event("response.created", json!({"response": response("in_progress", vec![])})),
                    event(
                        "response.output_item.added",
                        json!({"output_index": 0, "item": call_item("", "in_progress")}),
                    ),
                    event(
                        "response.function_call_arguments.delta",
                        json!({"item_id": "fc_1", "output_index": 0, "delta": arguments}),
                    ),
                    event(
                        "response.function_call_arguments.done",
                        json!({"item_id": "fc_1", "output_index": 0, "arguments": arguments}),
                    ),
                    event(
                        "response.output_item.done",
                        json!({"output_index": 0, "item": call_item(arguments, "completed")}),
                    ),
                    event(
                        "response.completed",
                        json!({"response": response("completed", vec![call_item(arguments, "completed")])}),
                    ),
                ];
                let mut writes = sse(&events, " ");
                writes.push(done(" "));
                writes
            }
            Fault::ErrorFinishReason | Fault::ToolCallNullUsage => {
                unreachable!("not a Responses scenario")
            }
        };
        Reply::sse(writes)
    })
}

/// A non-streaming Responses body whose generation failed.
pub fn responses_failed_json() -> Script {
    Arc::new(|_body: &str| Reply::json(&failed_response()))
}
