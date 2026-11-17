//! 3D scenes and depth effects in extension catalogs (spec 3.3 §6.1 `genui`, §7.4 rule 5): a
//! scene's every string is checked and its summary written by the server; its limits hold;
//! a pick fills only a choice field the drawing shows and never sends; panels, walls and
//! stages hold only what shows; a scene's text view holds no component; and the home gets the
//! scene, its panels and its text view as nodes.

#![cfg(feature = "genui")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalService, Event, InMemoryApprovalStore,
    RequestedCall, SessionId, StoreApprovalService, UserId,
};
use adk_harness::{Harness, HarnessAgent, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::genui::{
    Catalog, ChildSlot, DepthEffect, DrawRefusal, DrawToolset, DrawnDepth, DrawnKind, DrawnNode,
    DrawnScene, ExtensionComponent, ExtensionSpec, HomeNode, HomePanel, HomeScene,
    MAX_SCENE_INSTANCES, MAX_SCENE_OBJECTS, RefusalKind, ResolvedComponent, ScenePanel,
    SceneSelect, SurfaceDraft, Validator,
};
use adk_surface::{
    Caller, Command, CommandOutcome, LocalSource, Origin, Source, SurfaceEngine, SurfaceKind,
    ThreadItem,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Map, Value, json};

const SCENES_ID: &str = "https://example.com/a2ui/catalogs/scenes/1.0/catalog.json";
const COMMON: &str = "https://a2ui.org/specification/v0_9/common_types.json#/$defs";

/// A fixture component: a name, its schema's own properties, its slots and its home view.
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
        "A fixture component."
    }

    fn home(&self, component: &ResolvedComponent<'_>) -> Result<HomeNode, String> {
        (self.home)(component)
    }
}

fn id_ref() -> Value {
    json!({ "$ref": format!("{COMMON}/ComponentId") })
}

/// The scene a `Space` passes through as written: what an extension projects to. Its pick
/// is a plain string, `pickInto`, since `{"path": ...}` in a property is a binding.
fn scene_of(component: &ResolvedComponent<'_>) -> Result<DrawnScene, String> {
    let written = component.get("scene").cloned().unwrap_or_default();
    let mut scene: DrawnScene =
        serde_json::from_value(written.clone()).map_err(|error| error.to_string())?;
    if let Some(path) = written.get("pickInto").and_then(Value::as_str) {
        scene.select = Some(SceneSelect { path: path.to_string(), note: String::new() });
    }
    Ok(scene)
}

fn depth(effect: DepthEffect, component: &ResolvedComponent<'_>) -> HomeNode {
    HomeNode::Depth {
        depth: DrawnDepth { effect, titles: Vec::new() },
        children: (0..component.children).map(HomeNode::Child).collect(),
    }
}

static SCENES: LazyLock<Catalog> = LazyLock::new(|| {
    let fixtures = vec![
        Fixture {
            name: "Space",
            properties: || json!({ "scene": {}, "panels": {}, "words": {}, "wordsHoldChild": {} }),
            slots: &[ChildSlot::EachOf { array: "panels", child: "child" }],
            home: |space| {
                let scene = scene_of(space)?;
                let panels = space
                    .get("panels")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, panel)| HomePanel {
                        anchor: panel["anchor"].as_str().unwrap_or_default().to_string(),
                        content: HomeNode::Child(index),
                    })
                    .collect();
                let text = if space.get("wordsHoldChild") == Some(&json!(true)) {
                    Some(HomeNode::Child(0))
                } else {
                    space.str("words").map(HomeNode::text)
                };
                Ok(HomeNode::Scene(Box::new(HomeScene { scene, panels, text })))
            },
        },
        Fixture {
            name: "Flip",
            properties: || json!({ "front": id_ref(), "back": id_ref() }),
            slots: &[ChildSlot::One("front"), ChildSlot::One("back")],
            home: |flip| Ok(depth(DepthEffect::Flip, flip)),
        },
        Fixture {
            name: "Wall",
            properties: || json!({ "children": { "type": "array", "items": id_ref() } }),
            slots: &[ChildSlot::Many("children")],
            home: |wall| Ok(depth(DepthEffect::Wall, wall)),
        },
        Fixture {
            name: "Carousel",
            properties: || json!({ "children": { "type": "array", "items": id_ref() } }),
            slots: &[ChildSlot::Many("children")],
            home: |carousel| Ok(depth(DepthEffect::Carousel, carousel)),
        },
        Fixture {
            name: "SceneOnCard",
            properties: || json!({ "scene": {} }),
            slots: &[],
            home: |boxed| {
                Ok(HomeNode::Depth {
                    depth: DrawnDepth { effect: DepthEffect::Tilt, titles: Vec::new() },
                    children: vec![HomeNode::Scene(Box::new(HomeScene::new(scene_of(boxed)?)))],
                })
            },
        },
    ];
    Catalog::extend(ExtensionSpec {
        id: SCENES_ID,
        name: "scenes",
        version: "1.0",
        components: fixtures
            .into_iter()
            .map(|fixture| Arc::new(fixture) as Arc<dyn ExtensionComponent>)
            .collect(),
    })
    .expect("the fixture catalog compiles")
});

