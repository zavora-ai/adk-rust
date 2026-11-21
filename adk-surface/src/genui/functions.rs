//! A2UI 1.0 function calls across the wire (spec 3.3 §6.1 `genui`): `callRendererFunction`
//! (the agent asks the renderer to run a catalog function), `callAgentFunction` (the renderer
//! asks the agent), and their answers, `rendererFunctionResponse` and `agentFunctionResponse`.
//!
//! `genui` offers no function either side may call on the other. The `forms` catalog's
//! functions are all `rendererOnly`: formatting and checks the renderer evaluates while it
//! draws, never on an agent's word. No agent function is offered either. So every call is
//! answered, as A2UI 1.0 requires, with `INVALID_FUNCTION_CALL` and the reason
//! ([`FunctionInvocation::refusal`]), and a drawing never carries a call: [`from_wire`] refuses one
//! and points here.
//!
//! [`from_wire`]: super::from_wire

use serde_json::{Map, Value, json};

use super::catalog::Catalog;
use super::wire::{A2uiVersion, WireError};

/// The error code A2UI 1.0 answers a call with when the function may not be called by the
/// side that asked, is not registered, or names a catalog the answering side does not know.
pub const INVALID_FUNCTION_CALL: &str = "INVALID_FUNCTION_CALL";

/// Who asks for a function call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionCaller {
    /// The agent asks the renderer (`callRendererFunction`).
    Agent,
    /// The renderer asks the agent (`callAgentFunction`).
    Renderer,
}

/// A function call one side asks the other to run.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionInvocation {
    /// Who asks.
    pub caller: FunctionCaller,
    /// The id the answer copies verbatim.
    pub function_call_id: String,
    /// The surface whose data model the call reads; required when the renderer asks.
    pub surface_id: Option<String>,
    /// The function's registered name (`@call`).
    pub name: String,
    /// The catalog that defines it; required when the agent asks.
    pub catalog_id: Option<String>,
    /// Its arguments (`args`), `{}` when absent.
    pub args: Value,
}

/// Why a function call failed: `{ code, message }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionError {
    /// A code such as [`INVALID_FUNCTION_CALL`] or `EXECUTION_ERROR`.
    pub code: String,
    /// What went wrong, in words.
    pub message: String,
}

/// The answer to a [`FunctionInvocation`].
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionResponse {
    /// Who asked the call this answers: an agent's call is answered by the renderer
    /// (`rendererFunctionResponse`), a renderer's by the agent (`agentFunctionResponse`).
    pub caller: FunctionCaller,
    /// The id of the call it answers.
    pub function_call_id: String,
    /// The value the function returned, or why it did not run.
    pub outcome: Result<Value, FunctionError>,
}

/// An A2UI 1.0 function message.
#[derive(Debug, Clone, PartialEq)]
pub enum FunctionMessage {
    /// `callRendererFunction` or `callAgentFunction`.
    Call(FunctionInvocation),
    /// `rendererFunctionResponse` or `agentFunctionResponse`.
    Response(FunctionResponse),
}

/// The message keys of A2UI 1.0's function calls and their answers.
pub(crate) const FUNCTION_MESSAGES: [&str; 4] = [
    "callRendererFunction",
    "callAgentFunction",
    "rendererFunctionResponse",
    "agentFunctionResponse",
];

impl FunctionInvocation {
    /// The answer `genui` gives this call: `INVALID_FUNCTION_CALL`, with why. A `forms`
    /// function is `rendererOnly`, so an agent may not call it; a renderer may call no agent
    /// function, because none is offered.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_surface::genui::{Catalog, FunctionMessage, INVALID_FUNCTION_CALL, read_function_message};
    /// use serde_json::json;
    ///
    /// let message = json!({ "version": "1.0", "callRendererFunction": {
    ///     "functionCallId": "c1",
    ///     "callFunction": { "@call": "formatString", "catalogId": Catalog::forms().id() } } });
    /// let Some(FunctionMessage::Call(call)) = read_function_message(&message)? else { unreachable!() };
    /// let answer = call.refusal(Catalog::forms()).to_wire();
    /// assert_eq!(answer["rendererFunctionResponse"]["functionCallId"], "c1");
    /// assert_eq!(answer["rendererFunctionResponse"]["error"]["code"], INVALID_FUNCTION_CALL);
    /// # Ok::<(), adk_surface::genui::WireError>(())
    /// ```
    pub fn refusal(&self, catalog: &Catalog) -> FunctionResponse {
        let message = match self.caller {
            FunctionCaller::Renderer => {
                format!("this agent offers no functions to call; {} is not one", self.name)
            }
            FunctionCaller::Agent => {
                let named = self.catalog_id.as_deref().unwrap_or_default();
                if named != catalog.id() {
                    format!("{named} is not a catalog this renderer registers")
                } else if catalog.functions().any(|function| function == self.name) {
                    format!(
                        "{} is rendererOnly in {named}: the renderer evaluates it while it draws, \
                         and an agent cannot call it",
                        self.name
                    )
                } else {
                    format!("{} is not a function of {named}", self.name)
                }
            }
        };
        FunctionResponse {
            caller: self.caller,
            function_call_id: self.function_call_id.clone(),
            outcome: Err(FunctionError { code: INVALID_FUNCTION_CALL.to_string(), message }),
        }
    }

