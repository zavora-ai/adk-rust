use crate::document::SchemaDocument;
use crate::error::Result;
use crate::policy::IngestionPolicy;
use crate::role::{Input, Output};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;

#[cfg(feature = "schemars")]
impl SchemaDocument<Input> {
    /// Generate an InputSchema for type T using the default IngestionPolicy.
    pub fn for_type<T: JsonSchema>() -> Result<Self> {
        Self::for_type_with_policy::<T>(&IngestionPolicy::default())
    }

    /// Generate an InputSchema for type T using a custom IngestionPolicy.
    pub fn for_type_with_policy<T: JsonSchema>(policy: &IngestionPolicy) -> Result<Self> {
        let settings = SchemaSettings::draft2020_12()
            .with(|s| {
                s.inline_subschemas = false;
            })
            .for_deserialize();
        generate_static::<T, Input>(settings, policy)
    }
}

#[cfg(feature = "schemars")]
impl SchemaDocument<Output> {
    /// Generate an OutputSchema for type T using the default IngestionPolicy.
    pub fn for_type<T: JsonSchema>() -> Result<Self> {
        Self::for_type_with_policy::<T>(&IngestionPolicy::default())
    }

    /// Generate an OutputSchema for type T using a custom IngestionPolicy.
    pub fn for_type_with_policy<T: JsonSchema>(policy: &IngestionPolicy) -> Result<Self> {
        let settings = SchemaSettings::draft2020_12()
            .with(|s| {
                s.inline_subschemas = false;
            })
            .for_serialize();
        generate_static::<T, Output>(settings, policy)
    }
}

#[cfg(feature = "schemars")]
fn generate_static<T: JsonSchema, R: crate::role::SchemaRole>(
    settings: SchemaSettings,
    policy: &IngestionPolicy,
) -> Result<SchemaDocument<R>> {
    use schemars::generate::SchemaGenerator;
    let generator = SchemaGenerator::new(settings);
    let root = generator.into_root_schema_for::<T>();
    let value = serde_json::to_value(root)
        .map_err(|e| crate::error::SchemaError::Serialization(e.to_string()))?;
    SchemaDocument::<R>::from_value(value, policy)
}
