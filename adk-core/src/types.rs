#[cfg(feature = "base64")]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use derive_more::{AsRef, Deref, Display};
use mime::Mime;
use serde::{Deserialize, Serialize};

pub mod mime_serde {
    use mime::Mime;
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::str::FromStr;

    pub fn serialize<S>(mime: &Mime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(mime.as_ref())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Mime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Mime::from_str(&s).map_err(serde::de::Error::custom)
    }
}

pub mod base64_bytes {
    #[cfg(feature = "base64")]
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use bytes::Bytes;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
        #[cfg(feature = "base64")]
        {
            serializer.serialize_str(&STANDARD.encode(bytes))
        }
        #[cfg(not(feature = "base64"))]
        {
            serializer.serialize_str(&format!("<binary data: {} bytes>", bytes.len()))
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
        use serde::Deserialize;
        let s = String::deserialize(deserializer)?;
        #[cfg(feature = "base64")]
        {
            let decoded = STANDARD.decode(s).map_err(serde::de::Error::custom)?;
            Ok(Bytes::from(decoded))
        }
        #[cfg(not(feature = "base64"))]
        {
            let _ = s;
            Err(serde::de::Error::custom("base64 feature not enabled"))
        }
    }
}

macro_rules! define_id_type {
    ($name:ident, $err_name:ident) => {
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            Hash,
            Display,
            AsRef,
            Deref,
            Serialize,
            Deserialize,
            Default,
            PartialOrd,
            Ord,
        )]
        pub struct $name(String);

        impl $name {
            /// Creates a new ID, ensuring it does not contain invalid characters (like colons).
            pub fn new(id: impl Into<String>) -> Result<Self, crate::AdkError> {
                let id_str = id.into();
                if id_str.is_empty() {
                    return Err(crate::AdkError::$err_name(format!(
                        "{} cannot be empty",
                        stringify!($name)
                    )));
                }
                if id_str.contains(':') {
                    return Err(crate::AdkError::$err_name(format!(
                        "{} cannot contain a colon (':')",
                        stringify!($name)
                    )));
                }
                Ok(Self(id_str))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = crate::AdkError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self::new(s).expect("Valid ID expected")
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self::new(s).expect("Valid ID expected")
            }
        }

        impl From<&String> for $name {
            fn from(s: &String) -> Self {
                Self::new(s.clone()).expect("Valid ID expected")
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<std::ffi::OsStr> for $name {
            fn as_ref(&self) -> &std::ffi::OsStr {
                std::ffi::OsStr::new(&self.0)
            }
        }
    };
}

define_id_type!(SessionId, Session);
define_id_type!(InvocationId, Agent);
define_id_type!(UserId, Agent);

impl From<SessionId> for InvocationId {
    fn from(id: SessionId) -> Self {
        InvocationId(id.0)
    }
}

/// A consolidated identity capsule for ADK execution.
///
/// This struct groups the foundational identifiers that define a specific "run"
/// or "turn" of an agent. Using a single struct ensures consistency across
/// the framework and simplifies context propagation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdkIdentity {
    pub invocation_id: InvocationId,
    pub session_id: SessionId,
    pub user_id: UserId,
    pub app_name: String,
    pub branch: String,
    pub agent_name: String,
}

impl Default for AdkIdentity {
    fn default() -> Self {
        Self {
            invocation_id: InvocationId::default(),
            session_id: SessionId::default(),
            user_id: UserId::new("anonymous").unwrap(),
            app_name: "adk-app".to_string(),
            branch: "main".to_string(),
            agent_name: "generic-agent".to_string(),
        }
    }
}

/// Maximum allowed size for inline binary data (10 MB).
/// Prevents accidental or malicious embedding of oversized payloads in Content parts.
pub const MAX_INLINE_DATA_SIZE: usize = 10 * 1024 * 1024;

/// Represents the role of the author of a message.
#[derive(Debug, Clone, PartialEq, Eq, Display)]
pub enum Role {
    #[display("user")]
    User,
    #[display("model")]
    Model,
    #[display("system")]
    System,
    #[display("tool")]
    Tool,
    #[display("{_0}")]
    Custom(String),
}

impl std::str::FromStr for Role {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "user" | "human" => Role::User,
            "model" | "assistant" => Role::Model,
            "system" | "developer" => Role::System,
            "tool" | "function" => Role::Tool,
            _ => Role::Custom(s.to_string()),
        })
    }
}

impl From<String> for Role {
    fn from(s: String) -> Self {
        std::str::FromStr::from_str(&s).unwrap()
    }
}

impl From<&str> for Role {
    fn from(s: &str) -> Self {
        std::str::FromStr::from_str(s).unwrap()
    }
}

impl From<Role> for String {
    fn from(role: Role) -> String {
        role.to_string()
    }
}

