//! Extension catalogs (spec 3.3 §6.1 `genui`, §7.4 rule 5): another crate's components on
//! top of `forms` 1.0, which only show, whose every string and bound value is checked, whose
//! home view shows every child once in forms components and charts, and which a home, the
//! send of a form and AG-UI accept only where the catalog is registered.

#![cfg(feature = "genui")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalService, Content, DRAW_TOOL_NAMES, Event,
    FunctionResponseData, InMemoryApprovalStore, Part, RequestedCall, SessionId,
    StoreApprovalService, UserId,
};
use adk_harness::{Harness, HarnessAgent, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::rest::controllers::AgUiExtension;
use adk_server::rest::controllers::runtime::AgUiInputMessage;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::genui::{
    A2uiBody, A2uiMessage, Catalog, CatalogError, Catalogs, ChartAxis, ChartKind, ChartSeries,
    ChildSlot, DRAWN_STATE_KEY, DrawRefusal, DrawToolset, DrawnChart, DrawnEnvelope, DrawnKind,
    DrawnNode, DrawnRecord, ExtensionComponent, ExtensionSpec, FORMS_CATALOG_ID, GenuiAgUi,
    GenuiWiring, HomeNode, MAX_DEPTH, RefusalKind, ResolvedComponent, SurfaceDraft, Validator,
    capabilities_with,
};
use adk_surface::{
    Caller, Command, CommandOutcome, LocalSource, Origin, Source, SurfaceEngine, SurfaceError,
    SurfaceKind, ThreadItem,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Map, Value, json};

const EXT_ID: &str = "https://example.com/a2ui/catalogs/insight/1.0/catalog.json";
const COMMON: &str = "https://a2ui.org/specification/v0_9/common_types.json#/$defs";

/// A fixture component: a name, a schema's own properties, its slots and its home view.
struct Fixture {
    name: &'static str,
    properties: fn() -> Value,
    slots: &'static [ChildSlot],
    home: fn(&ResolvedComponent<'_>) -> Result<HomeNode, String>,
}

impl ExtensionComponent for Fixture {
    fn name(&self) -> &'static str {
        self.name
    }

    fn schema(&self) -> Value {
        let mut properties = (self.properties)();
        properties["id"] = json!({ "$ref": format!("{COMMON}/ComponentId") });
        properties["component"] = json!({ "const": self.name });
        json!({
            "type": "object",
            "properties": properties,
            "required": ["id", "component"],
            "additionalProperties": false
        })
    }

    fn children(&self) -> &'static [ChildSlot] {
        self.slots
    }

    fn usage(&self) -> &'static str {
        match self.name {
            "Metric" => "Metric: {label, value, unit} — one number with its label.",
            "Trend" => "Trend: {label, values} — a small line of numbers.",
            _ => "A fixture component.",
        }
    }

    fn home(&self, component: &ResolvedComponent<'_>) -> Result<HomeNode, String> {
        (self.home)(component)
    }
}

fn dynamic_string() -> Value {
    json!({ "$ref": format!("{COMMON}/DynamicString") })
}

fn numbers(value: Option<&Value>) -> Result<Vec<serde_json::Number>, String> {
    value
        .and_then(Value::as_array)
        .ok_or("values is not a list")?
        .iter()
        .map(|value| {
            value.as_number().cloned().ok_or_else(|| "a value is not a number".to_string())
        })
        .collect()
}

fn children(component: &ResolvedComponent<'_>) -> Vec<HomeNode> {
    (0..component.children).map(HomeNode::Child).collect()
}

