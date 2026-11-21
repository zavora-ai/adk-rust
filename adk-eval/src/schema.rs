//! Test file schema definitions
//!
//! Defines the structure for test files (`.test.json`) and eval sets (`.evalset.json`).
//!
//! A turn can show the agent images: an [`ImageFixture`] part names an image file by its
//! path, relative to the eval file, and its SHA-256. Loading the file verifies every
//! fixture's digest, so a case never runs against an image that changed after its
//! expectations were written.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{EvalError, Result};
use crate::test_generator::EvalCaseMetadata;
use crate::vision::VisionExpected;

/// The directory relative fixture paths in the file at `path` resolve against.
fn base_dir(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("."))
}

/// A complete test file containing multiple evaluation cases
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFile {
    /// Unique identifier for this eval set
    pub eval_set_id: String,
    /// Human-readable name
    pub name: String,
    /// Description of what these tests cover
    #[serde(default)]
    pub description: String,
    /// List of evaluation cases
    pub eval_cases: Vec<EvalCase>,
}

impl TestFile {
    /// Load a test file from disk, verifying every image fixture against its digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or parsed, or an image fixture is
    /// missing or does not match its digest.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let mut test_file: TestFile = serde_json::from_str(&content)?;
        for eval_case in &mut test_file.eval_cases {
            eval_case.load_images(base_dir(path.as_ref()))?;
        }
        Ok(test_file)
    }

    /// Save test file to disk
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// An eval set references multiple test files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSet {
    /// Unique identifier
    pub eval_set_id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    #[serde(default)]
    pub description: String,
    /// List of test file paths or inline eval cases
    #[serde(default)]
    pub test_files: Vec<String>,
    /// Inline eval cases (alternative to test_files)
    #[serde(default)]
    pub eval_cases: Vec<EvalCase>,
}

impl EvalSet {
    /// Load an eval set from disk, verifying the image fixtures of its inline cases.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or parsed, or an image fixture is
    /// missing or does not match its digest.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let mut eval_set: EvalSet = serde_json::from_str(&content)?;
        for eval_case in &mut eval_set.eval_cases {
            eval_case.load_images(base_dir(path.as_ref()))?;
        }
        Ok(eval_set)
    }

    /// Get all eval cases, loading from test files if needed
    pub fn get_all_cases(&self, base_path: impl AsRef<Path>) -> Result<Vec<EvalCase>> {
        let mut all_cases = self.eval_cases.clone();

        for test_file_path in &self.test_files {
            let full_path = base_path.as_ref().join(test_file_path);
            let test_file = TestFile::load(&full_path).map_err(|e| {
                EvalError::LoadError(format!("Failed to load {}: {}", test_file_path, e))
            })?;
            all_cases.extend(test_file.eval_cases);
        }

        Ok(all_cases)
    }
}

/// A single evaluation case (test case)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    /// Unique identifier for this test case
    pub eval_id: String,
    /// Optional description
    #[serde(default)]
    pub description: String,
    /// The conversation turns to evaluate
    pub conversation: Vec<Turn>,
    /// Session configuration
    #[serde(default)]
    pub session_input: SessionInput,
    /// Optional tags for filtering
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional metadata (generation info, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EvalCaseMetadata>,
}

impl EvalCase {
    /// Reads and verifies every image fixture in the case's turns, resolving relative paths
    /// against `base_dir`. [`TestFile::load`] and [`EvalSet::load`] call it; call it
    /// yourself for a case built or parsed in code.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::LoadError`] naming the first fixture that is missing, unreadable,
    /// larger than [`adk_core::MAX_INLINE_DATA_SIZE`], or not the file its digest pins.
    pub fn load_images(&mut self, base_dir: &Path) -> Result<()> {
        for turn in &mut self.conversation {
            for content in std::iter::once(&mut turn.user_content).chain(&mut turn.final_response) {
                for part in &mut content.parts {
                    if let Part::Image { image } = part {
                        image.load(base_dir).map_err(|e| {
                            EvalError::LoadError(format!("eval case '{}': {e}", self.eval_id))
                        })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The image fixtures in the case's turns that have not been loaded and verified.
    pub fn unloaded_images(&self) -> Vec<&ImageFixture> {
        self.conversation
            .iter()
            .flat_map(|turn| std::iter::once(&turn.user_content).chain(&turn.final_response))
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::Image { image } if image.bytes().is_none() => Some(image),
                _ => None,
            })
            .collect()
    }
}

/// A single turn in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// Unique identifier for this turn
    pub invocation_id: String,
    /// User input content
    pub user_content: ContentData,
    /// Expected final response from the agent
    #[serde(default)]
    pub final_response: Option<ContentData>,
    /// Expected intermediate data (tool calls, etc.)
    #[serde(default)]
    pub intermediate_data: Option<IntermediateData>,
    /// Expected perception output — regions, page text, fields, click targets — scored by
    /// the [`vision`](crate::criteria::EvaluationCriteria::vision) criteria.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_vision: Option<VisionExpected>,
}

/// Content data structure (matches ADK Content)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentData {
    /// Content parts
    pub parts: Vec<Part>,
    /// Role (user, model, tool)
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "user".to_string()
}

impl ContentData {
    /// Create content from text
    pub fn text(text: &str) -> Self {
        Self { parts: vec![Part::Text { text: text.to_string() }], role: "user".to_string() }
    }

