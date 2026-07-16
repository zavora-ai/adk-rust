use crate::document::SchemaDocument;
use crate::error::{Result, SchemaError, ValidationIssue};
use crate::role::SchemaRole;
use std::sync::Arc;

/// A compiled, validated schema document.
#[cfg(feature = "runtime-validation")]
pub struct ValidatedSchemaDocument<R: SchemaRole> {
    document: SchemaDocument<R>,
    validator: Arc<jsonschema::Validator>,
}

#[cfg(feature = "runtime-validation")]
impl<R: SchemaRole> Clone for ValidatedSchemaDocument<R> {
    fn clone(&self) -> Self {
        Self { document: self.document.clone(), validator: self.validator.clone() }
    }
}

#[cfg(feature = "runtime-validation")]
impl<R: SchemaRole> std::fmt::Debug for ValidatedSchemaDocument<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatedSchemaDocument").field("document", &self.document).finish()
    }
}

#[cfg(feature = "runtime-validation")]
impl<R: SchemaRole> PartialEq for ValidatedSchemaDocument<R> {
    fn eq(&self, other: &Self) -> bool {
        self.document == other.document
    }
}

#[cfg(feature = "runtime-validation")]
impl<R: SchemaRole> ValidatedSchemaDocument<R> {
    /// Borrow the underlying SchemaDocument.
    pub fn as_document(&self) -> &SchemaDocument<R> {
        &self.document
    }
    /// Access the underlying canonical JSON Value.
    pub fn document(&self) -> &serde_json::Value {
        self.document.as_value()
    }
    /// Access the digest.
    pub fn digest(&self) -> crate::digest::SchemaDigest {
        self.document.digest()
    }

    /// Validate an instance JSON Value against this schema.
    pub fn validate(&self, instance: &serde_json::Value) -> Result<()> {
        let errors = self.validator.iter_errors(instance);
        let mut issues = Vec::new();
        for err in errors {
            issues.push(ValidationIssue {
                pointer: err.instance_path().to_string(),
                message: err.to_string(),
            });
        }
        if !issues.is_empty() {
            return Err(SchemaError::InvalidInstance { issues });
        }
        Ok(())
    }
}

#[cfg(feature = "runtime-validation")]
impl<R: SchemaRole> SchemaDocument<R> {
    /// Compile this schema into a `ValidatedSchemaDocument`.
    pub fn compile(self) -> Result<ValidatedSchemaDocument<R>> {
        use jsonschema::{Draft, Validator};
        // Build validator options explicitly denying external resolution authority.
        // Since resolve-http and resolve-file features are not enabled,
        // no resolver handles these schemes.
        let validator = Validator::options()
            .with_draft(Draft::Draft202012)
            .build(self.as_value())
            .map_err(|e| {
                let issues = vec![ValidationIssue {
                    pointer: e.instance_path().to_string(),
                    message: e.to_string(),
                }];
                SchemaError::InvalidSchema { issues }
            })?;
        Ok(ValidatedSchemaDocument { document: self, validator: Arc::new(validator) })
    }
}

#[cfg(all(test, feature = "runtime-validation"))]
mod tests {
    use super::*;
    use crate::IngestionPolicy;
    use crate::InputSchema;
    use serde_json::json;

    #[test]
    fn test_validator_sharing_on_clone() {
        let schema = json!({
            "type": "object",
            "properties": {
                "foo": { "type": "string" }
            }
        });
        let policy = IngestionPolicy::default();
        let doc = InputSchema::from_value(schema, &policy).unwrap();
        let validated = doc.compile().unwrap();
        let cloned = validated.clone();
        assert!(Arc::ptr_eq(&validated.validator, &cloned.validator));
    }
}