impl serde::Serialize for Role {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Role {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(std::str::FromStr::from_str(&s).unwrap())
    }
}

impl Role {
    pub fn is_user(&self) -> bool {
        match self {
            Role::User => true,
            Role::Custom(s) => s == "user" || s == "human",
            _ => false,
        }
    }

    pub fn is_model(&self) -> bool {
        match self {
            Role::Model => true,
            Role::Custom(s) => s == "model" || s == "assistant",
            _ => false,
        }
    }

    pub fn is_system(&self) -> bool {
        match self {
            Role::System => true,
            Role::Custom(s) => s == "system" || s == "developer",
            _ => false,
        }
    }

    pub fn is_tool(&self) -> bool {
        match self {
            Role::Tool => true,
            Role::Custom(s) => s == "tool" || s == "function",
            _ => false,
        }
    }
}

impl Default for Role {
    fn default() -> Role {
        Role::User
    }
}

impl PartialEq<&str> for Role {
    fn eq(&self, other: &&str) -> bool {
        let other_role: Role = (*other).into();
        self == &other_role
    }
}

impl PartialEq<String> for Role {
    fn eq(&self, other: &String) -> bool {
        self.eq(&other.as_str())
    }
}

/// Data associated with a function response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionResponseData {
    /// The name of the function that was called.
    pub name: String,
    /// The response from the function.
    pub response: serde_json::Value,
}

/// A consolidated message content structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Content {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Content {
    pub fn new(role: impl Into<Role>) -> Self {
        Self { role: role.into(), parts: Vec::new() }
    }

    pub fn user() -> Self {
        Self::new(Role::User)
    }

    pub fn model() -> Self {
        Self::new(Role::Model)
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.parts.push(Part::text(text));
        self
    }

    /// Returns the concatenated text from all text parts.
    pub fn text(&self) -> String {
        self.parts.iter().filter_map(|p| p.as_text()).collect::<Vec<_>>().join("")
    }

    pub fn with_inline_data(
        mut self,
        mime_type: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Result<Self, crate::AdkError> {
        self.parts.push(Part::inline_data(mime_type, data)?);
        Ok(self)
    }

    pub fn with_thinking(mut self, thought: impl Into<String>) -> Self {
        self.parts.push(Part::thinking(thought));
        self
    }

    pub fn with_file_uri(
        mut self,
        mime_type: impl Into<String>,
        file_uri: impl Into<String>,
    ) -> Self {
        self.parts.push(Part::file_data(mime_type, file_uri));
        self
    }

    pub fn with_part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    pub fn collect_text(&self) -> String {
        self.parts.iter().map(|p| p.to_text()).collect::<Vec<_>>().join("\n")
    }

    pub fn is_assistant(&self) -> bool {
        self.role.is_model()
    }
}

