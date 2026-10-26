//! The `forms` 1.0 catalog (spec 3.3 §6.1 `genui`): the A2UI v0.9.1 basic catalog without
//! the components that can fake a decision or cover the home, embedded and never fetched.
//! `catalogs/README.md` records its source, its upstream copyright and what changed.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::{Value, json};

/// The id of the `forms` 1.0 catalog.
pub const FORMS_CATALOG_ID: &str = "https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json";

/// The id the A2UI v0.9.1 common types carry.
const COMMON_TYPES_ID: &str = "https://a2ui.org/specification/v0_9/common_types.json";

/// Where the common types look for the catalog's functions: `catalog.json`, relative to
/// their own id.
const COMMON_TYPES_CATALOG: &str = "https://a2ui.org/specification/v0_9/catalog.json";

const FORMS_DOCUMENT: &str = include_str!("catalogs/forms-1.0.json");
const COMMON_TYPES_DOCUMENT: &str = include_str!("catalogs/common_types-0.9.1.json");

/// An A2UI catalog this home renders: the components and functions a drawing may use, each
/// component checked against the catalog's JSON Schema (spec 3.3 §6.1 `genui`).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{Catalog, FORMS_CATALOG_ID};
///
/// let forms = Catalog::forms();
/// assert_eq!(forms.id(), FORMS_CATALOG_ID);
/// assert_eq!(forms.label(), "forms 1.0");
/// assert!(forms.has_component("TextField"));
/// assert!(!forms.has_component("Image"));
/// assert!(!forms.functions().any(|name| name == "openUrl"));
/// ```
pub struct Catalog {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    document: Value,
    components: BTreeMap<String, jsonschema::Validator>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("id", &self.id)
            .field("components", &self.components.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

static FORMS: LazyLock<Catalog> = LazyLock::new(|| {
    Catalog::compile(FORMS_CATALOG_ID, "forms", "1.0", FORMS_DOCUMENT)
        .unwrap_or_else(|error| panic!("the embedded forms 1.0 catalog does not compile: {error}"))
});

impl Catalog {
    /// The `forms` 1.0 catalog.
    pub fn forms() -> &'static Catalog {
        &FORMS
    }

    fn compile(
        id: &'static str,
        name: &'static str,
        version: &'static str,
        document: &str,
    ) -> Result<Self, String> {
        let document: Value = serde_json::from_str(document).map_err(|error| error.to_string())?;
        let common: Value =
            serde_json::from_str(COMMON_TYPES_DOCUMENT).map_err(|error| error.to_string())?;
        let registry = jsonschema::Registry::new()
            .add(id, document.clone())
            .and_then(|registry| registry.add(COMMON_TYPES_CATALOG, document.clone()))
            .and_then(|registry| registry.add(COMMON_TYPES_ID, common))
            .and_then(jsonschema::RegistryBuilder::prepare)
            .map_err(|error| error.to_string())?;
        let names: Vec<String> = document["components"]
            .as_object()
            .map(|components| components.keys().cloned().collect())
            .unwrap_or_default();
        let mut components = BTreeMap::new();
        for component in names {
            let schema = json!({ "$ref": format!("{id}#/components/{component}") });
            let validator = jsonschema::options()
                .with_registry(&registry)
                .build(&schema)
                .map_err(|error| format!("{component}: {error}"))?;
            components.insert(component, validator);
        }
        Ok(Self { id, name, version, document, components })
    }

    /// The catalog's id.
    pub fn id(&self) -> &str {
        self.id
    }

    /// The catalog's name and version as the developer panel shows them, `forms 1.0`.
    pub fn label(&self) -> String {
        format!("{} {}", self.name, self.version)
    }

    /// The catalog's JSON Schema document.
    pub fn document(&self) -> &Value {
        &self.document
    }

    /// The component types it renders.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.components.keys().map(String::as_str)
    }

    /// The functions a drawing may call.
    pub fn functions(&self) -> impl Iterator<Item = &str> {
        self.document["functions"]
            .as_object()
            .into_iter()
            .flat_map(|functions| functions.keys().map(String::as_str))
    }

    /// Whether it renders `component`.
    pub fn has_component(&self, component: &str) -> bool {
        self.components.contains_key(component)
    }

    /// Checks `value` against the schema of its component type, `component`: `None` when it
    /// conforms, else where and why it does not.
    pub(crate) fn schema_error(&self, component: &str, value: &Value) -> Option<String> {
        let validator = self.components.get(component)?;
        validator.iter_errors(value).next().map(|error| {
            let at = error.instance_path().as_str();
            if at.is_empty() { error.to_string() } else { format!("{at}: {error}") }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forms_catalog_is_the_basic_catalog_without_what_can_fake_a_decision() {
        let forms = Catalog::forms();
        assert_eq!(
            forms.components().collect::<Vec<_>>(),
            vec![
                "Button",
                "Card",
                "CheckBox",
                "ChoicePicker",
                "Column",
                "DateTimeInput",
                "Divider",
                "List",
                "Row",
                "Slider",
                "Tabs",
                "Text",
                "TextField"
            ]
        );
        assert_eq!(
            forms.functions().collect::<Vec<_>>(),
            vec![
                "and",
                "email",
                "formatString",
                "length",
                "not",
                "numeric",
                "or",
                "pluralize",
                "regex",
                "required"
            ]
        );
        assert_eq!(forms.document()["catalogId"], FORMS_CATALOG_ID);
    }

    #[test]
    fn components_are_checked_against_their_schema() {
        let forms = Catalog::forms();
        let text = json!({ "id": "t", "component": "Text", "text": "Invoice INV-10" });
        assert_eq!(forms.schema_error("Text", &text), None);
        let bound = json!({ "id": "t", "component": "Text", "text": { "path": "/title" } });
        assert_eq!(forms.schema_error("Text", &bound), None);
        let extra = json!({ "id": "t", "component": "Text", "text": "x", "onClick": "y" });
        assert!(forms.schema_error("Text", &extra).is_some());
        let obscured = json!({
            "id": "p", "component": "TextField", "label": "Password", "variant": "obscured"
        });
        assert!(forms.schema_error("TextField", &obscured).is_some());
        let open = json!({
            "id": "b", "component": "Button", "child": "t",
            "action": { "functionCall": { "call": "openUrl", "args": { "url": "https://x.test" } } }
        });
        assert!(forms.schema_error("Button", &open).is_some(), "openUrl is not in the catalog");
    }
}