static EXT: LazyLock<Catalog> = LazyLock::new(|| {
    let fixtures = vec![
        Fixture {
            name: "Metric",
            properties: || {
                json!({
                    "label": dynamic_string(),
                    "value": { "$ref": format!("{COMMON}/DynamicValue") },
                    "unit": dynamic_string()
                })
            },
            slots: &[],
            home: |metric| {
                let value = match metric.get("value") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                let unit = metric.str("unit").map(|unit| format!(" {unit}")).unwrap_or_default();
                Ok(HomeNode::Row(vec![
                    HomeNode::caption(metric.str("label").unwrap_or_default()),
                    HomeNode::heading(3, format!("{value}{unit}")),
                ]))
            },
        },
        Fixture {
            name: "Trend",
            properties: || json!({ "label": dynamic_string(), "values": {} }),
            slots: &[],
            home: |trend| {
                let y = numbers(trend.get("values"))?;
                let x = (0..y.len()).map(serde_json::Number::from).collect();
                Ok(HomeNode::Chart(DrawnChart {
                    kind: ChartKind::Sparkline,
                    x: ChartAxis::Number,
                    x_label: None,
                    y_label: None,
                    ticks: Vec::new(),
                    series: vec![ChartSeries {
                        name: trend.str("label").unwrap_or_default().to_string(),
                        x,
                        y,
                    }],
                    summary: String::new(),
                }))
            },
        },
        Fixture {
            name: "Lines",
            properties: || json!({ "series": {} }),
            slots: &[],
            home: |lines| {
                let series = lines
                    .get("series")
                    .and_then(Value::as_array)
                    .ok_or("series is not a list")?
                    .iter()
                    .map(|series| {
                        let y = numbers(series.get("values"))?;
                        let x = (0..y.len()).map(serde_json::Number::from).collect();
                        let name = series.get("name").and_then(Value::as_str).unwrap_or_default();
                        Ok(ChartSeries { name: name.to_string(), x, y })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(HomeNode::Chart(DrawnChart {
                    kind: ChartKind::Line,
                    x: ChartAxis::Number,
                    x_label: None,
                    y_label: None,
                    ticks: Vec::new(),
                    series,
                    summary: String::new(),
                }))
            },
        },
        Fixture {
            name: "Share",
            properties: || json!({ "slices": {} }),
            slots: &[],
            home: |share| {
                let slices = share.get("slices").and_then(Value::as_array).ok_or("no slices")?;
                let labels = slices
                    .iter()
                    .map(|slice| slice["name"].as_str().unwrap_or_default().to_string())
                    .collect();
                let y = slices
                    .iter()
                    .map(|slice| slice["value"].as_number().cloned().ok_or("not a number"))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(HomeNode::Chart(DrawnChart {
                    kind: ChartKind::Pie,
                    x: ChartAxis::Category { labels },
                    x_label: None,
                    y_label: None,
                    ticks: Vec::new(),
                    series: vec![ChartSeries { name: "Share".into(), x: Vec::new(), y }],
                    summary: String::new(),
                }))
            },
        },
        Fixture {
            name: "Pages",
            properties: || {
                json!({
                    "title": dynamic_string(),
                    "pages": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": dynamic_string(),
                                "child": { "$ref": format!("{COMMON}/ComponentId") }
                            },
                            "required": ["title", "child"],
                            "additionalProperties": false
                        }
                    }
                })
            },
            slots: &[ChildSlot::EachOf { array: "pages", child: "child" }],
            home: |pages| {
                let titles: Vec<String> = pages
                    .get("pages")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|page| page["title"].as_str().unwrap_or_default().to_string())
                    .collect();
                Ok(HomeNode::Tabs(
                    titles.into_iter().enumerate().map(|(i, t)| (t, HomeNode::Child(i))).collect(),
                ))
            },
        },
        Fixture {
            name: "Panel",
            properties: || {
                json!({
                    "title": dynamic_string(),
                    "children": { "type": "array", "items": { "$ref": format!("{COMMON}/ComponentId") } }
                })
            },
            slots: &[ChildSlot::Many("children")],
            home: |panel| {
                let mut nodes = vec![HomeNode::heading(3, panel.str("title").unwrap_or_default())];
                nodes.extend(children(panel));
                Ok(HomeNode::Card(nodes))
            },
        },
        Fixture {
            name: "Sneaky",
            properties: || json!({ "note": dynamic_string() }),
            slots: &[],
            home: |sneaky| {
                Ok(HomeNode::Column(vec![
                    HomeNode::text(sneaky.str("note").unwrap_or_default()),
                    HomeNode::text("Allow once"),
                ]))
            },
        },
        Fixture {
            name: "Dropper",
            properties: || json!({ "children": { "type": "array", "items": { "type": "string" } } }),
            slots: &[ChildSlot::Many("children")],
            home: |_| Ok(HomeNode::Column(vec![HomeNode::text("Nothing here")])),
        },
        Fixture {
            name: "Twice",
            properties: || json!({ "child": { "type": "string" } }),
            slots: &[ChildSlot::One("child")],
            home: |_| Ok(HomeNode::Column(vec![HomeNode::Child(0), HomeNode::Child(0)])),
        },
    ];
    Catalog::extend(ExtensionSpec {
        id: EXT_ID,
        name: "insight",
        version: "1.0",
        components: fixtures
            .into_iter()
            .map(|fixture| Arc::new(fixture) as Arc<dyn ExtensionComponent>)
            .collect(),
    })
    .expect("the fixture catalog compiles")
});

fn ext() -> &'static Catalog {
    &EXT
}

fn draft(kind: DrawnKind, components: Vec<Value>, model: Value) -> SurfaceDraft {
    SurfaceDraft {
        surface_id: "insight".into(),
        kind,
        title: None,
        components,
        data_model: model,
        actions: Vec::new(),
    }
}

