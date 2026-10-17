//! A scripted model provider on 127.0.0.1 that reports `null` and missing token counts.
//!
//! The real provider client is pointed at it through its base URL. Every request is
//! recorded; the first is answered with a `get_weather` call and the second with a text
//! answer, each in the provider's own wire format, unary or streamed, and each carrying a
//! usage object with `null` or missing counts and fields the typed client does not model.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// The provider wire formats the scripted upstream speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    OpenAiResponses,
    Anthropic,
    Gemini,
    GeminiInteractions,
    DeepSeek,
    Groq,
    Ollama,
}

/// What the server reported for one model call, and what the agent must see for it.
pub struct ExpectedUsage {
    pub prompt: i32,
    pub candidates: i32,
    pub total: i32,
    /// The usage object as the provider sent it; for a streamed Anthropic message, the
    /// `message_start` usage with the `message_delta` usage applied over it.
    pub provider_usage: Value,
}

/// The usage each of the two scripted turns reports.
pub fn expected(provider: Provider, turn: usize, stream: bool) -> ExpectedUsage {
    let (prompt, candidates, total) = match (provider, turn) {
        (Provider::OpenAiResponses, 0) => (21, 0, 21),
        (Provider::OpenAiResponses, _) => (40, 8, 48),
        (Provider::Anthropic, 0) if stream => (14, 0, 14),
        (Provider::Anthropic, 0) => (0, 9, 9),
        (Provider::Anthropic, _) => (30, 0, 30),
        (Provider::Gemini, 0) => (30, 0, 30),
        (Provider::Gemini, _) => (52, 9, 61),
        (Provider::GeminiInteractions, 0) => (40, 0, 40),
        (Provider::GeminiInteractions, _) => (61, 5, 66),
        (Provider::DeepSeek, 0) => (17, 0, 17),
        (Provider::DeepSeek, _) => (33, 6, 39),
        (Provider::Groq, 0) => (19, 0, 19),
        (Provider::Groq, _) => (35, 7, 42),
        (Provider::Ollama, 0) => (0, 6, 6),
        (Provider::Ollama, _) => (44, 8, 52),
    };
    let provider_usage = match (provider, stream) {
        (Provider::Anthropic, true) => {
            let (start, delta) = anthropic_stream_usage(turn);
            let mut merged = start;
            merged.as_object_mut().unwrap().extend(delta.as_object().unwrap().clone());
            merged
        }
        (Provider::Ollama, _) => ollama_metrics(turn),
        _ => usage(provider, turn),
    };
    ExpectedUsage { prompt, candidates, total, provider_usage }
}

/// The usage object turn `turn` reports, with `null` and missing counts.
fn usage(provider: Provider, turn: usize) -> Value {
    match (provider, turn) {
        (Provider::OpenAiResponses, 0) => json!({
            "input_tokens": 21, "output_tokens": null, "total_tokens": null,
            "input_tokens_details": {"cached_tokens": null}, "output_tokens_details": null
        }),
        (Provider::OpenAiResponses, _) => json!({
            "input_tokens": 40, "output_tokens": 8, "total_tokens": null,
            "input_tokens_details": null, "output_tokens_details": {"reasoning_tokens": null}
        }),
        (Provider::Anthropic, 0) => json!({
            "input_tokens": null, "output_tokens": 9, "cache_read_input_tokens": null,
            "server_tool_use": {"web_search_requests": null}, "service_tier": "standard"
        }),
        (Provider::Anthropic, _) => json!({
            "input_tokens": 30, "output_tokens": null,
            "service_tier": "standard", "inference_geo": "global"
        }),
        (Provider::Gemini, 0) => json!({
            "promptTokenCount": 30, "candidatesTokenCount": null,
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": null}],
            "toolUsePromptTokenCount": 4, "trafficType": "ON_DEMAND"
        }),
        (Provider::Gemini, _) => json!({
            "promptTokenCount": 52, "candidatesTokenCount": 9, "totalTokenCount": null,
            "thoughtsTokenCount": null, "cacheTokensDetails": [{"modality": "TEXT"}],
            "serviceTier": "standard"
        }),
        (Provider::GeminiInteractions, 0) => json!({
            "total_input_tokens": 40, "total_output_tokens": null, "total_tokens": null,
            "input_tokens_by_modality": [{"modality": "text", "tokens": null}]
        }),
        (Provider::GeminiInteractions, _) => json!({
            "total_input_tokens": 61, "total_output_tokens": 5,
            "total_thought_tokens": null, "total_tokens": null
        }),
        (Provider::DeepSeek, 0) => json!({
            "prompt_tokens": 17, "completion_tokens": null, "total_tokens": null,
            "prompt_cache_hit_tokens": null, "prompt_cache_miss_tokens": 17
        }),
        (Provider::DeepSeek, _) => json!({
            "prompt_tokens": 33, "completion_tokens": 6, "total_tokens": null,
            "completion_tokens_details": {"reasoning_tokens": null}
        }),
        (Provider::Groq, 0) => json!({
            "queue_time": 0.25, "prompt_tokens": 19, "completion_tokens": null, "total_tokens": null
        }),
        (Provider::Groq, _) => json!({
            "queue_time": 0.1, "prompt_tokens": 35, "completion_tokens": 7, "total_tokens": null,
            "completion_tokens_details": {"reasoning_tokens": null}
        }),
        (Provider::Ollama, turn) => ollama_metrics(turn),
    }
}

