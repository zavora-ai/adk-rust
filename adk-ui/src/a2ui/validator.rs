use jsonschema::Validator;
use serde_json::{json, Value};

use super::messages::A2uiMessage;

#[derive(Debug, Clone, Copy)]
pub enum A2uiSchemaVersion {
    V0_9,
    V0_8,
}

#[derive(Debug, Clone)]
pub struct A2uiValidationError {
    pub message: String,
    pub instance_path: String,
}

impl std::fmt::Display for A2uiValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.message, self.instance_path)
    }
}

impl std::error::Error for A2uiValidationError {}

/// Lightweight A2UI schema validator.
///
/// This validates envelope structure and required fields. Component-level
/// validation is intentionally minimal and can be upgraded later with full
/// catalog schema resolution.
pub struct A2uiValidator {
    v0_9: Validator,
    v0_8: Validator,
}

impl A2uiValidator {
    pub fn new() -> Result<Self, A2uiValidationError> {
        let v0_9 = Validator::new(&schema_v0_9())
            .map_err(|e| A2uiValidationError { message: format!("Invalid v0.9 schema: {}", e), instance_path: "/".to_string() })?;
        let v0_8 = Validator::new(&schema_v0_8())
            .map_err(|e| A2uiValidationError { message: format!("Invalid v0.8 schema: {}", e), instance_path: "/".to_string() })?;

        Ok(Self { v0_9, v0_8 })
    }

    pub fn validate_message(&self, message: &A2uiMessage, version: A2uiSchemaVersion) -> Result<(), Vec<A2uiValidationError>> {
        let value = serde_json::to_value(message).map_err(|e| {
            vec![A2uiValidationError { message: format!("Serialization failed: {}", e), instance_path: "/".to_string() }]
        })?;
        self.validate_value(&value, version)
    }

    pub fn validate_value(&self, value: &Value, version: A2uiSchemaVersion) -> Result<(), Vec<A2uiValidationError>> {
        let validator = match version {
            A2uiSchemaVersion::V0_9 => &self.v0_9,
            A2uiSchemaVersion::V0_8 => &self.v0_8,
        };

        let mapped = validator
            .iter_errors(value)
            .map(|e| A2uiValidationError {
                message: e.to_string(),
                instance_path: e.instance_path.to_string(),
            })
            .collect::<Vec<_>>();

        if !mapped.is_empty() {
            return Err(mapped);
        }

        Ok(())
    }
}

fn schema_v0_9() -> Value {
    json!({
        "type": "object",
        "oneOf": [
            {
                "required": ["createSurface"],
                "properties": {
                    "createSurface": {
                        "type": "object",
                        "required": ["surfaceId", "catalogId"],
                        "properties": {
                            "surfaceId": { "type": "string" },
                            "catalogId": { "type": "string" },
                            "theme": { "type": "object" },
                            "sendDataModel": { "type": "boolean" }
                        }
                    }
                }
            },
            {
                "required": ["updateComponents"],
                "properties": {
                    "updateComponents": {
                        "type": "object",
                        "required": ["surfaceId", "components"],
                        "properties": {
                            "surfaceId": { "type": "string" },
                            "components": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "required": ["id", "component"],
                                    "properties": {
                                        "id": { "type": "string" },
                                        "component": { 
                                            "type": "object",
                                            "description": "Component definition with nested structure: {ComponentName: {props}}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            {
                "required": ["updateDataModel"],
                "properties": {
                    "updateDataModel": {
                        "type": "object",
                        "required": ["surfaceId"],
                        "properties": {
                            "surfaceId": { "type": "string" },
                            "path": { "type": "string" },
                            "value": {}
                        }
                    }
                }
            },
            {
                "required": ["deleteSurface"],
                "properties": {
                    "deleteSurface": {
                        "type": "object",
                        "required": ["surfaceId"],
                        "properties": {
                            "surfaceId": { "type": "string" }
                        }
                    }
                }
            }
        ]
    })
}

fn schema_v0_8() -> Value {
    json!({
        "type": "object",
        "oneOf": [
            {
                "required": ["beginRendering"],
                "properties": {
                    "beginRendering": {
                        "type": "object",
                        "required": ["surfaceId", "root"],
                        "properties": {
                            "surfaceId": { "type": "string" },
                            "root": { "type": "string" },
                            "catalogId": { "type": "string" },
                            "styles": { "type": "object" }
                        }
                    }
                }
            },
            {
                "required": ["surfaceUpdate"],
                "properties": {
                    "surfaceUpdate": {
                        "type": "object",
                        "required": ["surfaceId", "components"],
                        "properties": {
                            "surfaceId": { "type": "string" },
                            "components": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "required": ["id", "component"],
                                    "properties": {
                                        "id": { "type": "string" },
                                        "component": { "type": "object" }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            {
                "required": ["dataModelUpdate"],
                "properties": {
                    "dataModelUpdate": {
                        "type": "object",
                        "required": ["surfaceId", "contents"],
                        "properties": {
                            "surfaceId": { "type": "string" },
                            "path": { "type": "string" },
                            "contents": { "type": "array" }
                        }
                    }
                }
            },
            {
                "required": ["deleteSurface"],
                "properties": {
                    "deleteSurface": {
                        "type": "object",
                        "required": ["surfaceId"],
                        "properties": {
                            "surfaceId": { "type": "string" }
                        }
                    }
                }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2ui::messages::{
        A2uiMessage,
        CreateSurface,
        CreateSurfaceMessage,
        UpdateComponents,
        UpdateComponentsMessage,
    };
    use serde_json::json;

    #[test]
    fn validates_v0_9_create_surface() {
        let validator = A2uiValidator::new().unwrap();
        let value = json!({
            "createSurface": {
                "surfaceId": "main",
                "catalogId": "catalog"
            }
        });
        assert!(validator.validate_value(&value, A2uiSchemaVersion::V0_9).is_ok());
    }

    #[test]
    fn rejects_invalid_v0_9_message() {
        let validator = A2uiValidator::new().unwrap();
        let value = json!({ "createSurface": { "catalogId": "missing_surface" } });
        assert!(validator.validate_value(&value, A2uiSchemaVersion::V0_9).is_err());
    }

    #[test]
    fn validates_struct_message() {
        let validator = A2uiValidator::new().unwrap();
        let message = A2uiMessage::CreateSurface(CreateSurfaceMessage {
            create_surface: CreateSurface {
                surface_id: "main".to_string(),
                catalog_id: "catalog".to_string(),
                theme: None,
                send_data_model: None,
            },
        });
        assert!(validator.validate_message(&message, A2uiSchemaVersion::V0_9).is_ok());
    }

    #[test]
    fn validates_update_components_minimal() {
        let validator = A2uiValidator::new().unwrap();
        let message = A2uiMessage::UpdateComponents(UpdateComponentsMessage {
            update_components: UpdateComponents {
                surface_id: "main".to_string(),
                components: vec![json!({ 
                    "id": "root", 
                    "component": { 
                        "Text": { 
                            "text": { "literalString": "Hello" } 
                        } 
                    } 
                })],
            },
        });
        assert!(validator.validate_message(&message, A2uiSchemaVersion::V0_9).is_ok());
    }
}
