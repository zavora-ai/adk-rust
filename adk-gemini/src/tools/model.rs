use schemars::{JsonSchema, SchemaGenerator, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{ResultExt, Snafu};

/// Tool that can be used by the model
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Tool {
    /// Function-based tool
    Function {
        /// The function declaration for the tool
        #[serde(rename = "functionDeclarations")]
        function_declarations: Vec<FunctionDeclaration>,
    },
    /// Google Search tool
    GoogleSearch {
        /// The Google Search configuration
        google_search: GoogleSearchConfig,
    },
    /// Google Maps tool
    GoogleMaps {
        /// The Google Maps configuration
        google_maps: Value,
    },
    /// Code execution tool
    CodeExecution {
        /// The code execution configuration
        code_execution: Value,
    },
    /// URL context tool
    URLContext {
        /// The URL context configuration
        url_context: URLContextConfig,
    },
    /// File search tool
    FileSearch {
        /// The file search configuration
        file_search: Value,
    },
    /// Computer use tool
    ComputerUse {
        /// The computer use configuration
        computer_use: Value,
    },
    /// MCP server tool
    McpServer {
        /// The MCP server configuration
        #[serde(rename = "mcp_server")]
        mcp_server: Value,
    },
}

/// Empty configuration for Google Search tool
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoogleSearchConfig {}

/// Empty configuration for URL Context tool
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct URLContextConfig {}

impl Tool {
    /// Create a new tool with a single function declaration
    pub fn new(function_declaration: FunctionDeclaration) -> Self {
        Self::Function { function_declarations: vec![function_declaration] }
    }

    /// Create a new tool with multiple function declarations
    pub fn with_functions(function_declarations: Vec<FunctionDeclaration>) -> Self {
        Self::Function { function_declarations }
    }

    /// Create a new Google Search tool
    pub fn google_search() -> Self {
        Self::GoogleSearch { google_search: GoogleSearchConfig {} }
    }

    /// Create a new URL Context tool
    pub fn url_context() -> Self {
        Self::URLContext { url_context: URLContextConfig {} }
    }

    /// Create a new Google Maps tool
    pub fn google_maps(config: Value) -> Self {
        Self::GoogleMaps { google_maps: config }
    }

    /// Create a new code execution tool
    pub fn code_execution() -> Self {
        Self::CodeExecution { code_execution: Value::Object(Default::default()) }
    }

    /// Create a new file search tool
    pub fn file_search(config: Value) -> Self {
        Self::FileSearch { file_search: config }
    }

    /// Create a new computer use tool
    pub fn computer_use(config: Value) -> Self {
        Self::ComputerUse { computer_use: config }
    }

    /// Create a new MCP server tool
    pub fn mcp_server(config: Value) -> Self {
        Self::McpServer { mcp_server: config }
    }

    /// Returns `true` if this tool is a server-side built-in tool (e.g., Google Search,
    /// URL Context, Google Maps, Code Execution) that Gemini 3 executes internally.
    ///
    /// When server-side tools are present, `includeServerSideToolInvocations` should be
    /// set in the `ToolConfig` so Gemini 3 returns `toolCall`/`toolResponse` parts instead
    /// of silently truncating the response.
    pub fn is_server_side(&self) -> bool {
        !matches!(self, Self::Function { .. })
    }
}

/// Defines the function behavior
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Behavior {
    /// `default` If set, the system will wait to receive the function response before
    /// continuing the conversation.
    #[default]
    Blocking,
    /// If set, the system will not wait to receive the function response. Instead, it will
    /// attempt to handle function responses as they become available while maintaining the
    /// conversation between the user and the model.
    NonBlocking,
}

/// Declaration of a function that can be called by the model
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionDeclaration {
    /// The name of the function
    pub name: String,
    /// The description of the function
    pub description: String,
    /// `Optional` Specifies the function Behavior. Currently only supported by the BidiGenerateContent method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<Behavior>,
    /// `Optional` The parameters for the function
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<Value>,
    /// `Optional` Describes the output from this function in JSON Schema format. Reflects the
    /// Open API 3.03 Response Object. The Schema defines the type used for the response value
    /// of the function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response: Option<Value>,
}

/// Returns JSON Schema for the given parameters
fn generate_parameters_schema<Parameters>() -> Value
where
    Parameters: JsonSchema + Serialize,
{
    // Create SchemaSettings with Gemini-optimized settings, see: https://ai.google.dev/api/caching#Schema
    let schema_generator = SchemaGenerator::new(SchemaSettings::openapi3().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    }));

    let mut schema = schema_generator.into_root_schema_for::<Parameters>();

    // Root schemas always include a title field, which we don't want or need
    schema.remove("title");
    schema.to_value()
}

