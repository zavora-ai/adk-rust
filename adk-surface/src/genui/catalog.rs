//! The `forms` 1.0 catalog (spec 3.3 §6.1 `genui`): the A2UI v0.9.1 basic catalog without
//! the components that can fake a decision or cover the home, embedded and never fetched.
//! `catalogs/README.md` records its source, its upstream copyright and what changed.
//!
//! An **extension catalog** is `forms` 1.0 plus components another crate defines
//! ([`Catalog::extend`]). A2UI gives a surface one catalog, so an extension is always a
//! superset of `forms` under its own id, and it can never loosen what `forms` refuses: its
//! components only show (no actions, no fields), every string in them and every data model
//! value they bind is checked by rule 5, and each one says what the home and the terminal
//! show for it ([`ExtensionComponent::home`]) in forms components and the home's own
//! elements only: charts, 3D scenes and depth effects.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value, json};

use super::drawn::DrawnChart;
use super::scene::{DrawnDepth, DrawnScene};

/// The id of the `forms` 1.0 catalog.
pub const FORMS_CATALOG_ID: &str = "https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json";

/// The upstream basic catalog's components `forms` 1.0 leaves out, which no extension may
/// bring back under the same name.
pub(crate) const LEFT_OUT: &[&str] = &["Image", "Icon", "Modal", "Video", "AudioPlayer"];

/// Properties an extension component's schema may not declare: a component that only shows
/// has no action to dispatch, no event to send and no field to check.
const FORBIDDEN_PROPERTIES: &[&str] = &["action", "event", "checks"];

/// String formats an extension component's schema may not declare: a drawing has no links.
const FORBIDDEN_FORMATS: &[&str] = &["uri", "uri-reference", "iri", "iri-reference", "url"];

/// A property of an extension component that holds other components of the drawing, by id
/// (spec 3.3 §6.1 `genui`, extension catalogs). The validator follows these slots for depth,
/// loops and every limit, so a container can never hide a component from a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildSlot {
    /// The property is one component id.
    One(&'static str),
    /// The property is a list of component ids.
    Many(&'static str),
    /// The property `array` is a list of objects, each naming one component id at `child`,
    /// such as an app's `pages: [{ "title": "Overview", "child": "overview" }]`.
    EachOf {
        /// The list.
        array: &'static str,
        /// The id's key in each of its objects.
        child: &'static str,
    },
}

/// An extension component as its projection reads it (spec 3.3 §7.4 rule 5): every property
/// with its bindings, `formatString` and `pluralize` already resolved on the server, and
/// every string already checked.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedComponent<'a> {
    /// Its id.
    pub id: &'a str,
    /// Its type, such as `Metric`.
    pub component: &'a str,
    /// Its properties, resolved.
    pub props: &'a Value,
    /// How many components its slots hold, in slot order: [`HomeNode::Child`] names them by
    /// index.
    pub children: usize,
}

impl ResolvedComponent<'_> {
    /// The resolved string property `name`.
    pub fn str(&self, name: &str) -> Option<&str> {
        self.props.get(name).and_then(Value::as_str)
    }

    /// The resolved property `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.props.get(name)
    }
}

/// The text style of a [`HomeNode::Text`], one of the `forms` `Text` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HomeText {
    /// A level 1 heading.
    H1,
    /// A level 2 heading.
    H2,
    /// A level 3 heading.
    H3,
    /// A level 4 heading.
    H4,
    /// A level 5 heading.
    H5,
    /// Small, muted text.
    Caption,
    /// Body text.
    #[default]
    Body,
}

impl HomeText {
    /// The `forms` `Text` variant it is drawn with.
    pub fn variant(self) -> &'static str {
        match self {
            Self::H1 => "h1",
            Self::H2 => "h2",
            Self::H3 => "h3",
            Self::H4 => "h4",
            Self::H5 => "h5",
            Self::Caption => "caption",
            Self::Body => "body",
        }
    }
}