    /// This call as an A2UI 1.0 message.
    pub fn to_wire(&self) -> Value {
        let mut call = Map::new();
        call.insert("@call".to_string(), json!(self.name));
        if let Some(catalog_id) = &self.catalog_id {
            call.insert("catalogId".to_string(), json!(catalog_id));
        }
        if self.args.as_object().is_some_and(|args| !args.is_empty()) {
            call.insert("args".to_string(), self.args.clone());
        }
        let mut body = Map::new();
        if let Some(surface_id) = &self.surface_id {
            body.insert("surfaceId".to_string(), json!(surface_id));
        }
        body.insert("functionCallId".to_string(), json!(self.function_call_id));
        body.insert("callFunction".to_string(), Value::Object(call));
        let key = match self.caller {
            FunctionCaller::Agent => "callRendererFunction",
            FunctionCaller::Renderer => "callAgentFunction",
        };
        json!({ "version": A2uiVersion::V1_0.wire(), key: body })
    }
}

impl FunctionResponse {
    /// This answer as an A2UI 1.0 message.
    pub fn to_wire(&self) -> Value {
        let mut body = Map::new();
        body.insert("functionCallId".to_string(), json!(self.function_call_id));
        match &self.outcome {
            Ok(value) => body.insert("value".to_string(), value.clone()),
            Err(error) => body.insert(
                "error".to_string(),
                json!({ "code": error.code, "message": error.message }),
            ),
        };
        let key = match self.caller {
            FunctionCaller::Agent => "rendererFunctionResponse",
            FunctionCaller::Renderer => "agentFunctionResponse",
        };
        json!({ "version": A2uiVersion::V1_0.wire(), key: body })
    }
}