impl FunctionDeclaration {
    /// Create a new function declaration
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        behavior: Option<Behavior>,
    ) -> Self {
        Self { name: name.into(), description: description.into(), behavior, ..Default::default() }
    }

    /// Set the parameters for the function using a struct that implements `JsonSchema`
    pub fn with_parameters<Parameters>(mut self) -> Self
    where
        Parameters: JsonSchema + Serialize,
    {
        self.parameters = Some(generate_parameters_schema::<Parameters>());
        self
    }

    /// Set the response schema for the function using a struct that implements `JsonSchema`
    pub fn with_response<Response>(mut self) -> Self
    where
        Response: JsonSchema + Serialize,
    {
        self.response = Some(generate_parameters_schema::<Response>());
        self
    }
}

/// A function call made by the model
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    /// The name of the function
    pub name: String,
    /// The arguments for the function
    pub args: serde_json::Value,
    /// Unique identifier for this function call (Gemini 3 series).
    ///
    /// Gemini 3 models return an `id` on each function call to correlate with
    /// the corresponding `FunctionResponse`. Earlier models may omit this field.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<String>,
    /// The thought signature for the function call (Gemini 2.5 series only).
    ///
    /// Gemini expects this at the enclosing `Part::FunctionCall` level, not inside the
    /// `functionCall` object. Preserve it in-memory for callers, but never emit it from the
    /// inner wire type.
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        rename = "thoughtSignature",
        alias = "thought_signature"
    )]
    pub thought_signature: Option<String>,
}

/// Errors that can occur when extracting parameters from a [`FunctionCall`].
#[derive(Debug, Snafu)]
pub enum FunctionCallError {
    /// Failed to deserialize a parameter value.
    #[snafu(display("failed to deserialize parameter '{key}'"))]
    Deserialization {
        /// The underlying deserialization error.
        source: serde_json::Error,
        /// The parameter key that failed to deserialize.
        key: String,
    },

    /// A required parameter is missing from the arguments.
    #[snafu(display("parameter '{key}' is missing in arguments '{args}'"))]
    MissingParameter {
        /// The missing parameter key.
        key: String,
        /// The arguments object that was searched.
        args: serde_json::Value,
    },

    /// The arguments value is not a JSON object.
    #[snafu(display("arguments should be an object; actual: {actual}"))]
    ArgumentTypeMismatch {
        /// String representation of the actual value type.
        actual: String,
    },
}

impl FunctionCall {
    /// Create a new function call
    pub fn new(name: impl Into<String>, args: serde_json::Value) -> Self {
        Self { name: name.into(), args, id: None, thought_signature: None }
    }

    /// Create a new function call with thought signature
    pub fn with_thought_signature(
        name: impl Into<String>,
        args: serde_json::Value,
        thought_signature: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            args,
            id: None,
            thought_signature: Some(thought_signature.into()),
        }
    }

    /// Get a parameter from the arguments
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T, FunctionCallError> {
        match &self.args {
            serde_json::Value::Object(obj) => {
                if let Some(value) = obj.get(key) {
                    serde_json::from_value(value.clone())
                        .with_context(|_| DeserializationSnafu { key: key.to_string() })
                } else {
                    Err(MissingParameterSnafu { key: key.to_string(), args: self.args.clone() }
                        .build())
                }
            }
            _ => Err(ArgumentTypeMismatchSnafu { actual: self.args.to_string() }.build()),
        }
    }
}