/// What the home and the terminal show for an extension component: forms components, charts,
/// 3D scenes and depth effects, never a button or a field, so a projection only shows (spec
/// 3.3 §7.4 rule 5). Every string in it is checked again, and every child the component's
/// slots hold must appear exactly once, as [`HomeNode::Child`].
///
/// A scene's panels and a wall or stage's children hold only what shows: no button or field
/// anywhere inside them. A scene's text view holds no child. Scenes and depth effects don't
/// nest in each other.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HomeNode {
    /// Text.
    Text {
        /// What it reads.
        text: String,
        /// How it is styled.
        variant: HomeText,
    },
    /// Side by side.
    Row(Vec<HomeNode>),
    /// One above another.
    Column(Vec<HomeNode>),
    /// On a card.
    Card(Vec<HomeNode>),
    /// Tabs, each a title and what it shows.
    Tabs(Vec<(String, HomeNode)>),
    /// A rule.
    Divider,
    /// The component's child at this index, drawn as the home draws it.
    Child(usize),
    /// A chart the home draws natively.
    Chart(DrawnChart),
    /// A 3D scene the home draws natively, with its panels and its text view.
    Scene(Box<HomeScene>),
    /// Children the home presents in depth, with CSS 3D.
    Depth {
        /// How.
        depth: DrawnDepth,
        /// What it presents.
        children: Vec<HomeNode>,
    },
}

/// A 3D scene in a home view.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeScene {
    /// The scene; the server writes its summary, its panels' keys and its text view's key.
    pub scene: DrawnScene,
    /// Held components shown beside its objects.
    pub panels: Vec<HomePanel>,
    /// What it shows, in words: a table or a list, holding no child. It is shown by "Show as
    /// text", where WebGL is missing, and in the terminal; without one, they list the
    /// labelled objects.
    pub text: Option<HomeNode>,
}

impl HomeScene {
    /// `scene`, with no panels and no text view.
    pub fn new(scene: DrawnScene) -> Self {
        Self { scene, panels: Vec::new(), text: None }
    }
}

/// A held component shown beside a scene's object.
#[derive(Debug, Clone, PartialEq)]
pub struct HomePanel {
    /// The object's id.
    pub anchor: String,
    /// What it shows: usually a [`HomeNode::Child`].
    pub content: HomeNode,
}

impl HomeNode {
    /// Body text.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into(), variant: HomeText::Body }
    }

    /// Small, muted text.
    pub fn caption(text: impl Into<String>) -> Self {
        Self::Text { text: text.into(), variant: HomeText::Caption }
    }

    /// A heading, level 1 to 5.
    pub fn heading(level: u8, text: impl Into<String>) -> Self {
        let variant = match level {
            0 | 1 => HomeText::H1,
            2 => HomeText::H2,
            3 => HomeText::H3,
            4 => HomeText::H4,
            _ => HomeText::H5,
        };
        Self::Text { text: text.into(), variant }
    }
}

