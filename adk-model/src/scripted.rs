//! A scripted [`Llm`](adk_core::Llm) for tests: one scripted reply per call, with every request recorded.

use std::collections::VecDeque;
use std::sync::Mutex;

use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, UsageMetadata,
};
use async_trait::async_trait;
use serde_json::Value;

/// The error code [`ScriptedLlm`] fails a call with once its script is used up.
pub const SCRIPT_EXHAUSTED_CODE: &str = "model.scripted.exhausted";

/// What a [`ScriptedLlm`] answers to one `generate_content` call.
///
/// A reply is a stream of responses, optionally followed by a stream error, or a call that
/// fails outright. Build one with [`text`](Self::text), [`function_call`](Self::function_call),
/// [`stream`](Self::stream) or [`error`](Self::error); an [`LlmResponse`] converts into a
/// one-response reply.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, UsageMetadata};
/// use adk_model::ScriptedResponse;
/// use serde_json::json;
///
/// let call = ScriptedResponse::function_call("get_weather", json!({ "city": "Nairobi" }));
/// let answer = ScriptedResponse::text("It is sunny.")
///     .with_usage(UsageMetadata { prompt_token_count: 12, ..Default::default() });
/// let cut = ScriptedResponse::text("It is").then_error(AdkError::model("stream reset"));
/// let refused = ScriptedResponse::error(AdkError::model("rate limited"));
/// # let _ = (call, answer, cut, refused);
/// ```
#[derive(Debug)]
pub struct ScriptedResponse {
    responses: Vec<LlmResponse>,
    stream_error: Option<AdkError>,
    call_error: Option<AdkError>,
}

impl ScriptedResponse {
    /// A final model response with one text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::response(final_response(vec![Part::Text { text: text.into() }]))
    }

    /// A final model response with one function call.
    ///
    /// The call has no ID; [`ScriptedLlm`] assigns one when it replies.
    pub fn function_call(name: impl Into<String>, args: Value) -> Self {
        Self::function_calls([(name, args)])
    }

    /// A final model response with several function calls, in order.
    pub fn function_calls<N: Into<String>>(calls: impl IntoIterator<Item = (N, Value)>) -> Self {
        let parts = calls
            .into_iter()
            .map(|(name, args)| Part::FunctionCall {
                name: name.into(),
                args,
                id: None,
                thought_signature: None,
            })
            .collect();
        Self::response(final_response(parts))
    }

    /// A reply of exactly `response`.
    pub fn response(response: LlmResponse) -> Self {
        Self::stream([response])
    }

    /// A reply that streams `responses` in order, as given.
    pub fn stream(responses: impl IntoIterator<Item = LlmResponse>) -> Self {
        Self { responses: responses.into_iter().collect(), stream_error: None, call_error: None }
    }

    /// A call that fails: `generate_content` returns `error` instead of a stream.
    pub fn error(error: AdkError) -> Self {
        Self { responses: Vec::new(), stream_error: None, call_error: Some(error) }
    }

    /// Ends the stream with `error` after its responses, as a provider whose stream breaks.
    pub fn then_error(mut self, error: AdkError) -> Self {
        self.stream_error = Some(error);
        self
    }

    /// Reports `usage` on the reply's last response.
    pub fn with_usage(mut self, usage: UsageMetadata) -> Self {
        if let Some(last) = self.responses.last_mut() {
            last.usage_metadata = Some(usage);
        }
        self
    }
}

impl From<LlmResponse> for ScriptedResponse {
    fn from(response: LlmResponse) -> Self {
        Self::response(response)
    }
}

impl From<AdkError> for ScriptedResponse {
    fn from(error: AdkError) -> Self {
        Self::error(error)
    }
}

fn final_response(parts: Vec<Part>) -> LlmResponse {
    let mut response = LlmResponse::new(Content { role: "model".to_string(), parts });
    response.finish_reason = Some(FinishReason::Stop);
    response.turn_complete = true;
    response
}