/// A single part of a message's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Part {
    Text(String),
    InlineData {
        #[serde(rename = "mimeType", with = "mime_serde")]
        mime_type: Mime,
        #[serde(with = "base64_bytes")]
        data: Bytes,
    },
    FunctionCall {
        name: String,
        args: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    FunctionResponse {
        name: String,
        response: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Thinking {
        thought: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    FileData {
        #[serde(rename = "mimeType", with = "mime_serde")]
        mime_type: Mime,
        #[serde(rename = "fileUri")]
        file_uri: String,
    },
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Part::Text(text.into())
    }

    pub fn inline_data(
        mime_type: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> crate::Result<Self> {
        let data = data.into();
        if data.len() > MAX_INLINE_DATA_SIZE {
            return Err(crate::AdkError::PayloadTooLarge(data.len()));
        }
        let mime_str = mime_type.into();
        let mime = mime_str
            .parse::<Mime>()
            .map_err(|e| crate::AdkError::Config(format!("Invalid mime type: {e}")))?;
        Ok(Part::InlineData { mime_type: mime, data })
    }

    pub fn thinking(thought: impl Into<String>) -> Self {
        Part::Thinking { thought: thought.into(), signature: None }
    }

    pub fn file_data(mime_type: impl Into<String>, file_uri: impl Into<String>) -> Self {
        let mime_str = mime_type.into();
        let mime = mime_str.parse::<Mime>().unwrap_or(mime::APPLICATION_OCTET_STREAM);
        Part::FileData { mime_type: mime, file_uri: file_uri.into() }
    }

    pub fn with_signature(mut self, sig: impl Into<String>) -> Self {
        if let Part::Thinking { ref mut signature, .. } = self {
            *signature = Some(sig.into());
        }
        self
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Part::Text(text) => Some(text),
            Part::Thinking { thought, .. } => Some(thought),
            _ => None,
        }
    }

    pub fn is_thinking(&self) -> bool {
        matches!(self, Part::Thinking { .. })
    }

    pub fn mime_type(&self) -> Option<&str> {
        match self {
            Part::InlineData { mime_type, .. } => Some(mime_type.as_ref()),
            Part::FileData { mime_type, .. } => Some(mime_type.as_ref()),
            _ => None,
        }
    }

    pub fn file_uri(&self) -> Option<&str> {
        match self {
            Part::FileData { file_uri, .. } => Some(file_uri),
            _ => None,
        }
    }

    pub fn is_media(&self) -> bool {
        matches!(self, Part::InlineData { .. } | Part::FileData { .. })
    }

    pub fn as_text_str(&self) -> Option<&str> {
        self.as_text()
    }

    pub fn to_text(&self) -> String {
        match self {
            Part::Text(text) => text.clone(),
            Part::Thinking { thought, .. } => thought.clone(),
            Part::InlineData { mime_type, data } => {
                #[cfg(feature = "base64")]
                {
                    let encoded = STANDARD.encode(data);
                    format!(
                        "<attachment mime_type=\"{mime_type}\" encoding=\"base64\">{encoded}</attachment>"
                    )
                }
                #[cfg(not(feature = "base64"))]
                {
                    format!("[Inline Data: {mime_type}]")
                }
            }
            Part::FileData { mime_type, file_uri } => {
                format!("[File: {file_uri}] (mime: {mime_type})")
            }
            Part::FunctionCall { name, args, .. } => {
                format!("[Function Call: {}({})]", name, args)
            }
            Part::FunctionResponse { name, response, .. } => {
                format!("[Function Response: {}: {}]", name, response)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_validation() {
        assert!(SessionId::new("valid").is_ok());
        assert!(SessionId::new("").is_err());
        assert!(SessionId::new("invalid:id").is_err());
    }

    #[test]
    fn test_id_from() {
        let id: SessionId = "test".into();
        assert_eq!(id.to_string(), "test");

        let id2: SessionId = "test".to_string().into();
        assert_eq!(id2.to_string(), "test");
    }

    #[test]
    fn test_role_display() {
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Custom("admin".to_string()).to_string(), "admin");
    }

    #[test]
    fn test_payload_limit() -> crate::Result<()> {
        let small_data = vec![0u8; 1024];
        let part = Part::inline_data("image/png", small_data)?;
        assert!(matches!(part, Part::InlineData { .. }));

        let large_data = vec![0u8; MAX_INLINE_DATA_SIZE + 1];
        let err = Part::inline_data("image/png", large_data);
        assert!(matches!(err, Err(crate::AdkError::PayloadTooLarge(_))));
        Ok(())
    }

    #[test]
    fn test_zero_copy_bytes() -> crate::Result<()> {
        let data = Bytes::from(vec![1, 2, 3]);
        let part = Part::inline_data("application/octet-stream", data.clone())?;
        if let Part::InlineData { data: part_data, .. } = part {
            assert_eq!(data, part_data);
            // In a real environment, we'd check if they share the same heap allocation.
        }
        Ok(())
    }

    #[test]
    fn test_exact_json_serialization_shapes() {
        // 1. Test standard text payload
        let text_content = Content::user().with_text("Hello");
        let text_json = serde_json::to_string(&text_content).unwrap();
        assert_eq!(text_json, r#"{"role":"user","parts":[{"text":"Hello"}]}"#);

        // 2. Test inline data (Base64 & camelCase verification)
        let data = bytes::Bytes::from("fake_image_bytes");
        let inline_content = Content::user().with_inline_data("image/png", data).unwrap();
        let inline_json = serde_json::to_string(&inline_content).unwrap();

        #[cfg(feature = "base64")]
        {
            // "fake_image_bytes" base64 encoded is "ZmFrZV9pbWFnZV9ieXRlcw=="
            assert_eq!(
                inline_json,
                r#"{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"ZmFrZV9pbWFnZV9ieXRlcw=="}}]}"#
            );
        }

        // 3. Test thinking variant
        let thinking_content = Content::model().with_thinking("Calculating steps...");
        let thinking_json = serde_json::to_string(&thinking_content).unwrap();
        assert_eq!(
            thinking_json,
            r#"{"role":"model","parts":[{"thinking":{"thought":"Calculating steps..."}}]}"#
        );

        // 4. Test tool call structure
        let tool_part = Part::FunctionCall {
            name: "get_weather".to_string(),
            args: serde_json::json!({"location": "Richmond"}),
            id: None,
            thought_signature: None,
        };
        let tool_content = Content::model().with_text("Let me check.").with_part(tool_part);
        let tool_json = serde_json::to_string(&tool_content).unwrap();
        assert_eq!(
            tool_json,
            r#"{"role":"model","parts":[{"text":"Let me check."},{"functionCall":{"name":"get_weather","args":{"location":"Richmond"}}}]}"#
        );
    }
}