    /// Create model response content
    pub fn model_response(text: &str) -> Self {
        Self { parts: vec![Part::Text { text: text.to_string() }], role: "model".to_string() }
    }

    /// Get all text parts concatenated
    pub fn get_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Convert to ADK Content
    ///
    /// A loaded [`ImageFixture`] becomes `Part::InlineData` carrying the verified bytes, with
    /// the fixture's resolved path and digest in its annotations under `adkEvalFixture`; an
    /// image that was not loaded is left out.
    pub fn to_adk_content(&self) -> adk_core::Content {
        let mut content = adk_core::Content::new(&self.role);
        for part in &self.parts {
            match part {
                Part::Text { text } => {
                    content = content.with_text(text);
                }
                Part::Image { image } => {
                    if let Some(part) = image.to_inline_part() {
                        content.parts.push(part);
                    }
                }
                Part::FunctionCall { .. } | Part::FunctionResponse { .. } => {
                    // Function calls/responses are handled separately in the evaluation
                    // The Content type doesn't have direct methods for these
                }
            }
        }
        content
    }
}

/// Content part variants
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    /// Text content
    Text { text: String },
    /// Function/tool call
    FunctionCall { name: String, args: Value },
    /// Function/tool response
    FunctionResponse { name: String, response: Value },
    /// An image file shown to the agent, pinned by its digest: `{"image": {"path", "digest"}}`
    Image { image: ImageFixture },
}

/// An image an eval case shows the agent: a file named by its path, relative to the eval
/// file's directory, and pinned by its SHA-256.
///
/// # Example
///
/// ```
/// use adk_eval::ImageFixture;
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("page.png"), b"not really a png").unwrap();
/// let mut image = ImageFixture::new("page.png", &format!("sha256:{}", "0".repeat(64)));
/// let err = image.load(dir.path()).unwrap_err();
/// assert!(err.to_string().contains("does not match"), "{err}");
///
/// let digest = ImageFixture::digest_of(b"not really a png");
/// let mut image = ImageFixture::new("page.png", &digest);
/// image.load(dir.path()).unwrap();
/// assert_eq!(image.bytes(), Some(&b"not really a png"[..]));
/// assert_eq!(image.mime_type(), "image/png");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageFixture {
    /// The image file, relative to the eval file's directory (or absolute).
    pub path: String,
    /// The file's SHA-256 as `sha256:<64 hex digits>`; the bare digits are accepted.
    pub digest: String,
    /// The MIME type; inferred from the extension when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The verified file, once loaded.
    #[serde(skip)]
    loaded: Option<LoadedImage>,
}

#[derive(Debug, Clone)]
struct LoadedImage {
    path: PathBuf,
    bytes: Arc<[u8]>,
}

impl ImageFixture {
    /// A fixture for the file at `path` with this digest.
    pub fn new(path: impl Into<String>, digest: &str) -> Self {
        Self { path: path.into(), digest: digest.to_string(), mime_type: None, loaded: None }
    }

    /// The digest of `bytes` in the form fixtures pin: `sha256:<64 hex digits>`.
    pub fn digest_of(bytes: &[u8]) -> String {
        let hex: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        format!("sha256:{hex}")
    }