/// A test double for [`Llm`] that answers each call with the next scripted reply and records
/// every request.
///
/// Script one [`ScriptedResponse`] per model call: a tool loop that calls one tool and then
/// answers is two replies, a function call and a text. [`requests`](Self::requests) returns
/// what the agent sent on each call, so a test can assert on the history, tool declarations,
/// and configuration the model received. A call past the end of the script fails with
/// [`SCRIPT_EXHAUSTED_CODE`], so a test that runs more model turns than it expects fails
/// instead of passing on a made-up reply.
///
/// A function call scripted without an ID is given `scripted_tc_{call}_{index}`, where `call`
/// counts `generate_content` calls from 0 and `index` counts the reply's function calls.
///
/// [`MockLlm`](crate::MockLlm) replays its whole response list on every call; use
/// `ScriptedLlm` to script more than one turn.
///
/// # Example
///
/// ```rust
/// use adk_core::Llm;
/// use adk_model::{ScriptedLlm, ScriptedResponse};
/// use futures::StreamExt;
/// use serde_json::json;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), adk_core::AdkError> {
/// let llm = ScriptedLlm::new(
///     "scripted",
///     [
///         ScriptedResponse::function_call("get_weather", json!({ "city": "Nairobi" })),
///         ScriptedResponse::text("It is sunny in Nairobi."),
///     ],
/// );
///
/// let request = adk_core::LlmRequest::new("scripted", vec![]);
/// let first = llm.generate_content(request, false).await?.next().await.unwrap()?;
/// assert!(first.content.unwrap().has_function_calls());
/// assert_eq!(llm.requests().len(), 1);
/// assert_eq!(llm.remaining(), 1);
/// # Ok(())
/// # }
/// ```
pub struct ScriptedLlm {
    name: String,
    script: Mutex<VecDeque<ScriptedResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedLlm {
    /// Creates a model named `name` that answers its calls with `script`, in order.
    ///
    /// Each item is anything that converts into a [`ScriptedResponse`], such as an
    /// [`LlmResponse`].
    pub fn new<R: Into<ScriptedResponse>>(
        name: impl Into<String>,
        script: impl IntoIterator<Item = R>,
    ) -> Self {
        Self {
            name: name.into(),
            script: Mutex::new(script.into_iter().map(Into::into).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Appends a reply to the end of the script.
    pub fn push(&self, reply: impl Into<ScriptedResponse>) {
        lock(&self.script).push_back(reply.into());
    }

    /// Every request received so far, in call order.
    pub fn requests(&self) -> Vec<LlmRequest> {
        lock(&self.requests).clone()
    }

    /// The most recent request, if any.
    pub fn last_request(&self) -> Option<LlmRequest> {
        lock(&self.requests).last().cloned()
    }

    /// How many scripted replies have not been used yet.
    pub fn remaining(&self) -> usize {
        lock(&self.script).len()
    }
}

/// A poisoned lock only means another test thread panicked; the data is still usable.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl std::fmt::Debug for ScriptedLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedLlm")
            .field("name", &self.name)
            .field("remaining", &self.remaining())
            .field("requests", &lock(&self.requests).len())
            .finish()
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        let call = {
            let mut requests = lock(&self.requests);
            requests.push(request);
            requests.len() - 1
        };
        let Some(reply) = lock(&self.script).pop_front() else {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                SCRIPT_EXHAUSTED_CODE,
                format!(
                    "ScriptedLlm '{}' has no reply scripted for call {}; script one more reply \
                     or expect fewer model calls.",
                    self.name,
                    call + 1
                ),
            ));
        };
        if let Some(error) = reply.call_error {
            return Err(error);
        }

        let mut index = 0;
        let mut items: Vec<Result<LlmResponse>> = reply
            .responses
            .into_iter()
            .map(|mut response| {
                let parts =
                    response.content.iter_mut().flat_map(|content| content.parts.iter_mut());
                for part in parts {
                    if let Part::FunctionCall { id, .. } = part {
                        if id.is_none() {
                            *id = Some(format!("scripted_tc_{call}_{index}"));
                        }
                        index += 1;
                    }
                }
                Ok(response)
            })
            .collect();
        items.extend(reply.stream_error.map(Err));
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;

    async fn call(llm: &ScriptedLlm, text: &str) -> Result<Vec<Result<LlmResponse>>> {
        let request = LlmRequest::new("m", vec![Content::new("user").with_text(text)]);
        Ok(llm.generate_content(request, true).await?.collect().await)
    }

    #[tokio::test]
    async fn each_call_gets_the_next_reply_and_is_recorded() {
        let llm = ScriptedLlm::new(
            "m",
            [
                ScriptedResponse::function_calls([("a", json!({ "x": 1 })), ("b", json!({}))]),
                ScriptedResponse::text("done"),
            ],
        );

        let first = call(&llm, "one").await.unwrap();
        assert_eq!(first.len(), 1);
        let parts = &first[0].as_ref().unwrap().content.as_ref().unwrap().parts;
        let ids: Vec<_> = parts
            .iter()
            .map(|part| match part {
                Part::FunctionCall { id, .. } => id.clone(),
                other => panic!("expected a function call, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, [Some("scripted_tc_0_0".to_string()), Some("scripted_tc_0_1".to_string())]);

        let second = call(&llm, "two").await.unwrap();
        let reply = second[0].as_ref().unwrap();
        assert_eq!(reply.content.as_ref().unwrap().parts[0].text(), Some("done"));
        assert!(reply.turn_complete);
        assert_eq!(reply.finish_reason, Some(FinishReason::Stop));

        let texts: Vec<_> = llm
            .requests()
            .iter()
            .map(|request| request.contents[0].parts[0].text().unwrap().to_string())
            .collect();
        assert_eq!(texts, ["one", "two"]);
        assert_eq!(llm.remaining(), 0);
    }

    #[tokio::test]
    async fn an_exhausted_script_fails_the_call() {
        let llm = ScriptedLlm::new("m", [ScriptedResponse::text("only")]);
        call(&llm, "one").await.unwrap();
        let err = call(&llm, "two").await.unwrap_err();
        assert_eq!(err.code, SCRIPT_EXHAUSTED_CODE);
        assert!(err.message.contains("call 2"), "{}", err.message);
        assert_eq!(llm.requests().len(), 2, "the unanswered request is still recorded");
    }

    #[tokio::test]
    async fn errors_usage_and_streams_are_scripted() {
        let mut chunk = LlmResponse::new(Content::new("model").with_text("par"));
        chunk.partial = true;
        let llm = ScriptedLlm::new(
            "m",
            [
                ScriptedResponse::error(AdkError::model("rate limited")),
                ScriptedResponse::stream([
                    chunk,
                    final_response(vec![Part::Text { text: "partial".into() }]),
                ])
                .with_usage(UsageMetadata { total_token_count: 7, ..Default::default() })
                .then_error(AdkError::model("stream reset")),
            ],
        );
        assert_eq!(call(&llm, "a").await.unwrap_err().message, "rate limited");

        let items = call(&llm, "b").await.unwrap();
        assert_eq!(items.len(), 3);
        assert!(items[0].as_ref().unwrap().partial);
        assert_eq!(
            items[1].as_ref().unwrap().usage_metadata.as_ref().unwrap().total_token_count,
            7
        );
        assert_eq!(items[2].as_ref().unwrap_err().message, "stream reset");
    }

    #[tokio::test]
    async fn scripted_ids_are_kept_and_replies_can_be_pushed() {
        let mut response = final_response(vec![Part::FunctionCall {
            name: "a".into(),
            args: json!({}),
            id: Some("call_mine".into()),
            thought_signature: None,
        }]);
        response.model = Some("scripted".into());
        let llm = ScriptedLlm::new("m", Vec::<ScriptedResponse>::new());
        llm.push(response);
        let items = call(&llm, "x").await.unwrap();
        let reply = items[0].as_ref().unwrap();
        assert!(matches!(
            &reply.content.as_ref().unwrap().parts[0],
            Part::FunctionCall { id: Some(id), .. } if id == "call_mine"
        ));
        assert_eq!(llm.last_request().unwrap().contents[0].parts[0].text(), Some("x"));
    }
}