/// A response from a function
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionResponse {
    /// The name of the function
    pub name: String,
    /// Unique identifier correlating this response with its [`FunctionCall`].
    ///
    /// Gemini 3.x models enforce strict response matching: every `FunctionResponse`
    /// must echo the `id` from the corresponding `FunctionCall`, the `name` must match,
    /// and the response count must equal the call count. Mismatches cause the model to
    /// return empty responses with `finish_reason: STOP`. Earlier models ignore this field.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<String>,
    /// The response from the function
    /// This must be a valid JSON object
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    /// Multimodal parts nested inside the functionResponse wire object.
    /// Contains `inlineData` and/or `fileData` entries that accompany the JSON response.
    /// Gemini 3 expects these inside the `functionResponse`, not as sibling Content parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<FunctionResponsePart>,
}

/// A part nested inside a `functionResponse` wire object.
///
/// Gemini 3 expects multimodal data (images, audio, files) as `inlineData` or `fileData`
/// entries in a `parts` array within the `functionResponse` JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FunctionResponsePart {
    /// Inline binary data (base64-encoded).
    InlineData {
        /// The inline blob data.
        #[serde(rename = "inlineData")]
        inline_data: crate::Blob,
    },
    /// File data referenced by URI.
    FileData {
        /// The file data reference.
        #[serde(rename = "fileData")]
        file_data: crate::FileDataRef,
    },
}

impl FunctionResponse {
    /// Create a new function response with a JSON value
    pub fn new(name: impl Into<String>, response: serde_json::Value) -> Self {
        let response = match response {
            serde_json::Value::Object(_) => response,
            other => serde_json::json!({ "result": other }),
        };
        Self { name: name.into(), id: None, response: Some(response), parts: Vec::new() }
    }

    /// Set the `id` correlating this response with its [`FunctionCall`].
    ///
    /// Required for Gemini 3.x strict response matching — pass the `id` from the
    /// originating function call.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Create with JSON response and inline data blobs.
    pub fn with_inline_data(
        name: impl Into<String>,
        response: serde_json::Value,
        inline_data: Vec<crate::Blob>,
    ) -> Self {
        let response = match response {
            serde_json::Value::Object(_) => response,
            other => serde_json::json!({ "result": other }),
        };
        let parts = inline_data
            .into_iter()
            .map(|blob| FunctionResponsePart::InlineData { inline_data: blob })
            .collect();
        Self { name: name.into(), id: None, response: Some(response), parts }
    }

    /// Create with JSON response and file data references.
    pub fn with_file_data(
        name: impl Into<String>,
        response: serde_json::Value,
        file_data: Vec<crate::FileDataRef>,
    ) -> Self {
        let response = match response {
            serde_json::Value::Object(_) => response,
            other => serde_json::json!({ "result": other }),
        };
        let parts = file_data
            .into_iter()
            .map(|fdr| FunctionResponsePart::FileData { file_data: fdr })
            .collect();
        Self { name: name.into(), id: None, response: Some(response), parts }
    }

    /// Create with inline data only (no JSON response).
    pub fn inline_data_only(name: impl Into<String>, inline_data: Vec<crate::Blob>) -> Self {
        let parts = inline_data
            .into_iter()
            .map(|blob| FunctionResponsePart::InlineData { inline_data: blob })
            .collect();
        Self { name: name.into(), id: None, response: None, parts }
    }

    /// Create a new function response from a serializable type that will be parsed as JSON
    pub fn from_schema<Response>(
        name: impl Into<String>,
        response: Response,
    ) -> Result<Self, serde_json::Error>
    where
        Response: JsonSchema + Serialize,
    {
        let json = serde_json::to_value(&response)?;
        Ok(Self::new(name, json))
    }

    /// Create a new function response with a string that will be parsed as JSON
    pub fn from_str(
        name: impl Into<String>,
        response: impl Into<String>,
    ) -> Result<Self, serde_json::Error> {
        let json = serde_json::from_str(&response.into())?;
        Ok(Self::new(name, json))
    }
}