fn screen(components: Vec<Value>, model: Value) -> SurfaceDraft {
    draft(DrawnKind::Screen, components, model)
}

fn check(draft: &SurfaceDraft) -> Result<(), DrawRefusal> {
    Validator::new(ext()).validate(draft).map(|_| ())
}

fn refused(draft: &SurfaceDraft) -> DrawRefusal {
    match Validator::new(ext()).validate(draft) {
        Ok(_) => panic!("expected a refusal for {:#}", json!(draft.components)),
        Err(refusal) => refusal,
    }
}

fn metric(label: Value, value: Value) -> Value {
    json!({ "id": "root", "component": "Metric", "label": label, "value": value })
}

/// A catalog that could loosen `forms` is refused when it is built.
#[test]
fn an_extension_catalog_that_could_loosen_forms_is_refused() {
    let one = |name: &'static str, properties: fn() -> Value| -> Arc<dyn ExtensionComponent> {
        Arc::new(Fixture { name, properties, slots: &[], home: |_| Ok(HomeNode::Divider) })
    };
    let spec = |id: &'static str, components: Vec<Arc<dyn ExtensionComponent>>| ExtensionSpec {
        id,
        name: "x",
        version: "1.0",
        components,
    };
    let detail = |result: Result<Catalog, CatalogError>| result.unwrap_err().detail;
    let other = "https://example.com/x.json";

    assert!(
        detail(Catalog::extend(spec(FORMS_CATALOG_ID, vec![one("Gauge", || json!({}))])))
            .contains("forms catalog's")
    );
    assert!(detail(Catalog::extend(spec(other, Vec::new()))).contains("adds no components"));
    assert!(
        detail(Catalog::extend(spec(other, vec![one("Text", || json!({}))])))
            .contains("is a forms component")
    );
    assert!(
        detail(Catalog::extend(spec(other, vec![one("Image", || json!({}))])))
            .contains("leaves out")
    );
    assert!(
        detail(Catalog::extend(spec(
            other,
            vec![one("Gauge", || json!({})), one("Gauge", || json!({}))]
        )))
        .contains("added twice")
    );
    assert!(
        detail(Catalog::extend(spec(other, vec![one("Gauge", || json!({ "action": {} }))])))
            .contains("'action'")
    );
    assert!(
        detail(Catalog::extend(spec(other, vec![one("Gauge", || json!({ "checks": {} }))])))
            .contains("'checks'")
    );
    assert!(
        detail(Catalog::extend(spec(
            other,
            vec![one("Gauge", || json!({ "href": { "type": "string", "format": "uri" } }))]
        )))
        .contains("format 'uri'")
    );
    let with_defs: Arc<dyn ExtensionComponent> = Arc::new(DefsComponent);
    assert!(detail(Catalog::extend(spec(other, vec![with_defs]))).contains("$defs"));
}

struct DefsComponent;

impl ExtensionComponent for DefsComponent {
    fn name(&self) -> &'static str {
        "Gauge"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "$defs": { "x": {} } })
    }
    fn usage(&self) -> &'static str {
        "Gauge"
    }
    fn home(&self, _: &ResolvedComponent<'_>) -> Result<HomeNode, String> {
        Ok(HomeNode::Divider)
    }
}

/// An extension catalog is `forms` 1.0 and more, under its own id; a registry keeps one
/// catalog per id.
#[test]
fn an_extension_catalog_is_forms_and_more() {
    let forms = Catalog::forms();
    assert!(forms.components().all(|name| ext().has_component(name)));
    assert!(ext().has_component("Metric") && ext().has_component("Pages"));
    assert!(!ext().has_component("Image"));
    assert_eq!(ext().id(), EXT_ID);
    assert_eq!(ext().label(), "insight 1.0");
    assert_eq!(ext().document()["catalogId"], EXT_ID);
    assert!(ext().is_extension() && !forms.is_extension());
    assert!(ext().extensions().any(|name| name == "Trend"));
    assert_eq!(forms.extensions().count(), 0);

    let catalogs = Catalogs::forms().with(ext()).with(ext());
    assert_eq!(catalogs.iter().count(), 2);
    assert_eq!(catalogs.iter().next().unwrap().id(), FORMS_CATALOG_ID);
    assert!(catalogs.get(EXT_ID).is_some());
    let claims = capabilities_with(GenuiWiring::default(), &catalogs);
    assert_eq!(claims.catalogs.len(), 2);
    assert_eq!(claims.catalogs[1].label, "insight 1.0");
    assert!(claims.catalogs[1].components.contains(&"Metric".to_string()));
    assert_eq!(claims.catalogs[1].left_out, ["Image", "Icon", "Modal", "Video", "AudioPlayer"]);
}