/// A component an extension catalog adds to `forms` 1.0 (spec 3.3 §6.1 `genui`, extension
/// catalogs).
///
/// A component only shows: its schema may not declare `action`, `event` or `checks`, and an
/// instance that carries any of them is refused. [`home`](Self::home) says what the web home
/// and the terminal show for it; a host that registers a renderer for the component (the
/// React `DrawnView`'s `renderers`) may draw its resolved properties instead.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ExtensionComponent, HomeNode, ResolvedComponent};
/// use serde_json::{Value, json};
///
/// struct Metric;
///
/// impl ExtensionComponent for Metric {
///     fn name(&self) -> &'static str {
///         "Metric"
///     }
///     fn schema(&self) -> Value {
///         json!({
///             "type": "object",
///             "properties": {
///                 "id": { "type": "string" },
///                 "component": { "const": "Metric" },
///                 "label": { "$ref": "https://a2ui.org/specification/v0_9/common_types.json#/$defs/DynamicString" },
///                 "value": { "$ref": "https://a2ui.org/specification/v0_9/common_types.json#/$defs/DynamicValue" }
///             },
///             "required": ["id", "component", "label", "value"],
///             "additionalProperties": false
///         })
///     }
///     fn usage(&self) -> &'static str {
///         "Metric: one number with its label, such as {label: \"Open incidents\", value: 3}"
///     }
///     fn home(&self, metric: &ResolvedComponent<'_>) -> Result<HomeNode, String> {
///         let value = metric.get("value").map(Value::to_string).unwrap_or_default();
///         Ok(HomeNode::Row(vec![
///             HomeNode::caption(metric.str("label").unwrap_or_default()),
///             HomeNode::heading(3, value.trim_matches('"')),
///         ]))
///     }
/// }
/// ```
pub trait ExtensionComponent: Send + Sync + 'static {
    /// The component's type, unique in the catalog and not one of `forms`' or of the
    /// components `forms` leaves out.
    fn name(&self) -> &'static str;

    /// Its A2UI component schema (JSON Schema). It may `$ref` the A2UI common types and the
    /// catalog's `#/$defs/CatalogComponentCommon`, and declares no `$defs`, `functions`,
    /// `action`, `event`, `checks` or URI formats.
    fn schema(&self) -> Value;

    /// The properties that hold other components, by id; none for a component that holds
    /// nothing.
    fn children(&self) -> &'static [ChildSlot] {
        &[]
    }

    /// One line the render tools tell the model about it: what it shows and its shape.
    fn usage(&self) -> &'static str;

    /// The component types that may hold it (A2UI 1.0 `allowedParents`, `"Surface"` for the
    /// top level); empty for any.
    fn allowed_parents(&self) -> &'static [&'static str] {
        &[]
    }

    /// The component types it may hold (A2UI 1.0 `allowedChildren`); empty for any.
    fn allowed_children(&self) -> &'static [&'static str] {
        &[]
    }

    /// What the home and the terminal show for it, from its resolved properties. It must be
    /// pure: the same component always projects the same way.
    ///
    /// # Errors
    ///
    /// Returns why the component cannot be shown; the drawing is refused with that reason.
    fn home(&self, component: &ResolvedComponent<'_>) -> Result<HomeNode, String>;
}

/// An extension catalog: its id, name, version and the components it adds to `forms` 1.0.
pub struct ExtensionSpec {
    /// Its id, which every surface drawn in it carries. A schema change takes a new id.
    pub id: &'static str,
    /// Its name, such as `insight`.
    pub name: &'static str,
    /// Its version, such as `1.0`.
    pub version: &'static str,
    /// The components it adds.
    pub components: Vec<Arc<dyn ExtensionComponent>>,
}

impl std::fmt::Debug for ExtensionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionSpec")
            .field("id", &self.id)
            .field("components", &self.components.iter().map(|c| c.name()).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Why an extension catalog was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("extension catalog {catalog}: {detail}")]
pub struct CatalogError {
    /// The catalog's id.
    pub catalog: String,
    /// What is wrong.
    pub detail: String,
}

/// The id the A2UI v0.9.1 common types carry.
const COMMON_TYPES_ID: &str = "https://a2ui.org/specification/v0_9/common_types.json";

/// Where the common types look for the catalog's functions: `catalog.json`, relative to
/// their own id.
const COMMON_TYPES_CATALOG: &str = "https://a2ui.org/specification/v0_9/catalog.json";

