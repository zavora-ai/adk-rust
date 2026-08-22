//! Wire DTOs for the `sandboxEnvironments` surface and the chunk-protocol
//! helpers implementing the code-execution conventions over `:execute`.

use adk_core::Result;
use adk_gcp::truncate_for_error;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The `metadata.attributes` key naming a file chunk (value is base64).
const FILE_NAME_ATTRIBUTE: &str = "file_name";
/// MIME type of the code-input and console-output chunks.
const JSON_MIME_TYPE: &str = "application/json";

/// Lifecycle state of a sandbox environment (output only).
///
/// Only [`Running`](Self::Running) sandboxes accept `:execute` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxState {
    /// The state is unspecified.
    #[serde(rename = "STATE_UNSPECIFIED")]
    Unspecified,
    /// The sandbox is being provisioned.
    #[serde(rename = "STATE_PROVISIONING")]
    Provisioning,
    /// The sandbox is running and accepts `:execute` calls.
    #[serde(rename = "STATE_RUNNING")]
    Running,
    /// The sandbox is being deprovisioned.
    #[serde(rename = "STATE_DEPROVISIONING")]
    Deprovisioning,
    /// The sandbox has been terminated.
    #[serde(rename = "STATE_TERMINATED")]
    Terminated,
    /// The sandbox has been deleted.
    #[serde(rename = "STATE_DELETED")]
    Deleted,
    /// Forward-compatibility catch-all for states this crate predates.
    #[serde(other, rename = "STATE_UNKNOWN")]
    Unknown,
}

/// Machine shape of a code-execution sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineConfig {
    /// The machine config is unspecified; the service picks the default.
    #[serde(rename = "MACHINE_CONFIG_UNSPECIFIED")]
    Unspecified,
    /// 4 vCPUs, 4 GiB RAM.
    #[serde(rename = "MACHINE_CONFIG_VCPU4_RAM4GIB")]
    Vcpu4Ram4Gib,
    /// Forward-compatibility catch-all for configs this crate predates.
    #[serde(other, rename = "MACHINE_CONFIG_UNKNOWN")]
    Unknown,
}

/// Language runtime of a code-execution sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeLanguage {
    /// The language is unspecified; the service picks the default.
    #[serde(rename = "LANGUAGE_UNSPECIFIED")]
    Unspecified,
    /// Python.
    #[serde(rename = "LANGUAGE_PYTHON")]
    Python,
    /// JavaScript.
    #[serde(rename = "LANGUAGE_JAVASCRIPT")]
    Javascript,
    /// Forward-compatibility catch-all for languages this crate predates.
    #[serde(other, rename = "LANGUAGE_UNKNOWN")]
    Unknown,
}

/// Spec of a code-execution sandbox environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionEnvironment {
    /// Machine shape; the service default applies when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_config: Option<MachineConfig>,
    /// Language runtime; the service default applies when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_language: Option<CodeLanguage>,
}

/// Spec of a computer-use sandbox environment (no fields).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUseEnvironment {}

/// The `sandbox_environment_category` oneof: exactly one category is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEnvironmentSpec {
    /// A code-execution sandbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution_environment: Option<CodeExecutionEnvironment>,
    /// A computer-use sandbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_use_environment: Option<ComputerUseEnvironment>,
}

impl SandboxEnvironmentSpec {
    /// A code-execution spec.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::vertex_sandbox::{CodeExecutionEnvironment, SandboxEnvironmentSpec};
    ///
    /// let spec = SandboxEnvironmentSpec::code_execution(CodeExecutionEnvironment::default());
    /// assert!(spec.code_execution_environment.is_some());
    /// ```
    pub fn code_execution(environment: CodeExecutionEnvironment) -> Self {
        Self { code_execution_environment: Some(environment), computer_use_environment: None }
    }

    /// A computer-use spec.
    pub fn computer_use() -> Self {
        Self {
            code_execution_environment: None,
            computer_use_environment: Some(ComputerUseEnvironment {}),
        }
    }
}