/// Configuration for tools
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolConfig {
    /// The function calling config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
    /// When true, tells Gemini 3 to include server-side tool invocation parts
    /// (`toolCall`/`toolResponse`) in the response instead of silently truncating.
    #[serde(skip_serializing_if = "Option::is_none", rename = "includeServerSideToolInvocations")]
    pub include_server_side_tool_invocations: Option<bool>,
    /// Retrieval configuration used by provider-native tools such as Google Maps.
    #[serde(skip_serializing_if = "Option::is_none", rename = "retrievalConfig")]
    pub retrieval_config: Option<Value>,
}

/// Configuration for function calling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCallingConfig {
    /// The mode for function calling
    pub mode: FunctionCallingMode,
    /// Restricts which functions the model may call.
    /// Only applicable when mode is `Any`. The model will only call functions
    /// whose names are in this list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_function_names: Option<Vec<String>>,
}

/// Mode for function calling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FunctionCallingMode {
    /// The model decides whether to call functions (default behavior)
    Auto,
    /// The model must call one of the provided functions
    Any,
    /// The model must not call any functions
    None,
    /// The model validates function calls against the schema but does not force calling.
    /// Available in Gemini 3 series models.
    Validated,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_function_declarations_uses_camel_case() {
        let tool = Tool::Function {
            function_declarations: vec![FunctionDeclaration::new("test_func", "desc", None)],
        };

        let json = serde_json::to_value(&tool).unwrap();
        assert!(json.get("functionDeclarations").is_some());
        assert!(json.get("function_declarations").is_none());
    }

    #[test]
    fn tool_config_include_server_side_tool_invocations_serde_round_trip() {
        let config = ToolConfig {
            function_calling_config: None,
            include_server_side_tool_invocations: Some(true),
            retrieval_config: None,
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["includeServerSideToolInvocations"], true);
        // field should use camelCase on the wire
        assert!(json.get("include_server_side_tool_invocations").is_none());

        let deserialized: ToolConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, config);
    }

    #[test]
    fn tool_config_default_omits_server_side_flag() {
        let config = ToolConfig::default();
        assert_eq!(config.include_server_side_tool_invocations, None);
        assert_eq!(config.retrieval_config, None);

        let json = serde_json::to_value(&config).unwrap();
        assert!(json.get("includeServerSideToolInvocations").is_none());
    }

    #[test]
    fn function_calling_mode_validated_serde_round_trip() {
        let config = FunctionCallingConfig {
            mode: FunctionCallingMode::Validated,
            allowed_function_names: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["mode"], "VALIDATED");
        let deserialized: FunctionCallingConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.mode, FunctionCallingMode::Validated);
    }

    #[test]
    fn function_calling_config_with_allowed_names() {
        let config = FunctionCallingConfig {
            mode: FunctionCallingMode::Any,
            allowed_function_names: Some(vec!["get_weather".to_string(), "search".to_string()]),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["mode"], "ANY");
        assert_eq!(json["allowed_function_names"], serde_json::json!(["get_weather", "search"]));

        let deserialized: FunctionCallingConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, config);
    }

    #[test]
    fn function_calling_config_omits_none_allowed_names() {
        let config =
            FunctionCallingConfig { mode: FunctionCallingMode::Auto, allowed_function_names: None };
        let json = serde_json::to_value(&config).unwrap();
        assert!(json.get("allowed_function_names").is_none());
    }

    #[test]
    fn function_call_with_id_serde_round_trip() {
        let call = FunctionCall {
            name: "get_weather".to_string(),
            args: serde_json::json!({"city": "Tokyo"}),
            id: Some("fc_001".to_string()),
            thought_signature: None,
        };
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["id"], "fc_001");

        let deserialized: FunctionCall = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.id, Some("fc_001".to_string()));
    }

    #[test]
    fn function_call_without_id_omits_field() {
        let call = FunctionCall::new("get_weather", serde_json::json!({"city": "Tokyo"}));
        let json = serde_json::to_value(&call).unwrap();
        assert!(json.get("id").is_none());
    }

    #[test]
    fn function_call_deserializes_without_id() {
        let json = serde_json::json!({
            "name": "get_weather",
            "args": {"city": "Tokyo"}
        });
        let call: FunctionCall = serde_json::from_value(json).unwrap();
        assert_eq!(call.id, None);
        assert_eq!(call.name, "get_weather");
    }
}