    /// Reads the file, resolving a relative path against `base_dir`, and checks it against
    /// the digest.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::LoadError`] when the digest is malformed, or the file is
    /// missing, unreadable, larger than [`adk_core::MAX_INLINE_DATA_SIZE`], or has another
    /// digest.
    pub fn load(&mut self, base_dir: &Path) -> Result<()> {
        let want = self.digest.strip_prefix("sha256:").unwrap_or(&self.digest).to_ascii_lowercase();
        if want.len() != 64 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EvalError::LoadError(format!(
                "image fixture {} has digest {:?}; write it as sha256:<64 hex digits>",
                self.path, self.digest
            )));
        }
        let path = base_dir.join(&self.path);
        let bytes = std::fs::read(&path).map_err(|e| {
            EvalError::LoadError(format!("image fixture {} could not be read: {e}", path.display()))
        })?;
        if bytes.len() > adk_core::MAX_INLINE_DATA_SIZE {
            return Err(EvalError::LoadError(format!(
                "image fixture {} is {} bytes, over the {} byte inline limit; downscale it",
                path.display(),
                bytes.len(),
                adk_core::MAX_INLINE_DATA_SIZE
            )));
        }
        let actual = Self::digest_of(&bytes);
        if actual.strip_prefix("sha256:") != Some(want.as_str()) {
            return Err(EvalError::LoadError(format!(
                "image fixture {} is {actual}, which does not match the pinned sha256:{want}; \
                 the file changed since the case was written, so restore it or update the digest \
                 and the case's expectations",
                path.display()
            )));
        }
        self.loaded = Some(LoadedImage { path, bytes: bytes.into() });
        Ok(())
    }

    /// The verified bytes, once [`load`](Self::load)ed.
    pub fn bytes(&self) -> Option<&[u8]> {
        self.loaded.as_ref().map(|loaded| &*loaded.bytes)
    }

    /// The resolved path, once [`load`](Self::load)ed.
    pub fn resolved_path(&self) -> Option<&Path> {
        self.loaded.as_ref().map(|loaded| loaded.path.as_path())
    }

    /// The MIME type: the one given, else from the extension (`png`, `jpg`/`jpeg`, `webp`,
    /// `gif`), else `application/octet-stream`.
    pub fn mime_type(&self) -> String {
        if let Some(mime) = &self.mime_type {
            return mime.clone();
        }
        let extension = Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => "application/octet-stream",
        }
        .to_string()
    }

    /// The fixture as `Part::InlineData`, or `None` before it is loaded.
    fn to_inline_part(&self) -> Option<adk_core::Part> {
        let loaded = self.loaded.as_ref()?;
        Some(adk_core::Part::InlineData {
            mime_type: self.mime_type(),
            data: loaded.bytes.to_vec(),
            uri: None,
            annotations: Some(serde_json::json!({
                "adkEvalFixture": {
                    "path": loaded.path.to_string_lossy(),
                    "digest": Self::digest_of(&loaded.bytes),
                }
            })),
        })
    }
}

/// Intermediate data during a turn (tool calls, etc.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntermediateData {
    /// Expected tool calls in order
    #[serde(default)]
    pub tool_uses: Vec<ToolUse>,
    /// Intermediate responses before final
    #[serde(default)]
    pub intermediate_responses: Vec<ContentData>,
}

/// A tool use (function call)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    /// Tool/function name
    pub name: String,
    /// Arguments passed to the tool.
    ///
    /// In an expected tool use, `Value::Null` (the value when `args` is omitted from a
    /// test file) places no constraint on the actual arguments.
    #[serde(default)]
    pub args: Value,
    /// Expected response (optional, for mocking)
    #[serde(default)]
    pub expected_response: Option<Value>,
}

impl ToolUse {
    /// Create a new tool use
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            args: Value::Object(Default::default()),
            expected_response: None,
        }
    }

    /// Add arguments
    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }

    /// Checks whether `other`, an actual tool call, satisfies this expected tool use.
    ///
    /// Names must be equal. Expected arguments of `Value::Null` match any arguments.
    /// Otherwise arguments are compared recursively, and numbers compare by value, so
    /// `1` matches `1.0`.
    ///
    /// | `strict_args` | Objects | Arrays |
    /// |---------------|---------|--------|
    /// | `false` | every expected key is present with a matching value; extra keys are allowed at any depth | same length, matched element by element |
    /// | `true` | same key set, values matched recursively | same length, matched element by element |
    ///
    /// # Example
    ///
    /// ```
    /// use adk_eval::ToolUse;
    /// use serde_json::json;
    ///
    /// let expected = ToolUse::new("search").with_args(json!({"filter": {"limit": 10}}));
    /// let actual = ToolUse::new("search")
    ///     .with_args(json!({"query": "rust", "filter": {"limit": 10.0, "page": 2}}));
    ///
    /// assert!(expected.matches(&actual, false));
    /// assert!(!expected.matches(&actual, true));
    /// ```
    pub fn matches(&self, other: &ToolUse, strict_args: bool) -> bool {
        self.name == other.name
            && (self.args.is_null() || json_matches(&self.args, &other.args, strict_args))
    }
}

/// Compares an expected JSON value against an actual one; see [`ToolUse::matches`].
fn json_matches(expected: &Value, actual: &Value, strict: bool) -> bool {
    match (expected, actual) {
        (Value::Number(expected), Value::Number(actual)) => {
            // Integers compare exactly so values beyond 2^53 do not collapse in f64.
            if let (Some(expected), Some(actual)) = (expected.as_i64(), actual.as_i64()) {
                expected == actual
            } else if let (Some(expected), Some(actual)) = (expected.as_u64(), actual.as_u64()) {
                expected == actual
            } else {
                expected.as_f64() == actual.as_f64()
            }
        }
        (Value::Object(expected), Value::Object(actual)) => {
            (!strict || expected.len() == actual.len())
                && expected.iter().all(|(key, value)| {
                    actual.get(key).is_some_and(|actual| json_matches(value, actual, strict))
                })
        }
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected.iter().zip(actual).all(|(e, a)| json_matches(e, a, strict))
        }
        _ => expected == actual,
    }
}