const FORMS_DOCUMENT: &str = include_str!("catalogs/forms-1.0.json");
/// The `forms` 1.0 catalog as an A2UI 1.0 catalog document, derived from the 1.0 basic catalog.
const FORMS_DOCUMENT_V1_0: &str = include_str!("catalogs/forms-1.0.a2ui-1.0.json");
/// The A2UI 1.0 common types and catalog meta-schema, for checking 1.0 catalog documents.
#[cfg(test)]
pub(crate) const COMMON_TYPES_V1_0: &str = include_str!("catalogs/common_types-1.0.json");
#[cfg(test)]
pub(crate) const CATALOG_DEFINITION_V1_0: &str =
    include_str!("catalogs/catalog_definition-1.0.json");
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
    instructions: Option<String>,
    document: Value,
    components: BTreeMap<String, jsonschema::Validator>,
    extensions: BTreeMap<String, Arc<dyn ExtensionComponent>>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("id", &self.id)
            .field("components", &self.components.keys().collect::<Vec<_>>())
            .field("extensions", &self.extensions.keys().collect::<Vec<_>>())
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

    /// An extension catalog: `forms` 1.0 and `spec`'s components, under `spec`'s id (spec 3.3
    /// §6.1 `genui`, extension catalogs). Keep it in a `static`
    /// ([`LazyLock`](std::sync::LazyLock)) and register it with the toolset that draws in it
    /// ([`DrawToolset::new`](super::DrawToolset::new)), the engine that shows it
    /// (`SurfaceEngineBuilder::with_catalog`) and, where set, `GenuiAgUi::with_catalog`.
    ///
    /// # Errors
    ///
    /// Refuses a catalog whose id is `forms`', that adds no component, or one of whose
    /// components reuses a name of `forms` or of what `forms` leaves out (`Image`, `Icon`,
    /// `Modal`, `Video`, `AudioPlayer`), repeats a name, or declares `$defs`, `functions`,
    /// `action`, `event`, `checks` or a URI format in its schema; and one whose schemas do
    /// not compile.
    pub fn extend(spec: ExtensionSpec) -> Result<Catalog, CatalogError> {
        let refuse = |detail: String| CatalogError { catalog: spec.id.to_string(), detail };
        if spec.id == FORMS_CATALOG_ID {
            return Err(refuse("its id is the forms catalog's".to_string()));
        }
        if spec.id.trim().is_empty()
            || spec.name.trim().is_empty()
            || spec.version.trim().is_empty()
        {
            return Err(refuse("it needs an id, a name and a version".to_string()));
        }
        if spec.components.is_empty() {
            return Err(refuse("it adds no components".to_string()));
        }
        let forms = Catalog::forms();
        let mut document = forms.document.clone();
        document["$id"] = json!(spec.id);
        document["catalogId"] = json!(spec.id);
        document["title"] = json!(format!("{} {}", spec.name, spec.version));
        document["description"] = json!(format!(
            "The forms 1.0 catalog and the components of {} {}",
            spec.name, spec.version
        ));
        let mut extensions: BTreeMap<String, Arc<dyn ExtensionComponent>> = BTreeMap::new();
        for component in &spec.components {
            let name = component.name();
            if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
                return Err(refuse(format!(
                    "'{name}' is not a component name of letters and digits"
                )));
            }
            if forms.has_component(name) {
                return Err(refuse(format!("{name} is a forms component")));
            }
            if LEFT_OUT.contains(&name) {
                return Err(refuse(format!("{name} is one of the components forms leaves out")));
            }
            if extensions.contains_key(name) {
                return Err(refuse(format!("{name} is added twice")));
            }
            let schema = component.schema();
            if let Some(why) = forbidden(&schema) {
                return Err(refuse(format!("{name}'s schema {why}")));
            }
            document["components"][name] = schema;
            if let Some(any) = document["$defs"]["anyComponent"]["oneOf"].as_array_mut() {
                any.push(json!({ "$ref": format!("#/components/{name}") }));
            }
            extensions.insert(name.to_string(), Arc::clone(component));
        }
        let mut catalog = Catalog::compile_document(spec.id, spec.name, spec.version, document)
            .map_err(refuse)?;
        catalog.extensions = extensions;
        Ok(catalog)
    }

    fn compile(
        id: &'static str,
        name: &'static str,
        version: &'static str,
        document: &str,
    ) -> Result<Self, String> {
        let document: Value = serde_json::from_str(document).map_err(|error| error.to_string())?;
        Self::compile_document(id, name, version, document)
    }

    fn compile_document(
        id: &'static str,
        name: &'static str,
        version: &'static str,
        document: Value,
    ) -> Result<Self, String> {
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
        Ok(Self {
            id,
            name,
            version,
            instructions: None,
            document,
            components,
            extensions: BTreeMap::new(),
        })
    }

    /// The catalog's id.
    pub fn id(&self) -> &str {
        self.id
    }

    /// The catalog's name and version as the developer panel shows them, `forms 1.0`.
    pub fn label(&self) -> String {
        format!("{} {}", self.name, self.version)
    }

    /// The catalog's JSON Schema document, in the internal (A2UI v0.9.1) form: `forms` and
    /// every component an extension adds, under the catalog's id.
    pub fn document(&self) -> &Value {
        &self.document
    }

    /// Design guidance for a model drawing in this catalog, carried in its A2UI 1.0 document
    /// (`instructions`) after each extension component's usage line.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// The catalog as a document of A2UI `version` (spec 3.3 §6.1 `genui`). For v0.9.1, the
    /// internal document. For 1.0: `forms` as an A2UI 1.0 catalog (derived from the 1.0 basic
    /// catalog); an extension catalog as a 1.0 catalog of only the components it adds, which a
    /// surface mixes with `forms` (each one names its catalog with `@catalogId`), with
    /// `protocolVersion`, `instructions` and each component's `allowedParents` and
    /// `allowedChildren`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_surface::genui::{A2uiVersion, Catalog, FORMS_CATALOG_ID};
    ///
    /// let forms = Catalog::forms().document_for(A2uiVersion::V1_0);
    /// assert_eq!(forms["protocolVersion"], "1.0");
    /// assert_eq!(forms["catalogId"], FORMS_CATALOG_ID);
    /// assert!(forms["components"]["TextField"].is_object());
    /// ```
    pub fn document_for(&self, version: super::wire::A2uiVersion) -> Value {
        if version == super::wire::A2uiVersion::V0_9_1 {
            return self.document.clone();
        }
        if !self.is_extension() {
            return serde_json::from_str(FORMS_DOCUMENT_V1_0).unwrap_or_default();
        }
        let mut components = Map::new();
        for (name, extension) in &self.extensions {
            let mut schema = schema_v1_0(&extension.schema());
            if let Value::Object(map) = &mut schema {
                let parents = extension.allowed_parents();
                if !parents.is_empty() {
                    map.insert("allowedParents".to_string(), json!(parents));
                }
                let children = extension.allowed_children();
                if !children.is_empty() {
                    map.insert("allowedChildren".to_string(), json!(children));
                }
            }
            components.insert(name.clone(), schema);
        }
        let mut instructions: Vec<String> = self.usage().into_iter().map(str::to_string).collect();
        if let Some(extra) = &self.instructions {
            instructions.push(extra.clone());
        }
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": self.id,
            "protocolVersion": "1.0",
            "title": self.label(),
            "description": format!(
                "The components {} adds to {FORMS_CATALOG_ID}, which a surface mixes with it",
                self.label()
            ),
            "catalogId": self.id,
            "instructions": instructions.join("\n"),
            "components": components,
        })
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

    /// The components an extension catalog adds to `forms`; none for `forms` itself.
    pub fn extensions(&self) -> impl Iterator<Item = &str> {
        self.extensions.keys().map(String::as_str)
    }

    /// Whether it is an extension catalog ([`Catalog::extend`]).
    pub fn is_extension(&self) -> bool {
        !self.extensions.is_empty()
    }

    /// The extension component `component`, if this catalog adds it.
    pub(crate) fn extension(&self, component: &str) -> Option<&dyn ExtensionComponent> {
        self.extensions.get(component).map(|extension| extension.as_ref())
    }

    /// What the render tools tell the model about the catalog's extension components, one
    /// line each.
    pub(crate) fn usage(&self) -> Vec<&'static str> {
        self.extensions.values().map(|extension| extension.usage()).collect()
    }

    /// Checks `value` against the schema of its component type, `component`: `None` when it
    /// conforms, else where and why it does not.
    pub(crate) fn schema_error(&self, component: &str, value: &Value) -> Option<String> {
        let validator = self.components.get(component)?;
        validator.iter_errors(value).next().map(|error| {
            let error = error.to_owned();
            let error = specific(&error);
            let at = error.instance_path().as_str();
            let why = shortened(&error.to_string());
            if at.is_empty() { why } else { format!("{at}: {why}") }
        })
    }
}