/// A `SandboxEnvironment` wire resource.
///
/// `state`, timestamps, `expire_time`, `connection_info`, and the snapshot
/// fields are output only; `ttl` is input only (a Duration string such as
/// `"3600s"`). On output the expiration oneof always carries `expire_time`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEnvironment {
    /// Resource name: `projects/*/locations/*/reasoningEngines/*/sandboxEnvironments/*`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Required user-supplied display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Creation timestamp (output only, RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// Last-update timestamp (output only, RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
    /// Lifecycle state (output only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SandboxState>,
    /// Sandbox category spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<SandboxEnvironmentSpec>,
    /// Template the sandbox was created from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_environment_template: Option<Value>,
    /// Connection details (output only; shape varies by category).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<Value>,
    /// Latest snapshot (output only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_sandbox_environment_snapshot: Option<Value>,
    /// Owner of the sandbox, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<Value>,
    /// Snapshot to restore the sandbox from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_environment_snapshot: Option<Value>,
    /// Expiration timestamp (output side of the expiration oneof, RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_time: Option<String>,
    /// Time-to-live (input side of the expiration oneof, e.g. `"3600s"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

/// Input to [`create_sandbox`](super::VertexSandboxClient::create_sandbox).
///
/// # Example
///
/// ```rust
/// use adk_code::vertex_sandbox::{
///     CodeExecutionEnvironment, CodeLanguage, CreateSandboxRequest, SandboxEnvironmentSpec,
/// };
///
/// let request = CreateSandboxRequest::new("my-sandbox")
///     .with_ttl("3600s")
///     .with_spec(SandboxEnvironmentSpec::code_execution(CodeExecutionEnvironment {
///         machine_config: None,
///         code_language: Some(CodeLanguage::Python),
///     }));
/// # let _ = request;
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CreateSandboxRequest {
    pub(crate) display_name: String,
    pub(crate) ttl: Option<String>,
    pub(crate) spec: Option<SandboxEnvironmentSpec>,
}

impl CreateSandboxRequest {
    /// Creates a request with the required display name.
    pub fn new(display_name: impl Into<String>) -> Self {
        Self { display_name: display_name.into(), ttl: None, spec: None }
    }

    /// Sets the time-to-live as a Duration string (e.g. `"3600s"`).
    ///
    /// The service documents no hard maximum — adk-python sends
    /// `"31536000s"` (one year) — but sandboxes may lose state after
    /// roughly 14 days of disuse. Every `:execute` call resets the TTL
    /// server-side.
    #[must_use]
    pub fn with_ttl(mut self, ttl: impl Into<String>) -> Self {
        self.ttl = Some(ttl.into());
        self
    }

    /// Sets the sandbox category spec. Defaults to a code-execution
    /// sandbox with service defaults when omitted.
    #[must_use]
    pub fn with_spec(mut self, spec: SandboxEnvironmentSpec) -> Self {
        self.spec = Some(spec);
        self
    }

    /// The `SandboxEnvironment` create body this request serializes to.
    pub(crate) fn into_body(self) -> SandboxEnvironment {
        SandboxEnvironment {
            display_name: Some(self.display_name),
            ttl: self.ttl,
            spec: self.spec,
            ..SandboxEnvironment::default()
        }
    }
}

/// Optional metadata carried by a [`Chunk`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMetadata {
    /// Attribute map; every value is a base64-encoded string.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

/// One unit of `:execute` input or output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    /// MIME type. Required on input; may be absent on output file chunks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Base64-encoded payload bytes.
    #[serde(default)]
    pub data: String,
    /// Optional attribute metadata (e.g. `file_name` for file chunks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ChunkMetadata>,
}

/// An input file for [`execute_code`](super::VertexSandboxClient::execute_code).
///
/// # Example
///
/// ```rust
/// use adk_code::vertex_sandbox::InputFile;
///
/// let file = InputFile::new("data.csv", "text/csv", b"a,b\n1,2\n".to_vec());
/// assert_eq!(file.name, "data.csv");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
    /// Filename the sandbox sees.
    pub name: String,
    /// MIME type of the file contents.
    pub mime_type: String,
    /// Raw (not base64) file bytes.
    pub data: Vec<u8>,
}

impl InputFile {
    /// Creates an input file from its parts.
    pub fn new(name: impl Into<String>, mime_type: impl Into<String>, data: Vec<u8>) -> Self {
        Self { name: name.into(), mime_type: mime_type.into(), data }
    }
}

/// A file the executed code wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Filename reported by the sandbox.
    pub name: String,
    /// MIME type, when the sandbox reported one.
    pub mime_type: Option<String>,
    /// Raw (decoded) file bytes.
    pub data: Vec<u8>,
}

/// Decoded result of one code execution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxExecutionResult {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Files the code wrote.
    pub output_files: Vec<OutputFile>,
}

/// The `:execute` request body.
#[derive(Debug, Serialize)]
pub(crate) struct ExecuteRequest {
    pub(crate) inputs: Vec<Chunk>,
}

/// The `:execute` response body.
#[derive(Debug, Deserialize)]
pub(crate) struct ExecuteResponse {
    #[serde(default)]
    pub(crate) outputs: Vec<Chunk>,
}

