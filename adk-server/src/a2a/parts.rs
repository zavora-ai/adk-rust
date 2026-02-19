use adk_core::{Part, Result};
use base64::{Engine as _, engine::general_purpose};
use serde_json::{Map, Value};

pub fn adk_parts_to_a2a(
    parts: &[Part],
    long_running_ids: &[String],
) -> Result<Vec<crate::a2a::Part>> {
    parts
        .iter()
        .map(|part| match part {
            Part::Text { text } => Ok(crate::a2a::Part::text(text.clone())),
            Part::InlineData { mime_type, data } => {
                let encoded = general_purpose::STANDARD.encode(data);
                Ok(crate::a2a::Part::file(crate::a2a::FileContent {
                    name: None,
                    mime_type: Some(mime_type.clone()),
                    bytes: Some(encoded),
                    uri: None,
                }))
            }
            Part::InlineDataBase64 { mime_type, data_base64 } => {
                // Preserve canonical base64 payload and avoid decode/re-encode.
                Ok(crate::a2a::Part::file(crate::a2a::FileContent {
                    name: None,
                    mime_type: Some(mime_type.clone()),
                    bytes: Some(data_base64.clone()),
                    uri: None,
                }))
            }
            Part::FileData { mime_type, file_uri } => {
                // FileData contains a URI reference to a file
                Ok(crate::a2a::Part::file(crate::a2a::FileContent {
                    name: None,
                    mime_type: Some(mime_type.clone()),
                    bytes: None,
                    uri: Some(file_uri.clone()),
                }))
            }
            Part::FunctionCall { name, args, id } => {
                let is_long_running = long_running_ids.contains(name);
                let mut data = Map::new();
                let mut call_data = Map::new();
                call_data.insert("name".to_string(), Value::String(name.clone()));
                call_data.insert("args".to_string(), args.clone());
                if let Some(call_id) = id {
                    call_data.insert("id".to_string(), Value::String(call_id.clone()));
                }
                data.insert("function_call".to_string(), Value::Object(call_data));

                let mut metadata = Map::new();
                metadata.insert("long_running".to_string(), Value::Bool(is_long_running));

                Ok(crate::a2a::Part::Data { data, metadata: Some(metadata) })
            }
            Part::FunctionResponse { function_response, id } => {
                let mut data = Map::new();
                let mut resp_data = Map::new();
                resp_data.insert("name".to_string(), Value::String(function_response.name.clone()));
                resp_data.insert("response".to_string(), function_response.response.clone());
                if let Some(resp_id) = id {
                    resp_data.insert("id".to_string(), Value::String(resp_id.clone()));
                }
                data.insert("function_response".to_string(), Value::Object(resp_data));

                Ok(crate::a2a::Part::Data { data, metadata: None })
            }
        })
        .collect()
}

pub fn a2a_parts_to_adk(parts: &[crate::a2a::Part]) -> Result<Vec<Part>> {
    parts
        .iter()
        .map(|part| match part {
            crate::a2a::Part::Text { text, .. } => Ok(Part::Text { text: text.clone() }),
            crate::a2a::Part::File { file, .. } => {
                if let Some(bytes) = &file.bytes {
                    let data = general_purpose::STANDARD.decode(bytes).map_err(|e| {
                        adk_core::AdkError::Agent(format!("Base64 decode error: {}", e))
                    })?;
                    if data.len() > adk_core::MAX_INLINE_DATA_SIZE {
                        return Err(adk_core::AdkError::Agent(format!(
                            "Inline data exceeds max inline size of {} bytes",
                            adk_core::MAX_INLINE_DATA_SIZE
                        )));
                    }
                    Ok(Part::InlineDataBase64 {
                        mime_type: file.mime_type.clone().unwrap_or_default(),
                        data_base64: bytes.clone(),
                    })
                } else {
                    Err(adk_core::AdkError::Agent("File part with URI not supported".to_string()))
                }
            }
            crate::a2a::Part::Data { data, .. } => {
                if let Some(call) = data.get("function_call") {
                    let name = call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            adk_core::AdkError::Agent("Missing function name".to_string())
                        })?
                        .to_string();
                    let args = call.get("args").cloned().unwrap_or(Value::Object(Map::new()));
                    let id = call.get("id").and_then(|v| v.as_str()).map(String::from);
                    Ok(Part::FunctionCall { name, args, id })
                } else if let Some(resp) = data.get("function_response") {
                    let name = resp
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            adk_core::AdkError::Agent("Missing function name".to_string())
                        })?
                        .to_string();
                    let response =
                        resp.get("response").cloned().unwrap_or(Value::Object(Map::new()));
                    let id = resp.get("id").and_then(|v| v.as_str()).map(String::from);
                    Ok(Part::FunctionResponse {
                        function_response: adk_core::FunctionResponseData { name, response },
                        id,
                    })
                } else {
                    Err(adk_core::AdkError::Agent("Unknown data part format".to_string()))
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_text_conversion() {
        let adk_parts = vec![Part::Text { text: "Hello".to_string() }];
        let a2a_parts = adk_parts_to_a2a(&adk_parts, &[]).unwrap();
        assert_eq!(a2a_parts.len(), 1);

        let back = a2a_parts_to_adk(&a2a_parts).unwrap();
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn test_function_call_conversion() {
        let adk_parts = vec![Part::FunctionCall {
            name: "test".to_string(),
            args: json!({"key": "value"}),
            id: Some("call_123".to_string()),
        }];
        let a2a_parts = adk_parts_to_a2a(&adk_parts, &[]).unwrap();
        assert_eq!(a2a_parts.len(), 1);

        let back = a2a_parts_to_adk(&a2a_parts).unwrap();
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn test_inline_data_base64_passthrough_to_a2a() {
        let adk_parts = vec![Part::InlineDataBase64 {
            mime_type: "application/pdf".to_string(),
            data_base64: "JVBERi0=".to_string(),
        }];
        let a2a_parts = adk_parts_to_a2a(&adk_parts, &[]).unwrap();
        assert_eq!(a2a_parts.len(), 1);

        match &a2a_parts[0] {
            crate::a2a::Part::File { file, .. } => {
                assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
                assert_eq!(file.bytes.as_deref(), Some("JVBERi0="));
            }
            _ => panic!("Expected file part"),
        }
    }

    #[test]
    fn test_a2a_file_bytes_to_inline_data_base64() {
        let a2a_parts = vec![crate::a2a::Part::file(crate::a2a::FileContent {
            name: Some("doc.pdf".to_string()),
            mime_type: Some("application/pdf".to_string()),
            bytes: Some("JVBERi0=".to_string()),
            uri: None,
        })];
        let adk_parts = a2a_parts_to_adk(&a2a_parts).unwrap();
        assert_eq!(adk_parts.len(), 1);
        assert!(matches!(
            &adk_parts[0],
            Part::InlineDataBase64 {
                mime_type,
                data_base64
            } if mime_type == "application/pdf" && data_base64 == "JVBERi0="
        ));
    }
}