/// Extension components whose every string passes are shown, bound data and all, and a
/// form inside an extension container keeps its fields and actions.
#[test]
fn extension_components_whose_every_string_passes_are_shown() {
    let validated = Validator::new(ext())
        .validate(&screen(
            vec![
                json!({
                    "id": "root", "component": "Pages", "title": "Overview",
                    "pages": [{ "title": "Now", "child": "now" }, { "title": "Trend", "child": "trend" }]
                }),
                json!({ "id": "now", "component": "Metric", "label": "Open incidents", "value": { "path": "/open" } }),
                json!({ "id": "trend", "component": "Trend", "label": "Last week", "values": { "path": "/week" } }),
            ],
            json!({ "open": 3, "week": [1, 4, 2, 6, 3, 5, 7] }),
        ))
        .unwrap();
    let shown: Vec<&str> = validated.texts.iter().map(|text| text.text.as_str()).collect();
    assert!(shown.contains(&"Open incidents") && shown.contains(&"Last week"), "{shown:?}");

    // Forms checks one level down: a form inside a page.
    let mut form = screen(
        vec![
            json!({ "id": "root", "component": "Pages", "pages": [{ "title": "Details", "child": "details" }] }),
            json!({ "id": "details", "component": "Column", "children": ["to", "send"] }),
            json!({ "id": "to", "component": "TextField", "label": "To", "value": { "path": "/to" } }),
            json!({ "id": "send", "component": "Button", "child": "send-label", "action": { "event": { "name": "send" } } }),
            json!({ "id": "send-label", "component": "Text", "text": "Send" }),
        ],
        json!({ "to": "" }),
    );
    form.actions = vec![
        serde_json::from_value(json!({
            "id": "send", "label": "Send", "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "invoice": "INV-10" }
        }))
        .unwrap(),
    ];
    assert!(check(&form).is_ok());
}

/// A decision's words are refused wherever an extension component carries them: a literal,
/// a bound string, a bound list, a bound object's key, any `pluralize` variant, and a
/// constant its own home view adds.
#[test]
fn a_decisions_words_are_refused_wherever_an_extension_carries_them() {
    let looks = |draft: SurfaceDraft| {
        let refusal = refused(&draft);
        assert_eq!(refusal.kind, RefusalKind::LooksLikeDecision, "{}", refusal.technical());
    };
    looks(screen(vec![metric(json!("Allow once"), json!(1))], json!({})));
    looks(screen(
        vec![metric(json!({ "path": "/label" }), json!(1))],
        json!({ "label": "Approve" }),
    ));
    looks(screen(
        vec![metric(json!("Total"), json!({ "path": "/v" }))],
        json!({ "v": ["Approved"] }),
    ));
    looks(screen(
        vec![metric(json!("Total"), json!({ "path": "/v" }))],
        json!({ "v": { "Deny": 1 } }),
    ));
    looks(screen(
        vec![metric(
            json!({ "call": "pluralize", "args": { "value": 1, "one": "1 item", "other": "Stop all" } }),
            json!(1),
        )],
        json!({}),
    ));
    looks(screen(
        vec![json!({ "id": "root", "component": "Sneaky", "note": "Looks fine" })],
        json!({}),
    ));
    // Each name passes alone; joined in the chart's summary they spell "Allow once".
    looks(screen(
        vec![json!({
            "id": "root", "component": "Lines",
            "series": [
                { "name": "Allow", "values": [1, 2] },
                { "name": "once", "values": [2, 3] },
                { "name": "more", "values": [3, 4] }
            ]
        })],
        json!({}),
    ));
}

/// A link is refused in an extension component, including one a `formatString` joins from
/// parts that are not links alone.
#[test]
fn links_are_refused_even_when_a_format_string_joins_them() {
    let refusal =
        refused(&screen(vec![metric(json!("see https://example.com"), json!(1))], json!({})));
    assert_eq!(refusal.kind, RefusalKind::CannotShow);
    let joined = refused(&screen(
        vec![metric(
            json!({ "call": "formatString", "args": { "value": "${/a}${/b}" } }),
            json!(1),
        )],
        json!({ "a": "https:", "b": "//example.com" }),
    ));
    assert_eq!(joined.kind, RefusalKind::CannotShow);
    assert!(joined.detail.contains("is a link"), "{}", joined.detail);
}