/// Session input configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionInput {
    /// Application name
    #[serde(default)]
    pub app_name: String,
    /// User identifier
    #[serde(default)]
    pub user_id: String,
    /// Initial state
    #[serde(default)]
    pub state: HashMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_test_file() {
        let json = r#"{
            "eval_set_id": "test_set",
            "name": "Test Set",
            "description": "A test set",
            "eval_cases": [
                {
                    "eval_id": "test_1",
                    "conversation": [
                        {
                            "invocation_id": "inv_1",
                            "user_content": {
                                "parts": [{"text": "Hello"}],
                                "role": "user"
                            },
                            "final_response": {
                                "parts": [{"text": "Hi there!"}],
                                "role": "model"
                            }
                        }
                    ]
                }
            ]
        }"#;

        let test_file: TestFile = serde_json::from_str(json).unwrap();
        assert_eq!(test_file.eval_set_id, "test_set");
        assert_eq!(test_file.eval_cases.len(), 1);
        assert_eq!(test_file.eval_cases[0].eval_id, "test_1");
    }

    #[test]
    fn test_tool_use_matching() {
        let expected = ToolUse::new("get_weather").with_args(json!({"location": "NYC"}));

        let actual_exact = ToolUse::new("get_weather").with_args(json!({"location": "NYC"}));
        assert!(expected.matches(&actual_exact, true));

        let actual_extra =
            ToolUse::new("get_weather").with_args(json!({"location": "NYC", "unit": "celsius"}));
        assert!(!expected.matches(&actual_extra, true)); // Strict fails
        assert!(expected.matches(&actual_extra, false)); // Partial passes

        let actual_wrong = ToolUse::new("get_weather").with_args(json!({"location": "LA"}));
        assert!(!expected.matches(&actual_wrong, true));
        assert!(!expected.matches(&actual_wrong, false));
    }

    #[test]
    fn partial_args_match_nested_objects_as_subsets() {
        let expected = ToolUse::new("book")
            .with_args(json!({"trip": {"from": "NBO", "legs": [{"seat": "12A"}]}}));
        let actual = ToolUse::new("book").with_args(json!({
            "trip": {"from": "NBO", "to": "LHR", "legs": [{"seat": "12A", "meal": "veg"}]},
            "currency": "KES"
        }));
        assert!(expected.matches(&actual, false));
        assert!(!expected.matches(&actual, true));

        let wrong_nested = ToolUse::new("book")
            .with_args(json!({"trip": {"from": "MBA", "legs": [{"seat": "12A"}]}}));
        assert!(!expected.matches(&wrong_nested, false));

        let missing_element =
            ToolUse::new("book").with_args(json!({"trip": {"from": "NBO", "legs": []}}));
        assert!(!expected.matches(&missing_element, false));
    }

    #[test]
    fn numbers_match_by_value() {
        let expected = ToolUse::new("set_volume").with_args(json!({"level": 1, "gain": 0.5}));
        let actual = ToolUse::new("set_volume").with_args(json!({"level": 1.0, "gain": 0.5}));
        assert!(expected.matches(&actual, false));
        assert!(expected.matches(&actual, true));

        let different = ToolUse::new("set_volume").with_args(json!({"level": 2, "gain": 0.5}));
        assert!(!expected.matches(&different, true));

        let large = ToolUse::new("seek").with_args(json!({"offset": 9_007_199_254_740_993_u64}));
        let off_by_one =
            ToolUse::new("seek").with_args(json!({"offset": 9_007_199_254_740_992_u64}));
        assert!(!large.matches(&off_by_one, true));
    }

    #[test]
    fn omitted_expected_args_match_any_args() {
        let expected: ToolUse = serde_json::from_value(json!({"name": "get_time"})).unwrap();
        let actual = ToolUse::new("get_time").with_args(json!({"timezone": "Africa/Nairobi"}));
        assert!(expected.matches(&actual, false));
        assert!(expected.matches(&actual, true));
        assert!(!expected.matches(&ToolUse::new("get_date"), false));
    }

    #[test]
    fn test_content_data() {
        let content = ContentData::text("Hello world");
        assert_eq!(content.get_text(), "Hello world");
        assert_eq!(content.role, "user");

        let model = ContentData::model_response("Hi there!");
        assert_eq!(model.role, "model");
    }
}