/// An extension component's schema as an A2UI 1.0 catalog component: references to the
/// v0.9 common types become the relative `common_types.json`, the component envelope (`id`,
/// `ComponentCommon`, `CatalogComponentCommon`) is left to the 1.0 envelope, which composes it,
/// and so is closing the object (`additionalProperties: false`; the envelope's
/// `unevaluatedProperties` closes it with its `@` keys).
fn schema_v1_0(schema: &Value) -> Value {
    fn refs(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| match (key.as_str(), value) {
                        ("$ref", Value::String(target)) => {
                            let relative = target
                                .split_once("common_types.json#")
                                .map(|(_, pointer)| format!("common_types.json#{pointer}"))
                                .unwrap_or_else(|| target.clone());
                            (key.clone(), Value::String(relative))
                        }
                        _ => (key.clone(), refs(value)),
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(refs).collect()),
            other => other.clone(),
        }
    }
    let mut out = refs(schema);
    if let Value::Object(map) = &mut out {
        if let Some(Value::Object(properties)) = map.get_mut("properties") {
            properties.remove("id");
        }
        if let Some(Value::Array(required)) = map.get_mut("required") {
            required.retain(|name| name != "id");
        }
        if map.get("additionalProperties") == Some(&Value::Bool(false)) {
            map.remove("additionalProperties");
        }
        if let Some(Value::Array(all)) = map.get_mut("allOf") {
            all.retain(|part| {
                let target = part.get("$ref").and_then(Value::as_str).unwrap_or_default();
                !target.ends_with("/ComponentCommon")
                    && !target.ends_with("/CatalogComponentCommon")
            });
            if all.is_empty() {
                map.remove("allOf");
            }
        }
    }
    out
}