/// An extension component only shows: no event or action anywhere in it, and only the text
/// functions.
#[test]
fn an_extension_component_only_shows() {
    let event = refused(&screen(
        vec![
            json!({ "id": "root", "component": "Metric", "label": "x", "value": 1, "event": { "name": "go" } }),
        ],
        json!({}),
    ));
    assert!(event.detail.contains("carries 'event'"), "{}", event.technical());
    let nested =
        refused(&screen(vec![metric(json!({ "path": "/x", "action": {} }), json!(1))], json!({})));
    assert!(nested.detail.contains("carries 'action'"), "{}", nested.technical());
    let check = refused(&screen(
        vec![metric(json!({ "call": "required", "args": { "value": "x" } }), json!(1))],
        json!({}),
    ));
    assert!(check.detail.contains("makes text only"), "{}", check.technical());
}

/// A container keeps every forms check for what it holds: secrets, a card's buttons, loops
/// and depth.
#[test]
fn a_container_keeps_every_forms_check_for_what_it_holds() {
    let secret = refused(&screen(
        vec![
            json!({ "id": "root", "component": "Panel", "title": "Sign in", "children": ["pw"] }),
            json!({ "id": "pw", "component": "TextField", "label": "Password", "value": { "path": "/pw" } }),
        ],
        json!({}),
    ));
    assert_eq!(secret.kind, RefusalKind::AsksForSecret);

    let button = refused(&draft(
        DrawnKind::Card,
        vec![
            json!({ "id": "root", "component": "Panel", "title": "Status", "children": ["b"] }),
            json!({ "id": "b", "component": "Button", "child": "l", "action": { "event": { "name": "go" } } }),
            json!({ "id": "l", "component": "Text", "text": "Go" }),
        ],
        json!({}),
    ));
    assert!(button.detail.contains("no buttons"), "{}", button.technical());

    let looped = refused(&screen(
        vec![
            json!({ "id": "root", "component": "Panel", "title": "A", "children": ["inner"] }),
            json!({ "id": "inner", "component": "Panel", "title": "B", "children": ["root"] }),
        ],
        json!({}),
    ));
    assert!(looped.detail.contains("loops"), "{}", looped.technical());

    let mut deep = Vec::new();
    for level in 0..=MAX_DEPTH + 1 {
        let id = if level == 0 { "root".to_string() } else { format!("p{level}") };
        let child = format!("p{}", level + 1);
        let children = if level == MAX_DEPTH + 1 { json!([]) } else { json!([child]) };
        deep.push(
            json!({ "id": id, "component": "Panel", "title": "Level", "children": children }),
        );
    }
    let deep = refused(&screen(deep, json!({})));
    assert!(deep.detail.contains("deeper than"), "{}", deep.technical());
}

/// A home view shows every child its component holds, exactly once.
#[test]
fn a_home_view_shows_every_child_exactly_once() {
    let dropped = refused(&screen(
        vec![
            json!({ "id": "root", "component": "Dropper", "children": ["t"] }),
            json!({ "id": "t", "component": "Text", "text": "Hidden?" }),
        ],
        json!({}),
    ));
    assert!(dropped.detail.contains("leaves out child 0"), "{}", dropped.technical());
    let twice = refused(&screen(
        vec![
            json!({ "id": "root", "component": "Twice", "child": "t" }),
            json!({ "id": "t", "component": "Text", "text": "Once" }),
        ],
        json!({}),
    ));
    assert!(twice.detail.contains("twice"), "{}", twice.technical());
}

/// The data extension components bind is bounded across the drawing, so a chart in a list
/// template cannot multiply it unchecked.
#[test]
fn bound_data_is_bounded_across_list_templates() {
    let week: Vec<u32> = (0..400).collect();
    let rows: Vec<Value> = (0..400).map(|i| json!({ "n": i })).collect();
    let refusal = refused(&screen(
        vec![
            json!({ "id": "root", "component": "List", "children": { "componentId": "trend", "path": "/rows" } }),
            json!({ "id": "trend", "component": "Trend", "label": "Week", "values": { "path": "/week" } }),
        ],
        json!({ "rows": rows, "week": week }),
    ));
    assert!(refusal.detail.contains("resolve to more than"), "{}", refusal.technical());
}

/// A chart stays within its limits: values per series, pie slices and negative slices.
#[test]
fn a_chart_stays_within_its_limits() {
    let long: Vec<u32> = (0..600).collect();
    let refusal = refused(&screen(
        vec![json!({ "id": "root", "component": "Trend", "label": "Long", "values": long })],
        json!({}),
    ));
    assert!(refusal.detail.contains("at most 500"), "{}", refusal.technical());

    let pie = |slices: Value| {
        screen(vec![json!({ "id": "root", "component": "Share", "slices": slices })], json!({}))
    };
    let negative =
        refused(&pie(json!([{ "name": "A", "value": 3 }, { "name": "B", "value": -1 }])));
    assert!(negative.detail.contains("negative"), "{}", negative.technical());
    let many: Vec<Value> = (0..9).map(|i| json!({ "name": format!("S{i}"), "value": 1 })).collect();
    let crowded = refused(&pie(json!(many)));
    assert!(crowded.detail.contains("slices"), "{}", crowded.technical());
    assert!(check(&pie(json!([{ "name": "A", "value": 3 }, { "name": "B", "value": 1 }]))).is_ok());
}