/// A `sandboxEnvironments.list` response page.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListSandboxesResponse {
    #[serde(default)]
    pub(crate) sandbox_environments: Vec<SandboxEnvironment>,
    #[serde(default)]
    pub(crate) next_page_token: Option<String>,
}

/// Console-output payload carried by an `application/json` output chunk.
///
/// The wire keys are `msg_out`/`msg_err` (not `stdout`/`stderr`); unknown
/// keys — such as `output_files` — are tolerated and ignored.
#[derive(Debug, Deserialize)]
struct ConsolePayload {
    #[serde(default)]
    msg_out: Option<String>,
    #[serde(default)]
    msg_err: Option<String>,
}

/// Encodes source code as the code-input chunk.
///
/// # Example
///
/// ```rust
/// use adk_code::vertex_sandbox::encode_code_chunk;
///
/// let chunk = encode_code_chunk("print('hi')");
/// assert_eq!(chunk.mime_type.as_deref(), Some("application/json"));
/// ```
pub fn encode_code_chunk(code: &str) -> Chunk {
    let payload = serde_json::json!({ "code": code });
    Chunk {
        mime_type: Some(JSON_MIME_TYPE.to_string()),
        data: BASE64.encode(payload.to_string()),
        metadata: None,
    }
}

/// Encodes an input file as a file chunk (`file_name` attribute set).
///
/// # Example
///
/// ```rust
/// use adk_code::vertex_sandbox::{InputFile, encode_file_chunk};
///
/// let chunk = encode_file_chunk(&InputFile::new("a.txt", "text/plain", b"hi".to_vec()));
/// assert!(chunk.metadata.unwrap().attributes.contains_key("file_name"));
/// ```
pub fn encode_file_chunk(file: &InputFile) -> Chunk {
    let mut attributes = BTreeMap::new();
    attributes.insert(FILE_NAME_ATTRIBUTE.to_string(), BASE64.encode(&file.name));
    Chunk {
        mime_type: Some(file.mime_type.clone()),
        data: BASE64.encode(&file.data),
        metadata: Some(ChunkMetadata { attributes }),
    }
}