/// The most specific cause of `error`. A `oneOf` or `anyOf` that no branch matched, such as a
/// property that is a binding or a literal list, names the deepest failure among its branches,
/// so the agent reads which value broke which rule rather than the whole value.
fn specific<'e>(
    error: &'e jsonschema::ValidationError<'static>,
) -> &'e jsonschema::ValidationError<'static> {
    let context = match error.kind() {
        jsonschema::error::ValidationErrorKind::OneOfNotValid { context }
        | jsonschema::error::ValidationErrorKind::AnyOf { context } => context,
        _ => return error,
    };
    let deepest = context
        .iter()
        .flatten()
        .map(specific)
        .max_by_key(|cause| cause.instance_path().as_str().matches('/').count());
    match deepest {
        Some(cause)
            if cause.instance_path().as_str().len() > error.instance_path().as_str().len() =>
        {
            cause
        }
        _ => error,
    }
}

/// The most of a schema error's text a reason carries; a value it quotes can be long.
const MAX_SCHEMA_ERROR_CHARS: usize = 300;

fn shortened(text: &str) -> String {
    if text.chars().count() <= MAX_SCHEMA_ERROR_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_SCHEMA_ERROR_CHARS).collect();
    format!("{kept}…")
}

/// What an extension component's schema declares that a component that only shows may not.
fn forbidden(schema: &Value) -> Option<String> {
    match schema {
        Value::Object(map) => {
            for key in ["$defs", "definitions", "functions"] {
                if map.contains_key(key) {
                    return Some(format!("declares {key}"));
                }
            }
            if let Some(properties) = map.get("properties").and_then(Value::as_object)
                && let Some(property) =
                    FORBIDDEN_PROPERTIES.iter().find(|name| properties.contains_key(**name))
            {
                return Some(format!("declares the property '{property}'"));
            }
            if let Some(format) = map.get("format").and_then(Value::as_str)
                && FORBIDDEN_FORMATS.contains(&format)
            {
                return Some(format!("declares the string format '{format}'"));
            }
            map.values().find_map(forbidden)
        }
        Value::Array(items) => items.iter().find_map(forbidden),
        _ => None,
    }
}

