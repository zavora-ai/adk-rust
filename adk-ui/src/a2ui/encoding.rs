use serde_json::Error as JsonError;

use super::messages::A2uiMessage;

/// Encode a single A2UI message as a JSON line.
pub fn encode_message_line(message: &A2uiMessage) -> Result<String, JsonError> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

/// Encode an iterator of A2UI messages as JSONL (newline-delimited JSON).
pub fn encode_jsonl<I>(messages: I) -> Result<String, JsonError>
where
    I: IntoIterator<Item = A2uiMessage>,
{
    let mut output = String::new();
    for message in messages {
        output.push_str(&serde_json::to_string(&message)?);
        output.push('\n');
    }
    Ok(output)
}

/// Encode an iterator of A2UI messages as JSONL bytes.
pub fn encode_jsonl_bytes<I>(messages: I) -> Result<Vec<u8>, JsonError>
where
    I: IntoIterator<Item = A2uiMessage>,
{
    Ok(encode_jsonl(messages)?.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2ui::messages::{
        A2uiMessage, CreateSurface, CreateSurfaceMessage, DeleteSurface, DeleteSurfaceMessage,
    };

    #[test]
    fn encodes_single_line_with_newline() {
        let message = A2uiMessage::CreateSurface(CreateSurfaceMessage {
            create_surface: CreateSurface {
                surface_id: "main".to_string(),
                catalog_id: "catalog".to_string(),
                theme: None,
                send_data_model: None,
            },
        });

        let line = encode_message_line(&message).unwrap();
        assert!(line.ends_with('\n'));

        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["createSurface"]["surfaceId"], "main");
    }

    #[test]
    fn encodes_multiple_messages_as_jsonl() {
        let create = A2uiMessage::CreateSurface(CreateSurfaceMessage {
            create_surface: CreateSurface {
                surface_id: "main".to_string(),
                catalog_id: "catalog".to_string(),
                theme: None,
                send_data_model: None,
            },
        });
        let delete = A2uiMessage::DeleteSurface(DeleteSurfaceMessage {
            delete_surface: DeleteSurface { surface_id: "main".to_string() },
        });

        let jsonl = encode_jsonl(vec![create, delete]).unwrap();
        let lines: Vec<&str> = jsonl.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 2);
    }
}