fn scenes() -> &'static Catalog {
    &SCENES
}

/// A floor and three docks, each with a state; `picks` gives each dock a value.
fn docks(picks: bool) -> Value {
    let dock = |n: u8, state: &str| {
        let mut dock = json!({
            "id": format!("dock-{n}"), "label": format!("Dock {n}"),
            "shape": { "type": "box", "size": [2, 1, 2] },
            "at": [f64::from(n) * 3.0 - 6.0, 0.5, 0], "state": state
        });
        if picks {
            dock["value"] = json!(format!("dock-{n}"));
        }
        dock
    };
    json!({
        "kind": "space",
        "objects": [
            { "id": "floor", "shape": { "type": "plane", "width": 12, "depth": 6 }, "look": { "color": "muted" } },
            dock(1, "ok"), dock(2, "warning"), dock(3, "critical")
        ],
        "summary": "whatever the extension says"
    })
}

fn space(id: &str, scene: Value) -> Value {
    json!({ "id": id, "component": "Space", "scene": scene })
}

fn draft(kind: DrawnKind, components: Vec<Value>) -> SurfaceDraft {
    SurfaceDraft {
        surface_id: "yard".into(),
        kind,
        title: None,
        components,
        data_model: json!({ "dock": [] }),
        actions: Vec::new(),
    }
}

fn screen(components: Vec<Value>) -> SurfaceDraft {
    draft(DrawnKind::Screen, components)
}

fn check(draft: &SurfaceDraft) -> Result<(), DrawRefusal> {
    Validator::new(scenes()).validate(draft).map(|_| ())
}

fn refused(draft: &SurfaceDraft) -> DrawRefusal {
    match Validator::new(scenes()).validate(draft) {
        Ok(_) => panic!("expected a refusal for {:#}", json!(draft.components)),
        Err(refusal) => refusal,
    }
}

/// `root` holds a scene with `scene` as written.
fn one(scene: Value) -> SurfaceDraft {
    screen(vec![space("root", scene)])
}

/// A dock picker the drawing shows, for scenes whose docks can be picked.
fn picker() -> Value {
    json!({
        "id": "picker", "component": "ChoicePicker", "label": "Dock", "value": { "path": "/dock" },
        "options": [
            { "label": "Dock 1", "value": "dock-1" },
            { "label": "Dock 2", "value": "dock-2" },
            { "label": "Dock 3", "value": "dock-3" }
        ]
    })
}

fn with_pick(mut scene: Value) -> Value {
    scene["pickInto"] = json!("/dock");
    scene
}

#[test]
fn a_scene_within_its_limits_is_shown() {
    check(&one(docks(false))).expect("three labelled docks on a floor are shown");
}

#[test]
fn a_scenes_words_are_checked_like_every_other() {
    let mut scene = docks(false);
    scene["objects"][1]["label"] = json!("Allow once");
    assert_eq!(refused(&one(scene)).kind, RefusalKind::LooksLikeDecision);

    let mut scene = docks(false);
    scene["objects"].as_array_mut().unwrap().push(json!({
        "id": "sign", "shape": { "type": "label", "text": "Approved" }
    }));
    assert_eq!(refused(&one(scene)).kind, RefusalKind::LooksLikeDecision);

    let mut scene = docks(false);
    scene["axes"] = json!({ "y": "https://example.com/approve" });
    assert!(refused(&one(scene)).detail.contains("link"));

    let mut scene = docks(false);
    scene["objects"][1]["label"] = json!("D".repeat(81));
    assert!(refused(&one(scene)).detail.contains("longer than 80"));
}