/// The catalogs a home shows, by id (spec 3.3 §6.1 `genui`): `forms` 1.0 always, and every
/// extension catalog a host registers. A drawing is checked again against its own catalog;
/// one in a catalog that is not here is refused ("this home does not render the catalog …").
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{Catalogs, FORMS_CATALOG_ID};
///
/// let catalogs = Catalogs::forms();
/// assert!(catalogs.get(FORMS_CATALOG_ID).is_some());
/// assert!(catalogs.get("https://example.com/other.json").is_none());
/// ```
#[derive(Clone)]
pub struct Catalogs(Arc<Vec<&'static Catalog>>);

impl Catalogs {
    /// `forms` 1.0 alone.
    pub fn forms() -> Self {
        Self(Arc::new(vec![Catalog::forms()]))
    }

    /// These catalogs and `catalog`. A catalog whose id is here already is not added again.
    #[must_use]
    pub fn with(self, catalog: &'static Catalog) -> Self {
        if self.get(catalog.id()).is_some() {
            return self;
        }
        let mut catalogs = (*self.0).clone();
        catalogs.push(catalog);
        Self(Arc::new(catalogs))
    }

    /// The catalog whose id is `id`.
    pub fn get(&self, id: &str) -> Option<&'static Catalog> {
        self.0.iter().copied().find(|catalog| catalog.id() == id)
    }

    /// Every catalog, `forms` first.
    pub fn iter(&self) -> impl Iterator<Item = &'static Catalog> + '_ {
        self.0.iter().copied()
    }
}

impl Default for Catalogs {
    fn default() -> Self {
        Self::forms()
    }
}