/// The render tools draw in the extension catalog under their usual names and tell the
/// model about its components.
#[test]
fn the_render_tools_draw_in_the_extension_catalog_under_their_usual_names() {
    let toolset = DrawToolset::new(Validator::new(ext()));
    assert_eq!(toolset.catalog().id(), EXT_ID);
    let tools = toolset.tools();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, DRAW_TOOL_NAMES);
    let screen = tools.iter().find(|tool| tool.name() == "render_screen").unwrap();
    let description = screen.description();
    assert!(description.contains("insight 1.0"), "{description}");
    assert!(description.contains("Metric: {label, value, unit}"), "{description}");
    assert!(description.contains("the other components only show"), "{description}");
    let schema = screen.parameters_schema().unwrap();
    assert!(
        schema["properties"]["components"]["description"].as_str().unwrap().contains("insight 1.0"),
        "{schema:#}"
    );

    let forms = DrawToolset::forms();
    let forms_screen =
        forms.tools().into_iter().find(|tool| tool.name() == "render_screen").unwrap();
    assert!(forms_screen.description().starts_with(
        "Show the person a screen of A2UI v0.9.1 components from the forms catalog (Text,"
    ));
}

// --- A home, AG-UI and the send ---------------------------------------------------------------

const APP: &str = "home";

fn create_in(catalog: &str, surface: &str) -> A2uiMessage {
    let mut message = A2uiMessage::create_surface(surface);
    if let A2uiBody::CreateSurface(create) = &mut message.body {
        create.catalog_id = catalog.to_string();
    }
    message
}

fn envelope_in(catalog: &str, components: Vec<Value>, model: Value) -> DrawnEnvelope {
    DrawnEnvelope {
        surface_id: "insight".into(),
        version: 1,
        kind: DrawnKind::Screen,
        catalog_id: catalog.into(),
        title: None,
        messages: vec![
            create_in(catalog, "insight"),
            A2uiMessage::update_components("insight", components),
            A2uiMessage::update_data_model("insight", None, Some(model)),
        ],
        actions: Vec::new(),
        deleted: false,
    }
}

fn proven_result(envelope: &DrawnEnvelope) -> Event {
    let mut event = Event::new("inv-1");
    event.author = "finance".into();
    event.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new(
                "render_screen",
                json!({ "status": "shown", "drawn": envelope }),
            ),
            id: Some("call-1".into()),
            annotations: None,
        }],
    });
    let record = DrawnRecord {
        surface_id: envelope.surface_id.clone(),
        version: envelope.version,
        digest: envelope.digest(),
    };
    event.actions.state_delta.insert(DRAWN_STATE_KEY.into(), json!(record));
    event
}

fn insight_envelope() -> DrawnEnvelope {
    envelope_in(
        EXT_ID,
        vec![metric(json!("Open incidents"), json!({ "path": "/open" }))],
        json!({ "open": 3 }),
    )
}

async fn thread_items(register: bool) -> Vec<ThreadItem> {
    thread_items_of(&insight_envelope(), register).await
}

async fn thread_items_of(envelope: &DrawnEnvelope, register: bool) -> Vec<ThreadItem> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    sessions.append_event("s1", proven_result(envelope)).await.unwrap();
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
        .build()
        .unwrap();
    let mut builder = SurfaceEngine::builder(Arc::new(source))
        .utc_offset(chrono::FixedOffset::east_opt(0).unwrap());
    if register {
        builder = builder.with_catalog(ext());
    }
    let engine = builder.build();
    let caller = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
    engine.thread(&caller, "s1").await.unwrap().into_iter().flat_map(|entry| entry.items).collect()
}

/// A home shows a drawing only in a catalog it registers; one in another catalog is a
/// refusal that names it.
#[tokio::test]
async fn a_home_shows_a_drawing_only_in_a_catalog_it_registers() {
    let shown = thread_items(true).await;
    let surface = shown
        .iter()
        .find_map(|item| match item {
            ThreadItem::Drawn { surface } => Some(surface),
            _ => None,
        })
        .expect("the drawing is shown");
    assert_eq!(surface.catalog, "insight 1.0");

    let unregistered = thread_items(false).await;
    let refusal = unregistered
        .iter()
        .find_map(|item| match item {
            ThreadItem::DrawnRefused { refusal } => Some(refusal),
            _ => None,
        })
        .expect("a drawing in a catalog the home does not register is refused");
    assert_eq!(refusal.kind, RefusalKind::CannotShow);
    assert!(refusal.reason.contains("does not render the catalog"), "{}", refusal.reason);
}