/// Reads an A2UI 1.0 function message: a call or its answer. `Ok(None)` when `message` is
/// not one (a surface message, say).
///
/// # Errors
///
/// A function message that is not A2UI 1.0 (v0.9.1 has none), or that lacks what 1.0
/// requires: a `functionCallId`; a `callFunction` with its `@call`; the `catalogId` of an
/// agent's call; the `surfaceId` of a renderer's; exactly one of an answer's `value` and
/// `error`, and an error's `code` and `message`.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{FunctionCaller, FunctionMessage, read_function_message};
/// use serde_json::json;
///
/// let read = read_function_message(&json!({ "version": "1.0", "callAgentFunction": {
///     "surfaceId": "s", "functionCallId": "c2", "callFunction": { "@call": "lookupProvider" } } }))?;
/// assert!(matches!(read, Some(FunctionMessage::Call(call)) if call.caller == FunctionCaller::Renderer));
/// assert_eq!(read_function_message(&json!({ "version": "1.0", "deleteSurface": { "surfaceId": "s" } }))?, None);
/// # Ok::<(), adk_surface::genui::WireError>(())
/// ```
pub fn read_function_message(message: &Value) -> Result<Option<FunctionMessage>, WireError> {
    let Some(kind) = FUNCTION_MESSAGES.into_iter().find(|kind| message.get(*kind).is_some()) else {
        return Ok(None);
    };
    let version = message.get("version").and_then(Value::as_str);
    if version != Some(A2uiVersion::V1_0.wire()) {
        return Err(WireError::new(
            "/version",
            format!("{kind} is A2UI 1.0; this message says version {}", version.unwrap_or("none")),
        ));
    }
    let at = format!("/{kind}");
    let body = message[kind]
        .as_object()
        .ok_or_else(|| WireError::new(&at, format!("{kind} is an object")))?;
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    let function_call_id = text("functionCallId").ok_or_else(|| {
        WireError::new(&format!("{at}/functionCallId"), "a functionCallId is required")
    })?;
    match kind {
        "callRendererFunction" | "callAgentFunction" => {
            let caller = if kind == "callRendererFunction" {
                FunctionCaller::Agent
            } else {
                FunctionCaller::Renderer
            };
            let call_at = format!("{at}/callFunction");
            let call = body
                .get("callFunction")
                .and_then(Value::as_object)
                .ok_or_else(|| WireError::new(&call_at, "a callFunction object is required"))?;
            let name = call
                .get("@call")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WireError::new(&format!("{call_at}/@call"), "@call names the function")
                })?
                .to_string();
            let catalog_id = call.get("catalogId").and_then(Value::as_str).map(str::to_string);
            if caller == FunctionCaller::Agent && catalog_id.is_none() {
                return Err(WireError::new(
                    &format!("{call_at}/catalogId"),
                    "callRendererFunction names the catalog of its function",
                ));
            }
            let surface_id = text("surfaceId");
            if caller == FunctionCaller::Renderer && surface_id.is_none() {
                return Err(WireError::new(
                    &format!("{at}/surfaceId"),
                    "callAgentFunction names the surface it came from",
                ));
            }
            let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
            if !args.is_object() {
                return Err(WireError::new(&format!("{call_at}/args"), "args is an object"));
            }
            Ok(Some(FunctionMessage::Call(FunctionInvocation {
                caller,
                function_call_id,
                surface_id,
                name,
                catalog_id,
                args,
            })))
        }
        _ => {
            let caller = if kind == "rendererFunctionResponse" {
                FunctionCaller::Agent
            } else {
                FunctionCaller::Renderer
            };
            let outcome = match (body.get("value"), body.get("error")) {
                (Some(value), None) => Ok(value.clone()),
                (None, Some(error)) => {
                    let field = |key: &str| {
                        error.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(
                            || {
                                WireError::new(
                                    &format!("{at}/error/{key}"),
                                    format!("an error has a {key}"),
                                )
                            },
                        )
                    };
                    Err(FunctionError { code: field("code")?, message: field("message")? })
                }
                _ => {
                    return Err(WireError::new(
                        &at,
                        "an answer has exactly one of value and error",
                    ));
                }
            };
            Ok(Some(FunctionMessage::Response(FunctionResponse {
                caller,
                function_call_id,
                outcome,
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(message: Value) -> FunctionInvocation {
        match read_function_message(&message).unwrap() {
            Some(FunctionMessage::Call(call)) => call,
            other => panic!("not a call: {other:?}"),
        }
    }

    #[test]
    fn every_call_is_answered_invalid_with_its_reason() {
        let forms = Catalog::forms();
        for (message, says) in [
            (
                json!({ "version": "1.0", "callRendererFunction": { "functionCallId": "a",
                    "callFunction": { "@call": "formatString", "catalogId": forms.id() } } }),
                "rendererOnly",
            ),
            (
                json!({ "version": "1.0", "callRendererFunction": { "functionCallId": "b",
                    "callFunction": { "@call": "getScreenResolution", "catalogId": forms.id() } } }),
                "not a function of",
            ),
            (
                json!({ "version": "1.0", "callRendererFunction": { "functionCallId": "c",
                    "callFunction": { "@call": "x", "catalogId": "https://x.test/c.json" } } }),
                "not a catalog this renderer registers",
            ),
            (
                json!({ "version": "1.0", "callAgentFunction": { "surfaceId": "s", "functionCallId": "d",
                    "callFunction": { "@call": "lookupProvider", "args": { "id": 1 } } } }),
                "offers no functions",
            ),
        ] {
            let call = call(message.clone());
            let answer = call.refusal(forms);
            assert_eq!(answer.function_call_id, call.function_call_id);
            let error = answer.outcome.as_ref().unwrap_err();
            assert_eq!(error.code, INVALID_FUNCTION_CALL);
            assert!(error.message.contains(says), "{message}: {}", error.message);
            // The answer reads back as what it is.
            assert_eq!(
                read_function_message(&answer.to_wire()).unwrap(),
                Some(FunctionMessage::Response(answer))
            );
            assert_eq!(call.to_wire(), message, "a call writes back as read");
        }
    }

    #[test]
    fn what_1_0_requires_of_a_function_message_is_checked() {
        for (message, says) in [
            (json!({ "version": "v0.9.1", "callRendererFunction": {} }), "A2UI 1.0"),
            (
                json!({ "version": "1.0", "callRendererFunction": { "callFunction": {} } }),
                "functionCallId",
            ),
            (
                json!({ "version": "1.0", "callRendererFunction": { "functionCallId": "a",
                    "callFunction": { "@call": "formatNumber" } } }),
                "catalog",
            ),
            (
                json!({ "version": "1.0", "callAgentFunction": { "functionCallId": "a",
                    "callFunction": { "@call": "f" } } }),
                "surface",
            ),
            (
                json!({ "version": "1.0", "agentFunctionResponse": { "functionCallId": "a" } }),
                "exactly one",
            ),
            (
                json!({ "version": "1.0", "rendererFunctionResponse": { "functionCallId": "a",
                    "value": 1, "error": { "code": "X", "message": "y" } } }),
                "exactly one",
            ),
            (
                json!({ "version": "1.0", "rendererFunctionResponse": { "functionCallId": "a",
                    "error": { "code": "X" } } }),
                "message",
            ),
        ] {
            let error = read_function_message(&message).unwrap_err();
            assert!(error.to_string().contains(says), "{message}: {error}");
        }
    }

    #[test]
    fn a_success_answer_reads_and_writes() {
        let answer = json!({ "version": "1.0", "rendererFunctionResponse": {
            "functionCallId": "r", "value": [1920, 1080] } });
        let Some(FunctionMessage::Response(read)) = read_function_message(&answer).unwrap() else {
            panic!("an answer")
        };
        assert_eq!(read.caller, FunctionCaller::Agent);
        assert_eq!(read.outcome, Ok(json!([1920, 1080])));
        assert_eq!(read.to_wire(), answer);
    }
}