/// Usage on `message_start` and on `message_delta` for a streamed Anthropic turn.
fn anthropic_stream_usage(turn: usize) -> (Value, Value) {
    match turn {
        0 => (
            json!({"input_tokens": 14, "output_tokens": null, "service_tier": "standard"}),
            json!({"output_tokens": null, "server_tool_use": {"web_search_requests": null}}),
        ),
        _ => (
            json!({"input_tokens": 30, "output_tokens": 1, "service_tier": "standard"}),
            json!({"output_tokens": null, "inference_geo": "global"}),
        ),
    }
}

/// Metrics on Ollama's final line. The first leaves out `prompt_eval_count`, as Ollama does
/// when the whole prompt came from its cache.
fn ollama_metrics(turn: usize) -> Value {
    match turn {
        0 => json!({"total_duration": 1200, "eval_count": 6, "eval_duration": 900}),
        _ => json!({
            "total_duration": 2000, "load_duration": 10, "prompt_eval_count": 44,
            "prompt_eval_duration": 300, "eval_count": 8, "eval_duration": 700
        }),
    }
}

pub const ANSWER: &str = "It is sunny in Paris.";
const ARGS: &str = r#"{"city":"Paris"}"#;

enum Reply {
    Json(Value),
    Sse(String),
}

fn sse(events: &[Value]) -> String {
    events.iter().map(|event| format!("data: {event}\n\n")).collect()
}