#[test]
fn a_scene_stays_within_its_limits() {
    let many: Vec<Value> = (0..=MAX_SCENE_OBJECTS)
        .map(|n| json!({ "id": format!("o{n}"), "shape": { "type": "sphere", "radius": 1 } }))
        .collect();
    let detail = refused(&one(json!({ "kind": "space", "objects": many }))).detail;
    assert!(detail.contains("objects"), "{detail}");

    let points: Vec<usize> = (0..(MAX_SCENE_INSTANCES + 1) * 3).map(|n| n % 50).collect();
    let scene = json!({ "kind": "points", "objects": [
        { "id": "cloud", "shape": { "type": "points", "at": points, "size": 0.1 } }
    ] });
    assert!(refused(&one(scene)).detail.contains("bars and points"));

    let heights = vec![1; 97 * 2];
    let scene = json!({ "kind": "surface", "objects": [
        { "id": "land", "shape": { "type": "grid", "columns": 97, "rows": 2, "heights": heights, "width": 10, "depth": 10 } }
    ] });
    assert!(refused(&one(scene)).detail.contains("grid"));

    let mut scene = docks(false);
    scene["lights"] = json!(
        (0..4)
            .map(|_| json!({ "kind": "point", "at": [0, 5, 0], "intensity": 1 }))
            .collect::<Vec<_>>()
    );
    assert!(refused(&one(scene)).detail.contains("lights"));

    let mut scene = docks(false);
    scene["motion"] = json!(
        (0..33)
            .map(|_| json!({ "target": "dock-1", "kind": "spin", "seconds": 4 }))
            .collect::<Vec<_>>()
    );
    assert!(refused(&one(scene)).detail.contains("moves 33"));

    let mut scene = docks(false);
    scene["objects"][1]["at"] = json!([20000, 0, 0]);
    assert!(refused(&one(scene)).detail.contains("beyond"));

    let mut scene = docks(false);
    scene["objects"][1]["look"] = json!({ "opacity": 0.05 });
    assert!(refused(&one(scene)).detail.contains("opacity"));

    // Five scenes through a template: each copy counts.
    let drawing = SurfaceDraft {
        data_model: json!({ "yards": [1, 2, 3, 4, 5] }),
        ..screen(vec![
            json!({ "id": "root", "component": "List", "children": { "componentId": "yard", "path": "/yards" } }),
            space("yard", docks(false)),
        ])
    };
    assert!(refused(&drawing).detail.contains("more than 4 scenes"));
}

#[test]
fn a_scenes_ids_parents_and_targets_hold_together() {
    let mut scene = docks(false);
    scene["objects"][2]["id"] = json!("dock-1");
    assert!(refused(&one(scene)).detail.contains("not unique"));

    let mut scene = docks(false);
    scene["objects"][1]["parent"] = json!("shed");
    assert!(refused(&one(scene)).detail.contains("doesn't have"));

    let mut scene = docks(false);
    scene["objects"][1]["parent"] = json!("floor");
    assert!(refused(&one(scene)).detail.contains("not a group"));

    let scene = json!({ "kind": "space", "objects": [
        { "id": "a", "parent": "b", "shape": { "type": "group" } },
        { "id": "b", "parent": "a", "shape": { "type": "group" } }
    ] });
    assert!(refused(&one(scene)).detail.contains("loop"));

    let mut scene = docks(false);
    scene["motion"] = json!([{ "target": "crane", "kind": "spin", "seconds": 4 }]);
    assert!(refused(&one(scene)).detail.contains("crane"));

    let mut scene = docks(false);
    scene["motion"] =
        json!([{ "target": "dock-1", "kind": "path", "seconds": 4, "path": [0, 0, 0] }]);
    assert!(refused(&one(scene)).detail.contains("points"));
}

#[test]
fn a_state_or_a_pick_comes_with_a_label() {
    let mut scene = docks(false);
    scene["objects"][1].as_object_mut().unwrap().remove("label");
    assert!(refused(&one(scene)).detail.contains("a state is a word"));

    let mut scene = with_pick(docks(true));
    scene["objects"][1].as_object_mut().unwrap().remove("label");
    scene["objects"][1].as_object_mut().unwrap().remove("state");
    let drawing = screen(vec![
        json!({ "id": "root", "component": "Column", "children": ["map", "picker"] }),
        space("map", scene),
        picker(),
    ]);
    assert!(refused(&drawing).detail.contains("sees what they pick"));
}