/// AG-UI carries a drawing, and keeps one a client sends back, only in a catalog it
/// registers.
#[test]
fn ag_ui_carries_only_drawings_in_catalogs_it_registers() {
    let event = proven_result(&insight_envelope());
    assert!(GenuiAgUi::new().events_for(&event).is_empty());
    let extension = GenuiAgUi::new().with_catalog(ext());
    let events = extension.events_for(&event);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["content"]["catalogId"], EXT_ID);

    let echoed = |content: Value| AgUiInputMessage {
        id: Some("echo".into()),
        role: Some("activity".into()),
        activity_type: Some("a2ui-surface".into()),
        content: Some(content),
        ..AgUiInputMessage::default()
    };
    let content = events[0]["content"].clone();
    assert!(extension.keeps_activity(&echoed(content.clone())));
    assert!(
        !GenuiAgUi::new().keeps_activity(&echoed(content)),
        "an unregistered catalog is dropped"
    );
}

// The send of a form drawn in an extension catalog.

const SESSION: &str = "thread-1";

struct Sending {
    inner: LocalSource,
    busy: AtomicBool,
    asked: Mutex<Vec<RequestedCall>>,
}

#[async_trait]
impl Source for Sending {
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        self.inner.transcript(user, session).await
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        self.inner.requests(caller, session, open_only).await
    }

    async fn request(&self, id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        self.inner.request(id).await
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        self.inner.decide(id, expected_version, decision).await
    }

    async fn busy(&self, _user: &str, _session: &str) -> adk_surface::Result<bool> {
        Ok(self.busy.load(Ordering::SeqCst))
    }

    async fn request_call(
        &self,
        _caller: &Caller,
        _session: &str,
        _text: &str,
        call: RequestedCall,
    ) -> adk_surface::Result<()> {
        self.asked.lock().unwrap().push(call);
        Ok(())
    }
}

struct SendInvoice;

#[async_trait]
impl adk_core::Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends an invoice."
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        _args: Value,
    ) -> adk_core::Result<Value> {
        panic!("send_invoice never runs here");
    }
}

/// Finance draws, in the extension catalog, pages whose first page is a form.
async fn drawn_home(register: bool) -> (SurfaceEngine, Arc<Sending>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let screen = json!({
        "surfaceId": "invoice-10",
        "title": "Invoice INV-10",
        "components": [
            { "id": "root", "component": "Pages",
              "pages": [{ "title": "Details", "child": "details" }, { "title": "Amount", "child": "amount" }] },
            { "id": "details", "component": "Column", "children": ["to", "send"] },
            { "id": "to", "component": "TextField", "label": "To", "value": { "path": "/to" } },
            { "id": "send", "component": "Button", "child": "send-label", "action": { "event": { "name": "send" } } },
            { "id": "send-label", "component": "Text", "text": "Send" },
            { "id": "amount", "component": "Metric", "label": "Amount", "value": { "path": "/amount" }, "unit": "USD" }
        ],
        "dataModel": { "to": "", "amount": 4200 },
        "actions": [{
            "id": "send", "label": "Send", "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "invoice": "INV-10" }
        }]
    });
    let agent = HarnessAgent::builder("finance")
        .model(Arc::new(ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("render_screen", screen),
                ScriptedResponse::text("drawn"),
            ],
        )))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .without_compaction()
        .toolset(Arc::new(DrawToolset::new(Validator::new(ext()))))
        .tool(Arc::new(SendInvoice))
        .build()
        .unwrap();
    let agent = Arc::new(agent);
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let user = UserId::new("ana").unwrap();
    let session =
        harness.open_session(&user, Some(&SessionId::new(SESSION).unwrap())).await.unwrap();
    let stream = harness.send(&user, &session, "draw the invoice").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("the drawing turn must finish");

    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let inner = LocalSource::builder(APP).sessions(sessions).approvals(approvals).build().unwrap();
    let source =
        Arc::new(Sending { inner, busy: AtomicBool::new(false), asked: Mutex::new(Vec::new()) });
    let mut builder =
        SurfaceEngine::builder(Arc::clone(&source) as Arc<dyn Source>).agent_name(APP, "Finance");
    if register {
        builder = builder.with_catalog(ext());
    }
    (builder.build(), source, dir)
}

