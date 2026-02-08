use serde_json::Value;

#[test]
fn default_catalog_is_valid_json() {
    let raw = include_str!("../catalog/default_catalog.json");
    let value: Value = serde_json::from_str(raw).expect("default catalog should be valid JSON");
    let catalog_id = value.get("catalogId").and_then(Value::as_str).unwrap_or("");
    assert_eq!(catalog_id, "zavora.ai:adk-ui/default@0.1.0");
    assert!(value.get("components").is_some(), "catalog should define components");
}

#[test]
fn catalog_metadata_is_valid_json() {
    let raw = include_str!("../catalog/metadata.json");
    let value: Value = serde_json::from_str(raw).expect("catalog metadata should be valid JSON");
    let catalog_id = value.get("catalogId").and_then(Value::as_str).unwrap_or("");
    assert_eq!(catalog_id, "zavora.ai:adk-ui/default@0.1.0");
    assert_eq!(value.get("license").and_then(Value::as_str), Some("Apache-2.0"));
}

#[test]
fn registry_resolves_default_catalog() {
    let registry = adk_ui::CatalogRegistry::default();
    let default_catalog_id = registry.default_catalog_id().to_string();
    let artifact =
        registry.load_local_catalog(&default_catalog_id).expect("default catalog should resolve");
    assert_eq!(artifact.catalog_id, default_catalog_id);
    assert!(artifact.catalog.get("components").is_some());
    assert!(artifact.metadata.is_some());
}