#[test]
fn a_pick_fills_only_a_choice_field_the_drawing_shows() {
    let shown = |scene: Value, picker: Value| {
        screen(vec![
            json!({ "id": "root", "component": "Column", "children": ["map", "picker"] }),
            space("map", scene),
            picker,
        ])
    };
    check(&shown(with_pick(docks(true)), picker())).expect("a pick into a shown picker");

    // Values but nothing to fill, and something to fill but no values.
    assert!(refused(&shown(docks(true), picker())).detail.contains("fills no field"));
    assert!(refused(&shown(with_pick(docks(false)), picker())).detail.contains("no object"));

    // No picker for the path.
    let alone = screen(vec![space("root", with_pick(docks(true)))]);
    assert!(refused(&alone).detail.contains("no choice field the drawing shows"));

    // A picker the drawing has but never shows.
    let unshown = screen(vec![space("root", with_pick(docks(true))), picker()]);
    assert!(refused(&unshown).detail.contains("no choice field the drawing shows"));

    // A text field at the path is not a choice.
    let typed = json!({ "id": "picker", "component": "TextField", "label": "Dock", "value": { "path": "/dock" } });
    assert!(refused(&shown(with_pick(docks(true)), typed)).detail.contains("no choice field"));

    // A value the picker doesn't offer.
    let mut scene = with_pick(docks(true));
    scene["objects"][3]["value"] = json!("dock-9");
    assert!(refused(&shown(scene, picker())).detail.contains("not one of /dock's options"));

    // A card has no fields at all.
    let card = draft(DrawnKind::Card, vec![space("root", with_pick(docks(true)))]);
    assert!(refused(&card).detail.contains("no fields"));
}

#[test]
fn panels_walls_and_stages_hold_only_what_shows() {
    let note = json!({ "id": "note", "component": "Text", "text": "Free since 09:00" });
    let mut paneled = json!({ "id": "root", "component": "Space", "scene": docks(false),
        "panels": [{ "anchor": "dock-3", "child": "note" }] });
    check(&screen(vec![paneled.clone(), note.clone()])).expect("a panel of text is shown");

    let button = json!({ "id": "note", "component": "Button", "child": "label",
        "action": { "event": { "name": "reserve" } } });
    let label = json!({ "id": "label", "component": "Text", "text": "Reserve" });
    let mut with_button = screen(vec![paneled.clone(), button, label]);
    with_button.actions = serde_json::from_value(json!([
        { "id": "reserve", "label": "Reserve", "tool": "reserve_dock", "args": {} }
    ]))
    .unwrap();
    let detail = refused(&with_button).detail;
    assert!(detail.contains("Button 'note'"), "{detail}");

    paneled["panels"][0]["anchor"] = json!("crane");
    assert!(refused(&screen(vec![paneled, note.clone()])).detail.contains("crane"));

    let held_field = json!({ "id": "note", "component": "Column", "children": ["when"] });
    let field = json!({ "id": "when", "component": "TextField", "label": "When", "value": { "path": "/when" } });
    let wall = json!({ "id": "root", "component": "Wall", "children": ["note"] });
    let detail = refused(&screen(vec![wall, held_field.clone(), field.clone()])).detail;
    assert!(detail.contains("TextField 'when'"), "{detail}");

    // A carousel turns only when the person turns it: it may hold a form.
    let carousel = json!({ "id": "root", "component": "Carousel", "children": ["note"] });
    check(&screen(vec![carousel, held_field, field])).expect("a carousel may hold a field");
}

#[test]
fn a_text_view_says_it_in_words_and_effects_dont_nest() {
    let note = json!({ "id": "note", "component": "Text", "text": "Free since 09:00" });
    let holds = json!({ "id": "root", "component": "Space", "scene": docks(false),
        "panels": [{ "anchor": "dock-3", "child": "note" }], "wordsHoldChild": true });
    assert!(refused(&screen(vec![holds, note])).detail.contains("text view holds a component"));

    let boxed = json!({ "id": "root", "component": "SceneOnCard", "scene": docks(false) });
    assert!(refused(&screen(vec![boxed])).detail.contains("scene inside"));

    let front = json!({ "id": "front", "component": "Text", "text": "p95 840 ms" });
    let flip_one = json!({ "id": "root", "component": "Flip", "front": "front" });
    assert!(refused(&screen(vec![flip_one, front])).detail.contains("flip 2"));
}

// The home's nodes, and a send after a pick.

const APP: &str = "home";
const SESSION: &str = "yard-thread";

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

struct ReserveDock;

#[async_trait]
impl adk_core::Tool for ReserveDock {
    fn name(&self) -> &str {
        "reserve_dock"
    }

    fn description(&self) -> &str {
        "Reserves a dock."
    }