fn send() -> Command {
    let mut values = Map::new();
    values.insert("/to".into(), json!("ana@example.com"));
    Command::SubmitDrawn {
        session_id: SESSION.into(),
        surface_id: "invoice-10".into(),
        version: 1,
        action_id: "send".into(),
        values,
    }
}

fn web() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

/// A form inside an extension container is sent as any form: the stored action, the
/// person's typed field and the stored constant, never an extension component's
/// properties; in a home that does not register the catalog it is not sent.
#[tokio::test]
async fn a_form_inside_an_extension_container_sends_only_its_fields() {
    let (engine, source, _dir) = drawn_home(true).await;
    let outcome = engine.execute(&web(), send()).await.expect("the form is sent");
    assert!(matches!(outcome, CommandOutcome::FormSent { .. }), "{outcome:?}");
    {
        let asked = source.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].tool_name, "send_invoice");
        assert_eq!(asked[0].args, json!({ "to": "ana@example.com", "invoice": "INV-10" }));
    }

    let (engine, source, _dir) = drawn_home(false).await;
    let error = engine.execute(&web(), send()).await.unwrap_err();
    assert!(
        matches!(error, SurfaceError::Invalid(ref why) if why.contains("does not render the catalog")),
        "{error:?}"
    );
    assert!(source.asked.lock().unwrap().is_empty());
}

fn drawn_root(items: &[ThreadItem]) -> &DrawnNode {
    items
        .iter()
        .find_map(|item| match item {
            ThreadItem::Drawn { surface } => surface.root.as_ref(),
            _ => None,
        })
        .expect("the drawing is shown")
}

fn find<'a>(node: &'a DrawnNode, key: &str) -> Option<&'a DrawnNode> {
    if node.key == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, key))
}

/// A home draws an extension component as its home view, checked, with the resolved
/// properties a host renderer reads; a chart comes with the sentence the server wrote for it.
#[tokio::test]
async fn a_home_draws_an_extension_component_as_its_home_view() {
    let items = thread_items(true).await;
    let root = drawn_root(&items);
    let extension = root.extension.as_ref().expect("the metric carries its resolved properties");
    assert_eq!(extension.catalog, EXT_ID);
    assert_eq!(extension.component, "Metric");
    assert_eq!(extension.props["value"], 3, "the binding is resolved");
    assert_eq!(root.children.len(), 1, "one child: what the home shows");
    let view = &root.children[0];
    assert_eq!(view.component, "Row");
    assert_eq!(view.children[0].text.as_deref(), Some("Open incidents"));
    assert_eq!(view.children[0].variant.as_deref(), Some("caption"));
    assert_eq!(view.children[1].text.as_deref(), Some("3"));
    assert_eq!(view.children[1].variant.as_deref(), Some("h3"));

    let trend = envelope_in(
        EXT_ID,
        vec![
            json!({ "id": "root", "component": "Trend", "label": "Latency", "values": { "path": "/v" } }),
        ],
        json!({ "v": [120, 98, 143] }),
    );
    let items = thread_items_of(&trend, true).await;
    let chart = drawn_root(&items).children[0].chart.as_ref().expect("a native chart");
    assert_eq!(chart.kind, ChartKind::Sparkline);
    assert_eq!(chart.series[0].y.len(), 3);
    assert_eq!(chart.summary, "Trend chart of Latency, 3 points, from 98 to 143");
}

/// The components an extension container holds are drawn where its home view puts them, so
/// a form inside pages keeps its fields in the tree the send reads.
#[tokio::test]
async fn held_components_are_drawn_where_the_home_view_puts_them() {
    let pages = envelope_in(
        EXT_ID,
        vec![
            json!({ "id": "root", "component": "Pages",
                    "pages": [{ "title": "Details", "child": "to" }, { "title": "Note", "child": "note" }] }),
            json!({ "id": "to", "component": "TextField", "label": "To", "value": { "path": "/to" } }),
            json!({ "id": "note", "component": "Text", "text": "Sent on the 10th" }),
        ],
        json!({ "to": "" }),
    );
    let items = thread_items_of(&pages, true).await;
    let root = drawn_root(&items);
    assert_eq!(root.extension.as_ref().unwrap().children, ["to", "note"]);
    let tabs = &root.children[0];
    assert_eq!(tabs.component, "Tabs");
    assert_eq!(tabs.tabs, ["Details", "Note"]);
    let to = find(root, "to").expect("the field is in the tree");
    assert_eq!(to.field.as_ref().unwrap().path, "/to");
    assert_eq!(find(root, "note").unwrap().text.as_deref(), Some("Sent on the 10th"));
}