impl std::fmt::Debug for Catalogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|catalog| catalog.id())).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genui::A2uiVersion;

    /// A validator of A2UI 1.0 catalog documents: the pinned 1.0 meta-schema, with the 1.0
    /// common types it and the documents refer to.
    fn v1_0_meta() -> jsonschema::Validator {
        let meta: Value = serde_json::from_str(CATALOG_DEFINITION_V1_0).unwrap();
        let common: Value = serde_json::from_str(COMMON_TYPES_V1_0).unwrap();
        // The common types refer to `catalog.json` for the components a surface may hold;
        // checking a catalog document needs only that it exists.
        let catalog = json!({ "$defs": { "anyComponent": {}, "anyFunction": {} } });
        let registry = jsonschema::Registry::new()
            .add("https://a2ui.org/specification/v1_0/common_types.json", common)
            .and_then(|registry| {
                registry.add("https://a2ui.org/specification/v1_0/catalog.json", catalog)
            })
            .and_then(jsonschema::RegistryBuilder::prepare)
            .unwrap();
        jsonschema::options().with_registry(&registry).build(&meta).unwrap()
    }

    struct Gauge;

    impl ExtensionComponent for Gauge {
        fn name(&self) -> &'static str {
            "Gauge"
        }
        fn schema(&self) -> Value {
            json!({
                "type": "object",
                "allOf": [{ "$ref": "#/$defs/CatalogComponentCommon" }],
                "properties": {
                    "id": { "$ref": "https://a2ui.org/specification/v0_9/common_types.json#/$defs/ComponentId" },
                    "component": { "const": "Gauge" },
                    "label": { "$ref": "https://a2ui.org/specification/v0_9/common_types.json#/$defs/DynamicString" },
                    "value": { "type": "number" }
                },
                "required": ["id", "component", "label", "value"],
                "additionalProperties": false
            })
        }
        fn usage(&self) -> &'static str {
            "Gauge: {label, value} one number on a dial"
        }
        fn allowed_parents(&self) -> &'static [&'static str] {
            &["Card", "Column"]
        }
        fn home(&self, _gauge: &ResolvedComponent<'_>) -> Result<HomeNode, String> {
            Ok(HomeNode::text("gauge"))
        }
    }

    #[test]
    fn catalogs_are_published_as_a2ui_1_0_documents_that_pass_the_1_0_meta_schema() {
        let meta = v1_0_meta();
        let forms = Catalog::forms().document_for(A2uiVersion::V1_0);
        if let Some(error) = meta.iter_errors(&forms).next() {
            panic!("forms 1.0 document: {error} at {}", error.instance_path());
        }
        assert_eq!(forms["functions"].as_object().unwrap().len(), 10);
        assert!(forms["components"].get("Image").is_none());
        assert!(forms["functions"].get("openUrl").is_none());
        assert_eq!(forms["components"]["Text"]["properties"]["variant"]["enum"][2], "h3");
        assert!(!forms["components"]["TextField"].to_string().contains("obscured"));

        let gauges = Catalog::extend(ExtensionSpec {
            id: "https://example.com/a2ui/catalogs/gauges/1.0/catalog.json",
            name: "gauges",
            version: "1.0",
            components: vec![Arc::new(Gauge) as Arc<dyn ExtensionComponent>],
        })
        .unwrap()
        .with_instructions("Use a gauge for one number against a known range.");
        let document = gauges.document_for(A2uiVersion::V1_0);
        if let Some(error) = meta.iter_errors(&document).next() {
            panic!("extension 1.0 document: {error} at {}", error.instance_path());
        }
        assert_eq!(document["protocolVersion"], "1.0");
        let gauge = &document["components"]["Gauge"];
        assert!(document["components"].get("Text").is_none(), "only what the extension adds");
        assert!(gauge["properties"].get("id").is_none());
        assert_eq!(gauge["required"], json!(["component", "label", "value"]));
        assert!(gauge.get("additionalProperties").is_none());
        assert_eq!(gauge["properties"]["label"]["$ref"], "common_types.json#/$defs/DynamicString");
        assert!(gauge.get("allOf").is_none(), "the envelope composes ComponentCommon");
        assert_eq!(gauge["allowedParents"], json!(["Card", "Column"]));
        assert!(document["instructions"].as_str().unwrap().contains("Gauge: {label, value}"));
        assert!(document["instructions"].as_str().unwrap().contains("known range"));
        // v0.9.1 keeps the superset document.
        assert!(gauges.document_for(A2uiVersion::V0_9_1)["components"]["Text"].is_object());
    }

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

    #[test]
    fn a_value_that_may_be_a_binding_or_a_list_names_what_broke_in_the_list() {
        let forms = Catalog::forms();
        let picker = json!({
            "id": "c", "component": "ChoicePicker", "label": "Dock",
            "options": [{ "label": "Dock 1", "value": "dock-1" }],
            "value": ["dock-1", 5]
        });
        let error = forms.schema_error("ChoicePicker", &picker).expect("a number among strings");
        assert!(error.starts_with("/value/1: "), "{error}");
        assert!(!error.contains("oneOf"), "{error}");
    }

    #[test]
    fn a_schema_error_quoting_a_long_value_is_shortened() {
        let forms = Catalog::forms();
        let long = json!({ "id": "t", "component": "Text", "text": vec![1; 400] });
        let error = forms.schema_error("Text", &long).expect("a list is not text");
        assert!(error.chars().count() <= MAX_SCHEMA_ERROR_CHARS + "/text: …".len(), "{error}");
        assert!(error.ends_with('…'), "{error}");
    }
}
