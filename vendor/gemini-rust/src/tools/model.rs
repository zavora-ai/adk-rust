use schemars::{generate::SchemaSettings, JsonSchema, SchemaGenerator};
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
        function_declarations: Vec<FunctionDeclaration>,
    },
    /// Google Search tool
    GoogleSearch {
        /// The Google Search configuration
        google_search: GoogleSearchConfig,
    },
    URLContext {
        url_context: URLContextConfig,
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
        Self::Function {
            function_declarations: vec![function_declaration],
        }
    }

    /// Create a new tool with multiple function declarations
    pub fn with_functions(function_declarations: Vec<FunctionDeclaration>) -> Self {
        Self::Function {
            function_declarations,
        }
    }

    /// Create a new Google Search tool
    pub fn google_search() -> Self {
        Self::GoogleSearch {
            google_search: GoogleSearchConfig {},
        }
    }

    /// Create a new URL Context tool
    pub fn url_context() -> Self {
        Self::URLContext {
            url_context: URLContextConfig {},
        }
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
        Self {
            name: name.into(),
            description: description.into(),
            behavior,
            ..Default::default()
        }
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
    /// The thought signature for the function call (Gemini 2.5 series only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Snafu)]
pub enum FunctionCallError {
    #[snafu(display("failed to deserialize parameter '{key}'"))]
    Deserialization {
        source: serde_json::Error,
        key: String,
    },

    #[snafu(display("parameter '{key}' is missing in arguments '{args}'"))]
    MissingParameter {
        key: String,
        args: serde_json::Value,
    },

    #[snafu(display("arguments should be an object; actual: {actual}"))]
    ArgumentTypeMismatch { actual: String },
}

impl FunctionCall {
    /// Create a new function call
    pub fn new(name: impl Into<String>, args: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            args,
            thought_signature: None,
        }
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
            thought_signature: Some(thought_signature.into()),
        }
    }

    /// Get a parameter from the arguments
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T, FunctionCallError> {
        match &self.args {
            serde_json::Value::Object(obj) => {
                if let Some(value) = obj.get(key) {
                    serde_json::from_value(value.clone()).with_context(|_| DeserializationSnafu {
                        key: key.to_string(),
                    })
                } else {
                    Err(MissingParameterSnafu {
                        key: key.to_string(),
                        args: self.args.clone(),
                    }
                    .build())
                }
            }
            _ => Err(ArgumentTypeMismatchSnafu {
                actual: self.args.to_string(),
            }
            .build()),
        }
    }
}

/// A response from a function
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionResponse {
    /// The name of the function
    pub name: String,
    /// The response from the function
    /// This must be a valid JSON object
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
}

impl FunctionResponse {
    /// Create a new function response with a JSON value
    pub fn new(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            response: Some(response),
        }
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
        Ok(Self {
            name: name.into(),
            response: Some(json),
        })
    }

    /// Create a new function response with a string that will be parsed as JSON
    pub fn from_str(
        name: impl Into<String>,
        response: impl Into<String>,
    ) -> Result<Self, serde_json::Error> {
        let json = serde_json::from_str(&response.into())?;
        Ok(Self {
            name: name.into(),
            response: Some(json),
        })
    }
}

/// Configuration for tools
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolConfig {
    /// The function calling config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
}

/// Configuration for function calling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCallingConfig {
    /// The mode for function calling
    pub mode: FunctionCallingMode,
}

/// Mode for function calling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FunctionCallingMode {
    /// The model may use function calling
    Auto,
    /// The model must use function calling
    Any,
    /// The model must not use function calling
    None,
}