fn reply(provider: Provider, turn: usize, stream: bool) -> Reply {
    let call = turn == 0;
    match provider {
        Provider::OpenAiResponses => {
            let output = if call {
                json!([{"type": "function_call", "id": "fc_1", "call_id": "call_1",
                    "name": "get_weather", "arguments": ARGS, "status": "completed"}])
            } else {
                json!([{"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": ANSWER, "annotations": []}]}])
            };
            let response = json!({"id": format!("resp_{turn}"), "object": "response",
                "status": "completed", "output": output, "usage": usage(provider, turn)});
            if stream {
                Reply::Sse(sse(&[
                    json!({"type": "response.created", "response": {"id": format!("resp_{turn}"), "status": "in_progress"}}),
                    json!({"type": "response.completed", "response": response}),
                ]))
            } else {
                Reply::Json(response)
            }
        }
        Provider::Anthropic => {
            if stream {
                let (start, delta) = anthropic_stream_usage(turn);
                let mut events = vec![(
                    "message_start",
                    json!({"type": "message_start", "message": {"id": format!("msg_{turn}"),
                        "type": "message", "role": "assistant", "content": [],
                        "model": "claude-sonnet-4-6", "stop_reason": null, "stop_sequence": null,
                        "usage": start}}),
                )];
                if call {
                    events.push(("content_block_start", json!({"type": "content_block_start", "index": 0,
                        "content_block": {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}})));
                    events.push((
                        "content_block_delta",
                        json!({"type": "content_block_delta", "index": 0,
                        "delta": {"type": "input_json_delta", "partial_json": ARGS}}),
                    ));
                } else {
                    events.push((
                        "content_block_start",
                        json!({"type": "content_block_start", "index": 0,
                        "content_block": {"type": "text", "text": ""}}),
                    ));
                    events.push((
                        "content_block_delta",
                        json!({"type": "content_block_delta", "index": 0,
                        "delta": {"type": "text_delta", "text": ANSWER}}),
                    ));
                }
                events.push((
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": 0}),
                ));
                events.push(("message_delta", json!({"type": "message_delta",
                    "delta": {"stop_reason": if call { "tool_use" } else { "end_turn" }, "stop_sequence": null},
                    "usage": delta})));
                events.push(("message_stop", json!({"type": "message_stop"})));
                Reply::Sse(
                    events
                        .iter()
                        .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
                        .collect(),
                )
            } else {
                let content = if call {
                    json!([{"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}])
                } else {
                    json!([{"type": "text", "text": ANSWER}])
                };
                Reply::Json(
                    json!({"id": format!("msg_{turn}"), "type": "message", "role": "assistant",
                    "model": "claude-sonnet-4-6", "content": content,
                    "stop_reason": if call { "tool_use" } else { "end_turn" }, "stop_sequence": null,
                    "usage": usage(provider, turn)}),
                )
            }
        }
        Provider::Gemini => {
            let part = if call {
                json!({"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}})
            } else {
                json!({"text": ANSWER})
            };
            let chunk = json!({
                "candidates": [{"content": {"role": "model", "parts": [part]}, "finishReason": "STOP"}],
                "usageMetadata": usage(provider, turn)
            });
            if stream { Reply::Sse(sse(&[chunk])) } else { Reply::Json(chunk) }
        }
        Provider::GeminiInteractions => {
            let id = format!("v1_turn{turn}");
            let (status, steps) = if call {
                (
                    "requires_action",
                    json!([{"type": "function_call", "id": "call_1",
                    "name": "get_weather", "arguments": {"city": "Paris"}}]),
                )
            } else {
                (
                    "completed",
                    json!([{"type": "model_output", "content": [{"type": "text", "text": ANSWER}]}]),
                )
            };
            let interaction = |steps: Value| {
                json!({"id": id, "model": "gemini-3.7-flash", "status": status, "steps": steps,
                    "usage": usage(provider, turn)})
            };
            if stream {
                let (start, delta) = if call {
                    (
                        json!({"type": "function_call", "id": "call_1", "name": "get_weather", "arguments": {}}),
                        json!({"type": "function_call", "id": "call_1", "name": "get_weather", "arguments_delta": ARGS}),
                    )
                } else {
                    (json!({"type": "model_output"}), json!({"type": "text", "text": ANSWER}))
                };
                Reply::Sse(sse(&[
                    json!({"event_type": "interaction.created", "interaction": {"id": id, "status": "in_progress", "steps": []}}),
                    json!({"event_type": "step.start", "index": 0, "step": start}),
                    json!({"event_type": "step.delta", "index": 0, "delta": delta}),
                    json!({"event_type": "step.stop", "index": 0}),
                    json!({"event_type": "interaction.completed", "interaction": interaction(json!([]))}),
                ]))
            } else {
                Reply::Json(interaction(steps))
            }
        }
        Provider::DeepSeek | Provider::Groq => chat(provider, turn, stream),
        Provider::Ollama => {
            let message = if call {
                json!({"role": "assistant", "content": "",
                    "tool_calls": [{"function": {"name": "get_weather", "arguments": {"city": "Paris"}}}]})
            } else {
                json!({"role": "assistant", "content": ANSWER})
            };
            let mut line = json!({"model": "qwen3", "created_at": "2026-10-17T00:00:00Z",
                "message": message, "done": true, "done_reason": "stop"});
            line.as_object_mut().unwrap().extend(ollama_metrics(turn).as_object().unwrap().clone());
            Reply::Json(line)
        }
    }
}

/// A Chat Completions reply; the streamed finish chunk carries the usage.
fn chat(provider: Provider, turn: usize, stream: bool) -> Reply {
    let call = turn == 0;
    let finish = if call { "tool_calls" } else { "stop" };
    let tool_call = json!({"id": "call_1", "type": "function",
        "function": {"name": "get_weather", "arguments": ARGS}});
    if !stream {
        let message = if call {
            json!({"role": "assistant", "content": null, "tool_calls": [tool_call]})
        } else {
            json!({"role": "assistant", "content": ANSWER})
        };
        return Reply::Json(json!({"id": "chat-1", "object": "chat.completion", "created": 0,
            "model": "model", "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": usage(provider, turn)}));
    }
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chat-1", "object": "chat.completion.chunk", "created": 0, "model": "model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let first = if call {
        let mut streamed = tool_call.clone();
        streamed["index"] = json!(0);
        chunk(json!({"role": "assistant", "tool_calls": [streamed]}), Value::Null)
    } else {
        chunk(json!({"role": "assistant", "content": ANSWER}), Value::Null)
    };
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage(provider, turn);
    if provider == Provider::Groq {
        // Groq repeats the usage in its extension object, as its API does.
        last["x_groq"] = json!({"id": "req_1", "usage": usage(provider, turn)});
    }
    Reply::Sse(format!("{}data: [DONE]\n\n", sse(&[first, last])))
}

struct Shared {
    provider: Provider,
    stream: bool,
    requests: Mutex<Vec<(String, Value)>>,
}

/// A running scripted upstream.
pub struct Upstream {
    pub base_url: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Upstream {
    pub async fn start(provider: Provider, stream: bool) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared { provider, stream, requests: Mutex::new(Vec::new()) });
        let app = Router::new().fallback(answer).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url, shared, task })
    }

    /// The path and body of every request received so far, in order.
    pub fn requests(&self) -> Vec<(String, Value)> {
        self.shared.requests.lock().unwrap().clone()
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(State(shared): State<Arc<Shared>>, uri: Uri, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let turn = {
        let mut requests = shared.requests.lock().unwrap();
        requests.push((uri.to_string(), request));
        requests.len() - 1
    };
    match reply(shared.provider, turn, shared.stream) {
        Reply::Json(value) => axum::Json(value).into_response(),
        Reply::Sse(text) => ([(header::CONTENT_TYPE, "text/event-stream")], text).into_response(),
    }
}