    async fn execute(
        &self,
        _: Arc<dyn adk_core::ToolContext>,
        _: Value,
    ) -> adk_core::Result<Value> {
        panic!("reserve_dock never runs here: a send asks the person first");
    }
}

/// The yard: docks to pick, a panel beside the free one, words for the text view, the dock
/// picker and a Reserve button.
fn yard() -> Value {
    json!({
        "surfaceId": "yard",
        "title": "Loading yard",
        "components": [
            { "id": "root", "component": "Column", "children": ["map", "picker", "reserve"] },
            { "id": "map", "component": "Space", "scene": with_pick(docks(true)),
              "panels": [{ "anchor": "dock-3", "child": "note" }],
              "words": "Dock 1 is fine, dock 2 needs a look, dock 3 is critical." },
            { "id": "note", "component": "Text", "text": "Closed since 09:00" },
            picker(),
            { "id": "reserve", "component": "Button", "child": "reserve-label",
              "action": { "event": { "name": "reserve" } } },
            { "id": "reserve-label", "component": "Text", "text": "Reserve" }
        ],
        "dataModel": { "dock": [] },
        "actions": [{ "id": "reserve", "label": "Reserve", "tool": "reserve_dock",
                      "args": { "dock": { "path": "/dock" } } }]
    })
}

async fn drawn_yard() -> (SurfaceEngine, Arc<Sending>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let agent = HarnessAgent::builder("yard")
        .model(Arc::new(ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("render_screen", yard()),
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
        .toolset(Arc::new(DrawToolset::new(Validator::new(scenes()))))
        .tool(Arc::new(ReserveDock))
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
    let stream = harness.send(&user, &session, "show me the yard").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("the drawing turn must finish");
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let inner = LocalSource::builder(APP).sessions(sessions).approvals(approvals).build().unwrap();
    let source =
        Arc::new(Sending { inner, busy: AtomicBool::new(false), asked: Mutex::new(Vec::new()) });
    let engine = SurfaceEngine::builder(Arc::clone(&source) as Arc<dyn Source>)
        .agent_name(APP, "Yard")
        .with_catalog(scenes())
        .build();
    (engine, source, dir)
}

fn web() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn find<'a>(node: &'a DrawnNode, key: &str) -> Option<&'a DrawnNode> {
    if node.key == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, key))
}

#[tokio::test]
async fn a_home_gets_the_scene_its_panels_and_its_text_view_and_a_pick_sends_as_a_field() {
    let (engine, source, _dir) = drawn_yard().await;
    let items: Vec<ThreadItem> = engine
        .thread(&web(), SESSION)
        .await
        .unwrap()
        .into_iter()
        .flat_map(|entry| entry.items)
        .collect();
    let root = items
        .iter()
        .find_map(|item| match item {
            ThreadItem::Drawn { surface } => surface.root.as_ref(),
            _ => None,
        })
        .expect("the yard is drawn");
    let map = find(root, "map").expect("the scene's component");
    assert_eq!(map.extension.as_ref().unwrap().component, "Space");
    let node = &map.children[0];
    assert_eq!(node.component, "Scene");
    let scene = node.scene.as_ref().expect("the scene, for the home to draw");
    assert_eq!(scene.summary, "3D scene of Dock 1, Dock 2 and Dock 3, 4 objects");
    assert_eq!(scene.select.as_ref().unwrap().note, "Picking one fills Dock. Nothing is sent.");
    assert_eq!(scene.panels, vec![ScenePanel { anchor: "dock-3".into(), key: "note".into() }]);
    assert_eq!(node.children[0].key, "note");
    assert_eq!(node.children[0].text.as_deref(), Some("Closed since 09:00"));
    let words = scene.text_view.as_deref().expect("a text view");
    assert_eq!(
        find(node, words).and_then(|text| text.text.as_deref()),
        Some("Dock 1 is fine, dock 2 needs a look, dock 3 is critical.")
    );

    // Picking dock 3 fills the picker; Send asks for exactly that.
    let mut values = Map::new();
    values.insert("/dock".into(), json!(["dock-3"]));
    let outcome = engine
        .execute(
            &web(),
            Command::SubmitDrawn {
                session_id: SESSION.into(),
                surface_id: "yard".into(),
                version: 1,
                action_id: "reserve".into(),
                values,
            },
        )
        .await
        .expect("the form is sent");
    assert!(matches!(outcome, CommandOutcome::FormSent { .. }), "{outcome:?}");
    let asked = source.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].tool_name, "reserve_dock");
    assert_eq!(asked[0].args, json!({ "dock": ["dock-3"] }));
}