/// Decodes `:execute` output chunks into stdout, stderr, and output files.
///
/// Chunks carrying a `file_name` attribute become [`OutputFile`]s;
/// `application/json` chunks without one carry `msg_out`/`msg_err` console
/// text. Unrecognized chunks are skipped.
///
/// # Errors
///
/// Returns an error when chunk data or a `file_name` attribute is not
/// valid base64, a filename is not UTF-8, or a console payload is not
/// valid JSON.
pub fn decode_output_chunks(outputs: &[Chunk]) -> Result<SandboxExecutionResult> {
    let errors = super::errors();
    let mut result = SandboxExecutionResult::default();
    for chunk in outputs {
        let file_name =
            chunk.metadata.as_ref().and_then(|meta| meta.attributes.get(FILE_NAME_ATTRIBUTE));
        if let Some(encoded_name) = file_name {
            let name_bytes = BASE64.decode(encoded_name).map_err(|error| {
                errors.invalid_response(format!(
                    "vertex sandbox output chunk carries a file_name attribute that is not valid base64: {error}",
                ))
            })?;
            let name = String::from_utf8(name_bytes).map_err(|_| {
                errors.invalid_response(
                    "vertex sandbox output chunk carries a file_name that is not valid UTF-8",
                )
            })?;
            let data = BASE64.decode(&chunk.data).map_err(|error| {
                errors.invalid_response(format!(
                    "vertex sandbox output file '{}' data is not valid base64: {error}",
                    truncate_for_error(&name),
                ))
            })?;
            result.output_files.push(OutputFile { name, mime_type: chunk.mime_type.clone(), data });
            continue;
        }
        if chunk.mime_type.as_deref() == Some(JSON_MIME_TYPE) {
            let bytes = BASE64.decode(&chunk.data).map_err(|error| {
                errors.invalid_response(format!(
                    "vertex sandbox console output chunk data is not valid base64: {error}",
                ))
            })?;
            let payload: ConsolePayload = serde_json::from_slice(&bytes).map_err(|error| {
                errors.invalid_response(format!(
                    "vertex sandbox console output chunk is not valid JSON: {error}",
                ))
            })?;
            if let Some(out) = payload.msg_out {
                result.stdout.push_str(&out);
            }
            if let Some(err) = payload.msg_err {
                result.stderr.push_str(&err);
            }
            continue;
        }
        tracing::debug!(chunk.mime_type = ?chunk.mime_type, "skipping unrecognized output chunk");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_chunk_encodes_the_json_code_convention() {
        let chunk = encode_code_chunk("print('hi')");
        assert_eq!(chunk.mime_type.as_deref(), Some(JSON_MIME_TYPE));
        assert_eq!(chunk.metadata, None);
        let decoded: Value = serde_json::from_slice(&BASE64.decode(&chunk.data).unwrap()).unwrap();
        assert_eq!(decoded, json!({ "code": "print('hi')" }));
    }

    #[test]
    fn file_chunk_carries_base64_name_and_bytes() {
        let file = InputFile::new("data.csv", "text/csv", b"a,b\n".to_vec());
        let chunk = encode_file_chunk(&file);
        assert_eq!(chunk.mime_type.as_deref(), Some("text/csv"));
        let attributes = chunk.metadata.unwrap().attributes;
        assert_eq!(attributes.get(FILE_NAME_ATTRIBUTE).unwrap(), &BASE64.encode("data.csv"));
        assert_eq!(BASE64.decode(&chunk.data).unwrap(), b"a,b\n");
    }

    #[test]
    fn output_decoding_separates_console_and_files() {
        let outputs = vec![
            Chunk {
                mime_type: Some(JSON_MIME_TYPE.to_string()),
                data: BASE64.encode(
                    json!({
                        "msg_out": "hello\n",
                        "msg_err": "warning\n",
                        "output_files": ["ignored.txt"],
                        "unknown_key": 42,
                    })
                    .to_string(),
                ),
                metadata: None,
            },
            Chunk {
                mime_type: None,
                data: BASE64.encode(b"bytes"),
                metadata: Some(ChunkMetadata {
                    attributes: BTreeMap::from([(
                        FILE_NAME_ATTRIBUTE.to_string(),
                        BASE64.encode("out.bin"),
                    )]),
                }),
            },
        ];
        let result = decode_output_chunks(&outputs).unwrap();
        assert_eq!(
            result,
            SandboxExecutionResult {
                stdout: "hello\n".to_string(),
                stderr: "warning\n".to_string(),
                output_files: vec![OutputFile {
                    name: "out.bin".to_string(),
                    mime_type: None,
                    data: b"bytes".to_vec(),
                }],
            },
        );
    }

    #[test]
    fn console_chunks_accumulate_across_outputs() {
        let console = |payload: Value| Chunk {
            mime_type: Some(JSON_MIME_TYPE.to_string()),
            data: BASE64.encode(payload.to_string()),
            metadata: None,
        };
        let outputs = vec![
            console(json!({ "msg_out": "one" })),
            console(json!({ "msg_out": "two", "msg_err": "err" })),
        ];
        let result = decode_output_chunks(&outputs).unwrap();
        assert_eq!(result.stdout, "onetwo");
        assert_eq!(result.stderr, "err");
    }

    #[test]
    fn unrecognized_chunks_are_skipped() {
        let outputs = vec![Chunk {
            mime_type: Some("image/png".to_string()),
            data: BASE64.encode(b"png-bytes"),
            metadata: None,
        }];
        let result = decode_output_chunks(&outputs).unwrap();
        assert_eq!(result, SandboxExecutionResult::default());
    }

    #[test]
    fn invalid_base64_in_outputs_is_rejected() {
        let outputs = vec![Chunk {
            mime_type: Some(JSON_MIME_TYPE.to_string()),
            data: "not base64!!!".to_string(),
            metadata: None,
        }];
        let error = decode_output_chunks(&outputs).unwrap_err();
        assert_eq!(error.code, "code.vertex_sandbox.invalid_response");
    }

    #[test]
    fn unknown_enum_values_deserialize_to_the_catch_all() {
        let state: SandboxState = serde_json::from_value(json!("STATE_HIBERNATED")).unwrap();
        assert_eq!(state, SandboxState::Unknown);
        let running: SandboxState = serde_json::from_value(json!("STATE_RUNNING")).unwrap();
        assert_eq!(running, SandboxState::Running);
    }

    #[test]
    fn create_body_serializes_camel_case_and_skips_absent_fields() {
        let body = CreateSandboxRequest::new("default_sandbox")
            .with_ttl("31536000s")
            .with_spec(SandboxEnvironmentSpec::code_execution(CodeExecutionEnvironment::default()))
            .into_body();
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({
                "displayName": "default_sandbox",
                "ttl": "31536000s",
                "spec": { "codeExecutionEnvironment": {} },
            }),
        );
    }
}
